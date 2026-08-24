//! Borrow cost: the accrual period, the snapshot, and why a negative rate is
//! refused.
//!
//! # Why this type exists at all
//!
//! `arb-math` models exchange fees and slippage. Until this module, it modelled
//! **no borrow cost whatsoever** — and the classic reverse carry (short spot,
//! long perp), which is exactly what a negative funding rate calls for, requires
//! borrowing the base asset to sell it. Margin-borrow rates on the venues in
//! scope are commonly quoted at daily rates that annualise into the tens of
//! percent, against a best-row net of 26.79% and a claimed edge of 12 basis
//! points. A row that omits borrow is not imprecise; it is capable of being wrong
//! in sign. See `OPEN.md` A1 and `decisions/023` item 1.
//!
//! # The two hazards this module makes unrepresentable
//!
//! **1. The accrual period.** Venues do not agree on it: some quote margin
//! interest per hour, some per day. A daily rate read as hourly overstates the
//! cost by 24x; hourly read as daily understates it by the same factor, and the
//! understating direction is the one that manufactures an opportunity. So the
//! period is a validated newtype ([`BorrowInterval`]) travelling with the rate,
//! never an assumption at the call site — the same treatment
//! [`crate::FundingInterval`] gets, for the same reason.
//!
//! **2. The sign.** A borrow rate is a cost, and a cost is non-negative. A
//! venue does not pay you to borrow from it. [`BorrowSnapshot::new`] refuses a
//! negative rate rather than propagating it, because a negative cost *adds* to
//! net APR: the row would rank first, on revenue that does not exist. This is
//! the same fence `arb-math` puts on round-trip cost (`OPEN.md` F4).

use crate::instrument::VenueKey;
use crate::quality::DataQuality;
use crate::strings::Asset;
use crate::time::{LocalTs, Millis, WallTs};
use crate::funding::SECONDS_PER_YEAR;
use crate::TypeError;
use arb_num::Rate;
use core::fmt;

/// The period a borrow rate is quoted **per**, in seconds.
///
/// A validated newtype for the same two reasons as [`crate::FundingInterval`]:
/// zero divides by zero in every annualisation, and the field is the one a
/// hand-written adapter most easily gets wrong. The two real conventions among
/// the venues in scope are hourly and daily, and they differ by 24x.
///
/// Deliberately **not** the same type as [`crate::FundingInterval`]. They measure
/// different things — one is a perp's settlement cadence, the other a loan's
/// accrual period — and sharing the type would make `BorrowInterval::EIGHT_HOURS`
/// spellable, which is not a borrow convention any venue in scope uses. The
/// duplicated thirty lines buy a wrong value that cannot be typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BorrowInterval(u32);

impl BorrowInterval {
    /// Per hour. Binance-style margin interest.
    pub const HOURLY: Self = Self(3600);
    /// Per day, 24 hours exactly.
    pub const DAILY: Self = Self(24 * 3600);

    /// Construct from a second count, rejecting zero.
    pub const fn from_secs(secs: u32) -> Result<Self, TypeError> {
        if secs == 0 {
            return Err(TypeError::ZeroBorrowInterval);
        }
        Ok(Self(secs))
    }

    /// The period in seconds.
    #[inline]
    pub const fn secs(self) -> u32 {
        self.0
    }

    /// The period as a duration.
    #[inline]
    pub const fn as_millis(self) -> Millis {
        Millis::from_secs(self.0 as i64)
    }

    /// Number of accrual periods per year, when it divides exactly.
    ///
    /// `None` for periods that do not divide the year evenly, so callers
    /// annualise by exact rational arithmetic (`rate * SECONDS_PER_YEAR /
    /// interval`) rather than rounding a period count — the convention
    /// `decisions/005` fixes. Hourly gives 8760, daily gives 365.
    pub const fn periods_per_year_exact(self) -> Option<i64> {
        let secs = self.0 as i64;
        if SECONDS_PER_YEAR % secs == 0 {
            Some(SECONDS_PER_YEAR / secs)
        } else {
            None
        }
    }
}

impl fmt::Display for BorrowInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0;
        if s % (24 * 3600) == 0 {
            write!(f, "{}d", s / (24 * 3600))
        } else if s % 3600 == 0 {
            write!(f, "{}h", s / 3600)
        } else if s % 60 == 0 {
            write!(f, "{}m", s / 60)
        } else {
            write!(f, "{s}s")
        }
    }
}

/// One observation of what it costs to borrow one asset at one venue.
///
/// # What `rate` means
///
/// The interest accrued over **one [`BorrowInterval`]**, as a fraction of the
/// borrowed amount. It is a **cost**, always non-negative, and the constructor
/// refuses anything else. Annualising it is `arb-math`'s job, not this type's.
///
/// # What this type deliberately does not model
///
/// **Availability.** Knowing the rate does not mean the venue has inventory to
/// lend, and a borrow that cannot be opened is not an expensive opportunity but
/// an absent one. Nothing here expresses that, so a priced borrow is not a
/// tradeable borrow. Recorded in `OPEN.md` rather than half-modelled.
///
/// **Rate variability.** Margin rates move, and on some venues they move with
/// utilisation, so holding a position for a week at today's quoted rate is an
/// assumption of the same kind as assuming a funding rate persists — see
/// [`crate::RateHistory`], which exists because that assumption needed to stop
/// being invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorrowSnapshot {
    /// Which instrument's venue this borrow was quoted at. The instrument key
    /// travels along so a rate cannot be silently attached to the wrong venue —
    /// borrow rates differ by venue for the same asset.
    pub vkey: VenueKey,
    /// The asset being borrowed. For a short spot leg this is the **base**
    /// asset: shorting BTC/USDT means borrowing BTC in order to sell it. Carried
    /// explicitly because margin accounts can borrow either side, and a quote
    /// borrow priced as a base borrow is wrong by the price.
    pub asset: Asset,
    /// Interest accrued over one `interval`, as a fraction of principal.
    /// Non-negative by construction.
    pub rate: Rate,
    /// The period `rate` is quoted per.
    pub interval: BorrowInterval,
    /// The venue's own timestamp on this payload, where it supplied one.
    pub exchange_ts: Option<WallTs>,
    /// When we observed it, on our clock.
    pub observed_at: LocalTs,
    /// Provenance and warnings.
    pub quality: DataQuality,
}

impl BorrowSnapshot {
    /// Build a snapshot, refusing a negative rate.
    ///
    /// The refusal is the point. A negative borrow rate would enter a net-APR
    /// sum as revenue, so the row it belongs to would rank *above* honest rows on
    /// income that no venue pays. Ranking selects for favourable error, which is
    /// why a fence in the safe direction is not optional here.
    ///
    /// Zero **is** accepted: a promotional or fee-waived borrow is a real thing,
    /// and refusing it would force callers to represent "free" as `None`, which
    /// then reads as "unknown" and raises `BORROW_COST_UNKNOWN` on a cost we
    /// actually know.
    pub fn new(
        vkey: VenueKey,
        asset: Asset,
        rate: Rate,
        interval: BorrowInterval,
        exchange_ts: Option<WallTs>,
        observed_at: LocalTs,
        quality: DataQuality,
    ) -> Result<Self, TypeError> {
        if rate.is_negative() {
            return Err(TypeError::NegativeBorrowRate);
        }
        Ok(Self {
            vkey,
            asset,
            rate,
            interval,
            exchange_ts,
            observed_at,
            quality,
        })
    }

    /// True when borrowing this asset costs nothing at this venue.
    ///
    /// Distinct from "unknown": see [`BorrowSnapshot::new`].
    #[inline]
    pub const fn is_free(&self) -> bool {
        self.rate.is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{InstrumentKey, MarketKind};
    use crate::venue::Venue;

    fn asset(s: &str) -> Asset {
        // Literal known valid; `unwrap_or` keeps the helper panic-free per rule 4.
        Asset::new(s).unwrap_or_default()
    }

    fn vkey() -> VenueKey {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::Spot, "USDT").unwrap_or(
            InstrumentKey::new(
                Asset::default(),
                Asset::default(),
                MarketKind::Spot,
                Asset::default(),
            ),
        );
        VenueKey::new(Venue::Binance, key)
    }

    fn snap(rate: &str, interval: BorrowInterval) -> Result<BorrowSnapshot, TypeError> {
        BorrowSnapshot::new(
            vkey(),
            asset("BTC"),
            Rate::parse(rate).unwrap_or(Rate::ZERO),
            interval,
            Some(WallTs::from_millis(1_699_999_000_000)),
            LocalTs::from_millis(1_699_999_000_040),
            DataQuality::SYNTHESIZED,
        )
    }

    #[test]
    fn a_negative_borrow_rate_is_refused() {
        // The whole reason the constructor is fallible. A -0.01%/day "cost" would
        // add 3.65% of annual revenue to every row that borrows, and those rows
        // would then outrank rows that pay honestly.
        assert_eq!(
            snap("-0.0001", BorrowInterval::DAILY),
            Err(TypeError::NegativeBorrowRate)
        );
    }

    #[test]
    fn a_zero_borrow_rate_is_accepted_because_free_is_not_unknown() {
        let s = snap("0", BorrowInterval::DAILY).expect("zero is a legitimate cost");
        assert!(s.is_free());
    }

    #[test]
    fn a_positive_borrow_rate_survives_with_its_magnitude() {
        let s = snap("0.0002", BorrowInterval::HOURLY).expect("ok");
        assert_eq!(s.rate, Rate::parse("0.0002").expect("ok"));
        assert!(!s.is_free());
        assert_eq!(s.interval, BorrowInterval::HOURLY);
    }

    #[test]
    fn the_borrowed_asset_is_carried_not_inferred() {
        // Shorting BTC/USDT borrows BTC. A quote-side borrow at the same venue is
        // a different rate, and nothing in the key says which one this is.
        let s = snap("0.0001", BorrowInterval::DAILY).expect("ok");
        assert_eq!(s.asset, asset("BTC"));
    }

    #[test]
    fn interval_rejects_zero() {
        assert_eq!(
            BorrowInterval::from_secs(0),
            Err(TypeError::ZeroBorrowInterval)
        );
        assert_eq!(
            BorrowInterval::from_secs(3600).expect("ok"),
            BorrowInterval::HOURLY
        );
        assert_eq!(
            BorrowInterval::from_secs(86_400).expect("ok"),
            BorrowInterval::DAILY
        );
    }

    #[test]
    fn periods_per_year_matches_hand_arithmetic() {
        // 365 days, 365*24 = 8760 hours. Both divide the year exactly.
        assert_eq!(BorrowInterval::DAILY.periods_per_year_exact(), Some(365));
        assert_eq!(BorrowInterval::HOURLY.periods_per_year_exact(), Some(8760));
    }

    #[test]
    fn an_odd_period_refuses_to_pretend_it_divides() {
        // 7 hours: 8760/7 is not an integer. Some(1251) would discard 3 hours of
        // accrual per year and look right.
        let seven_h = BorrowInterval::from_secs(7 * 3600).expect("ok");
        assert_eq!(seven_h.periods_per_year_exact(), None);
    }

    #[test]
    fn the_hourly_daily_confusion_is_a_factor_of_twenty_four() {
        // Stated numerically so the stakes are on the record rather than in prose.
        // The same 0.01% quoted per hour and per day differ by 24x in annual cost:
        // hourly  0.0001 * 8760 = 0.876  -> 87.6% APR
        // daily   0.0001 *  365 = 0.0365 ->  3.65% APR
        // The first exceeds any funding edge we can find; the second does not.
        let h = BorrowInterval::HOURLY.periods_per_year_exact().expect("divides");
        let d = BorrowInterval::DAILY.periods_per_year_exact().expect("divides");
        assert_eq!(h, 24 * d);
    }

    #[test]
    fn interval_display_is_compact() {
        assert_eq!(BorrowInterval::HOURLY.to_string(), "1h");
        assert_eq!(BorrowInterval::DAILY.to_string(), "1d");
        assert_eq!(
            BorrowInterval::from_secs(1800).expect("ok").to_string(),
            "30m"
        );
        assert_eq!(BorrowInterval::from_secs(90).expect("ok").to_string(), "90s");
        assert_eq!(
            BorrowInterval::from_secs(48 * 3600).expect("ok").to_string(),
            "2d"
        );
    }

    #[test]
    fn a_borrow_snapshot_is_not_a_funding_snapshot_in_disguise() {
        // FundingInterval::EIGHT_HOURS is not spellable as a BorrowInterval
        // constant, which is the point of the separate type. 8h is expressible
        // via from_secs — a venue could quote it — but it cannot arrive by
        // reaching for the funding constant out of habit.
        let eight_h = BorrowInterval::from_secs(8 * 3600).expect("ok");
        assert_eq!(eight_h.secs(), crate::FundingInterval::EIGHT_HOURS.secs());
        assert_eq!(eight_h.periods_per_year_exact(), Some(1095));
    }
}
