//! Funding: the snapshot type, the interval, and the sign law.
//!
//! # The sign law
//!
//! **A positive funding rate means longs pay shorts.** This is the convention
//! every venue in scope publishes, and it is stated once here so that no other
//! crate re-derives it. Getting it backwards inverts the sign of the headline
//! APR on every row of the table, which is not a rounding error — it is a buy
//! signal on a losing trade.
//!
//! The consequence for the classic cash-and-carry: holding spot long and perp
//! short *receives* funding when the rate is positive. Use
//! [`PositionSide::funding_receipt_sign`] rather than writing the minus sign by
//! hand at the call site.

use crate::instrument::VenueKey;
use crate::quality::DataQuality;
use crate::time::{LocalTs, Millis, WallTs};
use crate::TypeError;
use arb_num::{Price, Rate};
use core::fmt;

/// Seconds in the year used for every annualisation in this workspace.
///
/// 365 days exactly. Not 365.25: the alternative changes a headline APR by 7
/// basis points per 100% of APR, which is a real difference at our edge size, so
/// the convention is fixed here and documented in `decisions/004` rather than
/// chosen independently in five places.
pub const SECONDS_PER_YEAR: i64 = 365 * 24 * 60 * 60;

/// Which side of a perpetual position is held.
///
/// Distinct from [`crate::Side`], which is the side of an *order*. Buying to
/// close a short is a `Buy` order that reduces a `Short` position; conflating
/// the two flips funding signs during unwinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PositionSide {
    /// Long the perpetual. Pays funding when the rate is positive.
    Long,
    /// Short the perpetual. Receives funding when the rate is positive.
    Short,
}

impl PositionSide {
    /// The multiplier that turns a funding rate into a signed cash flow for the
    /// holder of this side.
    ///
    /// `Long` yields `-1`: a positive rate is money out. `Short` yields `+1`.
    /// Multiply this by rate and by notional to get the settlement amount, with
    /// the sign already correct.
    ///
    /// This is the single authority for the funding sign in the workspace. If a
    /// crate contains a hand-written `-rate` next to the word funding, that
    /// crate is wrong.
    #[inline]
    pub const fn funding_receipt_sign(self) -> i64 {
        match self {
            PositionSide::Long => -1,
            PositionSide::Short => 1,
        }
    }

    /// The other side.
    #[inline]
    pub const fn opposite(self) -> Self {
        match self {
            PositionSide::Long => PositionSide::Short,
            PositionSide::Short => PositionSide::Long,
        }
    }

    /// Short tag for table output.
    pub const fn tag(self) -> &'static str {
        match self {
            PositionSide::Long => "long",
            PositionSide::Short => "short",
        }
    }
}

impl fmt::Display for PositionSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// How often funding settles, in seconds.
///
/// A validated newtype because zero divides by zero in every annualisation, and
/// because the interval is the most commonly wrong field in a hand-written
/// adapter: assuming 8h for a venue that settles every 4h halves the true APR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FundingInterval(u32);

impl FundingInterval {
    /// Eight hours, the most common interval.
    pub const EIGHT_HOURS: Self = Self(8 * 3600);
    /// Four hours.
    pub const FOUR_HOURS: Self = Self(4 * 3600);
    /// Two hours.
    pub const TWO_HOURS: Self = Self(2 * 3600);
    /// One hour.
    pub const ONE_HOUR: Self = Self(3600);

    /// Construct from a second count, rejecting zero.
    pub const fn from_secs(secs: u32) -> Result<Self, TypeError> {
        if secs == 0 {
            return Err(TypeError::ZeroFundingInterval);
        }
        Ok(Self(secs))
    }

    /// The interval in seconds.
    #[inline]
    pub const fn secs(self) -> u32 {
        self.0
    }

    /// The interval as a duration.
    #[inline]
    pub const fn as_millis(self) -> Millis {
        Millis::from_secs(self.0 as i64)
    }

    /// Number of settlements per year, when it divides exactly.
    ///
    /// Returns `None` for intervals that do not divide the year evenly, so that
    /// callers annualise by exact rational arithmetic
    /// (`rate * SECONDS_PER_YEAR / interval`) instead of rounding a period
    /// count. 8h gives 1095, 4h gives 2190, 1h gives 8760.
    pub const fn periods_per_year_exact(self) -> Option<i64> {
        let secs = self.0 as i64;
        if SECONDS_PER_YEAR % secs == 0 {
            Some(SECONDS_PER_YEAR / secs)
        } else {
            None
        }
    }
}

impl fmt::Display for FundingInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0;
        if s % 3600 == 0 {
            write!(f, "{}h", s / 3600)
        } else if s % 60 == 0 {
            write!(f, "{}m", s / 60)
        } else {
            write!(f, "{s}s")
        }
    }
}

/// One observation of an instrument's funding state.
///
/// # What `rate` means
///
/// `rate` is **the rate that will be applied at `next_funding_at`** — the
/// current or predicted rate, not the last settled one. When an adapter can only
/// obtain the historical rate it must set
/// [`crate::QualityFlags::FUNDING_RATE_HISTORICAL`], because the predicted and
/// last-settled rates routinely differ in magnitude and sometimes in sign.
/// Ranking on a settled rate while claiming to forecast the next one is a
/// silent, plausible-looking error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FundingSnapshot {
    /// Which instrument at which venue.
    pub vkey: VenueKey,
    /// Rate applied at the next settlement. See the type docs.
    pub rate: Rate,
    /// How often this instrument settles funding.
    pub interval: FundingInterval,
    /// When the next settlement occurs, on the venue's clock. `None` when the
    /// venue did not supply it; pair with
    /// [`crate::QualityFlags::PRICE_UNAVAILABLE`]-style flagging by the adapter.
    pub next_funding_at: Option<WallTs>,
    /// Mark price, which funding is charged against.
    pub mark: Option<Price>,
    /// Index price, the spot reference used to compute basis.
    pub index: Option<Price>,
    /// The venue's own timestamp on this payload.
    pub exchange_ts: WallTs,
    /// When we observed it, on our clock.
    pub observed_at: LocalTs,
    /// Provenance and warnings.
    pub quality: DataQuality,
}

impl FundingSnapshot {
    /// Time from `now` until the next settlement, on the venue's clock.
    ///
    /// `None` when the venue did not report a settlement time. A negative result
    /// means the settlement time has passed without the snapshot being refreshed,
    /// which is a staleness signal rather than a negative wait.
    pub fn time_to_settlement(&self, now: WallTs) -> Option<Millis> {
        self.next_funding_at.map(|next| next.since(now))
    }

    /// Signed funding cash flow *rate* for a holder of `side`.
    ///
    /// Applies the sign law once, here. Multiply the result by notional to get an
    /// amount. Returns an error only on the arithmetic edge where the rate is
    /// [`Rate::MIN`] and cannot be negated.
    pub fn receipt_rate(&self, side: PositionSide) -> Result<Rate, arb_num::NumError> {
        self.rate.checked_mul_int(side.funding_receipt_sign())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{InstrumentKey, MarketKind};
    use crate::quality::DataQuality;
    use crate::venue::Venue;

    fn vkey() -> VenueKey {
        // Literals known valid; `unwrap_or` keeps the helper panic-free per rule 4.
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT")
            .unwrap_or(InstrumentKey::new(
                crate::Asset::default(),
                crate::Asset::default(),
                MarketKind::LinearPerp,
                crate::Asset::default(),
            ));
        VenueKey::new(Venue::Binance, key)
    }

    fn snap(rate: &str) -> FundingSnapshot {
        FundingSnapshot {
            vkey: vkey(),
            rate: Rate::parse(rate).unwrap_or(Rate::ZERO),
            interval: FundingInterval::EIGHT_HOURS,
            next_funding_at: Some(WallTs::from_millis(1_700_000_000_000)),
            mark: None,
            index: None,
            exchange_ts: WallTs::from_millis(1_699_999_000_000),
            observed_at: LocalTs::from_millis(1_699_999_000_040),
            quality: DataQuality::SYNTHESIZED,
        }
    }

    #[test]
    fn positive_rate_means_longs_pay() {
        // The sign law, asserted rather than assumed.
        assert_eq!(PositionSide::Long.funding_receipt_sign(), -1);
        assert_eq!(PositionSide::Short.funding_receipt_sign(), 1);

        let s = snap("0.0001"); // +1 bp
        // Long pays: receipt is negative.
        assert_eq!(
            s.receipt_rate(PositionSide::Long).expect("ok"),
            Rate::parse("-0.0001").expect("ok")
        );
        // Short receives: receipt is positive.
        assert_eq!(
            s.receipt_rate(PositionSide::Short).expect("ok"),
            Rate::parse("0.0001").expect("ok")
        );
    }

    #[test]
    fn negative_rate_flips_who_pays() {
        let s = snap("-0.00037"); // shorts pay longs
        assert_eq!(
            s.receipt_rate(PositionSide::Long).expect("ok"),
            Rate::parse("0.00037").expect("ok")
        );
        assert_eq!(
            s.receipt_rate(PositionSide::Short).expect("ok"),
            Rate::parse("-0.00037").expect("ok")
        );
    }

    #[test]
    fn cash_and_carry_receives_on_positive_funding() {
        // Long spot + short perp. The perp leg is what earns funding.
        let s = snap("0.0001");
        let carry = s.receipt_rate(PositionSide::Short).expect("ok");
        assert!(
            carry.is_positive(),
            "long-spot/short-perp must earn when funding is positive"
        );
    }

    #[test]
    fn zero_rate_pays_nobody() {
        let s = snap("0");
        assert_eq!(s.receipt_rate(PositionSide::Long).expect("ok"), Rate::ZERO);
        assert_eq!(s.receipt_rate(PositionSide::Short).expect("ok"), Rate::ZERO);
    }

    #[test]
    fn sides_are_involutive() {
        assert_eq!(PositionSide::Long.opposite(), PositionSide::Short);
        assert_eq!(PositionSide::Long.opposite().opposite(), PositionSide::Long);
    }

    #[test]
    fn interval_rejects_zero() {
        assert_eq!(
            FundingInterval::from_secs(0),
            Err(TypeError::ZeroFundingInterval)
        );
        assert_eq!(
            FundingInterval::from_secs(28_800).expect("ok"),
            FundingInterval::EIGHT_HOURS
        );
    }

    #[test]
    fn periods_per_year_matches_hand_arithmetic() {
        // 365*24 = 8760 hours per year.
        assert_eq!(FundingInterval::EIGHT_HOURS.periods_per_year_exact(), Some(1095));
        assert_eq!(FundingInterval::FOUR_HOURS.periods_per_year_exact(), Some(2190));
        assert_eq!(FundingInterval::TWO_HOURS.periods_per_year_exact(), Some(4380));
        assert_eq!(FundingInterval::ONE_HOUR.periods_per_year_exact(), Some(8760));
        assert_eq!(SECONDS_PER_YEAR, 31_536_000);
    }

    #[test]
    fn odd_intervals_refuse_to_pretend_they_divide() {
        // 7 hours does not divide the year. Returning Some(1251) would silently
        // discard 3 hours per year; None forces exact rational annualisation.
        let seven_h = FundingInterval::from_secs(7 * 3600).expect("ok");
        assert_eq!(seven_h.periods_per_year_exact(), None);
    }

    #[test]
    fn interval_display_is_compact() {
        assert_eq!(FundingInterval::EIGHT_HOURS.to_string(), "8h");
        assert_eq!(FundingInterval::ONE_HOUR.to_string(), "1h");
        assert_eq!(FundingInterval::from_secs(1800).expect("ok").to_string(), "30m");
        assert_eq!(FundingInterval::from_secs(90).expect("ok").to_string(), "90s");
    }

    #[test]
    fn time_to_settlement_is_signed() {
        let s = snap("0.0001");
        let next = s.next_funding_at.expect("set in fixture");
        let before = next.add_millis(Millis::from_millis(-5000));
        assert_eq!(
            s.time_to_settlement(before),
            Some(Millis::from_millis(5000))
        );
        // Past the boundary: negative, not clamped. A stale snapshot must look
        // stale rather than look like an imminent settlement.
        let after = next.add_millis(Millis::from_millis(5000));
        assert_eq!(s.time_to_settlement(after), Some(Millis::from_millis(-5000)));
    }

    #[test]
    fn missing_settlement_time_is_none_not_zero() {
        let mut s = snap("0.0001");
        s.next_funding_at = None;
        assert_eq!(s.time_to_settlement(WallTs::EPOCH), None);
    }
}
