//! Timestamps and durations.
//!
//! The canonical unit everywhere in this workspace is the **millisecond**.
//! All eight venues in scope report millisecond timestamps, so there is no
//! conversion step to forget. There is intentionally no constructor taking
//! seconds, microseconds, or nanoseconds: a unit mistake here shows up as a
//! staleness check that is wrong by 1000x, which means a two-minute-old quote
//! passes a 120 ms freshness gate.

use core::fmt;

/// A duration in milliseconds. Signed, because clock skew has a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Millis(i64);

impl Millis {
    /// Zero duration.
    pub const ZERO: Self = Self(0);

    /// Construct from a millisecond count.
    #[inline]
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    /// Construct from whole seconds. Saturates rather than wrapping.
    #[inline]
    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(1000))
    }

    /// The millisecond count.
    #[inline]
    pub const fn millis(self) -> i64 {
        self.0
    }

    /// Absolute magnitude, saturating at [`i64::MAX`] for [`i64::MIN`].
    #[inline]
    pub const fn abs(self) -> Self {
        Self(self.0.saturating_abs())
    }

    /// True when strictly negative.
    #[inline]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }
}

impl fmt::Display for Millis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

/// A timestamp **the exchange reported**, in milliseconds since the Unix epoch.
///
/// Never compare this against a [`LocalTs`] with `<` or `-` directly: the two
/// clocks are not synchronised, and the difference between them is what
/// `arb-detect` measures rather than something callers should assume away. Use
/// [`WallTs::apparent_skew_from`] and read its documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct WallTs(i64);

impl WallTs {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// Construct from exchange-reported milliseconds since the Unix epoch.
    #[inline]
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    /// Milliseconds since the Unix epoch, as the exchange reported them.
    #[inline]
    pub const fn millis(self) -> i64 {
        self.0
    }

    /// Elapsed time from `earlier` to `self`. Negative when `self` precedes it.
    ///
    /// Both operands must come from the *same* venue's clock for the result to
    /// mean anything. Two venues' wall clocks differ by tens of milliseconds.
    #[inline]
    pub const fn since(self, earlier: WallTs) -> Millis {
        Millis(self.0.saturating_sub(earlier.0))
    }

    /// Shift forward by a duration.
    #[inline]
    pub const fn add_millis(self, d: Millis) -> Self {
        Self(self.0.saturating_add(d.0))
    }

    /// Apparent difference between a venue's clock and ours.
    ///
    /// This value mixes two things that cannot be separated from a single
    /// observation: genuine clock skew between the venue and us, and network
    /// latency in delivering the message. It is therefore an *upper bound* on
    /// skew, not a measurement of it, and is named `apparent_` to keep that in
    /// view at the call site. A positive result means the venue's timestamp is
    /// ahead of our clock.
    #[inline]
    pub const fn apparent_skew_from(self, local: LocalTs) -> Millis {
        Millis(self.0.saturating_sub(local.0))
    }
}

/// A timestamp **our own process recorded**, in milliseconds since the Unix
/// epoch.
///
/// Used for staleness: the age of a quote is measured against our clock,
/// because that is the clock our order will be submitted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LocalTs(i64);

impl LocalTs {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// Construct from milliseconds since the Unix epoch on our clock.
    ///
    /// Reading the system clock is I/O and does not happen in this crate — rule
    /// 5. The engine's replay harness supplies these values, which is also what
    /// makes replay deterministic.
    #[inline]
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    /// Milliseconds since the Unix epoch on our clock.
    #[inline]
    pub const fn millis(self) -> i64 {
        self.0
    }

    /// Age of something observed at `earlier`, measured on our clock.
    ///
    /// Negative results are possible and are not clamped: they mean the
    /// observation is stamped in our future, which is a bug worth surfacing
    /// rather than hiding behind a `max(0)`.
    #[inline]
    pub const fn since(self, earlier: LocalTs) -> Millis {
        Millis(self.0.saturating_sub(earlier.0))
    }

    /// Shift forward by a duration.
    #[inline]
    pub const fn add_millis(self, d: Millis) -> Self {
        Self(self.0.saturating_add(d.0))
    }

    /// Reinterpret our clock as wall-clock time, *assuming zero skew*.
    ///
    /// Needed to answer "how long until the next funding settlement", where the
    /// deadline is venue wall time and the only clock we have is ours. The
    /// assumption is unsound by exactly the skew that
    /// [`WallTs::apparent_skew_from`] estimates, so callers near an interval
    /// boundary must treat the answer as approximate — see
    /// `QualityFlags::CLOCK_SKEW`.
    #[inline]
    pub const fn assume_no_skew(self) -> WallTs {
        WallTs(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_signed_and_directional() {
        let t0 = LocalTs::from_millis(1_700_000_000_000);
        let t1 = LocalTs::from_millis(1_700_000_000_250);
        assert_eq!(t1.since(t0), Millis::from_millis(250));
        // Backwards is negative, not clamped: a future-stamped quote is a bug.
        assert_eq!(t0.since(t1), Millis::from_millis(-250));
        assert!(t0.since(t1).is_negative());
        assert_eq!(t0.since(t1).abs(), Millis::from_millis(250));
    }

    #[test]
    fn seconds_helper_matches_millis() {
        assert_eq!(Millis::from_secs(8 * 3600), Millis::from_millis(28_800_000));
        assert_eq!(Millis::from_secs(1), Millis::from_millis(1000));
    }

    #[test]
    fn apparent_skew_has_a_sign_we_can_reason_about() {
        let local = LocalTs::from_millis(1_700_000_000_000);
        // Venue claims a time 40 ms ahead of our clock.
        let ahead = WallTs::from_millis(1_700_000_000_040);
        assert_eq!(ahead.apparent_skew_from(local), Millis::from_millis(40));
        // Venue claims a time 40 ms behind ours.
        let behind = WallTs::from_millis(1_699_999_999_960);
        assert_eq!(behind.apparent_skew_from(local), Millis::from_millis(-40));
    }

    #[test]
    fn arithmetic_saturates_instead_of_panicking() {
        // Rule 4: no panics in library code, including on absurd inputs.
        let far = WallTs::from_millis(i64::MAX);
        assert_eq!(far.add_millis(Millis::from_millis(1)).millis(), i64::MAX);
        assert_eq!(far.since(WallTs::from_millis(i64::MIN)).millis(), i64::MAX);
        assert_eq!(Millis::from_millis(i64::MIN).abs().millis(), i64::MAX);
        assert_eq!(Millis::from_secs(i64::MAX).millis(), i64::MAX);
    }

    #[test]
    fn add_then_since_round_trips() {
        let t = LocalTs::from_millis(1_700_000_000_000);
        let d = Millis::from_millis(28_800_000);
        assert_eq!(t.add_millis(d).since(t), d);
    }

    #[test]
    fn ordering_works_within_a_clock() {
        assert!(LocalTs::from_millis(1) < LocalTs::from_millis(2));
        assert!(WallTs::from_millis(1) < WallTs::from_millis(2));
        assert_eq!(LocalTs::EPOCH.millis(), 0);
        assert_eq!(WallTs::EPOCH.millis(), 0);
    }

    #[test]
    fn assume_no_skew_is_the_identity_on_millis() {
        let l = LocalTs::from_millis(1_700_000_000_123);
        assert_eq!(l.assume_no_skew().millis(), l.millis());
    }

    #[test]
    fn display_is_unit_tagged() {
        assert_eq!(Millis::from_millis(250).to_string(), "250ms");
        assert_eq!(Millis::ZERO.to_string(), "0ms");
    }
}
