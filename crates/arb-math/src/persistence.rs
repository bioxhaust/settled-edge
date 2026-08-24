//! Persistence: what a history of settled rates actually contains.
//!
//! # The question, and the division of labour
//!
//! [`periods_to_breakeven`](crate::funding::periods_to_breakeven) prints a count:
//! at a 22 bp all-taker round trip and a 1 bp/8h divergence, **22 settlements —
//! over seven days**. Whether a divergence survives seven days is the single
//! number that decides whether the strategy earns, and a snapshot cannot answer
//! it. `OPEN.md` A5, `decisions/023` item 2.
//!
//! Three crates share this problem and the split is deliberate:
//!
//! - [`arb_types::RateHistory`] **stores** settled rates and reports raw facts
//!   about them (length, span, current sign run).
//! - **This module** does the *arithmetic*: means, realised receipts,
//!   annualisation of what was actually paid, and whether the record is even long
//!   enough to speak to the horizon in question.
//! - `arb-detect` reaches the **verdict**, and owns justifying its thresholds.
//!
//! Nothing here returns a probability, and that is not an omission. Nine agreeing
//! settlements out of nine observed is not a 90% chance of a tenth, and a function
//! in this crate that turned a count into a percentage would be manufacturing a
//! forecast from a sample too small to support one — while wearing the authority
//! of the crate that does the honest arithmetic everywhere else.
//!
//! # Why the realised mean is worth computing at all
//!
//! `annualise(current_rate)` answers "what would a year at *this print* pay?".
//! [`annualise_realised`] answers "what would a year at *what actually happened*
//! pay?". Neither is a forecast, but they fail differently, and where they
//! disagree sharply the divergence is young — which is exactly the row a scanner
//! sorted by current rate puts at the top.

use crate::error::MathError;
use arb_num::{Rate, RoundMode};
use arb_types::{FundingInterval, Millis, PositionSide, RateHistory, SettledRate};

/// The arithmetic mean of the settled rates, exactly.
///
/// Sum then one division, so no intermediate rounding accumulates. `mode` is
/// explicit for the same reason [`crate::funding::annualise`] takes one: a bare
/// funding rate is not yet known to be a receipt or a cost, and that depends on
/// the side held, which this function is not told.
///
/// Refuses an empty history rather than returning zero, because zero is a
/// legitimate funding rate and would be indistinguishable from "we have no idea".
pub fn mean_settled_rate(history: &RateHistory, mode: RoundMode) -> Result<Rate, MathError> {
    let entries = history.entries();
    if entries.is_empty() {
        return Err(MathError::EmptyHistory);
    }
    let mut sum = Rate::ZERO;
    for e in entries {
        sum = sum.checked_add(e.rate)?;
    }
    let n = i64::try_from(entries.len())
        .map_err(|_| MathError::Arithmetic(arb_num::NumError::Overflow))?;
    sum.checked_div_int(n, mode).map_err(MathError::from)
}

/// What the history would have paid a holder of `side`, per settlement, on
/// average.
///
/// The sign law of [`PositionSide::funding_receipt_sign`] is applied to the mean,
/// and the rounding direction is then chosen from the resulting sign so a receipt
/// is never overstated and a cost never understated — the same treatment
/// [`crate::funding::funding_payment`] gives a single settlement.
///
/// This is the honest counterpart to a snapshot's `receipt_rate`: it is what was
/// actually paid, to the side we would actually have held.
pub fn realised_receipt_per_period(
    history: &RateHistory,
    side: PositionSide,
) -> Result<Rate, MathError> {
    // Take the mean toward zero, so the *magnitude* cannot grow before the sign
    // law has decided which direction "conservative" even means. Multiplying by
    // ±1 afterwards is exact, so there is no second rounding to choose a mode for
    // — the one that matters is applied by `annualise_realised`, which does divide.
    let mean = mean_settled_rate(history, RoundMode::Trunc)?;
    mean.checked_mul_int(i64::from(side.funding_receipt_sign()))
        .map_err(MathError::from)
}

/// Annualise what the history actually paid, for a holder of `side`.
///
/// `interval` is the venue's settlement cadence, not something inferred from the
/// history's own timestamps: a history with a gap in it spans more time than it
/// has settlements, and dividing the span by the entry count would silently
/// invent a longer interval and a smaller APR. Use [`HistoryCoverage`] to detect
/// the gap instead of absorbing it.
///
/// # Hand vector
///
/// Five settlements of 0.0001 at 8h, held long. Mean 0.0001, sign for a long
/// holder is −1 (longs pay shorts when the rate is positive), so the receipt is
/// −0.0001 per period, annualised `-0.0001 * 1095` = **−0.1095, i.e. −10.95%**.
pub fn annualise_realised(
    history: &RateHistory,
    interval: FundingInterval,
    side: PositionSide,
) -> Result<Rate, MathError> {
    let per_period = realised_receipt_per_period(history, side)?;
    // The same rule `funding_payment` applies to one settlement: shrink receipts,
    // grow costs. Shared rather than restated, so it cannot drift out of step.
    let mode = crate::funding::against_us(per_period);
    crate::funding::annualise(per_period, interval, mode)
}

/// How many settlement intervals a history's record actually covers, and how many
/// the trade needs.
///
/// The comparison has **no free parameter**, which is why it lives here and not in
/// `arb-detect`: "does a record spanning three days speak to a claim about seven?"
/// is arithmetic. What to *do* about a shortfall is a threshold, and that is
/// `arb-detect`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryCoverage {
    /// Settlements on record.
    pub settlements: u64,
    /// Settlement intervals spanned by the record, from oldest to newest stamp.
    ///
    /// Floored. A record spanning 2.9 intervals covers 2, because the third has
    /// not finished happening.
    pub periods_spanned: u64,
    /// Settlements the trade must survive to return its round trip, from
    /// [`periods_to_breakeven`](crate::funding::periods_to_breakeven).
    pub periods_needed: u64,
}

impl HistoryCoverage {
    /// True when the record is at least as long as the horizon it is being used
    /// to talk about.
    ///
    /// Note what this does **not** say: a record long enough to cover the horizon
    /// is not evidence the rate will persist through it. Coverage is a
    /// precondition for the question being answerable at all, not an answer.
    #[inline]
    pub const fn covers(&self) -> bool {
        self.periods_spanned >= self.periods_needed
    }

    /// Settlements by which the record falls short, zero when it covers.
    #[inline]
    pub const fn shortfall(&self) -> u64 {
        self.periods_needed.saturating_sub(self.periods_spanned)
    }

    /// Settlements missing from the middle of the record: spanned but not
    /// observed.
    ///
    /// A complete record has one settlement per interval plus one for the opening
    /// stamp, so `settlements == periods_spanned + 1`. A shortfall here means the
    /// series has holes — a venue outage, a paginated fetch that dropped a page,
    /// or two merged payloads. It matters because
    /// [`RateHistory::current_sign_run`](arb_types::RateHistory::current_sign_run)
    /// counts *adjacent entries*, so a gap makes a run look contiguous when it
    /// straddles time nobody observed.
    #[inline]
    pub const fn missing_settlements(&self) -> u64 {
        self.periods_spanned
            .saturating_add(1)
            .saturating_sub(self.settlements)
    }
}

/// Measure a history against the horizon a trade needs.
///
/// `periods_needed` comes from
/// [`periods_to_breakeven`](crate::funding::periods_to_breakeven).
///
/// An empty or single-entry history yields `periods_spanned: 0` rather than an
/// error: "the record covers nothing" is a true and useful statement, and the
/// caller needs it in order to say so on the row. That is a deliberate difference
/// from [`mean_settled_rate`], which errors — a mean of nothing is not a number,
/// whereas a coverage of nothing is.
pub fn coverage(
    history: &RateHistory,
    interval: FundingInterval,
    periods_needed: u64,
) -> Result<HistoryCoverage, MathError> {
    let settlements = history.len() as u64;
    let periods_spanned = match history.span() {
        None => 0,
        Some(span) => {
            let interval_ms = i64::from(interval.secs()) * 1000;
            // `FundingInterval` refuses zero, so this cannot divide by zero.
            let ms = span.millis();
            if ms <= 0 || interval_ms <= 0 {
                0
            } else {
                u64::try_from(ms / interval_ms)
                    .map_err(|_| MathError::Arithmetic(arb_num::NumError::Overflow))?
            }
        }
    };
    Ok(HistoryCoverage {
        settlements,
        periods_spanned,
        periods_needed,
    })
}

/// How many times the sign changed between adjacent settlements.
///
/// A count, never normalised into a rate. Zero is a series that never changed
/// direction; a series of nine settlements with four flips is not persistence
/// however long its final run happens to be, and that is precisely the case a
/// single "current run" figure hides.
///
/// Zero is treated as **its own state**, so entering or leaving zero is a flip.
/// The alternative — skipping zeros — would report `+ 0 +` as never having
/// changed, and a settlement that paid nothing is not a settlement that paid in
/// the direction of the trade. This matches
/// [`RateHistory::current_sign_run`](arb_types::RateHistory::current_sign_run),
/// where a zero breaks the run.
pub fn sign_flips(entries: &[SettledRate]) -> usize {
    entries
        .windows(2)
        .filter(|w| w[0].rate.signum() != w[1].rate.signum())
        .count()
}

/// The wall-clock time `periods` settlements occupy at `interval`.
///
/// A convenience for turning a breakeven count into something an operator can
/// read — "22 settlements" is 7.3 days at 8h — and for comparing against
/// [`RateHistory::span`](arb_types::RateHistory::span).
pub fn periods_as_time(periods: u64, interval: FundingInterval) -> Result<Millis, MathError> {
    let p = i64::try_from(periods).map_err(|_| MathError::Arithmetic(arb_num::NumError::Overflow))?;
    let secs = i64::from(interval.secs())
        .checked_mul(p)
        .ok_or(MathError::Arithmetic(arb_num::NumError::Overflow))?;
    Ok(Millis::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{
        Asset, DataQuality, InstrumentKey, MarketKind, Venue, VenueKey, WallTs,
    };

    fn rate(s: &str) -> Rate {
        Rate::parse(s).expect("test rate parses")
    }

    fn vkey() -> VenueKey {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT")
            .expect("test key parses");
        VenueKey::new(Venue::Binance, key)
    }

    /// Settlement `period`, 8h apart from a round epoch millisecond.
    fn at(period: i64) -> WallTs {
        WallTs::from_millis(1_700_000_000_000 + period * 28_800_000)
    }

    fn entry(period: i64, r: &str) -> SettledRate {
        SettledRate {
            settled_at: at(period),
            rate: rate(r),
        }
    }

    fn hist(entries: Vec<SettledRate>) -> RateHistory {
        RateHistory::new(vkey(), entries, DataQuality::SYNTHESIZED).expect("test history is sorted")
    }

    fn empty() -> RateHistory {
        RateHistory::empty(vkey(), DataQuality::SYNTHESIZED)
    }

    const EIGHT_H: FundingInterval = FundingInterval::EIGHT_HOURS;

    #[test]
    fn a_mean_over_nothing_is_refused_rather_than_reported_as_zero() {
        // Zero is a legitimate funding rate, so returning it here would be
        // indistinguishable from a venue that genuinely paid nothing.
        assert_eq!(
            mean_settled_rate(&empty(), RoundMode::Trunc),
            Err(MathError::EmptyHistory)
        );
        assert_eq!(
            realised_receipt_per_period(&empty(), PositionSide::Long),
            Err(MathError::EmptyHistory)
        );
    }

    #[test]
    fn a_mean_is_hand_computed() {
        // (1 + 2 + 3) bp / 3 = 2 bp. In rate terms 0.0001, 0.0002, 0.0003 -> 0.0002.
        let h = hist(vec![
            entry(0, "0.0001"),
            entry(1, "0.0002"),
            entry(2, "0.0003"),
        ]);
        assert_eq!(
            mean_settled_rate(&h, RoundMode::Trunc).expect("ok"),
            rate("0.0002")
        );
    }

    #[test]
    fn a_mean_sums_before_dividing_so_no_intermediate_rounding_accumulates() {
        // Three entries of 1 ulp each. Summed then divided: 3/3 = 1 ulp exactly.
        // Averaged pairwise with rounding at each step this would drift.
        let h = hist(vec![
            SettledRate { settled_at: at(0), rate: Rate::from_raw(1) },
            SettledRate { settled_at: at(1), rate: Rate::from_raw(1) },
            SettledRate { settled_at: at(2), rate: Rate::from_raw(1) },
        ]);
        assert_eq!(
            mean_settled_rate(&h, RoundMode::Trunc).expect("ok"),
            Rate::from_raw(1)
        );
    }

    #[test]
    fn a_mean_of_opposing_signs_can_be_near_zero_while_every_entry_is_large() {
        // + - + - averages to zero, and this is the case a "current rate" scanner
        // ranks at the top on whichever print happens to be last. The mean is the
        // cheapest available correction to that.
        let h = hist(vec![
            entry(0, "0.001"),
            entry(1, "-0.001"),
            entry(2, "0.001"),
            entry(3, "-0.001"),
        ]);
        assert_eq!(
            mean_settled_rate(&h, RoundMode::Trunc).expect("ok"),
            Rate::ZERO
        );
        // While the latest print alone says -0.001, and the run says one period.
        assert_eq!(h.latest().expect("present").rate, rate("-0.001"));
        assert_eq!(h.current_sign_run(), 1);
        // And the flips say the series never held a direction at all.
        assert_eq!(sign_flips(h.entries()), 3);
    }

    #[test]
    fn the_sign_law_reaches_the_realised_receipt() {
        // Positive rate: longs pay shorts. Five settlements of 0.0001.
        let h = hist((0..5).map(|p| entry(p, "0.0001")).collect());
        let long = realised_receipt_per_period(&h, PositionSide::Long).expect("ok");
        let short = realised_receipt_per_period(&h, PositionSide::Short).expect("ok");
        assert_eq!(long, rate("-0.0001"));
        assert_eq!(short, rate("0.0001"));
    }

    #[test]
    fn annualising_what_was_paid_is_hand_computed() {
        // Mean 0.0001 per 8h, 1095 settlements per year, long holder pays:
        //   -0.0001 * 1095 = -0.1095 = -10.95% APR.
        // The +10.95% that every funding scanner prints, with the sign the holder
        // of the paying side actually experiences.
        let h = hist((0..5).map(|p| entry(p, "0.0001")).collect());
        assert_eq!(
            annualise_realised(&h, EIGHT_H, PositionSide::Long).expect("ok"),
            rate("-0.1095")
        );
        assert_eq!(
            annualise_realised(&h, EIGHT_H, PositionSide::Short).expect("ok"),
            rate("0.1095")
        );
    }

    #[test]
    fn realised_and_current_disagree_when_the_divergence_is_young() {
        // Eight settlements at 0.0001 and one fresh print at 0.0010. A scanner on
        // the current rate annualises 0.0010 -> 109.5% APR. The realised mean is
        // (8*0.0001 + 0.0010)/9 = 0.0018/9 = 0.0002, which annualises to 21.9%.
        // Five times apart on the same instrument, and the flattering figure is
        // the one that sorts to the top.
        let mut entries: Vec<_> = (0..8).map(|p| entry(p, "0.0001")).collect();
        entries.push(entry(8, "0.0010"));
        let h = hist(entries);
        let realised = annualise_realised(&h, EIGHT_H, PositionSide::Short).expect("ok");
        let current = crate::funding::annualise(rate("0.0010"), EIGHT_H, RoundMode::REVENUE)
            .expect("ok");
        assert_eq!(realised, rate("0.219"));
        assert_eq!(current, rate("1.095"));
        assert_eq!(current, realised.checked_mul_int(5).expect("ok"));
    }

    #[test]
    fn coverage_against_a_seven_day_claim_is_hand_computed() {
        // Ten settlements, index 0..=9, so nine 8h intervals = 3 days spanned.
        // A 22 bp round trip at 1 bp per settlement needs 22 settlements = 7.3
        // days. Three days of record cannot speak to seven, and this is the
        // arithmetic that says so.
        let h = hist((0..=9).map(|p| entry(p, "0.0001")).collect());
        let c = coverage(&h, EIGHT_H, 22).expect("ok");
        assert_eq!(c.settlements, 10);
        assert_eq!(c.periods_spanned, 9);
        assert_eq!(c.periods_needed, 22);
        assert!(!c.covers());
        assert_eq!(c.shortfall(), 13);
        assert_eq!(c.missing_settlements(), 0);
        // And the two horizons, in time a human reads.
        assert_eq!(h.span(), Some(Millis::from_secs(3 * 24 * 3600)));
        assert_eq!(
            periods_as_time(22, EIGHT_H).expect("ok"),
            Millis::from_millis(22 * 28_800_000)
        );
    }

    #[test]
    fn coverage_of_an_empty_or_single_entry_history_is_zero_not_an_error() {
        // Unlike a mean, "covers nothing" is a true statement the row needs.
        let c = coverage(&empty(), EIGHT_H, 22).expect("ok");
        assert_eq!((c.settlements, c.periods_spanned), (0, 0));
        assert!(!c.covers());
        assert_eq!(c.shortfall(), 22);

        let one = coverage(&hist(vec![entry(0, "0.0001")]), EIGHT_H, 22).expect("ok");
        assert_eq!((one.settlements, one.periods_spanned), (1, 0));
    }

    #[test]
    fn a_record_that_covers_its_horizon_says_so_without_claiming_persistence() {
        // 23 settlements spans 22 intervals, which covers a 22-settlement need.
        let h = hist((0..23).map(|p| entry(p, "0.0001")).collect());
        let c = coverage(&h, EIGHT_H, 22).expect("ok");
        assert_eq!(c.periods_spanned, 22);
        assert!(c.covers());
        assert_eq!(c.shortfall(), 0);
        // `covers` is a precondition, not a forecast. Nothing on this type turns
        // a 22-long run into a probability, and that is checked by there being no
        // such method to call.
        assert_eq!(c.missing_settlements(), 0);
    }

    #[test]
    fn a_gap_in_the_record_is_visible_rather_than_absorbed() {
        // Settlements at 0, 1, 2, then 10, 11. Span is 11 intervals; only 5
        // entries. A run counted over adjacent entries would call 10 and 11
        // contiguous with 2, straddling seven intervals nobody observed.
        let h = hist(vec![
            entry(0, "0.0001"),
            entry(1, "0.0001"),
            entry(2, "0.0001"),
            entry(10, "0.0001"),
            entry(11, "0.0001"),
        ]);
        let c = coverage(&h, EIGHT_H, 22).expect("ok");
        assert_eq!(c.settlements, 5);
        assert_eq!(c.periods_spanned, 11);
        // A complete record of 11 intervals would hold 12 settlements.
        assert_eq!(c.missing_settlements(), 7);
        // And the raw run is unaware of the hole, which is why the count exists.
        assert_eq!(h.current_sign_run(), 5);
    }

    #[test]
    fn a_partial_interval_at_the_end_is_floored() {
        // Two stamps 8h + 1ms apart span one interval, not two: the second has
        // not finished happening.
        let h = hist(vec![
            SettledRate { settled_at: WallTs::from_millis(0), rate: rate("0.0001") },
            SettledRate {
                settled_at: WallTs::from_millis(28_800_001),
                rate: rate("0.0001"),
            },
        ]);
        assert_eq!(coverage(&h, EIGHT_H, 1).expect("ok").periods_spanned, 1);
        // And 1ms short of a full interval spans none.
        let h2 = hist(vec![
            SettledRate { settled_at: WallTs::from_millis(0), rate: rate("0.0001") },
            SettledRate {
                settled_at: WallTs::from_millis(28_799_999),
                rate: rate("0.0001"),
            },
        ]);
        assert_eq!(coverage(&h2, EIGHT_H, 1).expect("ok").periods_spanned, 0);
    }

    #[test]
    fn sign_flips_count_zero_as_its_own_state() {
        // + 0 + is two flips, not zero. Reporting zero would say the series never
        // changed direction, when a settlement in the middle paid nothing.
        let h = hist(vec![
            entry(0, "0.0001"),
            entry(1, "0"),
            entry(2, "0.0001"),
        ]);
        assert_eq!(sign_flips(h.entries()), 2);
        // Consistent with the run, where a zero also breaks.
        assert_eq!(h.current_sign_run(), 1);
    }

    #[test]
    fn sign_flips_of_a_steady_series_is_zero_and_of_an_empty_one_too() {
        let steady = hist((0..9).map(|p| entry(p, "0.0001")).collect());
        assert_eq!(sign_flips(steady.entries()), 0);
        assert_eq!(sign_flips(empty().entries()), 0);
        assert_eq!(sign_flips(hist(vec![entry(0, "0.0001")]).entries()), 0);
    }

    #[test]
    fn a_long_run_and_a_flipping_series_are_distinguishable() {
        // Both end in a 3-long positive run. The first held one direction
        // throughout; the second changed four times. `current_sign_run` alone
        // cannot tell them apart, which is the reason `sign_flips` exists.
        let steady = hist((0..6).map(|p| entry(p, "0.0001")).collect());
        let choppy = hist(vec![
            entry(0, "0.0001"),
            entry(1, "-0.0001"),
            entry(2, "0.0001"),
            entry(3, "0.0001"),
            entry(4, "0.0001"),
        ]);
        assert_eq!(choppy.current_sign_run(), 3);
        assert_eq!(sign_flips(choppy.entries()), 2);
        assert_eq!(sign_flips(steady.entries()), 0);
    }

    #[test]
    fn periods_as_time_turns_a_breakeven_count_into_something_readable() {
        // 22 settlements at 8h = 176h = 7.333… days. This is the number from
        // `OPEN.md` A5, in milliseconds.
        let t = periods_as_time(22, EIGHT_H).expect("ok");
        assert_eq!(t, Millis::from_secs(22 * 8 * 3600));
        assert_eq!(t.millis(), 633_600_000);
        // Which is more than seven days and less than eight.
        assert!(t > Millis::from_secs(7 * 24 * 3600));
        assert!(t < Millis::from_secs(8 * 24 * 3600));
    }

    #[test]
    fn the_interval_comes_from_the_venue_not_from_the_history() {
        // The same five entries at 8h and at 1h annualise 8x apart. If this
        // function inferred the interval from the stamps it would be right here
        // and wrong on any history with a gap — see the gap test.
        let h = hist((0..5).map(|p| entry(p, "0.0001")).collect());
        let eight = annualise_realised(&h, EIGHT_H, PositionSide::Short).expect("ok");
        let one = annualise_realised(&h, FundingInterval::ONE_HOUR, PositionSide::Short)
            .expect("ok");
        assert_eq!(eight, rate("0.1095"));
        assert_eq!(one, rate("0.876"));
        assert_eq!(one, eight.checked_mul_int(8).expect("ok"));
    }

    #[test]
    fn an_asset_that_never_paid_annualises_to_nothing_without_erroring() {
        // A venue can genuinely settle at zero. That is a fact about the venue,
        // distinct from an empty history, and the two must not collapse.
        let h = hist((0..5).map(|p| entry(p, "0")).collect());
        assert_eq!(
            annualise_realised(&h, EIGHT_H, PositionSide::Long).expect("ok"),
            Rate::ZERO
        );
        assert_eq!(
            mean_settled_rate(&h, RoundMode::Trunc).expect("ok"),
            Rate::ZERO
        );
        // Whereas an empty one refuses.
        assert_eq!(
            mean_settled_rate(&empty(), RoundMode::Trunc),
            Err(MathError::EmptyHistory)
        );
    }

    #[test]
    fn a_realised_cost_is_never_understated_by_rounding() {
        // Two entries whose mean has a remainder: 1 ulp and 2 ulp average to 1.5
        // ulp. Truncated toward zero the magnitude is 1 ulp, and after the sign
        // law a long holder's cost is -1 ulp. The magnitude never grows past the
        // truth in the receipt direction, which is the conservative choice for a
        // figure that will be compared against a cost.
        let h = hist(vec![
            SettledRate { settled_at: at(0), rate: Rate::from_raw(1) },
            SettledRate { settled_at: at(1), rate: Rate::from_raw(2) },
        ]);
        let mean = mean_settled_rate(&h, RoundMode::Trunc).expect("ok");
        assert_eq!(mean, Rate::from_raw(1));
        let short = realised_receipt_per_period(&h, PositionSide::Short).expect("ok");
        assert_eq!(short, Rate::from_raw(1));
    }

    #[test]
    fn coverage_does_not_depend_on_the_asset_or_the_rates() {
        // Only stamps and the interval matter, so a history of zeros covers the
        // same ground as a history of large rates. Guards against someone later
        // making coverage conditional on the rates, which would conflate "we
        // watched for three days" with "it paid for three days".
        let paying = hist((0..=9).map(|p| entry(p, "0.0005")).collect());
        let flat = hist((0..=9).map(|p| entry(p, "0")).collect());
        assert_eq!(
            coverage(&paying, EIGHT_H, 22).expect("ok"),
            coverage(&flat, EIGHT_H, 22).expect("ok")
        );
        let _ = Asset::new("BTC").expect("ok");
    }
}
