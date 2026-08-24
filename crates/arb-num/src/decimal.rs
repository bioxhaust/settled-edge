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
        if drop_digits > 38 {
            return Err(NumError::PrecisionLoss);
        }
        let p = pow10_i128(drop_digits as u32).ok_or(NumError::Overflow)?;
        if mag % p != 0 {
            return Err(NumError::PrecisionLoss);
        }
        let out = mag / p;
        Ok(if negative { -out } else { out })
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
}
