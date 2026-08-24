#![forbid(unsafe_code)]
//! The live path: real venues, real numbers, `Provenance::LiveMeasured`.
//!
//! Everything in [`crate::replay`] runs on fixtures that were invented from memory
//! of exchange API shapes. This module is the other half: it fetches public market
//! data over HTTPS and runs the same assembly and detection code over it.
//!
//! # What is actually measured here, and what is not
//!
//! Measured, from the venue's own endpoint:
//!
//! - the order book, with the venue's update id and its own timestamp;
//! - the funding rate, the settlement interval, the next settlement time, mark
//!   and index price;
//! - tick size, lot size and minimum notional.
//!
//! **Not** measured:
//!
//! - the **fee tier**. No public endpoint reveals which tier an account is on, so
//!   [`arb_live::venues::bybit`] reports the venue's worst published tier and
//!   marks it [`Provenance::SynthesizedUnverified`] with
//!   [`QualityFlags::FEE_TIER_ASSUMED`]. The assumption is one-directional: the
//!   real fee can only be lower, so a row's `net_apr` can only be understated. It
//!   rides on the row as a flag the operator can see, not as a verdict that
//!   deletes it — see [`arb_scan::LegInput::quality`] for why that distinction is
//!   drawn there rather than here.
//! - **rate history**. A public ticker carries the current rate, not the last
//!   twenty settled ones, so nothing here can say whether a rate persists.
//! - **borrow cost**, which only matters once a spot leg exists.
//!
//! # One venue is not an arbitrage
//!
//! With a single adapter connected there are no cross-venue pairs, so the ranked
//! table is empty by construction, not by market condition. That, and not the fee
//! tier, is what stands between this module and a ranked row today. The per-leg
//! output below is the useful part until a second adapter lands, and it is
//! deliberately detailed enough to check an annualisation by hand.
//!
//! # Why the clock is read twice
//!
//! `ScanConfig::now` has to be no earlier than the observations it judges. Reading
//! it once before the fetches would make every book arrive from the future, and
//! `assess_age` reports a negative age as [`Staleness::Dead`] — a whole scan
//! condemned by the order of two lines. So the clock is read again after the last
//! fetch and that reading is the one the scan runs against.

use std::fmt::Write as _;

use arb_detect::{age_of, assess_age, assess_skew, ClockVerdict, Staleness};
use arb_live::venues::bybit::BybitAdapter;
use arb_live::{Clock, CurlTransport, SystemClock};
use arb_math::CarryLeg;
use arb_num::Money;
use arb_num::Rate;
use arb_scan::{scan, LegInput, Opportunity, ScanConfig, ScanError};
use arb_types::{
    DataQuality, InstrumentKey, LocalTs, MarketKind, Millis, PositionSide, Provenance, QualityFlags,
    Side, Venue, VenueKey,
};
use arb_venue::{ExchangeAdapter, VenueError};

use crate::{bps, flags, money, pct, provenance, qty, EngineError};

/// Base assets scanned when none are named.
///
/// Three liquid USDT perps. Enough to see the shape of the output without asking
/// the venue for six hundred books on a first run.
pub const DEFAULT_BASES: [&str; 3] = ["BTC", "ETH", "SOL"];

/// The quote and settlement asset. USDT-margined linear perps only, for now.
pub const QUOTE: &str = "USDT";

/// Per-leg notional the live scan sizes against, in quote units.
///
/// Deliberately smaller than [`crate::replay_config`]'s 100 000. This is the size
/// an account with a few thousand dollars of capital and a cap of 40% on any one
/// exchange could actually take, and a depth verdict is only meaningful against a
/// size somebody would really send.
pub const DEFAULT_NOTIONAL: i64 = 1_000;

/// One live scan: what was fetched, what it says, what could not be fetched.
pub struct LiveScan {
    /// Assembled legs, one per resolved instrument.
    pub legs: Vec<LegInput>,
    /// Instruments that could not be assembled, with the venue's reason. Reported
    /// rather than dropped: a missing symbol and an unlisted symbol look identical
    /// in a shorter table.
    pub failures: Vec<(Venue, String, VenueError)>,
    /// Ranked rows. Empty while only one venue is connected.
    pub rows: Vec<Opportunity>,
    /// Pairs that are trades but whose arithmetic failed.
    pub errors: Vec<(VenueKey, VenueKey, ScanError)>,
    /// Instruments each connected adapter listed, for context on the failures.
    pub listed: usize,
    /// The clock reading the scan ran against — taken *after* the last fetch.
    pub now: LocalTs,
}

/// The configuration a live scan runs under.
///
/// Same risk posture as [`crate::replay_config`] — 3x leverage, a seven-day
/// horizon, conservative staleness and paint limits — with a size an actual
/// account could take.
pub fn live_config(target_notional: Money, now: LocalTs) -> Result<ScanConfig, ScanError> {
    ScanConfig::conservative(target_notional, Rate::from_int(3)?, Millis::from_secs(7 * 86_400), now)
}

/// The fee provenance a public adapter is entitled to claim.
///
/// `SynthesizedUnverified`, because the number came from a fee schedule read by a
/// human, not from the account endpoint. Calling it measured here would be the one
/// lie that makes every downstream gate meaningless.
fn assumed_fee_quality() -> DataQuality {
    DataQuality::new(
        QualityFlags::FEE_TIER_ASSUMED,
        Provenance::SynthesizedUnverified,
    )
}

/// Fetch every named instrument from the connected venues and scan the result.
///
/// Takes `&dyn ExchangeAdapter` rather than a generic parameter because the
/// adapters are heterogeneous by construction — Bybit's, Binance's and OKX's are
/// three unrelated types — and because `LegInput::gather`'s `?Sized` bound exists
/// precisely so this is possible. The `clock` is the same seam
/// [`arb_live::clock::Clock`] is: production passes [`SystemClock`], tests pass a
/// stopped one, and the report comes out byte-identical on every run.
///
/// One HTTPS request per instrument list at connect, then two per instrument
/// (ticker and book). At [`DEFAULT_BASES`] against one venue that is seven
/// requests, well inside any venue's public rate limit.
pub fn scan_venues(
    adapters: &[&dyn ExchangeAdapter],
    clock: &dyn Clock,
    bases: &[String],
    notional: Money,
) -> Result<LiveScan, EngineError> {
    // Provisional, only so `LegInput::gather` has a book depth. Replaced below,
    // once the clock reading that the scan will actually run against exists.
    let depth = live_config(notional, LocalTs::EPOCH)?.book_depth;

    let mut legs = Vec::new();
    let mut failures = Vec::new();
    let mut listed = 0usize;

    for adapter in adapters {
        listed += adapter.instruments().map(|i| i.len()).unwrap_or(0);
        for base in bases {
            let key = match InstrumentKey::parse(base, QUOTE, MarketKind::LinearPerp, QUOTE) {
                Ok(k) => k,
                Err(e) => {
                    failures.push((adapter.venue(), base.clone(), VenueError::from(e)));
                    continue;
                }
            };
            match LegInput::gather(*adapter, &key, depth, assumed_fee_quality()) {
                Ok(leg) => legs.push(leg),
                // A venue that does not list this base is reported, not fatal: one
                // unlisted altcoin must not cost the operator the whole scan.
                Err(ScanError::Venue(e)) => failures.push((adapter.venue(), base.clone(), e)),
                Err(e) => return Err(EngineError::Scan(e)),
            }
        }
    }

    // The second reading. See the module docs on why this is not the first.
    let now = clock.now()?;
    let cfg = live_config(notional, now)?;
    let (rows, errors) = scan(&legs, &cfg);

    Ok(LiveScan {
        legs,
        failures,
        rows,
        errors,
        listed,
        now,
    })
}

/// Connect to every venue with a live adapter and scan.
///
/// The only function here that touches the network or the wall clock. Everything
/// it does beyond constructing those two is [`scan_venues`], which is why the
/// report can be tested end to end without either.
pub fn live(bases: &[String], notional: Money) -> Result<LiveScan, EngineError> {
    let bybit = BybitAdapter::connect(CurlTransport::new(), SystemClock)?;
    scan_venues(&[&bybit], &SystemClock, bases, notional)
}

// ---------------------------------------------------------------------------
// Rendering. Integer arithmetic only, same as the replay report.
// ---------------------------------------------------------------------------

/// A [`Rate`] as a percentage with six decimals.
///
/// [`crate::pct`]'s two decimals are right for an annualised figure and useless
/// for a published funding rate: Bybit's BTCUSDT rate of -0.00000869 renders as
/// `-0.00%` there, which is indistinguishable from a venue paying nothing. `Rate`
/// is scaled 1e12, so a percentage is the raw over 1e10 and six decimals of
/// percent is the raw over 1e4.
pub fn pct6(r: Rate) -> String {
    let raw = r.raw();
    let neg = raw < 0;
    let a = raw.unsigned_abs();
    let whole = a / 10_000_000_000;
    let frac = (a % 10_000_000_000) / 10_000;
    format!("{}{whole}.{frac:06}%", if neg { "-" } else { "" })
}

/// A duration in whole minutes, or `—` when there is none.
fn minutes(d: Option<Millis>) -> String {
    match d {
        Some(m) => format!("{}m", m.millis() / 60_000),
        None => "—".to_string(),
    }
}

fn staleness_label(s: Staleness) -> &'static str {
    match s {
        Staleness::Fresh => "fresh",
        Staleness::Warn => "warn",
        Staleness::Stale => "STALE",
        Staleness::Dead => "DEAD",
    }
}

fn clock_label(v: ClockVerdict) -> String {
    match v {
        ClockVerdict::Aligned => "aligned".to_string(),
        ClockVerdict::Lagging(m) => format!("lagging {}ms", m.millis()),
        ClockVerdict::Suspicious(m) => format!("SUSPICIOUS {}ms", m.millis()),
        ClockVerdict::ExchangeAhead(m) => format!("EXCHANGE AHEAD {}ms", m.millis()),
    }
}

/// The whole live report.
pub fn render_live(s: &LiveScan, cfg: &ScanConfig) -> String {
    let mut out = String::new();

    let _ = writeln!(
        out,
        "live scan — {} instrument(s) fetched, {} listed at the venue",
        s.legs.len(),
        s.listed
    );
    let _ = writeln!(
        out,
        "clock {}  |  size {} {QUOTE}  |  depth {} levels/side\n",
        s.now.millis(),
        money(cfg.target_notional),
        cfg.book_depth
    );

    for leg in &s.legs {
        render_leg(&mut out, leg, cfg);
    }

    if !s.failures.is_empty() {
        let _ = writeln!(out, "not fetched:");
        for (venue, base, e) in &s.failures {
            let _ = writeln!(out, "  {} {base}{QUOTE}: {e}", venue.as_str());
        }
        let _ = writeln!(out);
    }

    render_ranking(&mut out, s);

    // The counterpart to the replay report's "NOTHING HERE IS TRADEABLE". The
    // failure mode here is the opposite one and worse: these prices *are* real, so
    // the numbers look actionable, and the one input that was not measured shows up
    // as a five-letter flag on a crowded line.
    if !s.legs.is_empty() {
        let _ = writeln!(
            out,
            "\nEvery price, size, funding rate and contract spec above came off the venue's\n\
             own endpoint and is exact. The fee did not: no public endpoint reveals an\n\
             account's tier, so the worst published tier is assumed and each leg carries\n\
             FEE_ASSUMED. That assumption runs one way only — the real fee can be lower,\n\
             never higher — so every net figure here is understated rather than flattering.\n\
             A read-only API key (no trading permission, no withdrawal permission) replaces\n\
             the assumption with a measurement."
        );
        // True both when no row formed at all and when every row that formed was
        // sunk, and it has to cover both: an operator who reads real prices under a
        // short table concludes "no opportunity today", which is a market claim this
        // report is not entitled to make.
        if !s.rows.iter().any(|r| r.is_rankable()) {
            let _ = writeln!(
                out,
                "NOT A RANKING: nothing above is ranked. Either no two venues quoted the same\n\
                 pair, or every row that formed failed its own quality gate — the line above\n\
                 says which. Neither of those is the fee tier."
            );
        }
    }
    out
}

fn render_leg(out: &mut String, leg: &LegInput, cfg: &ScanConfig) {
    let i = &leg.instrument;
    let _ = writeln!(
        out,
        "{} {}  {}  settle {}",
        i.vkey.venue.as_str(),
        i.native.as_str(),
        i.spec.kind.tag(),
        i.spec.settle
    );

    // --- book -------------------------------------------------------------
    let b = &leg.book;
    match (b.best_bid(), b.best_ask()) {
        (Some(bid), Some(ask)) => {
            let _ = writeln!(
                out,
                "  book     bid {} x {}   ask {} x {}",
                bid.price,
                qty(bid.qty),
                ask.price,
                qty(ask.qty)
            );
        }
        _ => {
            let _ = writeln!(out, "  book     EMPTY on at least one side");
        }
    }
    // `total_depth` takes the side we would *act* on, not the side of the book:
    // selling consumes bids, buying consumes asks. Passing the intuitive-looking
    // one here would swap two adjacent columns and nothing would look wrong.
    let bid_depth = b
        .total_depth(Side::Sell)
        .map(qty)
        .unwrap_or_else(|e| format!("{e}"));
    let ask_depth = b
        .total_depth(Side::Buy)
        .map(qty)
        .unwrap_or_else(|e| format!("{e}"));
    let age = age_of(b.observed_at, cfg.now);
    let book_clock = match b.exchange_ts {
        Some(ts) => clock_label(assess_skew(ts, b.observed_at, cfg.skew)),
        None => "no exchange stamp".to_string(),
    };
    let _ = writeln!(
        out,
        "           {} levels, depth {bid_depth} / {ask_depth}   age {}ms ({})   clock {book_clock}",
        b.bids().len().max(b.asks().len()),
        age.millis(),
        staleness_label(assess_age(age, cfg.book_staleness)),
    );

    // --- funding ----------------------------------------------------------
    if let Some(f) = &leg.funding {
        let short = CarryLeg::from_snapshot(f, PositionSide::Short);
        let long = CarryLeg::from_snapshot(f, PositionSide::Long);
        let short_apr = short
            .annual_receipt()
            .map(pct)
            .unwrap_or_else(|e| format!("{e:?}"));
        let long_apr = long
            .annual_receipt()
            .map(pct)
            .unwrap_or_else(|e| format!("{e:?}"));
        let _ = writeln!(
            out,
            "  funding  published {} per {}h   annualised: short {short_apr}  long {long_apr}",
            pct6(f.rate),
            f.interval.secs() / 3600,
        );

        // `assume_no_skew` is exactly what it says: the venue's settlement time is
        // a wall clock and ours is too, so the difference is only meaningful to the
        // extent the two agree — which is what the `clock` verdict above reports.
        let to_settle = f.time_to_settlement(cfg.now.assume_no_skew());
        let mark = f.mark.map(|p| p.to_string()).unwrap_or("—".to_string());
        let index = f.index.map(|p| p.to_string()).unwrap_or("—".to_string());
        let f_age = age_of(f.observed_at, cfg.now);
        let _ = writeln!(
            out,
            "           next settlement {}   mark {mark}   index {index}   age {}ms ({})",
            minutes(to_settle),
            f_age.millis(),
            staleness_label(assess_age(f_age, cfg.funding_staleness)),
        );
    } else {
        let _ = writeln!(out, "  funding  none (spot does not fund)");
    }

    // --- spec and fees ----------------------------------------------------
    let min_notional = leg
        .instrument
        .spec
        .min_notional
        .map(money)
        .unwrap_or("—".to_string());
    let _ = writeln!(
        out,
        "  spec     tick {}   lot {}   min notional {min_notional}   order unit {}",
        i.spec.tick_size,
        qty(i.spec.lot_size),
        i.spec.size_unit.as_str(),
    );
    let _ = writeln!(
        out,
        "  fees     maker {}bp   taker {}bp   tier {}",
        bps(leg.fees.maker),
        bps(leg.fees.taker),
        if leg.fees.tier_known {
            "measured"
        } else {
            "ASSUMED (worst published — a read-only API key would measure it)"
        }
    );

    let q = leg.quality();
    let _ = writeln!(
        out,
        "  quality  {} {}   book {}   funding {}   fees {}\n",
        provenance(q.provenance),
        flags(q.flags),
        provenance(leg.book.quality.provenance),
        leg.funding
            .as_ref()
            .map(|f| provenance(f.quality.provenance))
            .unwrap_or("—"),
        // Printed separately because `LegInput::quality` deliberately does not fold
        // it in. If the leg says "measured" and this says "INVENTED", that is the
        // design and not a contradiction: the market data was measured, the cost
        // model was assumed, and collapsing the two into one word is what made the
        // scanner unable to rank anything it had actually observed.
        provenance(leg.fee_provenance()),
    );
}

fn render_ranking(out: &mut String, s: &LiveScan) {
    if s.legs.len() < 2 {
        let _ = writeln!(
            out,
            "no ranking: a cross-venue trade needs the same pair on two venues, \
             and {} instrument(s) were fetched.",
            s.legs.len()
        );
        return;
    }

    let venues: Vec<&str> = {
        let mut v: Vec<&str> = s.legs.iter().map(|l| l.venue().as_str()).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if venues.len() < 2 {
        let _ = writeln!(
            out,
            "no ranking: every leg above is on {} — a cross-venue trade needs two \
             venues quoting the same pair. This is a missing adapter, not a missing \
             opportunity.",
            venues.join(", ")
        );
        return;
    }

    if s.rows.is_empty() && s.errors.is_empty() {
        let _ = writeln!(
            out,
            "no ranking: no two instruments above form a hedge (same base/quote, \
             two different venues)."
        );
        return;
    }

    let _ = writeln!(out, "{} row(s):", s.rows.len());
    for row in &s.rows {
        let _ = writeln!(
            out,
            "  {} / {}   net {}   {}",
            row.short.vkey.venue.as_str(),
            row.long.vkey.venue.as_str(),
            pct(row.net_apr),
            if row.is_rankable() {
                "rankable".to_string()
            } else {
                format!(
                    "unrankable: {}",
                    row.blocked
                        .iter()
                        .map(|b| b.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
    }
    for (a, b, e) in &s.errors {
        let _ = writeln!(out, "  {} / {}: {e}", a.venue.as_str(), b.venue.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_live::venues::bybit::captured;
    use arb_live::{FixedClock, StubTransport};

    /// The real adapter, the real captured Bybit bytes, a stopped clock and no
    /// network. Everything below the network boundary is the production path.
    fn stub_adapter() -> BybitAdapter<StubTransport, FixedClock> {
        let transport = StubTransport::new()
            .on("instruments-info", captured::INSTRUMENTS_BTCUSDT)
            .on("market/tickers", captured::TICKERS_BTCUSDT)
            .on("market/orderbook", captured::ORDERBOOK_BTCUSDT);
        BybitAdapter::connect(transport, FixedClock::at_millis(captured::OBSERVED_MS))
            .expect("the captured instrument list parses")
    }

    fn run(bases: &[&str]) -> (LiveScan, ScanConfig) {
        let a = stub_adapter();
        let clock = FixedClock::at_millis(captured::OBSERVED_MS);
        let bases: Vec<String> = bases.iter().map(|s| (*s).to_string()).collect();
        let notional = Money::from_int(DEFAULT_NOTIONAL).expect("money");
        let s = scan_venues(&[&a], &clock, &bases, notional).expect("the stub scan runs");
        let cfg = live_config(notional, s.now).expect("config");
        (s, cfg)
    }

    /// The report renders, and the numbers in it are the venue's own.
    ///
    /// This is the test the live path did not have: `render_live` walks every
    /// `Option`, every `Result` from `total_depth`, and every annualisation, and a
    /// panic or a swapped column in any of them reaches the operator's screen
    /// before it reaches a test. Asserted on the venue's exact captured values,
    /// because a stopped clock and captured bytes make them exact.
    #[test]
    fn the_live_report_renders_the_venues_own_numbers() {
        let (s, cfg) = run(&["BTC"]);
        assert_eq!(s.legs.len(), 1);
        assert!(s.failures.is_empty(), "{:?}", s.failures);

        let out = render_live(&s, &cfg);

        // The book, as captured. `Price`'s Display trims trailing zeros, so the
        // venue's "78943.00" prints as 78943 — the value is exact, the padding is
        // not the number.
        assert!(out.contains("bid 78943 x 0.798000"), "{out}");
        assert!(out.contains("ask 78943.1 x 1.330000"), "{out}");
        // Total resting size per side: bids 0.798 + 0.002 + 0.001 = 0.801,
        // asks 1.330 + 0.002 + 0.736 = 2.068. Asserted because the two columns
        // are adjacent and `total_depth` takes the side one *acts* on, so a swap
        // here is a plausible mistake that changes nothing else in the output.
        assert!(out.contains("depth 0.801000 / 2.068000"), "{out}");
        // Book age is structurally zero here: the adapter stamped `observed_at`
        // from the same stopped clock the scan reads, so this asserts the wiring,
        // not the staleness logic — `arb-detect` owns that and tests it directly.
        assert!(out.contains("age 0ms (fresh)"), "{out}");
        // Skew *is* exercised: the venue's own `ts` is 498 ms before the
        // observation, which is the round trip and is well inside the limit.
        assert!(out.contains("clock lagging 498ms"), "{out}");

        // The funding rate, at a precision that distinguishes it from zero.
        assert!(out.contains("published -0.000869% per 8h"), "{out}");
        assert!(out.contains("mark 79033.51"), "{out}");
        assert!(out.contains("index 79074.8"), "{out}");
        // 1787616000000 - 1787598550500 = 17_449_500 ms = 290.8 minutes.
        assert!(out.contains("next settlement 290m"), "{out}");

        // The spec, read from the venue rather than inferred.
        assert!(out.contains("tick 0.1"), "{out}");
        assert!(out.contains("lot 0.001000"), "{out}");
        assert!(out.contains("min notional 5.00"), "{out}");

        assert!(out.contains("maker 2.00bp"), "{out}");
        assert!(out.contains("taker 5.50bp"), "{out}");

        // The captured book is three levels deep and the scan asked for twenty,
        // so the leg is honestly marked partial. A live run against the real
        // endpoint asks for and receives twenty and this flag is absent.
        assert!(out.contains("PARTIAL"), "{out}");
    }

    /// The annualisation is checkable by hand, which is the only reason to print
    /// the published rate next to it.
    ///
    /// -0.00000869 per 8h is 1095 settlements a year, so a **short** perp, which
    /// receives the rate, receives -0.00000869 × 1095 = -0.00951555 — that is,
    /// it *pays* 0.95%. The long's receipt is the exact negation. If the interval
    /// were misread as 480 *hours* the figure would be 0.0159%, sixty times
    /// smaller, and it would still look like a plausible funding number.
    #[test]
    fn the_annualised_figure_is_the_published_rate_times_the_periods_in_a_year() {
        let (s, cfg) = run(&["BTC"]);
        let out = render_live(&s, &cfg);
        assert!(
            out.contains("short -0.95%  long 0.95%"),
            "annualisation is wrong or the sign law flipped:\n{out}"
        );

        let f = s.legs[0].funding.as_ref().expect("a perp funds");
        assert_eq!(f.interval.secs(), 28_800);
        assert_eq!(f.rate.raw(), -8_690_000);
        let short = CarryLeg::from_snapshot(f, PositionSide::Short)
            .annual_receipt()
            .expect("annualises");
        let long = CarryLeg::from_snapshot(f, PositionSide::Long)
            .annual_receipt()
            .expect("annualises");
        // -0.00000869 * 1095 = -0.00951555, at Rate's 1e12 scale.
        assert_eq!(short.raw(), -9_515_550_000);
        assert_eq!(long.raw(), 9_515_550_000);
    }

    /// The report states, in words, that it is not a ranking and why.
    ///
    /// An operator who sees measured prices and a fresh timestamp will read the
    /// output as tradeable unless it says otherwise. Two things must be visible:
    /// that the fee tier is assumed, and that no ranking happened.
    #[test]
    fn the_report_says_the_fee_tier_is_assumed_and_that_nothing_is_ranked() {
        let (s, cfg) = run(&["BTC"]);
        let out = render_live(&s, &cfg);
        assert!(out.contains("tier ASSUMED"), "{out}");
        // The short name `QualityFlags::NAMED` uses, not the constant's name.
        assert!(out.contains("FEE_ASSUMED"), "{out}");
        assert!(out.contains("no ranking"), "{out}");
        // Every input except the fee came off the venue, and the report shows that
        // distinction rather than collapsing it to one verdict.
        assert!(out.contains("book measured"), "{out}");
        assert!(out.contains("funding measured"), "{out}");
        // And it says the consequence in a sentence, because an operator who
        // skims measured prices and a fresh timestamp will otherwise trade them.
        assert!(out.contains("NOT A RANKING"), "{out}");
    }

    /// The provenance a live leg ends up with, asserted rather than assumed.
    ///
    /// This test was inverted once, and the inversion is the point. It used to
    /// assert that the assumed fee dragged the whole leg to
    /// `SynthesizedUnverified` — which it did, and which meant no measured row
    /// could ever rank, on any venue, at any size. `LegInput::quality` now folds
    /// the fee schedule's *flags* without its provenance: the market data was
    /// measured and says so, and the assumption travels as `FEE_TIER_ASSUMED`,
    /// which `QualityFlags::BLOCKING` deliberately excludes.
    ///
    /// What a read-only API key still buys is precision, not permission — the
    /// real tier instead of the worst published one, so `net_apr` stops being
    /// understated.
    #[test]
    fn the_assumed_fee_is_a_flag_on_a_measured_leg_and_not_a_verdict_on_it() {
        let (s, _) = run(&["BTC"]);
        let leg = &s.legs[0];
        assert_eq!(leg.book.quality.provenance, Provenance::LiveMeasured);
        assert_eq!(
            leg.funding.as_ref().expect("perp").quality.provenance,
            Provenance::LiveMeasured
        );
        // The fee schedule itself is still honest about where it came from; the
        // leg simply does not inherit that verdict.
        assert_eq!(leg.fee_provenance(), Provenance::SynthesizedUnverified);
        assert_eq!(leg.quality().provenance, Provenance::LiveMeasured);
        // PARTIAL_BOOK because the captured book is three levels against a
        // twenty-level request; FEE_TIER_ASSUMED because no public endpoint
        // reveals the tier. Neither is in `QualityFlags::BLOCKING`.
        assert_eq!(
            leg.quality().flags,
            QualityFlags::PARTIAL_BOOK.union(QualityFlags::FEE_TIER_ASSUMED)
        );
        assert!(!leg.quality().flags.is_blocking());
        // Which is the whole consequence: this leg is rankable. It is not
        // *trustworthy* — that gate wants an empty flag set — and the report has
        // to keep saying so rather than letting a clean-looking row imply it.
        assert!(leg.quality().is_rankable());
        assert!(!leg.quality().is_trustworthy());
    }

    /// A base the venue does not list is named in the report, not dropped.
    ///
    /// The captured payload lists only BTCUSDT, so ETH and SOL cannot be fetched.
    /// A shorter table with no explanation reads as "no opportunity in ETH", which
    /// is the opposite conclusion from "we could not fetch ETH".
    #[test]
    fn a_base_the_venue_does_not_list_is_reported_rather_than_dropped() {
        let (s, cfg) = run(&["BTC", "ETH", "SOL"]);
        assert_eq!(s.legs.len(), 1, "only BTCUSDT is in the captured list");
        assert_eq!(s.failures.len(), 2);
        let out = render_live(&s, &cfg);
        assert!(out.contains("not fetched:"), "{out}");
        assert!(out.contains("ETHUSDT"), "{out}");
        assert!(out.contains("SOLUSDT"), "{out}");
    }

    /// One venue cannot produce a cross-venue row, and the report must say that
    /// this is a missing adapter rather than a quiet market.
    #[test]
    fn a_single_venue_reports_a_missing_adapter_and_not_an_empty_market() {
        let (s, cfg) = run(&["BTC"]);
        assert!(s.rows.is_empty());
        let out = render_live(&s, &cfg);
        assert!(out.contains("no ranking"), "{out}");
        assert!(!out.contains("0 row(s)"), "an empty table reads as no opportunity:\n{out}");
    }

    /// A stopped clock renders the same bytes twice. Without this, a diff of two
    /// live reports would be a statement about timing rather than about the code.
    #[test]
    fn the_same_inputs_render_the_same_bytes() {
        let (a, cfg) = run(&["BTC"]);
        let (b, _) = run(&["BTC"]);
        assert_eq!(render_live(&a, &cfg), render_live(&b, &cfg));
    }

    #[test]
    fn the_fee_provenance_is_never_a_measurement() {
        // The one lie that would defeat every gate downstream. If this ever says
        // LiveMeasured, an assumed fee becomes indistinguishable from a read one,
        // and nothing further downstream can recover the distinction.
        let q = assumed_fee_quality();
        assert_eq!(q.provenance, Provenance::SynthesizedUnverified);
        assert!(q.flags.contains(QualityFlags::FEE_TIER_ASSUMED));
        assert!(!q.is_trustworthy());
        // Not *blocking* — the flag is documented as confidence-degrading, not
        // falsifying, and `QualityFlags::BLOCKING` excludes it on purpose. So the
        // schedule on its own is rankable and not tradeable, and it is
        // `LegInput::quality` that decides how much of this verdict the leg
        // inherits: the flag, not the provenance.
        assert!(q.is_rankable());
        assert!(!q.is_tradeable());
    }

    #[test]
    fn a_funding_rate_too_small_for_two_decimals_is_still_visible() {
        // Bybit's captured BTCUSDT rate. `pct` renders this as "-0.00%", which
        // reads as a venue paying nothing; six decimals is the difference between
        // a number and a rounding artefact.
        let r = Rate::parse("-0.00000869").expect("rate");
        assert_eq!(pct(r), "-0.00%");
        assert_eq!(pct6(r), "-0.000869%");
    }

    #[test]
    fn the_live_size_is_one_an_account_with_five_thousand_could_take() {
        let cfg = live_config(
            Money::from_int(DEFAULT_NOTIONAL).expect("money"),
            LocalTs::EPOCH,
        )
        .expect("config");
        assert_eq!(cfg.target_notional.raw(), 1_000_000_000_000_000);
        // 3x on the hedge leg, per the standing risk limit.
        assert_eq!(cfg.leverage.raw(), 3_000_000_000_000);
        assert_eq!(cfg.horizon.millis(), 7 * 86_400 * 1000);
    }

    #[test]
    fn minutes_renders_an_absent_settlement_as_absent_not_as_zero() {
        assert_eq!(minutes(None), "—");
        assert_eq!(minutes(Some(Millis::from_secs(289 * 60))), "289m");
        // Sub-minute truncates toward zero rather than rounding up: a settlement
        // 59 seconds away is "0m", which is the honest reading of "imminent".
        assert_eq!(minutes(Some(Millis::from_secs(59))), "0m");
    }
}
