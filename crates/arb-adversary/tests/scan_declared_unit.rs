#![forbid(unsafe_code)]
//! `arb-scan` infers the order-size unit from `ContractSize` and never reads the
//! declared `ContractSpec::size_unit`. On OKX that is a factor of 100.
//!
//! # The rule being broken
//!
//! `decisions/024` point 2, verbatim: **«Вывод единицы из `contract_size`
//! запрещён.»** `SizeUnit::implied_by` may only cross-check a declared unit
//! against an implied one, never stand in for it. The field was made a required,
//! declared fixture key for exactly that reason.
//!
//! `crates/arb-scan/src/scan.rs` dispatches on `spec.size` in both places where a
//! size meets a price:
//!
//! ```text
//!   requested_in_book_units:  ContractSize::BasePerContract(_) => target.div_price(..)
//!   notional_of:              ContractSize::BasePerContract(_) => size.mul_price(..)
//! ```
//!
//! `ContractSize::BasePerContract(_) => SizeUnit::Base` is precisely
//! `SizeUnit::implied_by`. So both functions read the implied unit. Across the
//! whole workspace `spec.size_unit` is consumed in exactly one place —
//! `crates/arb-engine/src/live.rs:401` — where it is *printed*. No arithmetic
//! anywhere reads it.
//!
//! # Why this is money now and was not before
//!
//! Every fixture in `fixtures/` declares `size_unit = "base"`, and `arb-scan`'s own
//! test helper hardcodes `size_unit: SizeUnit::Base` on every spec it builds
//! (`scan.rs`, `fn spec`, with the comment "Every spec these tests build is linear
//! or spot, and `arb-scan` works in ladder units throughout"). That comment
//! conflates two different claims: that the scanner works in ladder units (true,
//! and correct), and that a *linear* instrument's ladder is in base (false).
//!
//! Round 2's OKX adapter is the first instrument in the workspace where declared
//! and implied disagree. `crates/arb-live/src/venues/okx.rs` reports
//! `MarketKind::LinearPerp` with `ContractSize::BasePerContract(0.01)` **and**
//! `SizeUnit::Contracts`, and passes the ladder through raw. The adapter's own test
//! states the disagreement outright:
//!
//! ```text
//!   okx.rs:876   assert_eq!(i.spec.size_unit, SizeUnit::Contracts);
//!   okx.rs:880   assert_ne!(i.spec.size_unit, SizeUnit::implied_by(i.spec.size));
//! ```
//!
//! So the adapter knows the two disagree, and the scanner follows the one the
//! adapter is telling it not to. `arb-scan`'s guard test
//! `a_requested_size_is_in_book_units_not_contracts` varies `contract_size` over
//! `["1", "0.01", "0.001", "0.0001"]` and asserts the answer must not change — with
//! `size_unit` pinned to `Base` in all four. That guard is bolted to the one case
//! that was never in question.
//!
//! # The three tests below
//!
//! Every expected value is derived here as an exact integer rational, from the
//! definition, before any of this was run. None of them is a number read off the
//! implementation.
//!
//! - OKX as the **long** leg: `Opportunity::notional` — the headline — is 100× the
//!   truth, and the two errors cancel inside `fit_to_budget` so the budget check
//!   passes.
//! - OKX as the **short** leg: the headline is right and `OppLeg::margin` is 100×,
//!   so `Opportunity::margin` is nearly double the truth.
//! - Either way: the row is not a hedge. One `size` is handed to both legs, and
//!   1.25 on Bybit against 1.25 on OKX is 1.2375 BTC of naked delta.

use arb_num::{Bps, Money, Price, Qty, Rate, SCALE};
use arb_scan::{price_pair, LegInput, Opportunity, ScanConfig};
use arb_types::{
    Asset, BookSnapshot, ContractSize, ContractSpec, DataQuality, FundingInterval,
    FundingSnapshot, Instrument, InstrumentKey, Level, LocalTs, MarketKind, Millis, SizeUnit,
    Symbol, Venue, VenueKey, WallTs,
};
use arb_venue::FeeSchedule;

// ===========================================================================
// Constructors. Panics are fine: this file is entirely a test.
// ===========================================================================

fn asset(s: &str) -> Asset {
    Asset::new(s).expect("asset parses")
}

fn price(s: &str) -> Price {
    Price::parse(s).expect("price parses")
}

fn qty(s: &str) -> Qty {
    Qty::parse(s).expect("qty parses")
}

fn money(s: &str) -> Money {
    Money::parse(s).expect("money parses")
}

fn level(p: &str, q: &str) -> Level {
    Level::new(price(p), qty(q)).expect("level is valid")
}

fn vkey(venue: Venue) -> VenueKey {
    let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT")
        .expect("instrument key parses");
    VenueKey::new(venue, key)
}

/// OKX `BTC-USDT-SWAP` exactly as `arb-live` reports it: a **linear** perp whose
/// ladder is denominated in **contracts**, one contract being 0.01 BTC.
///
/// `ctVal "0.01"`, `ctMult "1"`, `lotSz "0.01"`, `tickSz "0.1"` are verbatim from
/// `crates/arb-live/src/venues/okx.rs`, `captured::INSTRUMENTS_BTC_USDT_SWAP` —
/// real recorded bytes, not an invented payload.
fn okx_spec() -> ContractSpec {
    ContractSpec::new(
        MarketKind::LinearPerp,
        ContractSize::BasePerContract(qty("0.01")),
        asset("USDT"),
        price("0.1"),
        // The declared unit. `SizeUnit::implied_by(BasePerContract(_))` is `Base`,
        // so declared and implied disagree here — which is the whole point.
        SizeUnit::Contracts,
        qty("0.01"),
        None,
    )
    .expect("okx spec is valid")
}

/// Bybit `BTCUSDT`: a linear perp whose ladder is in base, one contract being one
/// coin. `crates/arb-live/src/venues/bybit.rs` reports
/// `BasePerContract(Qty::ONE)` with `SizeUnit::Base`.
///
/// This is the leg that makes the finding legible. On Bybit the two units coincide,
/// so its arithmetic is right under either reading, and every divergence in the row
/// is attributable to the OKX leg alone.
fn bybit_spec() -> ContractSpec {
    ContractSpec::new(
        MarketKind::LinearPerp,
        ContractSize::BasePerContract(Qty::ONE),
        asset("USDT"),
        price("0.1"),
        SizeUnit::Base,
        qty("0.001"),
        None,
    )
    .expect("bybit spec is valid")
}

/// A four-level ladder, one tick apart, equal size at every level, observed at
/// t=0 and marked measured — so no quality flag can be blamed for a wrong number.
///
/// `exchange_ts` is `None` deliberately: that yields `ClockVerdict::Aligned` and
/// keeps a skew verdict out of the row, leaving the arithmetic as the only thing
/// under test.
fn book(venue: Venue, bids: Vec<Level>, asks: Vec<Level>) -> BookSnapshot {
    BookSnapshot::new(
        vkey(venue),
        bids,
        asks,
        Some(1),
        None,
        LocalTs::from_millis(0),
        DataQuality::MEASURED,
    )
    .expect("book invariants hold")
}

fn funding(venue: Venue, r: &str) -> FundingSnapshot {
    FundingSnapshot {
        vkey: vkey(venue),
        rate: Rate::parse(r).expect("rate parses"),
        interval: FundingInterval::EIGHT_HOURS,
        next_funding_at: Some(WallTs::from_millis(0)),
        mark: None,
        index: None,
        exchange_ts: WallTs::from_millis(0),
        observed_at: LocalTs::from_millis(0),
        quality: DataQuality::MEASURED,
    }
}

/// A measured fee schedule, so `FEE_TIER_ASSUMED` does not enter the row. Fees do
/// not touch a notional; they are here only so the row is otherwise clean.
fn fees() -> FeeSchedule {
    FeeSchedule::known(
        Bps::from_whole(2).expect("2 bps"),
        Bps::from_whole(6).expect("6 bps"),
    )
}

/// Level sizes, in each venue's own book unit — contracts on OKX, BTC on Bybit.
///
/// Deliberately neither uniform nor round-numbered. A ladder of four equal sizes
/// that are all whole multiples of 1.0 scores 40 on `arb-detect`'s paint
/// detector (`UniformLevelSizes` 25 + `RoundNumberSizes` 15), which blocks the
/// row — and a blocked row would let this finding be dismissed as an artefact of
/// the fixture. With these sizes the paint score is 0 and the row ranks, so the
/// 100x number below is one the engine would print as actionable.
///
/// Level one holds 3.17 either way, comfortably more than the 1.25 that gets
/// filled, so the whole fill lands at the touch and every VWAP below is a single
/// exact price rather than a weighted average.
const DEPTHS: [&str; 4] = ["3.17", "2.53", "4.11", "1.87"];

fn leg(venue: Venue, spec: ContractSpec, rate: &str) -> LegInput {
    let native = Symbol::new(match venue {
        Venue::Okx => "BTC-USDT-SWAP",
        _ => "BTCUSDT",
    })
    .expect("symbol parses");
    // Same shape on both sides, so `OneSidedDepth` cannot fire either.
    let bids = vec![
        level("79999.0", DEPTHS[0]),
        level("79998.9", DEPTHS[1]),
        level("79998.8", DEPTHS[2]),
        level("79998.7", DEPTHS[3]),
    ];
    let asks = vec![
        level("80000.0", DEPTHS[0]),
        level("80000.1", DEPTHS[1]),
        level("80000.2", DEPTHS[2]),
        level("80000.3", DEPTHS[3]),
    ];
    LegInput::new(
        Instrument::new(vkey(venue), native, spec),
        Some(funding(venue, rate)),
        book(venue, bids, asks),
        fees(),
        DataQuality::MEASURED,
    )
    .expect("leg is internally consistent")
}

/// The pair with OKX on the long side: OKX pays, Bybit collects, so
/// `assign_sides` shorts Bybit and longs OKX.
fn okx_long_pair() -> (LegInput, LegInput) {
    (
        leg(Venue::Okx, okx_spec(), "-0.0001"),
        leg(Venue::Bybit, bybit_spec(), "0.0001"),
    )
}

/// The same pair with the funding signs reversed, putting OKX on the short side.
fn okx_short_pair() -> (LegInput, LegInput) {
    (
        leg(Venue::Okx, okx_spec(), "0.0001"),
        leg(Venue::Bybit, bybit_spec(), "-0.0001"),
    )
}

/// Target 100 000 USDT, leverage 1 so that `margin == notional` on each leg and a
/// margin assertion is a notional assertion with nothing in between, horizon one
/// funding interval.
fn cfg() -> ScanConfig {
    ScanConfig::conservative(
        money("100000"),
        Rate::ONE,
        Millis::from_secs(8 * 3600),
        LocalTs::from_millis(0),
    )
    .expect("config is valid")
}

/// Base exposure of `size` book units, from the **declared** unit.
///
/// Implemented here rather than borrowed from `arb-scan`, because borrowing the
/// arithmetic under audit would make the test agree with the defect by
/// construction. This is the definition from `decisions/019` and `decisions/024`
/// and nothing else: a base-denominated ladder counts coins, a
/// contract-denominated one counts contracts and each contract is
/// `base_per_contract` coins.
fn base_exposure_raw(size: Qty, spec: &ContractSpec) -> i128 {
    match spec.size_unit {
        SizeUnit::Base => size.raw(),
        SizeUnit::Contracts => match spec.size {
            ContractSize::BasePerContract(base_per) => size.raw() * base_per.raw() / SCALE,
            ContractSize::QuotePerContract(_) => {
                panic!("an inverse contract's base exposure needs a price; not this test")
            }
        },
    }
}

/// Which of the two returned legs is OKX's. `price_pair` decides the sides itself,
/// so the test asks rather than assumes.
fn okx_leg(opp: &Opportunity) -> &arb_scan::OppLeg {
    if opp.short.vkey.venue == Venue::Okx {
        &opp.short
    } else {
        &opp.long
    }
}

// ===========================================================================
// Finding 1 — OKX as the long leg: the headline notional is 100x the truth.
//
// Hand derivation, exact integers, independent of the code.
//
//   target      = 100 000 USDT
//   OKX ask[0]  = 80 000.0        (long buys asks)
//   OKX ctVal   = 0.01 BTC/contract
//   lot         = max(0.001, 0.01) = 0.01     (price_pair takes the coarser)
//
//   requested_in_book_units(100000, 80000, okx) takes the BasePerContract branch:
//       100000 / 80000 = 5/4 = 1.25          exactly, so REVENUE truncates nothing
//   round_down_to_lot(1.25, 0.01): 1.25 / 0.01 = 125 exactly  ->  size = 1.25
//
//   fit_to_budget: notional_of(1.25, 80000, okx, COST) = 1.25 * 80000 = 100 000
//       100 000 <= 100 000, so the budget check passes on the first pass. The
//       under-request and the over-report cancel; nothing in the loop can see it.
//
//   REPORTED  notional = (5/4) * 80000       = 400000/4     = 100 000 USDT
//   TRUE      notional = (5/4) * (1/100) * 80000
//                      = 5 * 80000 / 400     = 400000/400   =   1 000 USDT
//
//   1.25 contracts is 0.0125 BTC. At 80 000 that is 1 000 USDT of exposure, and
//   the row prints 100 000. Factor of exactly 100 = 1 / ctVal.
//
//   raw at scale 1e12:   1 000 -> 1_000_000_000_000_000
//                      100 000 -> 100_000_000_000_000_000
// ===========================================================================

/// OKX long, Bybit short. `Opportunity::notional` is the long leg's, so the
/// headline number is the OKX one.
///
/// Sides: OKX rate −0.0001 per 8h, Bybit +0.0001. `assign_sides` sums the signed
/// annual receipts, so shorting Bybit and longing OKX collects on both legs
/// (+0.1095 each) while the reverse pays on both. OKX therefore ends up long.
#[test]
fn okx_long_reports_a_notional_one_hundred_times_the_position_it_opens() {
    let (okx, bybit) = okx_long_pair();

    let opp = price_pair(&okx, &bybit, &cfg()).expect("pair prices");

    assert_eq!(
        opp.long.vkey.venue,
        Venue::Okx,
        "test setup: OKX must be the long leg for this assertion to be about the headline"
    );
    assert_eq!(opp.size.raw(), 1_250_000_000_000, "size is 1.25 book units");

    // 1.25 contracts of a 0.01 BTC contract at 80 000 is 1 000 USDT, not 100 000.
    assert_eq!(
        opp.notional,
        money("1000"),
        "OKX declares size_unit = Contracts and ctVal = 0.01, so 1.25 book units is \
         0.0125 BTC = 1 000 USDT. Reported {:?} (raw {}), which is 100x the position \
         the operator opens by typing 1.25 into OKX. blocked = {:?}",
        opp.notional,
        opp.notional.raw(),
        opp.blocked,
    );
}

// ===========================================================================
// Finding 2 — OKX as the short leg: the headline is right, the margin is not.
//
//   OKX bid[0] = 79 999.0        (short sells into bids)
//   size       = 1.25            (solved on the Bybit leg, which is base-denominated
//                                 and therefore correct: 100000/80000 = 1.25)
//
//   79999 * 125 = 79999*100 + 79999*25 = 7 999 900 + 1 999 975 = 9 999 875
//
//   REPORTED  OKX notional = (5/4) * 79999          = 9 999 875 / 100  = 99 998.75
//   TRUE      OKX notional = (5/4) * (1/100) * 79999 = 9 999 875 / 10000 =   999.9875
//
//   At leverage 1, margin == notional on each leg, so:
//
//   REPORTED  Opportunity::margin = 99 998.75 + 100 000 = 199 998.75
//   TRUE      Opportunity::margin =    999.9875 + 100 000 = 100 999.9875
//
//   And Opportunity::notional is the *long* leg's — Bybit's, 100 000, correct.
//   The headline is clean while the capital requirement is nearly doubled: this is
//   the direction the charter section 0 calls the dangerous one, because the row
//   ranks on net_apr and net_apr survives intact.
//
//   raw at scale 1e12:     999.9875 -> 999_987_500_000_000
//                       99 998.75   -> 99_998_750_000_000_000
// ===========================================================================

/// OKX short, Bybit long. Signs reversed from finding 1.
#[test]
fn okx_short_reports_a_leg_margin_one_hundred_times_the_capital_at_risk() {
    let (okx, bybit) = okx_short_pair();

    let opp = price_pair(&okx, &bybit, &cfg()).expect("pair prices");

    assert_eq!(
        opp.short.vkey.venue,
        Venue::Okx,
        "test setup: OKX must be the short leg here"
    );
    assert_eq!(opp.size.raw(), 1_250_000_000_000, "size is 1.25 book units");

    // The headline is the long leg's and is correct — that is what hides this.
    assert_eq!(
        opp.notional,
        money("100000"),
        "the Bybit leg is base-denominated, so the headline notional is right"
    );

    // 1.25 contracts at 79 999.0 is 999.9875 USDT of exposure, hence of margin at
    // leverage 1.
    let okx_side = okx_leg(&opp);
    assert_eq!(
        okx_side.margin,
        money("999.9875"),
        "OKX margin: 1.25 contracts * 0.01 BTC * 79 999.0 = 999.9875 USDT at 1x. \
         Reported {:?} (raw {}) — 100x, while opp.notional above is correct, so \
         nothing on the row disagrees with itself. opp.margin = {:?} against a true \
         100 999.9875. blocked = {:?}",
        okx_side.margin,
        okx_side.margin.raw(),
        opp.margin,
        opp.blocked,
    );
}

// ===========================================================================
// Finding 3 — the row is not a hedge, whichever way the sides fall.
//
// price_pair solves one `size` and hands the same number to price_leg for both
// legs (scan.rs, the two calls that differ only in side). A single scalar cannot
// be a size in two different units.
//
//   Bybit, size_unit = Base:      1.25 book units = 1.25    BTC
//   OKX,   size_unit = Contracts: 1.25 book units = 1.25 * 0.01 = 0.0125 BTC
//
//   naked delta = 1.25 - 0.0125 = 1.2375 BTC
//               = 1.2375 * 80 000 = 99 000 USDT of directional exposure
//
//   1.2375 * 8 = 9.9, so 1.2375 * 80000 = 9.9 * 10^4 = 99 000.
//
//   raw at scale 1e12:  1.25   -> 1_250_000_000_000
//                       0.0125 ->    12_500_000_000
//                       1.2375 -> 1_237_500_000_000
//
// net_apr, fee_bps and slippage_bps are all ratios computed consistently within
// book units, so every one of them survives this intact. Nothing in the APR column
// or in the quality flags signals that 99 000 USDT of the position is naked.
// ===========================================================================

/// The two legs' base exposures must be equal, or the position is not flat.
#[test]
fn one_size_for_two_units_leaves_the_position_naked() {
    let (okx, bybit) = okx_long_pair();

    let opp = price_pair(&okx, &bybit, &cfg()).expect("pair prices");

    let okx_base = base_exposure_raw(opp.size, &okx.instrument.spec);
    let bybit_base = base_exposure_raw(opp.size, &bybit.instrument.spec);

    assert_eq!(
        okx_base, 12_500_000_000,
        "1.25 OKX contracts at 0.01 BTC each is 0.0125 BTC"
    );
    assert_eq!(
        bybit_base, 1_250_000_000_000,
        "1.25 Bybit book units is 1.25 BTC"
    );

    assert_eq!(
        bybit_base,
        okx_base,
        "a carry must be flat in the base asset. One size ({}) sent to both venues \
         opens 1.25 BTC on Bybit against 0.0125 BTC on OKX: 1.2375 BTC naked, \
         99 000 USDT of directional exposure at 80 000, presented as a hedge. \
         Residual raw = {}. net_apr = {:?} is unaffected, which is why no column \
         on the row shows it.",
        opp.size.raw(),
        bybit_base - okx_base,
        opp.net_apr,
    );
}

// ===========================================================================
// Evidence, not a finding. This one is expected to PASS.
//
// Its job is to close the one escape route the three failures above have: that
// the row is blocked anyway and therefore harmless. It is not blocked. Nothing in
// `blocked`, `is_rankable()` true, paint score 0 on both legs — the 100x row sorts
// among the actionable ones, above every honest fixture row, because
// `rank_key` puts rankable rows first and orders them by `net_apr`, and `net_apr`
// is the one number this defect leaves intact.
//
// If this test ever starts failing, read why before touching the three above: a
// new blocker on this row would change what the defect costs, not whether it
// exists.
// ===========================================================================

/// The hundredfold row is ranked, not blocked.
#[test]
fn the_hundredfold_row_is_not_blocked_by_anything() {
    let (okx, bybit) = okx_long_pair();

    let opp = price_pair(&okx, &bybit, &cfg()).expect("pair prices");

    assert_eq!(
        opp.blocked.len(),
        0,
        "nothing blocks this row: {:?}",
        opp.blocked
    );
    assert_eq!(opp.short.paint_score, 0, "short ladder is clean");
    assert_eq!(opp.long.paint_score, 0, "long ladder is clean");
    assert!(
        opp.is_rankable(),
        "the row carrying a 100x notional is rankable, quality = {:?}",
        opp.quality
    );
}
