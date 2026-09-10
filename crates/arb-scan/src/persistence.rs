#![forbid(unsafe_code)]
//! What the settled record says about the rate a row is priced on.
//!
//! # The gap this module closes
//!
//! `net_apr` is computed from **one** number per leg: the venue's currently
//! published funding rate. That rate is a snapshot, often a *prediction*, and one
//! observation cannot distinguish a rate that has paid the same way for thirty
//! days from a rate that flipped sign an hour ago. Until this module existed the
//! row could not either: a leg with thirty settlements behind it and a leg with
//! none produced byte-identical rows apart from one non-blocking flag
//! (`RATE_HISTORY_MISSING`), and `RateHistory::current_sign_run` — written
//! precisely for this — had no callers anywhere in the workspace (`OPEN.md` A5).
//!
//! # What is here and what is deliberately not
//!
//! Every field of [`PersistenceEvidence`] is a **measurement of the past**. There
//! is no probability, no confidence interval, no forecast, and no score. Nine
//! agreeing settlements out of nine is not a 90% chance of a tenth, and a field
//! that looked like one would be read as one — which is how a scanner ends up
//! ranking on a number nobody measured.
//!
//! The one derived quantity, [`PersistenceEvidence::realised_annual_receipt`], is
//! still a measurement: it is the same annualisation the row already applies to
//! the current rate, applied instead to the mean of the rates that actually
//! settled. Read beside the row's `gross_apr` it answers a question the operator
//! cannot otherwise ask — *is the rate this row is priced on the rate this venue
//! has been paying?* — without claiming anything about the next settlement.
//!
//! # Why this warns and never blocks
//!
//! An unmeasured *probability* of persistence makes `net_apr` conditional: "this
//! is the return if the rate holds". That conditional is honestly useful, and the
//! report's job is to state the condition. An unmeasured *cost* — the borrow leg
//! of a reverse carry — makes `net_apr` simply wrong, and only that may not be
//! ranked. The asymmetry is deliberate and it is the reason
//! [`arb_types::QualityFlags::RATE_HISTORY_MISSING`] is outside
//! [`arb_types::QualityFlags::BLOCKING`] while
//! [`arb_types::QualityFlags::BORROW_COST_UNKNOWN`] is inside it.

use arb_math::{annualise_realised, coverage, mean_settled_rate, sign_flips, HistoryCoverage};
use arb_num::{Rate, RoundMode};
use arb_types::{FundingInterval, Millis, PositionSide, RateHistory, VenueKey};

use crate::error::ScanError;

/// The settled record of one leg, measured.
///
/// Constructed only by [`evidence`], which returns `None` rather than a zeroed
/// value for a record that does not exist — the whole point is that "nothing
/// behind this row" must not be representable as "zeros behind this row".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceEvidence {
    /// The leg this record belongs to. Carried because a row has two of them and
    /// they can disagree completely.
    pub vkey: VenueKey,
    /// Settlements on record.
    pub settlements: u64,
    /// Settlement intervals between the oldest and newest stamp, floored.
    ///
    /// Not the same as `settlements`: a record with a hole in it spans more time
    /// than it observed, and the difference is what
    /// [`PersistenceEvidence::is_contiguous`] reports.
    pub periods_spanned: u64,
    /// Wall-clock the record spans. `None` for a single settlement, which spans
    /// no time.
    pub span: Option<Millis>,
    /// How many of the most recent settlements share the sign of the latest.
    ///
    /// **This is the number that makes the two rows different.** Thirty against
    /// one is the whole content of `OPEN.md` A5.
    ///
    /// A count, never a probability. And read it against
    /// [`PersistenceEvidence::is_contiguous`]: the run counts *adjacent entries*,
    /// so a gap in the record makes a run look continuous across time nobody
    /// observed.
    pub same_sign_run: usize,
    /// How many times the sign changed across the whole record.
    ///
    /// Carried alongside the run because the run alone flatters a flapping rate:
    /// nine settlements with four flips are not persistence however long the
    /// final run happens to be.
    pub sign_flips: usize,
    /// Mean of the settled rates, truncated toward zero.
    ///
    /// Truncated rather than rounded away from zero because the sign law has not
    /// been applied yet, so "away from zero" does not yet mean "conservative".
    /// This is the same mode [`arb_math::realised_receipt_per_period`] takes the
    /// mean with, so this field and
    /// [`PersistenceEvidence::realised_annual_receipt`] cannot disagree about
    /// what the mean was.
    pub mean_settled_rate: Rate,
    /// The signed annual receipt this leg **would have paid** the side we hold,
    /// computed from the mean settled rate instead of the current published one.
    ///
    /// Negative means the record would have charged us. Directly comparable with
    /// [`crate::OppLeg::annual_receipt`], and the comparison is the point: the
    /// row is priced on the published rate, and this is what the venue has
    /// actually been paying.
    pub realised_annual_receipt: Rate,
}

impl PersistenceEvidence {
    /// Measure this record against a hold of `periods_needed` settlements.
    ///
    /// A free function on the caller's number rather than a stored field, because
    /// the hold a row needs is **not one number**: each round-trip configuration
    /// breaks even at a different settlement count, and a record that covers the
    /// all-maker hold may fall well short of the all-taker one. Storing a single
    /// coverage verdict would have silently answered for whichever configuration
    /// happened to compute it.
    ///
    /// Only the third field is supplied here; `settlements` and `periods_spanned`
    /// come from [`arb_math::coverage`] and are not recomputed, so the
    /// span-over-interval division exists in exactly one place.
    #[inline]
    pub const fn coverage_for(&self, periods_needed: u64) -> HistoryCoverage {
        HistoryCoverage {
            settlements: self.settlements,
            periods_spanned: self.periods_spanned,
            periods_needed,
        }
    }

    /// True when the record has one settlement per interval it spans, with no
    /// holes.
    ///
    /// A hole means a venue outage, a dropped page of a paginated fetch, or two
    /// payloads merged badly. It matters here and not only as bookkeeping,
    /// because [`PersistenceEvidence::same_sign_run`] counts adjacent entries: a
    /// run that straddles a hole claims continuity through time nobody saw.
    #[inline]
    pub const fn is_contiguous(&self) -> bool {
        self.coverage_for(0).missing_settlements() == 0
    }
}

/// Measure one leg's settled record, for the side we would hold it on.
///
/// `Ok(None)` for a record with no entries. That is not a failure and not a zero:
/// an absent record is the state [`arb_types::QualityFlags::RATE_HISTORY_MISSING`]
/// exists to announce, and returning a struct full of zeros would let it be
/// summed, averaged and ranked as though it were a measurement of a rate that
/// never moved.
///
/// `interval` comes from the leg's own funding snapshot, never from the record's
/// timestamps: a record with a gap spans more time than it has settlements, and
/// dividing its span by its entry count would invent a longer interval and a
/// smaller annualised figure — the error being smaller is exactly what would let
/// it survive review.
pub fn evidence(
    history: &RateHistory,
    interval: FundingInterval,
    side: PositionSide,
) -> Result<Option<PersistenceEvidence>, ScanError> {
    if history.is_empty() {
        return Ok(None);
    }
    // `periods_needed: 0` here because this call is only being used for its
    // span-over-interval arithmetic; the real hold is supplied later, per
    // configuration, by `coverage_for`.
    let spanned = coverage(history, interval, 0)?;
    Ok(Some(PersistenceEvidence {
        vkey: history.vkey,
        settlements: spanned.settlements,
        periods_spanned: spanned.periods_spanned,
        span: history.span(),
        same_sign_run: history.current_sign_run(),
        sign_flips: sign_flips(history.entries()),
        mean_settled_rate: mean_settled_rate(history, RoundMode::Trunc)?,
        realised_annual_receipt: annualise_realised(history, interval, side)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{DataQuality, InstrumentKey, MarketKind, SettledRate, Venue, WallTs};

    fn vkey() -> VenueKey {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT")
            .expect("instrument key literal");
        VenueKey::new(Venue::Mexc, key)
    }

    /// 8h apart, oldest first, so `period` counts forward in settlements.
    fn at(period: i64) -> WallTs {
        WallTs::from_millis(1_700_000_000_000 + period * 28_800_000)
    }

    /// `rates[i]` settles at `at(i)`, so a run of `n` entries spans `n - 1`
    /// intervals.
    fn record(rates: &[&str]) -> RateHistory {
        let entries = rates
            .iter()
            .enumerate()
            .map(|(i, r)| SettledRate {
                settled_at: at(i as i64),
                rate: Rate::parse(r).expect("rate literal"),
            })
            .collect();
        RateHistory::new(vkey(), entries, DataQuality::MEASURED).expect("history")
    }

    /// Thirty settlements of the same sign, held short. The row this fixture
    /// backs is the one `OPEN.md` A5 says must not look like a row backed by
    /// nothing.
    #[test]
    fn thirty_one_signed_settlements_are_thirty_one_signed_settlements() {
        let h = record(&["0.0001"; 30]);
        let e = evidence(&h, FundingInterval::EIGHT_HOURS, PositionSide::Short)
            .expect("measures")
            .expect("a record exists");

        assert_eq!(e.settlements, 30);
        // Thirty stamps 8h apart span twenty-nine intervals, not thirty.
        assert_eq!(e.periods_spanned, 29);
        assert_eq!(e.span, Some(Millis::from_millis(29 * 28_800_000)));
        assert_eq!(e.same_sign_run, 30);
        assert_eq!(e.sign_flips, 0);
        // One settlement per interval spanned, plus the opening stamp.
        assert!(e.is_contiguous());

        // Mean of thirty identical rates is that rate, exactly.
        assert_eq!(e.mean_settled_rate, Rate::parse("0.0001").expect("rate"));
        // A short holder receives a positive rate: +0.0001 * 1095 = +0.1095.
        assert_eq!(e.realised_annual_receipt.raw(), 109_500_000_000);
    }

    /// The other row in `OPEN.md` A5: nothing behind it at all.
    ///
    /// `None`, not a zeroed struct. A zeroed struct would report a mean of zero,
    /// a run of zero and a realised receipt of zero — three numbers that read as
    /// measurements of a rate that never paid, when the truth is that nobody
    /// looked.
    #[test]
    fn an_empty_record_measures_to_nothing_rather_than_to_zero() {
        let h = RateHistory::empty(vkey(), DataQuality::MEASURED);
        assert_eq!(
            evidence(&h, FundingInterval::EIGHT_HOURS, PositionSide::Short).expect("measures"),
            None
        );
    }

    /// A single settlement is a record, but it spans no time and supports no
    /// claim about a hold.
    #[test]
    fn one_settlement_spans_nothing_and_says_so() {
        let h = record(&["0.0001"]);
        let e = evidence(&h, FundingInterval::EIGHT_HOURS, PositionSide::Short)
            .expect("measures")
            .expect("a record exists");
        assert_eq!(e.settlements, 1);
        assert_eq!(e.periods_spanned, 0);
        assert_eq!(e.span, None);
        assert_eq!(e.same_sign_run, 1);
        assert_eq!(e.sign_flips, 0);
        // It covers a hold of nothing and falls eleven short of a hold of eleven.
        assert!(e.coverage_for(0).covers());
        assert!(!e.coverage_for(11).covers());
        assert_eq!(e.coverage_for(11).shortfall(), 11);
    }

    /// The case a run length alone hides: the same final run, four times the
    /// churn.
    ///
    /// Both records below end in a run of two. One never changed direction; the
    /// other changed five times. If a row carried only the run, these two would
    /// be indistinguishable — which is the same defect as carrying only a flag,
    /// one level in.
    #[test]
    fn a_run_alone_cannot_tell_persistence_from_flapping() {
        let steady = record(&["0.0001", "0.0001"]);
        let flapping = record(&[
            "0.0001", "-0.0001", "0.0001", "-0.0001", "0.0001", "-0.0001", "0.0001", "0.0001",
        ]);
        let s = evidence(&steady, FundingInterval::EIGHT_HOURS, PositionSide::Short)
            .expect("measures")
            .expect("record");
        let f = evidence(&flapping, FundingInterval::EIGHT_HOURS, PositionSide::Short)
            .expect("measures")
            .expect("record");

        assert_eq!(s.same_sign_run, 2);
        assert_eq!(f.same_sign_run, 2);
        // The run agrees; everything that matters does not.
        assert_eq!(s.sign_flips, 0);
        assert_eq!(f.sign_flips, 6);

        // And the realised receipt separates them too: the flapping record's
        // rates sum to 0.0001*5 + (-0.0001)*3 = +0.0002 over 8 settlements, and
        // 0.0002/8 truncates to 0.000025.
        assert_eq!(f.mean_settled_rate, Rate::parse("0.000025").expect("rate"));
        assert_eq!(s.mean_settled_rate, Rate::parse("0.0001").expect("rate"));
    }

    /// A record whose sign flipped on the newest settlement.
    ///
    /// The run collapses to 1 while the mean stays positive, so the two fields
    /// disagree — and they should: "it has paid us on average" and "it is paying
    /// us now" are different claims and the operator needs both.
    #[test]
    fn the_newest_settlement_can_end_a_long_run_without_moving_the_mean_much() {
        let mut rates = vec!["0.0001"; 29];
        rates.push("-0.0001");
        let h = record(&rates);
        let e = evidence(&h, FundingInterval::EIGHT_HOURS, PositionSide::Short)
            .expect("measures")
            .expect("record");
        assert_eq!(e.same_sign_run, 1);
        assert_eq!(e.sign_flips, 1);
        // (29 * 0.0001 - 0.0001) / 30 = 0.0028/30 = 0.0000933... truncated.
        assert_eq!(e.mean_settled_rate.raw(), 93_333_333);
        assert!(e.realised_annual_receipt.is_positive());
    }

    /// A hole in the middle, and why the run must be read next to it.
    ///
    /// Six settlements spanning ten intervals: four are missing. All six agree in
    /// sign, so the run reports six — a claim of continuity across four
    /// settlements nobody observed. `is_contiguous` is what stops that being
    /// silent.
    #[test]
    fn a_hole_in_the_record_makes_a_run_claim_more_than_it_saw() {
        let entries = [0_i64, 1, 2, 8, 9, 10]
            .iter()
            .map(|&p| SettledRate {
                settled_at: at(p),
                rate: Rate::parse("0.0001").expect("rate literal"),
            })
            .collect();
        let h = RateHistory::new(vkey(), entries, DataQuality::MEASURED).expect("history");
        let e = evidence(&h, FundingInterval::EIGHT_HOURS, PositionSide::Short)
            .expect("measures")
            .expect("record");

        assert_eq!(e.settlements, 6);
        assert_eq!(e.periods_spanned, 10);
        assert_eq!(e.same_sign_run, 6);
        assert!(!e.is_contiguous());
        // Eleven stamps would be a complete record of ten intervals; six is five
        // short.
        assert_eq!(e.coverage_for(0).missing_settlements(), 5);
    }

    /// The side is applied, not assumed. The same record pays a short and charges
    /// a long, and the two must be exact negatives of each other here because the
    /// mean is exact.
    #[test]
    fn the_realised_receipt_takes_the_side_we_actually_hold() {
        let h = record(&["0.0001"; 30]);
        let short = evidence(&h, FundingInterval::EIGHT_HOURS, PositionSide::Short)
            .expect("measures")
            .expect("record");
        let long = evidence(&h, FundingInterval::EIGHT_HOURS, PositionSide::Long)
            .expect("measures")
            .expect("record");
        assert_eq!(short.realised_annual_receipt.raw(), 109_500_000_000);
        assert_eq!(long.realised_annual_receipt.raw(), -109_500_000_000);
    }

    /// The interval is the venue's, not the record's own spacing.
    ///
    /// The same thirty stamps read at a one-hour cadence annualise eight times
    /// higher. Inferring the interval from the stamps would have produced the 8h
    /// answer for both and hidden the difference.
    #[test]
    fn the_annualisation_uses_the_venues_interval_and_not_the_stamp_spacing() {
        let h = record(&["0.0001"; 30]);
        let hourly = evidence(&h, FundingInterval::ONE_HOUR, PositionSide::Short)
            .expect("measures")
            .expect("record");
        // 0.0001 * 8760 = 0.8760.
        assert_eq!(hourly.realised_annual_receipt.raw(), 876_000_000_000);
    }
}
