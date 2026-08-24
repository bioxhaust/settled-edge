#![forbid(unsafe_code)]
//! Wiring, the deterministic replay harness, and the report.
//!
//! # What the harness is for
//!
//! It is the only place where the layers meet, and it is deliberately the only
//! place that knows a file exists. `arb-venue` reads the directory, everything
//! above it takes `&str` and slices. That is what makes the replay deterministic:
//! run it twice on the same directory and the two reports are byte-identical, so a
//! diff of two reports is a statement about the *code*, not about the clock or the
//! filesystem's iteration order.
//!
//! # The clock in a replay
//!
//! A replay has no wall clock, and inventing one would be the single most
//! dishonest thing this binary could do. A fixture was never observed, so its
//! `observed_at` is [`LocalTs::EPOCH`]; stamping "now" against it would make every
//! invented book look freshly measured and sail through every staleness check.
//!
//! So the harness pins `now` to [`LocalTs::EPOCH`] as well. Ages come out as zero
//! and the staleness detector reports `fresh` — **not** because the data is fresh,
//! but because staleness is not the mechanism that should refuse it. Provenance is:
//! every row carries [`Provenance::SynthesizedUnverified`] and every row is
//! therefore unrankable. The distinction matters, because it is the difference
//! between "this number is old" and "this number was never a measurement".
//!
//! In live operation `now` comes from the caller, the adapter stamps real
//! observation times, and the same staleness code does real work.

use std::fmt::Write as _;
use std::path::Path;

use arb_detect::Staleness;
use arb_num::{Bps, Money, Qty, Rate};
use arb_scan::{scan, LegInput, Opportunity, ScanConfig, ScanError};
use arb_types::{LocalTs, Millis, Provenance, QualityFlags, VenueKey};
use arb_venue::{ExchangeAdapter, FixtureAdapter, VenueError};

/// What can go wrong between a directory and a report.
#[derive(Debug)]
pub enum EngineError {
    /// The fixtures could not be read or parsed. Fatal: a replay over an unknown
    /// subset of the fixtures is not a replay.
    Venue(VenueError),
    /// A leg could not be assembled, or the configuration is unusable.
    Scan(ScanError),
    /// The directory held fixtures but no two of them form a trade. Reported
    /// rather than printed as an empty table, which reads like "no opportunity".
    NoPairs,
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineError::Venue(e) => write!(f, "fixtures: {e}"),
            EngineError::Scan(e) => write!(f, "scan: {e}"),
            EngineError::NoPairs => f.write_str(
                "no two instruments in this directory form a trade \
                 (need the same base/quote on two different venues)",
            ),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<VenueError> for EngineError {
    fn from(e: VenueError) -> Self {
        EngineError::Venue(e)
    }
}

impl From<ScanError> for EngineError {
    fn from(e: ScanError) -> Self {
        EngineError::Scan(e)
    }
}

/// A whole replay: what was loaded, what was ranked, what was refused.
pub struct Replay {
    /// Ranked rows, best first, unrankable rows sunk to the bottom.
    pub rows: Vec<Opportunity>,
    /// Pairs that are trades but whose arithmetic failed. Never silently dropped.
    pub errors: Vec<(VenueKey, VenueKey, ScanError)>,
    /// How many instruments were loaded, across how many venues.
    pub instruments: usize,
    /// How many venues contributed at least one instrument.
    pub venues: usize,
    /// Instruments that were loaded but could not become a leg, with the reason.
    ///
    /// Reported rather than skipped. An instrument that quietly vanishes between
    /// "16 fixtures loaded" and a table of fewer pairs than that implies is
    /// indistinguishable from a venue with no opportunity, and the operator would
    /// draw the wrong conclusion from a shorter table.
    pub excluded: Vec<(VenueKey, VenueError)>,
}

/// The configuration a replay runs under.
///
/// Pinned, not defaulted from the environment: a report whose inputs depend on an
/// env var is not reproducible, and reproducibility is the only thing a replay is
/// for. Change these by editing the call site, where the change is visible in a
/// diff.
pub fn replay_config() -> Result<ScanConfig, ScanError> {
    ScanConfig::conservative(
        // 100 000 quote units. Large enough that depth matters — which is the whole
        // point — and small enough to be a size a real account might take.
        Money::from_int(100_000)?,
        // 3x. Not the venue maximum: the distance-to-liquidation column is only
        // informative at a leverage somebody would actually use.
        Rate::from_int(3)?,
        // Seven days. Amortising a round trip over one 8h settlement flatters it
        // threefold; a week is a holding period a carry trade is actually held for.
        Millis::from_secs(7 * 86_400),
        // See the module docs: the replay clock is the fixtures' own clock.
        LocalTs::EPOCH,
    )
}

/// Load a fixture directory, assemble every leg, and rank every pair.
pub fn replay(dir: &Path, cfg: &ScanConfig) -> Result<Replay, EngineError> {
    let adapters = FixtureAdapter::load_dir(dir)?;
    let venues = adapters.len();

    let mut legs = Vec::new();
    let mut excluded = Vec::new();
    let mut instruments = 0usize;
    for adapter in &adapters {
        for instrument in adapter.instruments()? {
            instruments += 1;
            let fee_quality = adapter.fee_quality(&instrument.vkey.key)?;
            // A leg without a book cannot be sized, and an unsized leg cannot
            // produce a number that means anything — so it is excluded rather
            // than priced off the touch. `Unsupported` is the adapter saying the
            // fixture never carried the section; any other error is a real
            // failure and stays fatal.
            match LegInput::gather(adapter, &instrument.vkey.key, cfg.book_depth, fee_quality) {
                Ok(leg) => legs.push(leg),
                Err(ScanError::Venue(e @ VenueError::Unsupported(_))) => {
                    excluded.push((instrument.vkey, e));
                }
                Err(e) => return Err(EngineError::Scan(e)),
            }
        }
    }

    let (rows, errors) = scan(&legs, cfg);
    if rows.is_empty() && errors.is_empty() {
        return Err(EngineError::NoPairs);
    }

    Ok(Replay {
        rows,
        errors,
        instruments,
        venues,
        excluded,
    })
}

// ---------------------------------------------------------------------------
// Formatting. Integer arithmetic only — hard rule 2 applies to the report as
// much as to the engine, and a float here would be a float in the one number
// the operator actually reads.
// ---------------------------------------------------------------------------

/// A [`Rate`] as a percentage with two decimals.
///
/// `Rate` is scaled 1e12, so a percentage is the raw over 1e10. Done on the raw
/// integer: `as f64 * 100.0` would be the one float in the codebase and it would
/// be in the headline column.
pub fn pct(r: Rate) -> String {
    let raw = r.raw();
    let neg = raw < 0;
    let a = raw.unsigned_abs();
    let whole = a / 10_000_000_000;
    let frac = (a % 10_000_000_000) / 100_000_000;
    format!("{}{whole}.{frac:02}%", if neg { "-" } else { "" })
}

/// A [`Bps`] with two decimals. `Bps` is scaled 1e8.
pub fn bps(b: Bps) -> String {
    let raw = b.raw();
    let neg = raw < 0;
    let a = raw.unsigned_abs();
    let whole = a / 100_000_000;
    let frac = (a % 100_000_000) / 1_000_000;
    format!("{}{whole}.{frac:02}", if neg { "-" } else { "" })
}

/// A [`Money`] with two decimals. Scaled 1e12.
pub fn money(m: Money) -> String {
    let raw = m.raw();
    let neg = raw < 0;
    let a = raw.unsigned_abs();
    let whole = a / 1_000_000_000_000;
    let frac = (a % 1_000_000_000_000) / 10_000_000_000;
    format!("{}{whole}.{frac:02}", if neg { "-" } else { "" })
}

/// A [`Qty`] with six decimals — enough for a BTC lot size of 0.001 and for the
/// 1e-6 lot sizes the smaller venues use.
pub fn qty(q: Qty) -> String {
    let raw = q.raw();
    let neg = raw < 0;
    let a = raw.unsigned_abs();
    let whole = a / 1_000_000_000_000;
    let frac = (a % 1_000_000_000_000) / 1_000_000;
    format!("{}{whole}.{frac:06}", if neg { "-" } else { "" })
}

/// The set flags, pipe-separated, or `-`.
pub fn flags(f: QualityFlags) -> String {
    let mut out = String::new();
    for (flag, name) in QualityFlags::NAMED {
        if f.contains(flag) {
            if !out.is_empty() {
                out.push('|');
            }
            out.push_str(name);
        }
    }
    if out.is_empty() {
        out.push('-');
    }
    out
}

/// Provenance, short form. Present on every row because it is the difference
/// between a measurement and an invention.
fn provenance(p: Provenance) -> &'static str {
    match p {
        Provenance::LiveMeasured => "measured",
        Provenance::RecordedFixture => "recorded",
        Provenance::SynthesizedUnverified => "INVENTED",
    }
}

fn staleness(s: Staleness) -> &'static str {
    match s {
        Staleness::Fresh => "fresh",
        Staleness::Warn => "warn",
        Staleness::Stale => "stale",
        Staleness::Dead => "dead",
    }
}

/// Render the whole report.
///
/// Deliberately one string rather than a stream of `println!`: the caller decides
/// where it goes, and a test can assert on it without capturing stdout.
pub fn render(r: &Replay, cfg: &ScanConfig) -> String {
    let mut s = String::new();

    let _ = writeln!(s, "arb-engine replay");
    let _ = writeln!(
        s,
        "  loaded         {} instruments across {} venues",
        r.instruments, r.venues
    );
    let _ = writeln!(s, "  target size    {} quote units", money(cfg.target_notional));
    let _ = writeln!(s, "  leverage       {}x", cfg.leverage);
    let _ = writeln!(
        s,
        "  horizon        {} days (round-trip cost is amortised over this)",
        cfg.horizon.millis() / 86_400_000
    );
    let _ = writeln!(s, "  slip budget    {} bps", bps(cfg.slippage_budget.bps()));
    let rankable = r.rows.iter().filter(|x| x.is_rankable()).count();
    let _ = writeln!(
        s,
        "  pairs          {} priced, {} rankable, {} refused by arithmetic",
        r.rows.len(),
        rankable,
        r.errors.len()
    );
    if !r.excluded.is_empty() {
        let _ = writeln!(
            s,
            "  excluded       {} instrument{} could not be priced:",
            r.excluded.len(),
            if r.excluded.len() == 1 { "" } else { "s" }
        );
        for (vkey, e) in &r.excluded {
            let _ = writeln!(
                s,
                "                   {} {}/{} — {e}",
                vkey.venue.as_str(),
                vkey.key.base.as_str(),
                vkey.key.quote.as_str()
            );
        }
    }
    let _ = writeln!(s);

    let _ = writeln!(
        s,
        "{:<11} {:<8} {:<8} {:>12} {:>10} {:>10} {:>8} {:>8} {:>14} {:>10} {:>5}  {}",
        "PAIR", "SHORT", "LONG", "SIZE", "NET APR", "GROSS APR", "FEES", "SLIP", "MARGIN",
        "DIST LIQ", "BE", "MARKS"
    );
    let _ = writeln!(s, "{}", "-".repeat(140));

    for row in &r.rows {
        let pair = format!(
            "{}/{}",
            row.short.vkey.key.base.as_str(),
            row.short.vkey.key.quote.as_str()
        );
        let dist = match row.distance_to_liq {
            Some(d) => bps(d),
            None => "n/a".to_string(),
        };
        let be = match row.periods_to_breakeven {
            Some(n) => n.to_string(),
            None => "never".to_string(),
        };
        let _ = writeln!(
            s,
            "{:<11} {:<8} {:<8} {:>12} {:>10} {:>10} {:>8} {:>8} {:>14} {:>10} {:>5}  {} {}",
            pair,
            row.short.vkey.venue.as_str(),
            row.long.vkey.venue.as_str(),
            qty(row.size),
            pct(row.net_apr),
            pct(row.gross_apr),
            bps(row.fee_bps),
            bps(row.slippage_bps),
            money(row.margin),
            dist,
            be,
            provenance(row.quality.provenance),
            flags(row.quality.flags),
        );
        if !row.is_rankable() {
            for b in &row.blocked {
                let _ = writeln!(s, "{:<11} └─ not rankable: {b}", "");
            }
        }
    }

    if !r.errors.is_empty() {
        let _ = writeln!(s);
        let _ = writeln!(s, "REFUSED — a trade, but the arithmetic would not close:");
        for (a, b, e) in &r.errors {
            let _ = writeln!(
                s,
                "  {} {} <-> {} {}: {e}",
                a.venue.as_str(),
                a.key.base.as_str(),
                b.venue.as_str(),
                b.key.base.as_str()
            );
        }
    }

    let _ = writeln!(s);
    if rankable == 0 {
        let _ = writeln!(
            s,
            "NOTHING HERE IS TRADEABLE. Every row above is priced from fixtures that were\n\
             invented from memory of exchange API shapes and never compared against a live\n\
             response. The arithmetic is real; the inputs are not. See FIXTURES-UNVERIFIED.md\n\
             for what has to be confirmed before any of these numbers means anything."
        );
    }
    let _ = writeln!(
        s,
        "\nNET APR is gross funding minus four fee legs minus depth cost both ways,\n\
         amortised over the horizon, for the SIZE in that row and no other size.\n\
         Exit slippage is assumed equal to entry slippage — see OPEN.md."
    );

    let _ = writeln!(s, "\nPER-LEG DETAIL");
    for row in &r.rows {
        let _ = writeln!(
            s,
            "  {} {} <-> {} {}",
            row.short.vkey.venue.as_str(),
            row.short.vkey.key.base.as_str(),
            row.long.vkey.venue.as_str(),
            row.long.vkey.key.base.as_str()
        );
        for (tag, leg) in [("short", &row.short), ("long", &row.long)] {
            let _ = writeln!(
                s,
                "    {tag:<5} {:<8} pub_rate {:>16} annual {:>10} touch {:>16} vwap {:>16} slip {:>7} cap {:>12} book {:<5} funding {:<5}",
                leg.vkey.venue.as_str(),
                leg.published_rate,
                pct(leg.annual_receipt),
                leg.touch,
                leg.vwap,
                bps(leg.slippage),
                qty(leg.capacity),
                staleness(leg.book_age),
                match leg.funding_age {
                    Some(a) => staleness(a),
                    None => "spot",
                },
            );
        }
    }

    s
}

// ===========================================================================
// Tests
//
// These run the fixtures that ship with the repository through the real
// adapter, the real scanner and the real report. They assert **invariants**
// rather than values, because the fixture numbers were invented and an
// assertion on an invented number tests nothing but that nobody retyped the
// file. An invariant holds whatever the fixtures say, so it keeps working when
// the fixtures are replaced by measurements.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use arb_scan::Unrankable;

    fn fixtures() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
    }

    fn run() -> (Replay, ScanConfig) {
        let cfg = replay_config().expect("the pinned config is usable");
        let r = replay(&fixtures(), &cfg).expect("the shipped fixtures replay");
        (r, cfg)
    }

    /// The whole directory loads, and the census is asserted so that a fixture
    /// silently failing to parse shows up here rather than as a shorter table.
    #[test]
    fn the_shipped_fixtures_replay_into_a_table() {
        let (r, _) = run();
        assert_eq!(r.instruments, 16, "16 fixture files ship with this repo");
        assert_eq!(r.venues, 8, "the brief names eight venues");
        assert!(!r.rows.is_empty(), "eight venues on BTC and ETH must pair");
    }

    /// The invariant that catches a units error.
    ///
    /// Whatever the fixtures say, a row sized to a 100 000-unit target must have
    /// a notional at or just under 100 000 — under, because the size is rounded
    /// **down** to the lot. It cannot be 1 000, and it cannot be 10 000 000.
    ///
    /// This is the assertion that `decisions/019` was found by: with the size in
    /// contracts and the notional taken off the contract count, OKX's 0.01 BTC
    /// contract produced 994.64 against a 100 000 target and Gate's 0.0001
    /// produced 9.95. Nothing else in the report looked wrong.
    ///
    /// Rows where depth bound before the target are excluded from the bound, but
    /// only those, and only downwards — a row may legitimately be smaller than
    /// asked for, never larger.
    #[test]
    fn no_row_is_sized_to_a_notional_the_configuration_did_not_ask_for() {
        let (r, cfg) = run();
        let target = cfg.target_notional;
        // One lot of BTC at ~68 000 is ~68 units of quote; ETH's 0.1 lot is ~334.
        // 1% of target is comfortably above both and far below any unit error,
        // the smallest of which is 100x.
        let floor = Money::from_raw(target.raw() / 100 * 99);

        for row in &r.rows {
            assert!(
                row.notional <= target,
                "{} {} <-> {} notional {} exceeds the {} asked for",
                row.short.vkey.venue.as_str(),
                row.short.vkey.key.base.as_str(),
                row.long.vkey.venue.as_str(),
                money(row.notional),
                money(target),
            );
            let depth_bound = row
                .blocked
                .iter()
                .any(|b| matches!(b, Unrankable::SizeExceedsDepth { .. }));
            if !depth_bound {
                assert!(
                    row.notional >= floor,
                    "{} {} <-> {} notional {} is far below the {} asked for, and \
                     depth did not bind — that is a units error, not a lot rounding",
                    row.short.vkey.venue.as_str(),
                    row.short.vkey.key.base.as_str(),
                    row.long.vkey.venue.as_str(),
                    money(row.notional),
                    money(target),
                );
            }
        }
    }

    /// Margin follows from notional and leverage and from nothing else.
    ///
    /// Two claims. The exact one: the row's margin is the sum of its legs' — an
    /// identity, so it is asserted as one.
    ///
    /// The banded one: the total sits within 1% of two legs of the target at the
    /// configured leverage. It is a band and not an equality because a hedge means
    /// equal size in the **base** asset, and two venues do not price the base
    /// identically — so the two legs' quote notionals differ by the basis, which is
    /// the trade itself. On the shipped fixtures MEXC's bid VWAP is 68 122.8 while
    /// Binance's ask VWAP is 68 119.75, and the margin comes out 0.85 over
    /// two-thirds of 100 000: 1.3 bp, and it is the spread, not an error.
    ///
    /// 1% is far above that and far below the smallest units error, which is 100×
    /// (`decisions/019`). The band would not have passed before that fix: margin
    /// then ranged from 4.69 to 261 447.99 on the same 100 000 target.
    #[test]
    fn margin_is_two_legs_of_the_target_at_the_configured_leverage() {
        let (r, cfg) = run();
        // On the raws rather than through `margin_required`, so this is not the
        // implementation checked against itself. Rate is scaled 1e12.
        let two_legs = cfg.target_notional.raw() * 2 * 1_000_000_000_000 / cfg.leverage.raw();
        let hi = Money::from_raw(two_legs / 100 * 101);
        let lo = Money::from_raw(two_legs / 100 * 99);

        for row in &r.rows {
            let pair = format!(
                "{} <-> {}",
                row.short.vkey.venue.as_str(),
                row.long.vkey.venue.as_str()
            );
            assert_eq!(
                row.margin,
                row.short
                    .margin
                    .checked_add(row.long.margin)
                    .expect("no overflow"),
                "{pair}: the row's margin is not its legs' margin"
            );
            assert!(
                row.margin <= hi,
                "{pair}: margin {} is more than 1% above two legs of {} at {}x",
                money(row.margin),
                money(cfg.target_notional),
                cfg.leverage,
            );
            let depth_bound = row
                .blocked
                .iter()
                .any(|b| matches!(b, Unrankable::SizeExceedsDepth { .. }));
            if !depth_bound {
                assert!(
                    row.margin >= lo,
                    "{pair}: margin {} is more than 1% below two legs of {} at {}x, \
                     and depth did not bind",
                    money(row.margin),
                    money(cfg.target_notional),
                    cfg.leverage,
                );
            }
        }
    }

    /// Nothing in a fixture replay may be rankable. This is the honesty of the
    /// number expressed as a test: the arithmetic is real, the inputs are not,
    /// and no configuration of invented data may present itself as tradeable.
    #[test]
    fn not_one_row_of_a_fixture_replay_is_rankable() {
        let (r, _) = run();
        for row in &r.rows {
            assert!(
                !row.is_rankable(),
                "{} <-> {} claims to be rankable off invented fixtures",
                row.short.vkey.venue.as_str(),
                row.long.vkey.venue.as_str(),
            );
            assert_eq!(row.quality.provenance, Provenance::SynthesizedUnverified);
            assert!(
                row.blocked
                    .iter()
                    .any(|b| matches!(b, Unrankable::Unverified(_))),
                "provenance must appear as a stated reason, not only as a flag"
            );
        }
    }

    /// An instrument that cannot be priced is reported, not dropped. A shorter
    /// table with no explanation reads as "no opportunity here", which is the
    /// opposite conclusion from "we could not price this".
    #[test]
    fn an_instrument_that_cannot_be_priced_is_named_in_the_report() {
        let (r, cfg) = run();
        assert!(
            !r.excluded.is_empty(),
            "one shipped fixture carries no [book] section"
        );
        let out = render(&r, &cfg);
        for (vkey, _) in &r.excluded {
            assert!(
                out.contains(vkey.venue.as_str()),
                "excluded venue {} is absent from the rendered report",
                vkey.venue.as_str()
            );
        }
        assert!(out.contains("could not be priced"));
    }

    /// Two runs over the same directory produce the same bytes. Without this a
    /// diff of two reports is a statement about `read_dir` ordering rather than
    /// about the code.
    #[test]
    fn the_same_directory_replays_to_the_same_bytes() {
        let (a, cfg) = run();
        let (b, _) = run();
        assert_eq!(render(&a, &cfg), render(&b, &cfg));
    }

    /// The report must say so in words, not only in flag columns. An operator who
    /// skims the table and misses `INVENTED` has been misled by us.
    #[test]
    fn the_report_says_in_words_that_nothing_is_tradeable() {
        let (r, cfg) = run();
        let out = render(&r, &cfg);
        assert!(out.contains("NOTHING HERE IS TRADEABLE"));
        assert!(out.contains("FIXTURES-UNVERIFIED.md"));
        assert!(out.contains("INVENTED"));
    }

    /// A directory with no fixtures is an error. An empty table would be read as
    /// "scanned, found nothing".
    #[test]
    fn an_empty_directory_is_an_error_not_an_empty_table() {
        let cfg = replay_config().expect("config");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert!(replay(&dir, &cfg).is_err());
    }

    /// Percent and bps formatting is integer arithmetic, so it needs pinning at
    /// the boundaries where integer division truncates.
    #[test]
    fn the_formatters_do_not_lie_about_small_and_negative_numbers() {
        assert_eq!(pct(Rate::from_raw(0)), "0.00%");
        // 0.1095 as a Rate is 10.95%.
        assert_eq!(pct(Rate::from_raw(109_500_000_000)), "10.95%");
        assert_eq!(pct(Rate::from_raw(-109_500_000_000)), "-10.95%");
        // Truncation, not rounding: 10.959% must not print as 10.96%.
        assert_eq!(pct(Rate::from_raw(109_590_000_000)), "10.95%");
        assert_eq!(bps(Bps::from_raw(0)), "0.00");
        // Bps is scaled 1e8, so 22 bps is 2_200_000_000.
        assert_eq!(bps(Bps::from_raw(2_200_000_000)), "22.00");
        assert_eq!(bps(Bps::from_raw(-2_200_000_000)), "-22.00");
        assert_eq!(money(Money::from_raw(1_000_000_000_000)), "1.00");
        assert_eq!(qty(Qty::from_raw(1_000_000)), "0.000001");
    }
}
