//! The one error type this crate returns.

use arb_num::NumError;
use arb_types::TypeError;

/// Everything that can go wrong while maintaining a book.
///
/// No variant means "probably fine": each one says the ladder cannot be trusted
/// until the caller does something about it.
#[derive(Debug)]
pub enum BookError {
    /// Fixed-point arithmetic failed — overflow while summing depth, etc.
    Arithmetic(NumError),
    /// A level violated an `arb-types` invariant (non-positive price or size).
    Type(TypeError),
    /// No snapshot has ever been applied, so there is no ladder to describe.
    NotInitialized,
    /// A gap or reset was observed; the ladder is unknown until a new snapshot.
    AwaitingResync,
    /// A single message carried more levels than [`crate::MAX_INPUT_LEVELS`].
    /// That is not a depth update, it is corruption or hostility.
    LevelLimitExceeded,
    /// Best bid crossed best ask. Only returned by callers who explicitly
    /// refuse to proceed on a crossed book — see
    /// [`crate::BookState::ensure_not_crossed`]. The book itself only *flags*
    /// crossing ([`arb_types::QualityFlags::CROSSED_BOOK`]); it never
    /// normalises it away, because a crossed book is usually the truth about a
    /// stale side and hiding it destroys the evidence.
    CrossedAfterApply,
    /// A delta carried a negative size. Absolute-size semantics have no meaning
    /// for a negative number, so we refuse rather than clamp to zero (clamping
    /// would silently delete a level the venue never asked us to delete).
    NegativeQty,
    /// A full refresh contained the same price twice. One of the two sizes is
    /// wrong and we cannot know which.
    DuplicatePrice,
}

impl core::fmt::Display for BookError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The inner foundation errors are printed with `Debug`, not `Display`:
        // the frozen digest does not promise a `Display` impl on `NumError` or
        // `TypeError`, and inventing one here is not our call. `Debug` on those
        // enums is a variant name, which is exactly the detail we want anyway.
        match self {
            BookError::Arithmetic(e) => write!(f, "book arithmetic failed: {e:?}"),
            BookError::Type(e) => write!(f, "invalid level: {e:?}"),
            BookError::NotInitialized => f.write_str("book has never received a snapshot"),
            BookError::AwaitingResync => {
                f.write_str("book is awaiting resync after a sequence gap")
            }
            BookError::LevelLimitExceeded => f.write_str("update exceeded the level limit"),
            BookError::CrossedAfterApply => f.write_str("best bid crossed best ask"),
            BookError::NegativeQty => f.write_str("negative size in a depth update"),
            BookError::DuplicatePrice => f.write_str("duplicate price in a full refresh"),
        }
    }
}

impl std::error::Error for BookError {}

impl From<NumError> for BookError {
    fn from(e: NumError) -> Self {
        BookError::Arithmetic(e)
    }
}

impl From<TypeError> for BookError {
    fn from(e: TypeError) -> Self {
        BookError::Type(e)
    }
}
