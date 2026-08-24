#![forbid(unsafe_code)]

//! `arb-detect` — the one question this crate answers is: **is this data good
//! enough to bet money on?**
//!
//! Every judgement here is deliberately pessimistic, and the asymmetry is the
//! whole design. A false "the data is fine" pays a real spread against a book
//! that no longer exists — the loss is bounded only by how wrong the stale
//! picture was. A false "the data is stale" costs exactly one missed
//! opportunity, and the next one arrives in seconds. Those two errors are not
//! the same size, so they do not get the same treatment: at every boundary,
//! ties go to the worse verdict, unknowns are penalised rather than ignored,
//! and no amount of good news is allowed to outvote one fatal signal.
//!
//! Concretely, three rules hold across all five detectors:
//!
//! 1. **Boundaries belong to the worse bucket.** An age exactly equal to
//!    `stale_after` is Stale, not Warn. A gap ratio exactly at the healthy
//!    threshold is Degraded, not Healthy. If a threshold is right, being
//!    exactly on it is already the bad case.
//! 2. **Rounding goes against us.** Where a comparison needs a division, the
//!    quotient is rounded in whichever direction makes the data look worse
//!    ([`arb_num::RoundMode::COST`]).
//! 3. **"We did not look" is not "it was fine."** Un-assessed inputs are
//!    scored as defects, not as zeroes.
//!
//! What this crate does **not** do: it never claims data is good. The strongest
//! statement available is "we found no evidence against it". That distinction
//! matters most in [`painted`], where a confident "this depth is real" is more
//! dangerous than an admitted unknown.
//!
//! No I/O, no clock reads, no floats. Timestamps come in as arguments; sequence
//! counters come in as plain integers so this crate stays decoupled from the
//! book-maintenance crate that produces them.

pub mod clock;
pub mod error;
pub mod painted;
pub mod sequence;
pub mod staleness;
pub mod trust;

pub use clock::{assess_skew, ClockVerdict, SkewLimits};
pub use error::DetectError;
pub use painted::{assess_paint, PaintLimits, PaintReport, PaintSignal};
pub use sequence::{assess_sequence, SeqCounters, SeqHealth, SeqHealthLimits};
pub use staleness::{age_of, assess_age, Staleness, StalenessLimits};
pub use trust::{assess_venue, flags_for, is_crossed, TrustNote, VenueEvidence, VenueTrust};
