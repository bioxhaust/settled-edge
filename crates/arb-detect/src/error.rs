//! Errors. Every fallible detector returns one of these; nothing in this crate
//! panics, so an arithmetic edge case surfaces as a value the caller has to
//! deal with rather than a process that dies mid-quote.

use std::fmt;

use arb_num::NumError;

/// Why a detector could not reach a verdict.
///
/// Note the shape: there is no `Unknown` catch-all and no error that means
/// "probably fine". If a detector cannot compute its answer, the caller gets
/// told which specific thing was impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectError {
    /// Fixed-point arithmetic failed — overflow, or a division whose divisor
    /// was zero. Both mean the inputs were outside the range this crate can
    /// reason about, which is itself a data-quality answer: do not trade it.
    Arithmetic(NumError),
    /// The side of the book under test has no levels at all. This is not a
    /// paint signal and it is not staleness; it is missing data, and the caller
    /// owns the `BOOK_EMPTY` flag for it.
    EmptySide,
    /// The limits handed to a detector cannot produce a meaningful answer (for
    /// example a zero rounding unit, or a level count that does not fit the
    /// arithmetic). We refuse rather than silently substituting a default,
    /// because a substituted default is a threshold nobody justified.
    UnusableLimits(&'static str),
}

impl From<NumError> for DetectError {
    fn from(e: NumError) -> Self {
        DetectError::Arithmetic(e)
    }
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The digest does not state that `NumError` implements `Display`, so we
        // do not format the inner value and we do not claim a source chain.
        // Naming the failure class is enough for an operator to act on.
        match self {
            DetectError::Arithmetic(_) => {
                f.write_str("fixed-point arithmetic failed (overflow or division by zero)")
            }
            DetectError::EmptySide => f.write_str("the side under test has no levels"),
            DetectError::UnusableLimits(what) => {
                write!(f, "detector limits are unusable: {what}")
            }
        }
    }
}

impl std::error::Error for DetectError {}
