//! The one error type this crate returns.

use core::fmt;

use arb_num::NumError;
use arb_types::TypeError;

/// Everything that can go wrong in a private strategy.
///
/// Four variants, and the interesting one is [`PrivateError::NotImplemented`].
/// An unfinished strategy is a normal, expected outcome of calling into this
/// crate right now, so it is a value and not a panic. The caller gets a
/// `Result` it can log, count and alert on; it does not get a process that dies
/// the first time a scheduler pokes an unfinished path.
#[derive(Debug)]
pub enum PrivateError {
    /// The strategy exists as a type but cannot produce a plan yet.
    ///
    /// `blocked_on` names the missing capability in words a human can act on
    /// ("needs live orderbook stream"), not a ticket number. The stub therefore
    /// documents its own requirement at the point of failure, which is the only
    /// place anybody reads.
    NotImplemented {
        /// Stable machine name of the strategy, e.g. `"quote_and_cover"`.
        strategy: &'static str,
        /// What has to exist before this strategy can return a plan.
        blocked_on: &'static str,
    },
    /// An input the strategy needs was absent, empty or degenerate.
    ///
    /// Distinct from `NotImplemented`: the code is ready for this input, the
    /// caller did not supply it. Fixable by the caller, not by us.
    MissingData {
        /// The specific input that was missing, in caller-facing terms.
        what: &'static str,
    },
    /// Fixed-point arithmetic failed — overflow, division by zero, precision
    /// loss. Never silently absorbed: a rejected number is a rejected trade.
    Arithmetic(NumError),
    /// A domain value from `arb-types` was rejected.
    Type(TypeError),
}

impl PrivateError {
    /// Build a `NotImplemented` error. Both strings are `'static` on purpose:
    /// they describe code, not runtime data, so there is nothing to allocate.
    pub const fn not_implemented(strategy: &'static str, blocked_on: &'static str) -> Self {
        PrivateError::NotImplemented {
            strategy,
            blocked_on,
        }
    }

    /// The strategy name, for `NotImplemented` only.
    pub fn strategy(&self) -> Option<&'static str> {
        match self {
            PrivateError::NotImplemented { strategy, .. } => Some(strategy),
            _ => None,
        }
    }

    /// What the strategy is waiting on, for `NotImplemented` only.
    pub fn blocked_on(&self) -> Option<&'static str> {
        match self {
            PrivateError::NotImplemented { blocked_on, .. } => Some(blocked_on),
            _ => None,
        }
    }

    /// True if this is an unfinished-strategy error rather than a bad input or
    /// a failed calculation. Callers use this to decide whether to alert a
    /// human (no) or to stop asking (yes).
    pub fn is_not_implemented(&self) -> bool {
        matches!(self, PrivateError::NotImplemented { .. })
    }
}

impl fmt::Display for PrivateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The strategy name is in the message because these errors are
            // counted in aggregate; a log line reading "not implemented" with
            // no subject is a log line nobody can act on.
            PrivateError::NotImplemented {
                strategy,
                blocked_on,
            } => write!(
                f,
                "private strategy `{strategy}` is not implemented: blocked on {blocked_on}"
            ),
            PrivateError::MissingData { what } => write!(f, "missing input: {what}"),
            PrivateError::Arithmetic(e) => write!(f, "fixed-point arithmetic failed: {e}"),
            PrivateError::Type(e) => write!(f, "invalid domain value: {e}"),
        }
    }
}

impl std::error::Error for PrivateError {}

impl From<NumError> for PrivateError {
    fn from(e: NumError) -> Self {
        PrivateError::Arithmetic(e)
    }
}

impl From<TypeError> for PrivateError {
    fn from(e: TypeError) -> Self {
        PrivateError::Type(e)
    }
}
