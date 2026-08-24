//! The one error type this crate returns.

use arb_num::NumError;
use arb_types::TypeError;
use core::fmt;

/// Everything that can go wrong in a calculation here.
///
/// Deliberately not a `String`: a caller deciding whether to hide a row from
/// the ranking needs to distinguish "the book was empty" from "the arithmetic
/// overflowed", and a message forces it to match on prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathError {
    /// Fixed-point arithmetic failed.
    Arithmetic(NumError),
    /// A type invariant was violated.
    Type(TypeError),
    /// The side of the book we needed to take had no levels at all.
    EmptyBook,
    /// A size of zero was requested. Not an error the market would report — an
    /// error in our own sizing, and one that silently produces a division by
    /// zero in every average.
    ZeroSize,
    /// A price was zero or negative where only a positive price is meaningful.
    NonPositivePrice,
    /// Margin was zero or negative. A position with no margin is not a position
    /// with infinite leverage, it is a bug upstream.
    NonPositiveMargin,
    /// Leverage was zero or negative.
    NonPositiveLeverage,
    /// The maintenance margin rate was outside `[0, 1)`. At exactly 1 the
    /// liquidation formula divides by zero; above 1 it changes sign and returns
    /// a plausible-looking number on the wrong side of the entry.
    MaintenanceRateOutOfRange,
    /// The position is already below its maintenance requirement at the entry
    /// price. There is no liquidation price to report because liquidation is
    /// not in the future.
    AlreadyBelowMaintenance,
    /// Funding never pays off the round trip, because the per-period receipt is
    /// zero or runs the wrong way.
    NoBreakeven,
    /// A horizon of zero or negative seconds.
    NonPositiveHorizon,
    /// A notional was negative. Direction is carried by
    /// [`arb_types::PositionSide`], never by the sign of the size, and quietly
    /// taking the absolute value here would hide a sign error upstream at
    /// exactly the point where a funding receipt becomes a funding cost.
    NegativeNotional,
    /// A size or quantity was negative. Sizes here are magnitudes; direction is
    /// carried by [`arb_types::PositionSide`]. Rounding a negative size "down"
    /// is ambiguous between floor and truncate, and the two disagree by a whole
    /// lot, so the sign is refused rather than interpreted.
    NegativeSize,
    /// A fee tier table was empty, so there is no tier to fall back to.
    EmptyTierTable,
    /// A fee tier table was not sorted ascending by volume threshold, so
    /// resolution would silently pick the wrong tier.
    UnsortedTierTable,
    /// Two rows of a fee tier table share a volume threshold. Which of them
    /// applies is then decided by their order in the `Vec` rather than by the
    /// venue's schedule, and [`crate::FeeTable::resolve`] keeps the last one —
    /// so a transcription that duplicated a threshold would silently charge the
    /// cheaper of the two. Duplicates are refused rather than disambiguated,
    /// because there is no reading of a duplicated threshold that is a fact
    /// about the venue instead of a mistake about it.
    DuplicateTierThreshold,
    /// A fee rate was negative. Venue rebates exist, but they are not modelled
    /// here: a negative fee turns a cost into revenue and every conservative
    /// rounding decision in this crate inverts.
    NegativeFeeRate,
    /// The formula for this market kind is not implemented, and guessing it
    /// would produce a confident wrong number.
    UnsupportedMarket(&'static str),
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MathError::Arithmetic(e) => write!(f, "arithmetic: {e}"),
            MathError::Type(e) => write!(f, "type: {e}"),
            MathError::EmptyBook => f.write_str("book side has no levels"),
            MathError::ZeroSize => f.write_str("requested size is zero"),
            MathError::NonPositivePrice => f.write_str("price must be positive"),
            MathError::NonPositiveMargin => f.write_str("margin must be positive"),
            MathError::NonPositiveLeverage => f.write_str("leverage must be positive"),
            MathError::MaintenanceRateOutOfRange => {
                f.write_str("maintenance margin rate must be in [0, 1)")
            }
            MathError::AlreadyBelowMaintenance => {
                f.write_str("position is already below maintenance at entry price")
            }
            MathError::NoBreakeven => {
                f.write_str("funding does not pay off the round trip in any number of periods")
            }
            MathError::NonPositiveHorizon => f.write_str("horizon must be positive"),
            MathError::NegativeNotional => f.write_str("notional must not be negative"),
            MathError::NegativeSize => f.write_str("size must not be negative"),
            MathError::EmptyTierTable => f.write_str("fee tier table is empty"),
            MathError::UnsortedTierTable => {
                f.write_str("fee tier table is not sorted ascending by volume")
            }
            MathError::DuplicateTierThreshold => {
                f.write_str("fee tier table has two rows at the same volume threshold")
            }
            MathError::NegativeFeeRate => f.write_str("fee rate is negative"),
            MathError::UnsupportedMarket(what) => write!(f, "unsupported market: {what}"),
        }
    }
}

impl std::error::Error for MathError {}

impl From<NumError> for MathError {
    fn from(e: NumError) -> Self {
        MathError::Arithmetic(e)
    }
}

impl From<TypeError> for MathError {
    fn from(e: TypeError) -> Self {
        MathError::Type(e)
    }
}
