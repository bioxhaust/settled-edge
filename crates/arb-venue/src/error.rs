//! Errors that cross the venue seam.
//!
//! One error type for the whole crate. A caller that gets a [`VenueError`] can
//! always answer two questions without downcasting: *can I retry?* and *which
//! line of which fixture do I go fix?*

use std::fmt;

use arb_num::NumError;
use arb_types::TypeError;

/// Why one line of a fixture was rejected.
///
/// Every variant that can name the offending token does name it. A parse error
/// that says only "bad file" costs an engineer ten minutes; one that says
/// `line 14: unknown key "tick_sizee"` costs ten seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseReason {
    /// The file did not begin with the exact `!unverified …` header line.
    ///
    /// This is the single most important error in the crate. See
    /// [`crate::fixture::UNVERIFIED_HEADER`].
    MissingUnverifiedHeader,
    /// A `[section]` name this parser does not know.
    UnknownSection(String),
    /// The same `[section]` appeared twice.
    DuplicateSection(String),
    /// A key this parser does not know. Not a warning — typos in a fixture are
    /// how a fee of 0.00055 silently becomes a default of zero.
    UnknownKey(String),
    /// The same key appeared twice in one section. Last-write-wins is how two
    /// people "fix" a fixture and neither notices the other's edit.
    DuplicateKey(String),
    /// A required key was absent. Line points at the section header.
    MissingKey(&'static str),
    /// A required section was absent. Line points at the end of the file.
    MissingSection(&'static str),
    /// A non-blank, non-comment line with no `=` and no `[section]`.
    ExpectedKeyValue,
    /// A key appeared before any `[section]` header that accepts it.
    KeyOutsideSection(String),
    /// A key/value pair whose value was empty after trimming.
    EmptyValue,
    /// A book level that was not `PRICE x QTY`.
    BadLevel(String),
    /// A `contract_size` that was not `base:QTY` or `quote:MONEY`.
    BadContractSize(String),
    /// An enumerated field with a value outside its fixed set.
    BadEnum {
        /// Key name, e.g. `"kind"`.
        field: &'static str,
        /// What the file actually said.
        value: String,
    },
    /// An integer field (timestamps, sequence, interval seconds) that did not
    /// parse as an integer. Integers go through `str::parse::<i64>` /
    /// `parse::<u64>` — never through a float.
    BadInteger {
        /// Key name, e.g. `"next_funding_at"`.
        field: &'static str,
        /// What the file actually said.
        value: String,
    },
    /// A `[funding]` section on a spot instrument. Spot does not fund; a
    /// funding rate attached to spot is a copy-paste error that would double
    /// count carry.
    FundingOnSpot,
    /// A fixed-point parse or conversion failed. `PrecisionLoss` here means the
    /// file carried more than 12 decimal places, which we refuse to round
    /// silently.
    Number(NumError),
    /// A domain invariant in `arb-types` rejected the value (unsorted levels,
    /// non-positive size, settle/contract-size mismatch, …).
    Type(TypeError),
}

impl fmt::Display for ParseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseReason::MissingUnverifiedHeader => write!(
                f,
                "first line must be exactly the !unverified provenance header"
            ),
            ParseReason::UnknownSection(s) => write!(f, "unknown section [{s}]"),
            ParseReason::DuplicateSection(s) => write!(f, "duplicate section [{s}]"),
            ParseReason::UnknownKey(k) => write!(f, "unknown key {k:?}"),
            ParseReason::DuplicateKey(k) => write!(f, "duplicate key {k:?}"),
            ParseReason::MissingKey(k) => write!(f, "missing required key {k:?}"),
            ParseReason::MissingSection(s) => write!(f, "missing required section [{s}]"),
            ParseReason::ExpectedKeyValue => write!(f, "expected `key = value` or `[section]`"),
            ParseReason::KeyOutsideSection(k) => {
                write!(f, "key {k:?} is not valid outside a section")
            }
            ParseReason::EmptyValue => write!(f, "value is empty"),
            ParseReason::BadLevel(v) => write!(f, "expected `PRICE x QTY`, got {v:?}"),
            ParseReason::BadContractSize(v) => {
                write!(f, "expected `base:QTY` or `quote:MONEY`, got {v:?}")
            }
            ParseReason::BadEnum { field, value } => {
                write!(f, "key {field:?} does not accept {value:?}")
            }
            ParseReason::BadInteger { field, value } => {
                write!(f, "key {field:?} needs an integer, got {value:?}")
            }
            ParseReason::FundingOnSpot => {
                write!(f, "[funding] is not valid for kind = spot")
            }
            // NumError / TypeError are printed via Debug: this crate does not
            // assume they implement Display, and their variant names are
            // already the whole message.
            ParseReason::Number(e) => write!(f, "number rejected: {e:?}"),
            ParseReason::Type(e) => write!(f, "domain invariant rejected: {e:?}"),
        }
    }
}

/// Anything that can go wrong on the venue side of the seam.
///
/// `Clone + PartialEq + Eq` are deliberate, and they are the reason
/// [`VenueError::Io`] carries a `String` instead of a `std::io::Error`:
/// `io::Error` is neither `Clone` nor `PartialEq`, so keeping it would make
/// `assert_eq!` on an error impossible and force every test to hand-write a
/// `match`. The `ErrorKind` we lose is not worth that. If a live adapter later
/// needs to branch on `ErrorKind` (retry a timeout, do not retry a 400), the
/// right fix is a dedicated `Transport { retryable: bool, detail: String }`
/// variant — noted here rather than guessed at now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueError {
    /// The venue (or fixture set) has no such instrument. Not an error the
    /// engine should retry: the symbol map is wrong.
    NotFound,
    /// A fixture (later: a wire payload) could not be parsed. Carries the
    /// 1-based line number so the fix is mechanical.
    Parse {
        /// 1-based line number in the source text.
        line: usize,
        /// What was wrong with that line.
        reason: ParseReason,
    },
    /// Filesystem or transport failure, already rendered to text.
    Io(String),
    /// The request is well-formed but this adapter cannot answer it — funding
    /// on a spot instrument, depth 0, an order kind the venue does not offer.
    /// Distinct from `NotFound` because the caller's *question* is wrong, not
    /// its symbol.
    Unsupported(&'static str),
    /// An `arb-types` invariant was violated while building a snapshot.
    Type(TypeError),
    /// Fixed-point arithmetic failed: overflow, division by zero, or a
    /// precision loss we refuse to paper over.
    Arithmetic(NumError),
    /// Structurally wrong in a way that has no line number — e.g. a fixture
    /// file whose name claims one venue and whose body claims another.
    Malformed,
}

impl VenueError {
    /// Convenience for the parser: attach a line number to a reason.
    pub fn parse(line: usize, reason: ParseReason) -> Self {
        VenueError::Parse { line, reason }
    }

    /// The 1-based line number, when there is one.
    ///
    /// Exists so a loader can print `file:line` without matching on every
    /// variant.
    pub fn line(&self) -> Option<usize> {
        match self {
            VenueError::Parse { line, .. } => Some(*line),
            _ => None,
        }
    }
}

impl fmt::Display for VenueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VenueError::NotFound => write!(f, "instrument not found"),
            VenueError::Parse { line, reason } => write!(f, "line {line}: {reason}"),
            VenueError::Io(m) => write!(f, "io: {m}"),
            VenueError::Unsupported(what) => write!(f, "unsupported: {what}"),
            VenueError::Type(e) => write!(f, "type: {e:?}"),
            VenueError::Arithmetic(e) => write!(f, "arithmetic: {e:?}"),
            VenueError::Malformed => write!(f, "malformed fixture"),
        }
    }
}

impl std::error::Error for VenueError {}

impl From<TypeError> for VenueError {
    fn from(e: TypeError) -> Self {
        VenueError::Type(e)
    }
}

impl From<NumError> for VenueError {
    fn from(e: NumError) -> Self {
        VenueError::Arithmetic(e)
    }
}

impl From<std::io::Error> for VenueError {
    fn from(e: std::io::Error) -> Self {
        // Rendered eagerly: see the type-level comment on why we do not keep
        // the io::Error itself.
        VenueError::Io(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_reports_its_line() {
        let e = VenueError::parse(14, ParseReason::UnknownKey("tick_sizee".to_string()));
        assert_eq!(e.line(), Some(14));
        assert_eq!(e.to_string(), "line 14: unknown key \"tick_sizee\"");
    }

    #[test]
    fn non_parse_errors_have_no_line() {
        assert_eq!(VenueError::NotFound.line(), None);
        assert_eq!(VenueError::Io("disk on fire".to_string()).line(), None);
    }

    #[test]
    fn errors_are_comparable_which_is_why_io_holds_a_string() {
        // The whole justification for Io(String) in one assertion.
        assert_eq!(
            VenueError::Io("nope".to_string()),
            VenueError::Io("nope".to_string())
        );
    }
}
