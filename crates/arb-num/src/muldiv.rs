//! Full-precision `a * b / d` for `i128`, with a 256-bit intermediate.
//!
//! # Why this is not `(a * b) / d`
//!
//! Every scaled operation in this crate is a `mul_div`: multiplying two scale-1e12
//! values produces a scale-1e24 value that must be divided back down by 1e12.
//! The intermediate product overflows `i128` long before the *result* does.
//!
//! Concrete example that the naive version gets wrong. A $50,000,000 notional at
//! a price of $68,000:
//!   - `Money` raw = 5e7 * 1e12 = 5e19
//!   - `Price` raw = 6.8e4 * 1e12 = 6.8e16
//!   - naive `raw_m * SCALE` = 5e19 * 1e12 = 5e31 — fits `i128` (max 1.7e38), fine
//!   - but `Qty` * `Price` for a large altcoin position: 1e12 raw * 1e18 raw = 1e30, fine
//!   - the real failure: `Rate::mul_rate` chains, and any `mul_div` where both
//!     operands exceed ~1.3e19 raw (i.e. ~13 million units) overflows, because
//!     1.3e19^2 = 1.7e38 = `i128::MAX`.
//!
//! 13 million units is an ordinary size in a SHIB or DOGE book. So the naive
//! form is not a theoretical concern, it is a live one. This module computes the
//! exact 256-bit product and does a 256-by-128 division, so the only possible
//! overflow is a *result* that genuinely does not fit — reported, never wrapped.

use crate::error::NumError;
use crate::round::RoundMode;

const LIMB: u128 = u64::MAX as u128;

/// Exact 128x128 -> 256 bit unsigned multiply, returned as `(high, low)`.
///
/// Schoolbook multiplication on four 64-bit limbs. Each partial product fits
/// `u128` because both factors are below 2^64. The `mid << 64` deliberately
/// discards bits that have already been folded into `high` via `mid >> 64`;
/// Rust shifts do not trap on discarded value bits, only on a shift amount at
/// or above the type width, and 64 < 128.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    let (a_lo, a_hi) = (a & LIMB, a >> 64);
    let (b_lo, b_hi) = (b & LIMB, b >> 64);

    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;

    // Sum of the three contributions to bits 64..128. Bounded by 3 * 2^64, so
    // it cannot overflow u128.
    let mid = (ll >> 64) + (lh & LIMB) + (hl & LIMB);

    let low = (ll & LIMB) | (mid << 64);
    let high = hh + (lh >> 64) + (hl >> 64) + (mid >> 64);
    (high, low)
}

/// Exact 256-by-128 unsigned division. Returns `(quotient, remainder)`, or
/// `None` if the quotient would not fit in `u128`.
///
/// Restoring shift-subtract long division, 128 iterations. Chosen over a
/// Knuth algorithm-D implementation because it is short enough to audit by eye
/// and this is not a throughput bottleneck: a scan touches thousands of these
/// per second, not billions.
fn div_wide(high: u128, low: u128, d: u128) -> Option<(u128, u128)> {
    if d == 0 {
        return None;
    }
    // If high >= d the quotient needs more than 128 bits.
    if high >= d {
        return None;
    }
    if high == 0 {
        return Some((low / d, low % d));
    }

    let mut rem = high;
    let mut quo: u128 = 0;
    let mut i = 128usize;
    while i > 0 {
        i -= 1;
        let bit = (low >> i) & 1;
        // `rem` may already have its top bit set, in which case `rem << 1`
        // drops a bit that is logically 2^128. Track it: if it was set, the
        // true shifted value exceeds any u128 divisor, so a subtraction is
        // always required and `wrapping_sub` yields the correct low 128 bits
        // (the true value is below 2^128 + d, so the difference is below 2^128).
        let carry_out = rem >> 127;
        rem = (rem << 1) | bit;
        quo <<= 1;
        if carry_out == 1 || rem >= d {
            rem = rem.wrapping_sub(d);
            quo |= 1;
        }
    }
    Some((quo, rem))
}

/// Compute `a * b / d` exactly, then round the remainder per `mode`.
///
/// Errors with [`NumError::DivByZero`] if `d == 0`, or [`NumError::Overflow`]
/// if the rounded result does not fit `i128`.
pub fn mul_div(a: i128, b: i128, d: i128, mode: RoundMode) -> Result<i128, NumError> {
    if d == 0 {
        return Err(NumError::DivByZero);
    }
    // Sign of the exact result. XOR of the three operand signs.
    let negative = (a < 0) ^ (b < 0) ^ (d < 0);

    // `unsigned_abs` is total: it handles i128::MIN, which `-x` would not.
    let (ua, ub, ud) = (a.unsigned_abs(), b.unsigned_abs(), d.unsigned_abs());

    let (high, low) = mul_wide(ua, ub);
    let (mag, rem) = div_wide(high, low, ud).ok_or(NumError::Overflow)?;

    // Decide whether to increase the *magnitude* by one.
    let bump = if rem == 0 {
        false
    } else {
        match mode {
            RoundMode::Trunc => false,
            RoundMode::Floor => negative,
            RoundMode::Ceil => !negative,
            RoundMode::AwayFromZero => true,
            RoundMode::HalfEven => {
                // Compare 2*rem against ud without overflowing: rem < ud, so
                // `ud - rem` is at least 1 and cannot underflow.
                let twice_cmp = rem.cmp(&(ud - rem));
                match twice_cmp {
                    core::cmp::Ordering::Greater => true,
                    core::cmp::Ordering::Less => false,
                    core::cmp::Ordering::Equal => mag % 2 == 1,
                }
            }
        }
    };

    let mag = if bump {
        mag.checked_add(1).ok_or(NumError::Overflow)?
    } else {
        mag
    };

    // Narrow back to i128. The negative range extends one further than positive.
    const MAX: u128 = i128::MAX as u128;
    if negative {
        if mag > MAX + 1 {
            Err(NumError::Overflow)
        } else if mag == MAX + 1 {
            Ok(i128::MIN)
        } else {
            Ok(-(mag as i128))
        }
    } else if mag > MAX {
        Err(NumError::Overflow)
    } else {
        Ok(mag as i128)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_mul_of_max_operands_is_exact() {
        // (2^128 - 1)^2 = 2^256 - 2^129 + 1, whose top 128 bits are 2^128 - 2
        // and bottom 128 bits are 1. Verified by hand, independent of the code.
        let (hi, lo) = mul_wide(u128::MAX, u128::MAX);
        assert_eq!(hi, u128::MAX - 1);
        assert_eq!(lo, 1);
    }

    #[test]
    fn wide_mul_small_values() {
        assert_eq!(mul_wide(0, 12345), (0, 0));
        assert_eq!(mul_wide(7, 6), (0, 42));
        // 2^64 * 2^64 = 2^128 -> hi = 1, lo = 0
        assert_eq!(mul_wide(1u128 << 64, 1u128 << 64), (1, 0));
    }

    #[test]
    fn wide_div_round_trips_against_wide_mul() {
        // q * d + r == a * b, checked on values whose product overflows 128 bits.
        let cases: [(u128, u128, u128); 6] = [
            (1u128 << 100, 1u128 << 100, 1u128 << 90),
            (12_345_678_901_234_567_890, 98_765_432_109_876_543_210, 1_000_000_000_000),
            (u128::MAX, 2, u128::MAX),
            (u128::MAX, u128::MAX, u128::MAX),
            (1, 1, 1),
            // Quotient does NOT fit: (2^128-1)^2 / 3 is about 5.7e75. The
            // contract is None, not a wrapped answer.
            (u128::MAX, u128::MAX, 3),
        ];
        for (a, b, d) in cases {
            let (hi, lo) = mul_wide(a, b);
            match div_wide(hi, lo, d) {
                None => {
                    // Only legitimate reason to decline: quotient exceeds u128.
                    assert!(hi >= d, "declined a quotient that fits: a={a} b={b} d={d}");
                }
                Some((q, r)) => {
                    assert!(r < d, "remainder must be reduced: a={a} b={b} d={d}");
                    // Rebuild q*d + r as a 256-bit value and compare to (hi, lo).
                    let (qh, ql) = mul_wide(q, d);
                    let (sum_lo, carry) = ql.overflowing_add(r);
                    let sum_hi = qh + u128::from(carry);
                    assert_eq!((sum_hi, sum_lo), (hi, lo), "a={a} b={b} d={d}");
                }
            }
        }
    }

    #[test]
    fn div_wide_reports_overflow_instead_of_wrapping() {
        // 2^128 / 1 does not fit u128.
        assert_eq!(div_wide(1, 0, 1), None);
        assert_eq!(div_wide(5, 0, 5), None); // high == d
        assert!(div_wide(4, 0, 5).is_some()); // high < d
    }

    #[test]
    fn div_by_zero_is_an_error_not_a_panic() {
        assert_eq!(mul_div(1, 1, 0, RoundMode::Trunc), Err(NumError::DivByZero));
    }

    #[test]
    fn rounding_modes_on_positive_remainder() {
        // 7 / 2 = 3.5
        assert_eq!(mul_div(7, 1, 2, RoundMode::Trunc), Ok(3));
        assert_eq!(mul_div(7, 1, 2, RoundMode::Floor), Ok(3));
        assert_eq!(mul_div(7, 1, 2, RoundMode::Ceil), Ok(4));
        assert_eq!(mul_div(7, 1, 2, RoundMode::AwayFromZero), Ok(4));
        assert_eq!(mul_div(7, 1, 2, RoundMode::HalfEven), Ok(4)); // 3 is odd -> up
        // 5 / 2 = 2.5, tie, 2 is even -> stays
        assert_eq!(mul_div(5, 1, 2, RoundMode::HalfEven), Ok(2));
    }

    #[test]
    fn rounding_modes_on_negative_remainder() {
        // -7 / 2 = -3.5
        assert_eq!(mul_div(-7, 1, 2, RoundMode::Trunc), Ok(-3));
        assert_eq!(mul_div(-7, 1, 2, RoundMode::Floor), Ok(-4));
        assert_eq!(mul_div(-7, 1, 2, RoundMode::Ceil), Ok(-3));
        assert_eq!(mul_div(-7, 1, 2, RoundMode::AwayFromZero), Ok(-4));
        assert_eq!(mul_div(-7, 1, 2, RoundMode::HalfEven), Ok(-4));
        // Sign carried by the denominator instead.
        assert_eq!(mul_div(7, 1, -2, RoundMode::Floor), Ok(-4));
        assert_eq!(mul_div(-7, -1, -2, RoundMode::Floor), Ok(-4));
        // Two negatives make a positive.
        assert_eq!(mul_div(-7, -1, 2, RoundMode::Ceil), Ok(4));
    }

    #[test]
    fn exact_division_never_bumps() {
        for mode in [
            RoundMode::Trunc,
            RoundMode::Floor,
            RoundMode::Ceil,
            RoundMode::AwayFromZero,
            RoundMode::HalfEven,
        ] {
            assert_eq!(mul_div(6, 1, 2, mode), Ok(3));
            assert_eq!(mul_div(-6, 1, 2, mode), Ok(-3));
            assert_eq!(mul_div(0, 12345, 7, mode), Ok(0));
        }
    }

    #[test]
    fn handles_i128_min_without_panicking() {
        // unsigned_abs is the reason this works; -i128::MIN would overflow.
        assert_eq!(mul_div(i128::MIN, 1, 1, RoundMode::Trunc), Ok(i128::MIN));
        assert_eq!(mul_div(i128::MIN, 1, 2, RoundMode::Trunc), Ok(i128::MIN / 2));
        // Magnitude one past i128::MAX is representable only as MIN.
        assert_eq!(mul_div(i128::MIN, -1, 1, RoundMode::Trunc), Err(NumError::Overflow));
    }

    #[test]
    fn product_beyond_i128_still_divides_correctly() {
        // 1e30 * 1e30 / 1e30 == 1e30. The intermediate is 1e60, far beyond
        // i128::MAX (1.7e38). Naive (a*b)/d would overflow here.
        let e30: i128 = 1_000_000_000_000_000_000_000_000_000_000;
        assert_eq!(mul_div(e30, e30, e30, RoundMode::Trunc), Ok(e30));
    }

    #[test]
    fn overflowing_result_is_reported() {
        assert_eq!(
            mul_div(i128::MAX, i128::MAX, 1, RoundMode::Trunc),
            Err(NumError::Overflow)
        );
    }
}
