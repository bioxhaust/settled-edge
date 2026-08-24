//! A history of settled funding rates, and the honest answer to "will it last?"
//!
//! # The question this type exists to stop us dodging
//!
//! `arb-math` prints `periods_to_breakeven`: at a 22 bp all-taker round trip and
//! a 1 bp/8h divergence, **22 periods — over seven days**. The probability that a
//! divergence survives seven days is the single number that decides whether the
//! strategy earns, and a snapshot cannot answer it. A snapshot says what will be
//! paid in eight hours; it says nothing about three days from now. See
//! `OPEN.md` A5 and `decisions/023` item 2.
//!
//! So this type carries **past settled rates**, and it is deliberately austere:
//! it stores observations and reports what they contain. It computes no
//! probability, fits no model, and predicts nothing. `arb-detect` owns the verdict
//! and owns justifying its thresholds; `arb-math` owns any arithmetic over the
//! series. A type in `arb-types` that returned "72% likely to persist" would be
//! inventing a forecast out of nine data points, which is the dishonesty this was
//! built to remove rather than relocate.
//!
//! # Why the rates are settled, not predicted
//!
//! [`crate::FundingSnapshot::rate`] is the rate that *will* apply at the next
//! settlement — a forecast the venue publishes and then revises. A history of
//! forecasts measures the venue's forecasting, not what was actually paid. Every
//! entry here is a rate that **settled**: money that changed hands. Mixing the two
//! series would make persistence look better than it was, because a revised
//! forecast is smoother than the realised sequence.

use crate::instrument::VenueKey;
use crate::quality::DataQuality;
use crate::time::WallTs;
use crate::TypeError;
use arb_num::Rate;

/// One funding payment that actually settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettledRate {
    /// When the settlement occurred, on the venue's clock.
    pub settled_at: WallTs,
    /// The rate that was applied. Signed: the sign law of
    /// [`crate::PositionSide::funding_receipt_sign`] applies unchanged, and a
    /// history whose signs were normalised away would make an inverted carry look
    /// like a persistent one.
    pub rate: Rate,
}

/// Past settled funding rates for one instrument at one venue, oldest first.
///
/// # What the ordering guarantee buys
///
/// Entries are **strictly increasing in `settled_at`**, enforced by the
/// constructor. Two settlements cannot share a timestamp — a venue settles once
/// per interval — so a duplicate stamp means two payloads were merged or one was
/// counted twice, and a double-counted period makes a run of agreement look one
/// period longer than it was. Out-of-order entries would break any windowed
/// reasoning `arb-detect` does over the series.
///
/// # What an empty history means
///
/// Nothing, and it says so: [`RateHistory::is_empty`] is true and callers must
/// raise [`crate::QualityFlags::RATE_HISTORY_MISSING`]. That flag is deliberately
/// non-blocking (see its own docs) because the row's arithmetic remains correct as
/// a conditional — "this is the return *if* the rate holds". It becomes dishonest
/// only when presented as a forecast, which the report must prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateHistory {
    /// Which instrument at which venue.
    pub vkey: VenueKey,
    /// Settled rates, oldest first, strictly increasing in time.
    entries: Vec<SettledRate>,
    /// Provenance and warnings for the series as a whole.
    ///
    /// One quality for the series rather than one per entry: a history assembled
    /// from a single endpoint call has a single provenance, and per-entry quality
    /// would invite a caller to average measured and invented rates into one
    /// number that is neither.
    pub quality: DataQuality,
}

impl RateHistory {
    /// Build a history, enforcing strictly increasing settlement times.
    ///
    /// An empty series is legal and means "we know nothing" — see the type docs.
    pub fn new(
        vkey: VenueKey,
        entries: Vec<SettledRate>,
        quality: DataQuality,
    ) -> Result<Self, TypeError> {
        for w in entries.windows(2) {
            if w[0].settled_at >= w[1].settled_at {
                return Err(TypeError::UnsortedRateHistory);
            }
        }
        Ok(Self {
            vkey,
            entries,
            quality,
        })
    }

    /// An explicitly empty history, for a venue that published none.
    ///
    /// Exists so a caller does not reach for `new(vkey, vec![], q)` and have to
    /// handle a `Result` that cannot fail, which is the kind of friction that
    /// gets resolved with `unwrap` in someone else's crate.
    pub fn empty(vkey: VenueKey, quality: DataQuality) -> Self {
        Self {
            vkey,
            entries: Vec::new(),
            quality,
        }
    }

    /// The settled rates, oldest first.
    #[inline]
    pub fn entries(&self) -> &[SettledRate] {
        &self.entries
    }

    /// How many settlements are on record.
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is on record. See the type docs on what this obliges.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The most recent settled rate, if any.
    #[inline]
    pub fn latest(&self) -> Option<SettledRate> {
        self.entries.last().copied()
    }

    /// The oldest settled rate, if any.
    #[inline]
    pub fn oldest(&self) -> Option<SettledRate> {
        self.entries.first().copied()
    }

    /// The time actually spanned by this history.
    ///
    /// `None` for fewer than two entries: one observation spans no time, and
    /// returning `Millis::ZERO` there would let a caller divide by it. This is the
    /// number to check a claim like "22 periods" against — a history spanning two
    /// days cannot support a statement about seven.
    pub fn span(&self) -> Option<crate::Millis> {
        match (self.oldest(), self.latest()) {
            (Some(o), Some(l)) if self.entries.len() >= 2 => Some(l.settled_at.since(o.settled_at)),
            _ => None,
        }
    }

    /// How many of the most recent settlements kept the same sign, counting back
    /// from the latest.
    ///
    /// This is the rawest possible persistence statistic and it is offered as a
    /// *count*, not a probability: nine agreeing periods out of nine observed is
    /// not a 100% chance of a tenth. Zero rates break a run — a period that paid
    /// nothing is not a period that paid in the direction of the trade.
    ///
    /// `arb-detect` decides what a run length means. This only reports it.
    pub fn current_sign_run(&self) -> usize {
        let Some(last) = self.latest() else {
            return 0;
        };
        if last.rate.is_zero() {
            return 0;
        }
        let sign = last.rate.signum();
        self.entries
            .iter()
            .rev()
            .take_while(|e| !e.rate.is_zero() && e.rate.signum() == sign)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{InstrumentKey, MarketKind};
    use crate::venue::Venue;
    use crate::Millis;

    fn vkey() -> VenueKey {
        // Literals known valid; `unwrap_or` keeps the helper panic-free per rule 4.
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").unwrap_or(
            InstrumentKey::new(
                crate::Asset::default(),
                crate::Asset::default(),
                MarketKind::LinearPerp,
                crate::Asset::default(),
            ),
        );
        VenueKey::new(Venue::Binance, key)
    }

    /// 8h apart, starting at a round epoch millisecond.
    fn at(period: i64) -> WallTs {
        WallTs::from_millis(1_700_000_000_000 + period * 28_800_000)
    }

    fn entry(period: i64, rate: &str) -> SettledRate {
        SettledRate {
            settled_at: at(period),
            rate: Rate::parse(rate).unwrap_or(Rate::ZERO),
        }
    }

    fn hist(entries: Vec<SettledRate>) -> Result<RateHistory, TypeError> {
        RateHistory::new(vkey(), entries, DataQuality::SYNTHESIZED)
    }

    #[test]
    fn an_empty_history_knows_that_it_knows_nothing() {
        let h = RateHistory::empty(vkey(), DataQuality::SYNTHESIZED);
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        assert_eq!(h.latest(), None);
        assert_eq!(h.oldest(), None);
        // No span, so nothing can divide by it.
        assert_eq!(h.span(), None);
        assert_eq!(h.current_sign_run(), 0);
    }

    #[test]
    fn duplicate_settlement_times_are_refused() {
        // A venue settles once per interval. The same stamp twice means two
        // payloads were merged, and a double-counted period makes a run look one
        // period longer than it was.
        let dup = vec![entry(0, "0.0001"), entry(0, "0.0001")];
        assert_eq!(hist(dup), Err(TypeError::UnsortedRateHistory));
    }

    #[test]
    fn out_of_order_entries_are_refused() {
        let backwards = vec![entry(3, "0.0001"), entry(1, "0.0001")];
        assert_eq!(hist(backwards), Err(TypeError::UnsortedRateHistory));
    }

    #[test]
    fn a_single_entry_spans_no_time() {
        // One observation spans nothing. Millis::ZERO here would be a divisor.
        let h = hist(vec![entry(0, "0.0001")]).expect("ok");
        assert_eq!(h.len(), 1);
        assert_eq!(h.span(), None);
    }

    #[test]
    fn span_matches_hand_arithmetic() {
        // Nine 8h periods from index 0 to index 9 is 9 * 28_800_000 ms
        // = 259_200_000 ms = 3 days exactly. By hand.
        let entries: Vec<_> = (0..=9).map(|p| entry(p, "0.0001")).collect();
        let h = hist(entries).expect("ok");
        assert_eq!(h.len(), 10);
        assert_eq!(h.span(), Some(Millis::from_millis(259_200_000)));
        // Which is three days, and therefore cannot support a claim about seven.
        assert_eq!(h.span(), Some(Millis::from_secs(3 * 24 * 3600)));
    }

    #[test]
    fn a_sign_run_counts_back_from_the_latest() {
        // Signs, oldest to newest: - - + + + . The run is the three positives.
        let h = hist(vec![
            entry(0, "-0.0001"),
            entry(1, "-0.0002"),
            entry(2, "0.0001"),
            entry(3, "0.0003"),
            entry(4, "0.0002"),
        ])
        .expect("ok");
        assert_eq!(h.current_sign_run(), 3);
    }

    #[test]
    fn a_zero_settlement_breaks_the_run() {
        // A period that paid nothing is not a period that paid in the direction
        // of the trade, so it truncates the run rather than being skipped over.
        let h = hist(vec![
            entry(0, "0.0001"),
            entry(1, "0.0001"),
            entry(2, "0"),
            entry(3, "0.0002"),
        ])
        .expect("ok");
        assert_eq!(h.current_sign_run(), 1);
        // And a history ending in zero has no run at all.
        let ends_zero = hist(vec![entry(0, "0.0001"), entry(1, "0")]).expect("ok");
        assert_eq!(ends_zero.current_sign_run(), 0);
    }

    #[test]
    fn a_wholly_consistent_history_runs_its_whole_length() {
        let entries: Vec<_> = (0..9).map(|p| entry(p, "0.0001")).collect();
        let h = hist(entries).expect("ok");
        assert_eq!(h.current_sign_run(), 9);
        assert_eq!(h.current_sign_run(), h.len());
        // Nine agreeing periods is a count, not a probability of a tenth. The
        // type offers no method that would round it into one.
    }

    #[test]
    fn signs_are_preserved_rather_than_normalised() {
        // A history of negative rates is a history of an inverted carry, and it
        // must not be indistinguishable from a positive one.
        let h = hist(vec![entry(0, "-0.0001"), entry(1, "-0.0002")]).expect("ok");
        assert!(h.latest().expect("present").rate.is_negative());
        assert_eq!(h.current_sign_run(), 2);
    }

    #[test]
    fn entries_come_back_oldest_first() {
        let h = hist(vec![entry(0, "0.0001"), entry(1, "0.0002")]).expect("ok");
        let e = h.entries();
        assert_eq!(e.len(), 2);
        assert!(e[0].settled_at < e[1].settled_at);
        assert_eq!(h.oldest(), Some(e[0]));
        assert_eq!(h.latest(), Some(e[1]));
    }
}
