//! Error type for all fallible numeric operations.

use core::fmt;

/// Every way a fixed-point operation can fail.
///
/// This enum is deliberately small and `Copy`: it is returned from hot paths
/// and must never allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NumError {
    /// The mathematically correct result does not fit the backing integer.
    Overflow,
    /// Division (or a `mul_div`) with a zero denominator.
    DivByZero,
    /// The input carries more decimal precision than the target scale can
    /// hold, and the excess digits are non-zero.
    ///
    /// Returned instead of silently truncating. See invariant 4 in the crate
    /// docs: a rejected quote costs a scan slot, a truncated one costs money.
    PrecisionLoss,
    /// The string is not a well-formed decimal number.
    InvalidFormat,
    /// A scientific-notation exponent outside the supported range.
    ExponentOutOfRange,
    /// A value that must be non-negative (a size, a fee) was negative.
    NegativeNotAllowed,
}

impl fmt::Display for NumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            NumError::Overflow => "fixed-point overflow",
            NumError::DivByZero => "division by zero",
            NumError::PrecisionLoss => "input precision exceeds fixed-point scale",
            NumError::InvalidFormat => "malformed decimal string",
            NumError::ExponentOutOfRange => "exponent out of supported range",
            NumError::NegativeNotAllowed => "negative value where non-negative required",
        };
        f.write_str(s)
    }
}

impl std::error::Error for NumError {}
