#![forbid(unsafe_code)]
//! Reading the local clock, as an injected capability.
//!
//! # Why this is a trait and not a call to `SystemTime::now`
//!
//! [`arb_venue::ExchangeAdapter`]'s documentation is explicit: *"no `now()`: an
//! adapter must never be the clock. `observed_at` comes from whoever built the
//! snapshot."*
//!
//! A live adapter nonetheless has to record *when* it observed a book, because
//! that instant is the entire input to staleness detection — `arb_detect`'s
//! `assess_age` compares `observed_at` against a scan's `now`, and a book with no
//! observation time cannot be judged stale at any threshold.
//!
//! Both things are satisfiable at once. The adapter does not *contain* a clock,
//! it is *given* one. The rule the trait states is about coupling: an adapter that
//! calls `SystemTime::now` internally is untestable, because every snapshot it
//! produces carries a different timestamp and no assertion about staleness can be
//! written against it. An adapter handed a [`FixedClock`] produces byte-identical
//! snapshots on every run, so the whole test suite is deterministic — and the
//! production path differs by one constructor argument.
//!
//! # This clock is wall time, and that is deliberate
//!
//! `LocalTs` is compared against a venue's `WallTs` by `arb_detect::assess_skew`,
//! so it has to be the same kind of quantity: milliseconds since the Unix epoch,
//! from the system's wall clock. A monotonic source would be immune to NTP steps
//! but meaningless to subtract from an exchange timestamp, which is the one
//! subtraction this value exists for.
//!
//! The consequence is worth stating plainly: if the local clock is wrong, every
//! skew verdict is wrong in the same direction, and `arb-detect` will read our own
//! misconfiguration as the venue's fault. That is not a flaw in this design so
//! much as the reason `ClockVerdict` is reported per venue — a skew that appears
//! on *all* venues at once is us, and a skew on one is them.

use std::time::{SystemTime, UNIX_EPOCH};

use arb_types::LocalTs;
use arb_venue::VenueError;

/// A source of the current local instant.
pub trait Clock {
    /// Milliseconds since the Unix epoch, by the local wall clock.
    fn now(&self) -> Result<LocalTs, VenueError>;
}

/// The system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Result<LocalTs, VenueError> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| {
                VenueError::Io(format!("system clock is before the Unix epoch: {e}"))
            })?
            .as_millis();
        // `as_millis` is u128. Past 2262 this no longer fits an i64, and a
        // machine whose clock reads that far ahead is misconfigured badly enough
        // that silently truncating would produce a plausible-looking timestamp
        // from nonsense.
        let millis = i64::try_from(millis).map_err(|_| {
            VenueError::Io("system clock is too far in the future to represent".to_string())
        })?;
        Ok(LocalTs::from_millis(millis))
    }
}

/// A clock that always reports the same instant.
///
/// The reason every adapter test in this crate is deterministic.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    at: LocalTs,
}

impl FixedClock {
    /// A clock stopped at `at`.
    pub const fn new(at: LocalTs) -> Self {
        Self { at }
    }

    /// A clock stopped at `millis` since the epoch.
    pub const fn at_millis(millis: i64) -> Self {
        Self {
            at: LocalTs::from_millis(millis),
        }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Result<LocalTs, VenueError> {
        Ok(self.at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_reports_a_plausible_instant() {
        let now = SystemClock.now().expect("clock");
        // 2020-01-01 in ms. Anything below it means the clock is unset, which is
        // worth catching here rather than as a mysterious staleness verdict.
        assert!(
            now.millis() > 1_577_836_800_000,
            "system clock reads before 2020: {}",
            now.millis()
        );
    }

    #[test]
    fn a_fixed_clock_does_not_move() {
        let c = FixedClock::at_millis(1_787_597_404_081);
        let first = c.now().expect("clock");
        let second = c.now().expect("clock");
        assert_eq!(first, second);
        assert_eq!(first.millis(), 1_787_597_404_081);
    }
}
