//! Decimal string <-> scaled integer conversion.
//!
//! Exchange REST and WebSocket payloads carry numbers as JSON *strings*
//! (`"68123.45000000"`, `"0.00001234"`, `"-0.00037500"`) precisely so clients do
//! not route them through a float. Parsing them into `f64` and back is the
//! single most common way a scanner starts lying. This module is the only
//! sanctioned entry point.
//!
//! Supported input grammar:
//!
//! ```text
//! number   := sign? ( digits ( '.' digits? )? | '.' digits ) exponent?
//! sign     := '+' | '-'
//! exponent := ('e' | 'E') sign? digits
//! ```
//!
//! Scientific notation is accepted because several venues emit it for very
//! small numbers (`"8.12e-9"`). Leading/trailing whitespace is **rejected**: a
//! stray space means the adapter mis-sliced the payload, and we want that loud.

use crate::error::NumError;
use crate::muldiv::mul_div;
use crate::round::RoundMode;

/// `10^n` as `i128`, or `None` if it does not fit.
pub fn pow10_i128(n: u32) -> Option<i128> {
    let mut p: i128 = 1;
    let mut i = 0;
    while i < n {
        p = p.checked_mul(10)?;
        i += 1;
    }
    Some(p)
}

/// `10^n` as `u128`, clamped at `10^38`.
///
/// Total by construction so that `Display` impls, which cannot return an error,
/// never need to unwrap. `n` is always a crate scale constant (8 or 12).
fn pow10_u128(n: u32) -> u128 {
    let mut p: u128 = 1;
    let mut i = 0;
    while i < n && i < 38 {
        p *= 10;
        i += 1;
    }
    p
}

/// Parse a decimal string into an integer scaled by `10^scale_digits`.
///
/// Returns [`NumError::PrecisionLoss`] rather than truncating when the input
/// carries non-zero digits below the target scale. Trailing zeros are not
/// precision: `"1.2500000000000000"` parses fine at scale 12.
pub fn parse_scaled(s: &str, scale_digits: u32) -> Result<i128, NumError> {
    parse_scaled_inner(s, scale_digits, None)
}

/// [`parse_scaled`], but rounding sub-scale digits instead of refusing them.
///
/// # When this is the right function, and when it hides a bug
///
/// Refusing to truncate is the correct default and stays the default: a price
/// with more decimal places than [`crate::SCALE_DIGITS`] is far more likely to be
/// a misread field — an interest rate parsed as a price, a payload whose units we
/// misunderstood — than a genuinely finer price. `PrecisionLoss` catches that, and
/// this function switches the catch off, so reaching for it needs a reason
/// specific to the field being read.
///
/// The reason that exists today: **OKX publishes `fundingRate` to sixteen decimal
/// places** (`"0.0000689368148824"`), and a funding rate really does carry
/// meaningful digits that far down, because it is a ratio and not a quoted price.
/// [факт, проверено — `/api/v5/public/funding-rate` прочитан живьём 2026-08-25]
/// Four digits fall below scale 12, so [`parse_scaled`] rejects the venue's own
/// number and the adapter can report no funding at all.
///
/// # The bound, stated rather than assumed away
///
/// At most half a unit in the last place is discarded: `5e-13` on the rate.
/// Annualised over 1095 eight-hour periods that is `5.5e-10`, or `5.5e-8`
/// percentage points of APR — six orders of magnitude below the twelve basis
/// points of edge any threshold in this workspace is set against. It cannot move
/// a ranking decision.
///
/// # Why the mode is a parameter with no default
///
/// Whether a funding rate is a cost or a revenue depends on which side the
/// position takes, which a parser cannot know, so neither
/// [`RoundMode::COST`] nor [`RoundMode::REVENUE`] is the conservative choice here.
/// [`RoundMode::HalfEven`] is documented as the neutral mode for exactly this
/// case and is what the OKX adapter passes; the parameter stays explicit so that a
/// caller reading a one-directional field can still say so.
pub fn parse_scaled_rounded(
    s: &str,
    scale_digits: u32,
    mode: RoundMode,
) -> Result<i128, NumError> {
    parse_scaled_inner(s, scale_digits, Some(mode))
}

/// `mode: None` means refuse sub-scale digits; `Some` means round them.
fn parse_scaled_inner(
    s: &str,
    scale_digits: u32,
    mode: Option<RoundMode>,
) -> Result<i128, NumError> {
    let b = s.as_bytes();
    if b.is_empty() {
        return Err(NumError::InvalidFormat);
    }

    let mut i = 0usize;
    let negative = match b[0] {
        b'-' => {
            i = 1;
            true
        }
        b'+' => {
            i = 1;
            false
        }
        _ => false,
    };

    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_end = i;

    let mut frac_start = i;
    let mut frac_end = i;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac_end = i;
    }

    // At least one digit total, somewhere.
    if int_start == int_end && frac_start == frac_end {
        return Err(NumError::InvalidFormat);
    }

    let mut exp: i64 = 0;
    if i < b.len() && (b[i] | 0x20) == b'e' {
        i += 1;
        let mut exp_negative = false;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            exp_negative = b[i] == b'-';
            i += 1;
        }
        let exp_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if exp_start == i {
            return Err(NumError::InvalidFormat);
        }
        if i - exp_start > 5 {
            return Err(NumError::ExponentOutOfRange);
        }
        let mut v: i64 = 0;
        for &c in &b[exp_start..i] {
            // At most 5 digits, so bounded by 99_999.
            v = v * 10 + i64::from(c - b'0');
        }
        if v > 10_000 {
            return Err(NumError::ExponentOutOfRange);
        }
        exp = if exp_negative { -v } else { v };
    }

    // Any trailing garbage is a malformed number, not something to ignore.
    if i != b.len() {
        return Err(NumError::InvalidFormat);
    }

    // Strip trailing fractional zeros before accumulating. "1.250000...0" and
    // "1.25" denote the same number, and stripping first prevents a long run of
    // insignificant zeros from pushing the accumulator into a false overflow.
    let mut frac_end = frac_end;
    while frac_end > frac_start && b[frac_end - 1] == b'0' {
        frac_end -= 1;
    }
    let frac_len = (frac_end - frac_start) as i64;

    // Accumulate significant digits. Leading zeros keep the accumulator at 0,
    // so they cannot overflow.
    let mut mag: i128 = 0;
    for &c in b[int_start..int_end].iter().chain(b[frac_start..frac_end].iter()) {
        mag = mag
            .checked_mul(10)
            .ok_or(NumError::Overflow)?
            .checked_add(i128::from(c - b'0'))
            .ok_or(NumError::Overflow)?;
    }

    // value          = mag * 10^(exp - frac_len)
    // scaled integer = value * 10^scale_digits
    let shift = exp - frac_len + i64::from(scale_digits);

    if mag == 0 {
        return Ok(0);
    }

    if shift >= 0 {
        if shift > 38 {
            return Err(NumError::Overflow);
        }
        let p = pow10_i128(shift as u32).ok_or(NumError::Overflow)?;
        let out = mag.checked_mul(p).ok_or(NumError::Overflow)?;
        Ok(if negative { -out } else { out })
    } else {
        let drop_digits = -shift;
        // Refused in both modes. An input needing more than 38 digits of shift is
        // outside this crate's range entirely, and flattening it to zero because a
        // rounding mode was supplied would turn "I cannot represent this" into "the
        // value is nothing".
        if drop_digits > 38 {
            return Err(NumError::PrecisionLoss);
        }
        let p = pow10_i128(drop_digits as u32).ok_or(NumError::Overflow)?;
        if mag % p == 0 {
            let out = mag / p;
            return Ok(if negative { -out } else { out });
        }
        // Sub-scale digits are present: strict mode refuses, rounding mode disposes
        // of them in the direction the caller named.
        //
        // The sign is applied *before* the division, not after, because `Trunc`,
        // `Floor` and `Ceil` are not sign-symmetric — `Floor` on a negative value
        // must increase magnitude — and dividing an unsigned magnitude first would
        // silently turn each of those modes into its mirror image.
        let mode = mode.ok_or(NumError::PrecisionLoss)?;
        let signed = if negative {
            mag.checked_neg().ok_or(NumError::Overflow)?
        } else {
            mag
        };
        mul_div(signed, 1, p, mode)
    }
}

/// Format a scaled integer as the shortest exact decimal string.
///
/// Trailing fractional zeros are trimmed; an integral value prints with no
/// decimal point. Guarantees `parse_scaled(&format_scaled(v, s), s) == Ok(v)`.
pub fn format_scaled(v: i128, scale_digits: u32) -> String {
    let p = pow10_u128(scale_digits);
    let mag = v.unsigned_abs();
    let int_part = mag / p;
    let frac_part = mag % p;

    let mut out = String::with_capacity(32);
    if v < 0 {
        out.push('-');
    }
    out.push_str(&int_part.to_string());

    if frac_part != 0 {
        let digits = scale_digits.min(38) as usize;
        let mut buf = [b'0'; 38];
        let mut x = frac_part;
        let mut idx = digits;
        while idx > 0 {
            idx -= 1;
            buf[idx] = b'0' + (x % 10) as u8;
            x /= 10;
        }
        let mut end = digits;
        while end > 0 && buf[end - 1] == b'0' {
            end -= 1;
        }
        out.push('.');
        for &c in &buf[..end] {
            out.push(char::from(c));
        }
    }
    out
}

/// Format with exactly `places` decimals, rounding half away from zero.
///
/// For human-facing tables only. Never feed the output of this back into
/// [`parse_scaled`] and treat it as the same number.
pub fn format_scaled_places(v: i128, scale_digits: u32, places: u32) -> String {
    let value = if places >= scale_digits {
        // Widening: exact, just append zeros' worth of scale.
        let extra = places - scale_digits;
        match pow10_i128(extra).and_then(|p| v.checked_mul(p)) {
            Some(x) => x,
            None => return format_scaled(v, scale_digits),
        }
    } else {
        let drop_digits = scale_digits - places;
        let p = match pow10_i128(drop_digits) {
            Some(p) => p,
            None => return format_scaled(v, scale_digits),
        };
        // Rust integer division truncates toward zero, so quotient and
        // remainder share the sign of `v`.
        let q = v / p;
        let r = v % p;
        let half_or_more = r.unsigned_abs() * 2 >= p.unsigned_abs();
        if half_or_more {
            let step = if v < 0 { -1 } else { 1 };
            q.checked_add(step).unwrap_or(q)
        } else {
            q
        }
    };
    // `value` is now scaled by 10^places; print it with exactly that many.
    let p = pow10_u128(places);
    let mag = value.unsigned_abs();
    let mut out = String::with_capacity(32);
    if value < 0 {
        out.push('-');
    }
    out.push_str(&(mag / p).to_string());
    if places > 0 {
        out.push('.');
        let digits = places.min(38) as usize;
        let mut buf = [b'0'; 38];
        let mut x = mag % p;
        let mut idx = digits;
        while idx > 0 {
            idx -= 1;
            buf[idx] = b'0' + (x % 10) as u8;
            x /= 10;
        }
        for &c in &buf[..digits] {
            out.push(char::from(c));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u32 = 12;

    #[test]
    fn plain_decimals() {
        assert_eq!(parse_scaled("0", S), Ok(0));
        assert_eq!(parse_scaled("1", S), Ok(1_000_000_000_000));
        assert_eq!(parse_scaled("-1", S), Ok(-1_000_000_000_000));
        assert_eq!(parse_scaled("1.5", S), Ok(1_500_000_000_000));
        assert_eq!(parse_scaled("0.5", S), Ok(500_000_000_000));
        assert_eq!(parse_scaled(".5", S), Ok(500_000_000_000));
        assert_eq!(parse_scaled("5.", S), Ok(5_000_000_000_000));
        assert_eq!(parse_scaled("+2.25", S), Ok(2_250_000_000_000));
        assert_eq!(parse_scaled("0000123", S), Ok(123_000_000_000_000));
    }

    #[test]
    fn realistic_exchange_strings() {
        // Binance-style 8-decimal price.
        assert_eq!(parse_scaled("68123.45000000", S), Ok(68_123_450_000_000_000));
        // A small-cap price at 8 decimals.
        assert_eq!(parse_scaled("0.00001234", S), Ok(12_340_000));
        // A negative funding rate at 8 decimals.
        assert_eq!(parse_scaled("-0.00037500", S), Ok(-375_000_000));
        // Zero funding, fully padded.
        assert_eq!(parse_scaled("0.00000000", S), Ok(0));
    }

    #[test]
    fn scientific_notation() {
        assert_eq!(parse_scaled("1e0", S), Ok(1_000_000_000_000));
        assert_eq!(parse_scaled("1e3", S), Ok(1_000_000_000_000_000));
        assert_eq!(parse_scaled("8.12e-9", S), Ok(8_120));
        assert_eq!(parse_scaled("-8.12E-9", S), Ok(-8_120));
        assert_eq!(parse_scaled("1.5e2", S), Ok(150_000_000_000_000));
        // 1e-12 is exactly one unit at scale 12.
        assert_eq!(parse_scaled("1e-12", S), Ok(1));
    }

    #[test]
    fn trailing_zeros_are_not_precision() {
        // 22 fractional digits, but all the significant ones fit.
        assert_eq!(parse_scaled("1.2500000000000000000000", S), Ok(1_250_000_000_000));
        // A digit run far longer than i128 could hold, all zeros.
        let long = format!("1.{}", "0".repeat(60));
        assert_eq!(parse_scaled(&long, S), Ok(1_000_000_000_000));
    }

    #[test]
    fn excess_precision_is_refused_not_truncated() {
        // 1e-13 cannot be represented at scale 12. Truncating would give 0,
        // which is a silently wrong number. We refuse instead.
        assert_eq!(parse_scaled("0.0000000000001", S), Err(NumError::PrecisionLoss));
        assert_eq!(parse_scaled("1e-13", S), Err(NumError::PrecisionLoss));
        // 13 significant fractional digits where the 13th is non-zero.
        assert_eq!(
            parse_scaled("1.0000000000001", S),
            Err(NumError::PrecisionLoss)
        );
    }

    #[test]
    fn malformed_input_is_rejected() {
        for bad in [
            "", " ", "1 ", " 1", "abc", "1.2.3", "--1", "1-", "1e", "1e+", "1ee5", "0x10", "1,5",
            "nan", "inf", "1.2e1.5", "1_000",
        ] {
            assert_eq!(
                parse_scaled(bad, S),
                Err(NumError::InvalidFormat),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn oversized_values_overflow_rather_than_wrap() {
        assert_eq!(parse_scaled("1e30", S), Err(NumError::Overflow));
        let huge = "9".repeat(40);
        assert_eq!(parse_scaled(&huge, S), Err(NumError::Overflow));
    }

    #[test]
    fn exponent_range_is_bounded() {
        assert_eq!(parse_scaled("1e999999", S), Err(NumError::ExponentOutOfRange));
        assert_eq!(parse_scaled("1e20000", S), Err(NumError::ExponentOutOfRange));
    }

    #[test]
    fn format_is_shortest_exact() {
        assert_eq!(format_scaled(0, S), "0");
        assert_eq!(format_scaled(1_000_000_000_000, S), "1");
        assert_eq!(format_scaled(-1_000_000_000_000, S), "-1");
        assert_eq!(format_scaled(1_500_000_000_000, S), "1.5");
        assert_eq!(format_scaled(12_340_000, S), "0.00001234");
        assert_eq!(format_scaled(-375_000_000, S), "-0.000375");
        assert_eq!(format_scaled(1, S), "0.000000000001");
        assert_eq!(format_scaled(68_123_450_000_000_000, S), "68123.45");
    }

    #[test]
    fn parse_format_round_trips() {
        let values: [i128; 12] = [
            0,
            1,
            -1,
            999,
            -999,
            1_000_000_000_000,
            -68_123_450_000_000_000,
            12_340_000,
            i64::MAX as i128,
            i64::MIN as i128,
            170_141_183_460_469_231_731_687_303_715_884_105_727, // i128::MAX
            -170_141_183_460_469_231_731_687_303_715_884_105_727,
        ];
        for v in values {
            let s = format_scaled(v, S);
            assert_eq!(parse_scaled(&s, S), Ok(v), "round-trip failed for {v} -> {s}");
        }
    }

    #[test]
    fn fixed_places_rounds_half_away_from_zero() {
        // 1.2345 at scale 12, shown with 3 places -> 1.235 (half rounds up)
        let v = parse_scaled("1.2345", S).expect("valid");
        assert_eq!(format_scaled_places(v, S, 3), "1.235");
        assert_eq!(format_scaled_places(v, S, 2), "1.23");
        assert_eq!(format_scaled_places(v, S, 0), "1");
        // Symmetric for negatives.
        let n = parse_scaled("-1.2345", S).expect("valid");
        assert_eq!(format_scaled_places(n, S, 3), "-1.235");
        // Padding out beyond available precision.
        let h = parse_scaled("1.5", S).expect("valid");
        assert_eq!(format_scaled_places(h, S, 4), "1.5000");
        assert_eq!(format_scaled_places(0, S, 2), "0.00");
    }

    #[test]
    fn pow10_bounds() {
        assert_eq!(pow10_i128(0), Some(1));
        assert_eq!(pow10_i128(12), Some(1_000_000_000_000));
        assert!(pow10_i128(38).is_some());
        assert_eq!(pow10_i128(39), None);
    }

    /// OKX's real `fundingRate`, which scale 12 cannot hold.
    ///
    /// Sixteen decimal places, the last four of them below scale. This exact
    /// string is what `/api/v5/public/funding-rate` returned on 2026-08-25, and
    /// it is why [`parse_scaled_rounded`] exists at all: without it the strict
    /// parser refuses the venue's own number and the OKX adapter can report no
    /// funding rate whatsoever.
    const OKX_FUNDING_RATE: &str = "0.0000689368148824";

    #[test]
    fn a_venue_rate_below_scale_is_refused_strictly_and_rounded_only_on_request() {
        // The strict parser rejects it, and that rejection is the default that
        // catches misidentified fields elsewhere.
        assert_eq!(parse_scaled(OKX_FUNDING_RATE, S), Err(NumError::PrecisionLoss));

        // 689368148824 / 10^4 = 68936814 remainder 8824. The remainder is more
        // than half of 10^4, so nearest rounds up.
        assert_eq!(
            parse_scaled_rounded(OKX_FUNDING_RATE, S, RoundMode::HalfEven),
            Ok(68_936_815)
        );
        assert_eq!(
            parse_scaled_rounded(OKX_FUNDING_RATE, S, RoundMode::Trunc),
            Ok(68_936_814)
        );
        assert_eq!(
            parse_scaled_rounded(OKX_FUNDING_RATE, S, RoundMode::AwayFromZero),
            Ok(68_936_815)
        );

        // The discarded amount, in the units the docs quote a bound in: under one
        // unit in the last place, i.e. below 1e-12 on the rate.
        let exact_upper = parse_scaled_rounded(OKX_FUNDING_RATE, S, RoundMode::Ceil)
            .expect("ceil");
        let exact_lower = parse_scaled_rounded(OKX_FUNDING_RATE, S, RoundMode::Floor)
            .expect("floor");
        assert_eq!(exact_upper - exact_lower, 1);
    }

    #[test]
    fn the_sign_is_applied_before_the_division_so_directional_modes_are_not_mirrored() {
        // The bug this guards: dividing an unsigned magnitude and negating after
        // would make `Floor` behave like `Ceil` on every negative rate. Funding
        // rates are negative about half the time, so the mirror would be live.
        let negative = "-0.0000689368148824";

        // Toward zero: magnitude decreases, on both signs.
        assert_eq!(parse_scaled_rounded(negative, S, RoundMode::Trunc), Ok(-68_936_814));
        // Toward negative infinity: magnitude *increases* on a negative value.
        assert_eq!(parse_scaled_rounded(negative, S, RoundMode::Floor), Ok(-68_936_815));
        // Toward positive infinity: magnitude decreases on a negative value.
        assert_eq!(parse_scaled_rounded(negative, S, RoundMode::Ceil), Ok(-68_936_814));
        // Away from zero: magnitude increases, on both signs.
        assert_eq!(
            parse_scaled_rounded(negative, S, RoundMode::AwayFromZero),
            Ok(-68_936_815)
        );

        // And each mode is the exact negation of its mirror on the positive input,
        // which is the property a sign-after-division implementation would break.
        for (mode, mirror) in [
            (RoundMode::Floor, RoundMode::Ceil),
            (RoundMode::Ceil, RoundMode::Floor),
            (RoundMode::Trunc, RoundMode::Trunc),
            (RoundMode::AwayFromZero, RoundMode::AwayFromZero),
            (RoundMode::HalfEven, RoundMode::HalfEven),
        ] {
            let neg = parse_scaled_rounded(negative, S, mode).expect("negative");
            let pos = parse_scaled_rounded(OKX_FUNDING_RATE, S, mirror).expect("positive");
            assert_eq!(neg, -pos, "{mode:?} is not the mirror of {mirror:?}");
        }
    }

    #[test]
    fn rounding_changes_nothing_when_the_input_already_fits() {
        // Every value the strict parser accepts must parse identically under every
        // mode, or the rounding path would be silently altering exact data.
        for input in ["0", "1.25", "-1.25", "78992.6", "0.00007087", "1.250000000000", "8.12e-9"] {
            let strict = parse_scaled(input, S).expect(input);
            for mode in [
                RoundMode::Trunc,
                RoundMode::Floor,
                RoundMode::Ceil,
                RoundMode::AwayFromZero,
                RoundMode::HalfEven,
            ] {
                assert_eq!(
                    parse_scaled_rounded(input, S, mode),
                    Ok(strict),
                    "{input} changed under {mode:?}"
                );
            }
        }
    }

    #[test]
    fn half_even_breaks_ties_toward_the_even_result() {
        // Exactly half a unit in the last place, both directions.
        assert_eq!(
            parse_scaled_rounded("0.0000000000005", S, RoundMode::HalfEven),
            Ok(0),
            "0.5 ulp with an even neighbour rounds down"
        );
        assert_eq!(
            parse_scaled_rounded("0.0000000000015", S, RoundMode::HalfEven),
            Ok(2),
            "1.5 ulp with an odd neighbour rounds up"
        );
    }

    #[test]
    fn a_value_far_below_scale_is_refused_even_with_a_rounding_mode() {
        // Beyond 38 digits of shift the answer is not "zero", it is "outside this
        // crate's range", and a rounding mode must not convert one into the other.
        assert_eq!(
            parse_scaled_rounded("1e-51", S, RoundMode::HalfEven),
            Err(NumError::PrecisionLoss)
        );
        assert_eq!(parse_scaled("1e-51", S), Err(NumError::PrecisionLoss));
    }

    #[test]
    fn a_rounding_mode_does_not_excuse_malformed_input() {
        // The grammar is unchanged: rounding disposes of digits below scale, it
        // does not make a garbage string parse.
        for bad in ["", " 1.5", "1.5 ", "1..5", "abc", "1.2.3"] {
            assert!(
                parse_scaled_rounded(bad, S, RoundMode::HalfEven).is_err(),
                "should have refused {bad:?}"
            );
        }
    }
}
