//! Basis: the gap between a perpetual's mark and its spot index.
//!
//! # The lie this module refuses to tell
//!
//! Basis on a *dated* future has a natural annualisation, because the contract
//! expires and the two prices must meet on a known date. Divide the basis by the
//! time to expiry and you get a real yield.
//!
//! **A perpetual has no expiry.** There is no date on which mark must equal
//! index, and nothing forces convergence at all — a perp can hold a 50 bp
//! premium for weeks, and the mechanism that pushes it back is funding, which
//! [`crate::funding`] already prices. Annualising a perp basis therefore
//! double-counts funding and invents a convergence date that does not exist.
//! It is the single most flattering number available to us: a 20 bp basis
//! "annualised over 8 hours" reads as 219% APR, and it is not a return at all.
//!
//! So this module has no `basis_apr`, and it will not grow one. What it has:
//!
//! - [`basis_ratio`] and [`basis_bps`] — the basis as it is, a one-off gap.
//! - [`convergence_capture`] — the cash actually captured if the gap moves from
//!   one level to another, in money, over no particular time.
//!
//! # Where basis does earn money
//!
//! Two places, both one-off rather than annual. First, entering a carry at a
//! wide basis and exiting at a narrow one captures the difference — that is
//! [`convergence_capture`]. Second, basis is the *execution* difference between
//! the two legs at the moment you open: pay 20 bp more for the perp than the
//! spot and that 20 bp is a cost of entry, not a yield. Both are amounts. The
//! only per-year number in a perp carry comes from funding.
//!
//! # Rounding
//!
//! The basis is what we hope to capture, whichever way it runs, so every figure
//! here rounds its **magnitude toward zero**. A positive basis is reported
//! smaller and a negative basis is reported nearer zero; both shrink the claimed
//! opportunity. Because the division is taken on `mark - index` rather than on
//! `mark / index`, [`RoundMode::REVENUE`] achieves that in one step for both
//! signs — truncation is always toward zero.

use crate::error::MathError;
use arb_num::{Bps, Money, Price, Rate, RoundMode};

/// The relative basis, `(mark - index) / index`.
///
/// Positive means the perpetual trades at a premium to spot, which is the
/// condition under which funding is usually positive and a short-perp carry
/// collects. Negative means a discount.
///
/// Computed as `(mark - index) / index` rather than `mark / index - 1`: the
/// subtraction is exact, so the whole expression carries exactly one rounding
/// instead of one rounding followed by a subtraction that cannot undo it. At a
/// BTC price near 68,000 the difference between the two forms reaches the twelfth
/// decimal place, which is one ulp — small, but this is the crate where we do not
/// spend ulps for nothing.
///
/// `mode` is explicit because whether a basis helps or hurts depends on which
/// leg the caller is long. Prefer [`basis_bps`], which picks the direction that
/// shrinks the opportunity.
pub fn basis_ratio(mark: Price, index: Price, mode: RoundMode) -> Result<Rate, MathError> {
    if !index.is_positive() {
        // A zero or negative index is not a thin market, it is a broken feed.
        return Err(MathError::NonPositivePrice);
    }
    let diff = mark.checked_sub(index)?;
    diff.div_price(index, mode).map_err(MathError::from)
}

/// The relative basis in basis points, rounded toward zero.
///
/// Truncation shrinks the magnitude whichever way the basis runs, so this figure
/// is a lower bound on the gap in both directions. See the module docs.
pub fn basis_bps(mark: Price, index: Price) -> Result<Bps, MathError> {
    basis_ratio(mark, index, RoundMode::REVENUE)?
        .to_bps()
        .map_err(MathError::from)
}

/// Cash captured when the basis moves from `entry` to `exit`, on `notional`.
///
/// Signed for the position that is **short the expensive leg and long the cheap
/// one** — the classic cash-and-carry, short perp against long spot. Such a
/// position profits when the basis narrows, so the capture is
/// `(entry - exit) * notional`.
///
/// The result is **money, over no particular period**. It is deliberately not a
/// rate and deliberately not annualised; see the module docs for why turning it
/// into an APR would be an invention.
///
/// A negative result is a real and common outcome: basis widens against carry
/// positions all the time, and the loss is only recovered by holding to a
/// narrower level, which nothing guarantees. Returning it signed rather than
/// clamping at zero is the point.
///
/// Rounds against us: a positive capture is truncated, a negative one is grown
/// in magnitude.
pub fn convergence_capture(
    entry: Rate,
    exit: Rate,
    notional: Money,
) -> Result<Money, MathError> {
    if notional.is_negative() {
        return Err(MathError::NegativeNotional);
    }
    let moved = entry.checked_sub(exit)?;
    let mode = if moved.is_negative() {
        // A loss. Grow its magnitude.
        RoundMode::COST
    } else {
        // A gain. Shrink it.
        RoundMode::REVENUE
    };
    notional.mul_rate(moved, mode).map_err(MathError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_num::NumError;

    fn price(s: &str) -> Price {
        Price::parse(s).expect("test price parses")
    }

    fn rate(s: &str) -> Rate {
        Rate::parse(s).expect("test rate parses")
    }

    fn money(s: &str) -> Money {
        Money::parse(s).expect("test money parses")
    }

    fn bps(s: &str) -> Bps {
        Bps::parse_bps(s).expect("test bps parses")
    }

    #[test]
    fn a_premium_is_positive_and_exact() {
        // mark 100.05, index 100.00. diff = 0.05, 0.05 / 100 = 0.0005 = 5 bps.
        assert_eq!(
            basis_ratio(price("100.05"), price("100"), RoundMode::REVENUE).expect("ok"),
            rate("0.0005")
        );
        assert_eq!(basis_bps(price("100.05"), price("100")).expect("ok"), bps("5"));
    }

    #[test]
    fn a_discount_is_negative_and_exact() {
        // mark 99.5, index 100. diff = -0.5, -0.5 / 100 = -0.005 = -50 bps.
        assert_eq!(
            basis_ratio(price("99.5"), price("100"), RoundMode::REVENUE).expect("ok"),
            rate("-0.005")
        );
        assert_eq!(
            basis_bps(price("99.5"), price("100")).expect("ok"),
            bps("50").checked_neg().expect("ok")
        );
    }

    #[test]
    fn equal_prices_give_a_flat_basis() {
        assert_eq!(
            basis_bps(price("68123.4"), price("68123.4")).expect("ok"),
            Bps::ZERO
        );
    }

    #[test]
    fn a_realistic_btc_premium_matches_hand_arithmetic() {
        // mark 30001, index 30000. diff = 1, 1 / 30000 = 0.0000333333...
        // Raw: 1e12 * 1e12 / 3.0e16 = 1e24 / 3e16 = 33_333_333.333...
        // Truncated -> 33_333_333 raw = 0.000033333333 = 0.33333333 bps.
        let got = basis_ratio(price("30001"), price("30000"), RoundMode::REVENUE).expect("ok");
        assert_eq!(got, Rate::from_raw(33_333_333));
        // Bps and Rate share their raw integer by construction, so this is a
        // reinterpretation and not a second rounding.
        assert_eq!(
            basis_bps(price("30001"), price("30000")).expect("ok"),
            Bps::from_raw(33_333_333)
        );
    }

    #[test]
    fn truncation_shrinks_the_magnitude_on_both_sides() {
        // The whole reason `basis_bps` fixes REVENUE rather than taking a mode.
        //
        // Premium: 1 / 30000 = 33_333_333.33 raw, truncated DOWN to 33_333_333.
        // Discount: -1 / 30000 = -33_333_333.33 raw, truncated toward zero to
        // -33_333_333 — not down to -33_333_334.
        let up = basis_bps(price("30001"), price("30000")).expect("ok");
        let down = basis_bps(price("29999"), price("30000")).expect("ok");
        assert_eq!(up, Bps::from_raw(33_333_333));
        assert_eq!(down, Bps::from_raw(-33_333_333));
        // Symmetric magnitudes, both understated. If REVENUE were floor rather
        // than truncate, `down` would read -33_333_334 and the discount would be
        // overstated.
        assert_eq!(up.raw(), -down.raw());
    }

    #[test]
    fn the_cost_mode_grows_the_magnitude_on_both_sides() {
        let up = basis_ratio(price("30001"), price("30000"), RoundMode::COST).expect("ok");
        let down = basis_ratio(price("29999"), price("30000"), RoundMode::COST).expect("ok");
        assert_eq!(up, Rate::from_raw(33_333_334));
        assert_eq!(down, Rate::from_raw(-33_333_334));
    }

    #[test]
    fn the_two_forms_of_the_formula_agree_where_both_are_exact() {
        // (mark - index)/index must equal mark/index - 1 whenever there is no
        // remainder. This guards against the subtraction and the division having
        // been transposed.
        let by_diff = basis_ratio(price("100.05"), price("100"), RoundMode::REVENUE).expect("ok");
        let by_ratio = price("100.05")
            .div_price(price("100"), RoundMode::REVENUE)
            .expect("ok")
            .checked_sub(Rate::ONE)
            .expect("ok");
        assert_eq!(by_diff, by_ratio);
    }

    #[test]
    fn a_broken_index_is_refused() {
        assert_eq!(
            basis_ratio(price("100"), Price::ZERO, RoundMode::REVENUE),
            Err(MathError::NonPositivePrice)
        );
        let neg = price("100").checked_neg().expect("ok");
        assert_eq!(
            basis_ratio(price("100"), neg, RoundMode::REVENUE),
            Err(MathError::NonPositivePrice)
        );
    }

    #[test]
    fn convergence_from_a_premium_to_flat_pays_the_whole_basis() {
        // Enter short-perp/long-spot at a 50 bp premium, exit at parity.
        // (0.005 - 0) * 100_000 = 500 USDT. Note: money, not a percentage per
        // year. There is no time in this number and none belongs in it.
        assert_eq!(
            convergence_capture(rate("0.005"), Rate::ZERO, money("100000")).expect("ok"),
            money("500")
        );
    }

    #[test]
    fn a_widening_basis_is_a_loss_and_is_reported_as_one() {
        // Entered at 50 bp, basis widened to 80 bp. The carry is down 30 bp:
        // (0.005 - 0.008) * 100_000 = -300 USDT. Clamping this at zero would
        // hide the most common way a carry goes wrong before it goes right.
        assert_eq!(
            convergence_capture(rate("0.005"), rate("0.008"), money("100000")).expect("ok"),
            money("-300")
        );
    }

    #[test]
    fn capture_across_a_sign_change_is_the_full_travel() {
        // From a 30 bp premium to a 20 bp discount is 50 bp of travel, not 10.
        // (0.003 - (-0.002)) * 100_000 = 0.005 * 100_000 = 500 USDT.
        assert_eq!(
            convergence_capture(rate("0.003"), rate("-0.002"), money("100000")).expect("ok"),
            money("500")
        );
    }

    #[test]
    fn no_movement_captures_nothing() {
        assert_eq!(
            convergence_capture(rate("0.005"), rate("0.005"), money("100000")).expect("ok"),
            Money::ZERO
        );
    }

    #[test]
    fn a_sub_ulp_gain_is_dropped_and_a_sub_ulp_loss_is_charged() {
        // notional = 1e-12, movement = 1e-4 -> true product 1e-16.
        // Gain truncates to zero; the identical-magnitude loss becomes one ulp.
        assert_eq!(
            convergence_capture(rate("0.0001"), Rate::ZERO, Money::EPSILON).expect("ok"),
            Money::ZERO
        );
        assert_eq!(
            convergence_capture(Rate::ZERO, rate("0.0001"), Money::EPSILON).expect("ok"),
            Money::EPSILON.checked_neg().expect("ok")
        );
    }

    #[test]
    fn a_negative_notional_is_refused_not_absolute_valued() {
        let neg = money("100000").checked_neg().expect("ok");
        assert_eq!(
            convergence_capture(rate("0.005"), Rate::ZERO, neg),
            Err(MathError::NegativeNotional)
        );
    }

    #[test]
    fn basis_overflow_is_an_error_not_a_wrap() {
        // Rule 4: no panics anywhere. A pathological index of one ulp makes the
        // ratio astronomically large and it must surface as Overflow.
        assert_eq!(
            basis_ratio(Price::MAX, Price::EPSILON, RoundMode::REVENUE),
            Err(MathError::Arithmetic(NumError::Overflow))
        );
    }

    #[test]
    fn this_module_exposes_no_annualisation() {
        // A guard written as a test because the temptation is real and recurring.
        // If someone adds `basis_apr`, `annualised_basis`, or a `basis::annualise`
        // to this module, that is a product decision requiring a `decisions/`
        // entry — not a convenience. The reason is in the module docs: a
        // perpetual has no expiry, so a per-year basis figure invents a
        // convergence date and double-counts funding.
        //
        // The check that this test stands in for is a human reading the module's
        // public surface. What is asserted here is only the arithmetic that such
        // a function would have produced, so the size of the lie is on record:
        // a 20 bp basis presented as an APR over an 8h funding period reads as
        // 0.002 * 1095 = 2.19, i.e. "219% per year", from a 20 bp gap.
        let twenty_bp = basis_ratio(price("100.2"), price("100"), RoundMode::REVENUE).expect("ok");
        assert_eq!(twenty_bp, rate("0.002"));
        let fictional_apr = twenty_bp.checked_mul_int(1095).expect("ok");
        assert_eq!(fictional_apr, rate("2.19"));
    }
}
