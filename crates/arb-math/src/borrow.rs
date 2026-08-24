//! Borrow cost: annualising it, charging it over a horizon, and keeping it out
//! of the round trip.
//!
//! # The cost that was missing entirely
//!
//! Until this module `arb-math` priced fees and slippage and **no borrow at
//! all**. The reverse carry — short spot, long perp — is what a negative funding
//! rate calls for, and it requires borrowing the base asset in order to sell it.
//! Margin-borrow rates on the venues in scope annualise into the tens of percent
//! against a best-row net of 26.79%, so a row that omits borrow is not imprecise;
//! it can be wrong in sign. `OPEN.md` A1, `decisions/023` item 1.
//!
//! # The distinction this module exists to protect
//!
//! **A round trip is a one-off. A borrow is continuous.** They are both costs and
//! they do not compose the same way, and putting borrow into
//! [`RoundTrip::total_bps`](crate::RoundTrip::total_bps) would be wrong in a way
//! that looks right:
//!
//! - [`net_apr_over_horizon`](crate::funding::net_apr_over_horizon) *amortises*
//!   the round trip — dividing by the horizon, so a shorter hold makes the annual
//!   drag **larger**.
//! - A borrow accrues *per period* — so a shorter hold makes the total cost
//!   smaller while the annual rate stays put.
//!
//! Feed a borrow through the amortising path and it gets divided by the horizon
//! it should have been multiplied by. At an 8h hold that is a factor of roughly
//! 1095 in the flattering direction. So borrow enters as an annual rate via
//! [`net_apr_after_borrow`], never as basis points of round trip.
//!
//! # No `RoundMode` parameter anywhere here
//!
//! [`crate::funding::annualise`] takes one because a bare funding rate carries no
//! information about whether it is good or bad for us. A borrow has no such
//! ambiguity: it is money leaving, always, and
//! [`BorrowSnapshot::new`](arb_types::BorrowSnapshot) already refuses a negative
//! rate. Every rounding here is therefore [`RoundMode::COST`], chosen by the
//! module rather than by the caller, because a caller who could pass
//! `RoundMode::REVENUE` to a borrow calculation would eventually do it.

use crate::error::MathError;
use arb_num::{Money, Rate, RoundMode};
use arb_types::{BorrowInterval, BorrowSnapshot, Millis, SECONDS_PER_YEAR};

/// Milliseconds in one accrual period. Widened to `i64` before scaling so the
/// multiplication cannot wrap a `u32`.
#[inline]
const fn interval_millis(interval: BorrowInterval) -> i64 {
    interval.secs() as i64 * 1000
}

/// Scale a per-period borrow rate to a yearly rate, exactly.
///
/// `rate * SECONDS_PER_YEAR / interval_secs`, one rounding, upward. Simple
/// annualisation over a 365-day year, the convention `decisions/005` fixes for
/// funding, applied here for the same reason: two costs annualised by different
/// conventions cannot be subtracted from one another honestly.
///
/// A negative rate is refused rather than propagated. It is not a hypothetical
/// guarded for tidiness — this figure is **subtracted** downstream, so a negative
/// borrow rate is added to net APR, and the row it belongs to rises to the top of
/// a ranking on revenue no venue pays. The same hole `OPEN.md` F4 records in
/// `net_apr_over_horizon`.
///
/// # Hand vector
///
/// 0.01% per day = `0.0001 * 365` = `0.0365` = **3.65% APR**. The same 0.01% per
/// hour = `0.0001 * 8760` = `0.876` = **87.6% APR**. The second exceeds any
/// funding edge this project can find and the first does not, which is why the
/// interval is a parameter and not a default.
pub fn annualise_borrow(rate: Rate, interval: BorrowInterval) -> Result<Rate, MathError> {
    if rate.is_negative() {
        return Err(MathError::NegativeBorrowRate);
    }
    // Multiply first: exact, no rounding possible. Then one division, upward.
    let scaled = rate.checked_mul_int(SECONDS_PER_YEAR)?;
    let secs = i64::from(interval.secs());
    scaled
        .checked_div_int(secs, RoundMode::COST)
        .map_err(MathError::from)
}

/// The annualised cost of the borrow this snapshot describes.
///
/// Convenience over [`annualise_borrow`] for the common case where the invariant
/// already holds — the snapshot's constructor refused a negative rate, so the
/// error path here is unreachable in practice and is kept rather than
/// `unwrap`ped, per hard rule 4.
pub fn annual_borrow_cost(snap: &BorrowSnapshot) -> Result<Rate, MathError> {
    annualise_borrow(snap.rate, snap.interval)
}

/// The fraction of principal a borrow accrues over `horizon`.
///
/// `rate * horizon / interval`, in milliseconds on both sides so a horizon that
/// is not a whole number of seconds is not truncated. One rounding, upward.
///
/// Note the deliberate difference from
/// [`net_apr_over_horizon`](crate::funding::net_apr_over_horizon), which converts
/// its horizon to whole seconds and discards the remainder. There the discard is
/// immaterial — sub-second holds are rejected outright and the relative error on
/// any real horizon is negligible. Here it is avoided anyway because truncating a
/// horizon **understates** an accruing cost, and this module has no rounding that
/// runs in that direction.
///
/// # Hand vector
///
/// 0.01% per day held 3 days = `0.0001 * 3` = `0.0003`. Held 12 hours =
/// `0.0001 * 0.5` = `0.00005`.
pub fn borrow_rate_over(
    rate: Rate,
    interval: BorrowInterval,
    horizon: Millis,
) -> Result<Rate, MathError> {
    if rate.is_negative() {
        return Err(MathError::NegativeBorrowRate);
    }
    let ms = horizon.millis();
    if ms <= 0 {
        return Err(MathError::NonPositiveHorizon);
    }
    let scaled = rate.checked_mul_int(ms)?;
    scaled
        .checked_div_int(interval_millis(interval), RoundMode::COST)
        .map_err(MathError::from)
}

/// The money a borrow of `principal` costs over `horizon`.
///
/// Two roundings, both upward: once in [`borrow_rate_over`] and once converting
/// the rate to money. Both run against us, so the figure is at or above the true
/// cost and never below it — the same direction, and the same argument, as
/// `margin_required` (`OPEN.md` C3). The bound is roughly two units in the last
/// place at scale 1e12, which on a 1,000,000 USDT principal is a fifth of a cent.
///
/// A negative principal is refused: a borrow of negative size is a deposit, and
/// this function would price it as revenue.
///
/// # Hand vector
///
/// 100,000 USDT at 0.01% per day for 3 days = `100000 * 0.0003` = **30 USDT**.
pub fn borrow_cost(
    principal: Money,
    rate: Rate,
    interval: BorrowInterval,
    horizon: Millis,
) -> Result<Money, MathError> {
    if principal.is_negative() {
        return Err(MathError::NegativeNotional);
    }
    let over = borrow_rate_over(rate, interval, horizon)?;
    principal
        .mul_rate(over, RoundMode::COST)
        .map_err(MathError::from)
}

/// Subtract an annualised borrow cost from an annualised net return.
///
/// The one correct way borrow enters the headline number. `net_apr` is what
/// [`net_apr_over_horizon`](crate::funding::net_apr_over_horizon) produced —
/// funding receipt less the amortised round trip — and `annual_borrow` is what
/// [`annualise_borrow`] produced.
///
/// # Why this is a named function and not a `-`
///
/// Three reasons, and the third is the one that earns it:
///
/// 1. It refuses a negative borrow, so the fence is at the last point the value
///    is used and not only at the first point it was built.
/// 2. It states the unit agreement in one place: both arguments annualised, both
///    by the same 365-day convention.
/// 3. **A subtraction that is skipped leaves no trace.** A missing call to a
///    named function is visible in a diff and in a grep for `borrow`; a missing
///    `-` term in an expression is invisible. Given that the entire defect class
///    this round exists to close is *a cost term that was silently absent*, the
///    operation that applies it should be the kind of thing whose absence can be
///    searched for.
///
/// The result may well be negative, and that is not an error: a reverse carry
/// whose funding receipt does not cover the borrow **is** a losing trade, and
/// reporting it as such is the entire point.
pub fn net_apr_after_borrow(net_apr: Rate, annual_borrow: Rate) -> Result<Rate, MathError> {
    if annual_borrow.is_negative() {
        return Err(MathError::NegativeBorrowRate);
    }
    net_apr.checked_sub(annual_borrow).map_err(MathError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{
        Asset, DataQuality, InstrumentKey, LocalTs, MarketKind, Venue, VenueKey, WallTs,
    };

    fn rate(s: &str) -> Rate {
        Rate::parse(s).expect("test rate parses")
    }

    fn money(s: &str) -> Money {
        Money::parse(s).expect("test money parses")
    }

    fn days(n: i64) -> Millis {
        Millis::from_secs(n * 24 * 3600)
    }

    #[test]
    fn daily_annualisation_is_hand_computed() {
        // 0.0001 per day * 365 days = 0.0365 = 3.65% APR.
        assert_eq!(
            annualise_borrow(rate("0.0001"), BorrowInterval::DAILY).expect("ok"),
            rate("0.0365")
        );
    }

    #[test]
    fn hourly_annualisation_is_hand_computed() {
        // 0.0001 per hour * 8760 hours = 0.876 = 87.6% APR.
        assert_eq!(
            annualise_borrow(rate("0.0001"), BorrowInterval::HOURLY).expect("ok"),
            rate("0.876")
        );
    }

    #[test]
    fn the_interval_is_a_factor_of_twenty_four_on_the_same_number() {
        // Stated as a relation, not two literals, so the stake is on the record:
        // reading a daily rate as hourly overstates the cost 24x, and reading an
        // hourly rate as daily understates it 24x — the direction that
        // manufactures an opportunity.
        let d = annualise_borrow(rate("0.0001"), BorrowInterval::DAILY).expect("ok");
        let h = annualise_borrow(rate("0.0001"), BorrowInterval::HOURLY).expect("ok");
        assert_eq!(h, d.checked_mul_int(24).expect("ok"));
    }

    #[test]
    fn a_borrow_rate_that_would_pay_us_is_refused() {
        assert_eq!(
            annualise_borrow(rate("-0.0001"), BorrowInterval::DAILY),
            Err(MathError::NegativeBorrowRate)
        );
        assert_eq!(
            borrow_rate_over(rate("-0.0001"), BorrowInterval::DAILY, days(1)),
            Err(MathError::NegativeBorrowRate)
        );
        assert_eq!(
            net_apr_after_borrow(rate("0.2"), rate("-0.0365")),
            Err(MathError::NegativeBorrowRate)
        );
    }

    #[test]
    fn a_free_borrow_costs_nothing_without_erroring() {
        // Zero is a real cost, not an unknown one — see `BorrowSnapshot::new`.
        assert_eq!(
            annualise_borrow(Rate::ZERO, BorrowInterval::DAILY).expect("ok"),
            Rate::ZERO
        );
        assert_eq!(
            borrow_cost(money("100000"), Rate::ZERO, BorrowInterval::DAILY, days(3)).expect("ok"),
            Money::ZERO
        );
    }

    #[test]
    fn an_odd_interval_annualises_exactly_rather_than_by_a_rounded_period_count() {
        // 7h does not divide the year: 31_536_000 / 25_200 = 1251.428571…
        // Exact rational: 0.0001 * 31_536_000 / 25_200 = 0.125142857142…
        // Rounded up at scale 1e12 that is 0.125142857143.
        // The tempting alternative — 0.0001 * 1251 — gives 0.1251, discarding
        // 4.29 bps of annual cost, which is a third of the 12 bp edge.
        let seven_h = BorrowInterval::from_secs(7 * 3600).expect("ok");
        assert_eq!(seven_h.periods_per_year_exact(), None);
        let got = annualise_borrow(rate("0.0001"), seven_h).expect("ok");
        assert_eq!(got, rate("0.125142857143"));
        assert!(got > rate("0.1251"));
    }

    #[test]
    fn a_partial_period_accrues_a_partial_rate() {
        // 0.0001/day for 12h = half a period = 0.00005.
        let half_day = Millis::from_secs(12 * 3600);
        assert_eq!(
            borrow_rate_over(rate("0.0001"), BorrowInterval::DAILY, half_day).expect("ok"),
            rate("0.00005")
        );
        // And for 3 days = 0.0003.
        assert_eq!(
            borrow_rate_over(rate("0.0001"), BorrowInterval::DAILY, days(3)).expect("ok"),
            rate("0.0003")
        );
    }

    #[test]
    fn a_sub_second_horizon_is_not_truncated_away() {
        // 500ms of a daily borrow is a real, tiny cost. Converting the horizon to
        // whole seconds first would make it zero, which is the understating
        // direction this module refuses.
        let half_sec = Millis::from_millis(500);
        let got = borrow_rate_over(rate("0.0001"), BorrowInterval::DAILY, half_sec).expect("ok");
        assert!(got.is_positive(), "a real cost rounded to nothing");
        // 0.0001 * 500 / 86_400_000 = 5.787e-10, which at scale 1e12 is 579 ulp
        // after rounding up (5.787...e-10 -> 0.000000000579).
        assert_eq!(got, Rate::from_raw(579));
    }

    #[test]
    fn a_non_positive_horizon_is_refused() {
        assert_eq!(
            borrow_rate_over(rate("0.0001"), BorrowInterval::DAILY, Millis::ZERO),
            Err(MathError::NonPositiveHorizon)
        );
        assert_eq!(
            borrow_rate_over(
                rate("0.0001"),
                BorrowInterval::DAILY,
                Millis::from_millis(-1)
            ),
            Err(MathError::NonPositiveHorizon)
        );
    }

    #[test]
    fn borrow_cost_in_money_is_hand_computed() {
        // 100_000 USDT at 0.01%/day for 3 days = 100000 * 0.0003 = 30 USDT.
        assert_eq!(
            borrow_cost(money("100000"), rate("0.0001"), BorrowInterval::DAILY, days(3))
                .expect("ok"),
            money("30")
        );
    }

    #[test]
    fn borrow_cost_never_falls_below_the_true_cost() {
        // Both roundings run upward, so the reported cost is a bound in the safe
        // direction. Checked on an input whose exact answer has a remainder:
        // 1 ulp of money at 0.0001/day over 1ms is far below one ulp, and must
        // become one ulp rather than nothing.
        let tiny = borrow_cost(
            Money::EPSILON,
            rate("0.0001"),
            BorrowInterval::DAILY,
            Millis::from_millis(1),
        )
        .expect("ok");
        assert_eq!(tiny, Money::EPSILON);
    }

    #[test]
    fn a_negative_principal_is_refused_rather_than_priced_as_a_deposit() {
        let negative = Money::ZERO.checked_sub(money("1")).expect("ok");
        assert_eq!(
            borrow_cost(negative, rate("0.0001"), BorrowInterval::DAILY, days(1)),
            Err(MathError::NegativeNotional)
        );
    }

    #[test]
    fn borrow_can_turn_a_profitable_row_into_a_loss() {
        // This is the whole point of the module, as one number.
        // A reverse carry netting +26.79% APR — round one's best row — against a
        // USDT borrow at 0.01% per hour, which is 87.6% APR:
        //   0.2679 - 0.876 = -0.6081, i.e. -60.81%.
        // The sign inverts. `OPEN.md` A1 said this was possible; here it is.
        let net = rate("0.2679");
        let borrow = annualise_borrow(rate("0.0001"), BorrowInterval::HOURLY).expect("ok");
        let after = net_apr_after_borrow(net, borrow).expect("ok");
        assert_eq!(after, rate("-0.6081"));
        assert!(net.is_positive() && after.is_negative());
    }

    #[test]
    fn a_daily_borrow_of_the_same_number_leaves_the_row_profitable() {
        // The counterpart, so the test above is not read as "borrow always kills
        // the trade": 0.2679 - 0.0365 = 0.2314, i.e. +23.14%. Same rate figure,
        // different interval, opposite conclusion — which is exactly why
        // `BorrowInterval` travels with the rate instead of being assumed.
        let net = rate("0.2679");
        let borrow = annualise_borrow(rate("0.0001"), BorrowInterval::DAILY).expect("ok");
        assert_eq!(net_apr_after_borrow(net, borrow).expect("ok"), rate("0.2314"));
    }

    #[test]
    fn borrow_is_not_amortised_the_way_a_round_trip_is() {
        // The module's central distinction, pinned. A shorter hold makes a
        // one-off round trip a *larger* annual drag and leaves an accruing
        // borrow's annual rate untouched. Feeding borrow through the amortising
        // path at an 8h hold would divide by 1095 what should not be divided at
        // all — flattering by three orders of magnitude.
        let eight_h = Millis::from_secs(8 * 3600);
        let year = Millis::from_secs(SECONDS_PER_YEAR);

        // Annual borrow rate does not depend on the horizon.
        let b = annualise_borrow(rate("0.0001"), BorrowInterval::DAILY).expect("ok");
        assert_eq!(b, rate("0.0365"));

        // Round-trip drag does, and steeply: 22 bps over a year is 22 bps;
        // over one 8h settlement it is 22 * 1095 = 24_090 bps = 240.9%.
        let cost = arb_num::Bps::parse_bps("22").expect("ok");
        let over_year =
            crate::funding::net_apr_over_horizon(Rate::ZERO, cost, year).expect("ok");
        let over_8h = crate::funding::net_apr_over_horizon(Rate::ZERO, cost, eight_h).expect("ok");
        assert_eq!(over_year, rate("-0.0022"));
        assert_eq!(over_8h, rate("-2.409"));
        assert!(over_8h < over_year);
    }

    #[test]
    fn a_snapshots_invariant_carries_into_the_annualisation() {
        // `annual_borrow_cost` cannot fail on a constructed snapshot, because the
        // constructor already refused the only rate that would make it fail. The
        // `Result` is kept anyway rather than unwrapped — rule 4 — and this test
        // is what makes the unreachability a checked claim instead of a comment.
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::Spot, "USDT").expect("ok");
        let snap = BorrowSnapshot::new(
            VenueKey::new(Venue::Binance, key),
            Asset::new("BTC").expect("ok"),
            rate("0.0001"),
            BorrowInterval::DAILY,
            Some(WallTs::from_millis(1_699_999_000_000)),
            LocalTs::from_millis(1_699_999_000_040),
            DataQuality::SYNTHESIZED,
        )
        .expect("ok");
        assert_eq!(annual_borrow_cost(&snap).expect("ok"), rate("0.0365"));
    }
}
