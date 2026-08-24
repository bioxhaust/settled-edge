//! Clock divergence between an exchange's timestamp and our receive clock.
//!
//! # What one sample can and cannot tell you
//!
//! `WallTs::apparent_skew_from` returns `exchange_ts - local_ts`. That single
//! number is the sum of at least three things:
//!
//! - the true offset between the venue's clock and ours,
//! - one-way network latency from the venue to us,
//! - however long the venue held the message between stamping and sending.
//!
//! **One sample cannot separate them.** There is no arithmetic in this module
//! that recovers the clock offset alone, and we do not pretend otherwise: the
//! word "apparent" in the foundation's method name is load-bearing. A 200 ms
//! reading is equally consistent with a perfectly synchronised venue 200 ms away
//! and a co-located venue whose clock is 200 ms behind. Distinguishing them
//! needs many samples over time (the minimum observed lag is the best available
//! bound on latency), which is a job for whoever owns feed statistics, not for a
//! single-shot detector.
//!
//! So this module classifies the *reading*, not the clock, and the verdict names
//! say so.
//!
//! # Why the exchange-ahead direction is the dangerous one
//!
//! A message cannot be received before it was stamped. If `exchange_ts` is later
//! than our receive time, at least one clock is wrong, and since the reading
//! already absorbs a positive latency term, the error has to be at least as large
//! as it appears — our clock is behind the venue's by the reported amount *plus*
//! the transit time.
//!
//! That direction is dangerous because of what it does to [`crate::staleness`].
//! Ages are computed as `now - observed_at` on our clock. If our clock runs
//! slow, `now` is too small, so **every age we compute is understated** and
//! everything looks fresher than it is. The failure is silent: no staleness flag
//! fires, and the data that gets traded is precisely the data we were unable to
//! see was old. The other direction (exchange behind us) inflates ages and costs
//! missed opportunities — annoying, not expensive.
//!
//! For that reason [`ClockVerdict::ExchangeAhead`] has **zero tolerance**: any
//! positive reading gets it. Millisecond quantisation will make this fire
//! occasionally on a healthy feed, and that is the trade we want, because the
//! remedy (resync our host clock, or widen after measuring) is cheap and the
//! alternative is understating every age by an unknown amount.

use arb_types::{LocalTs, Millis, WallTs};

/// How much apparent lag we accept before saying something about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkewLimits {
    /// At or past this lag, report [`ClockVerdict::Lagging`].
    pub tolerable: Millis,
    /// At or past this lag, report [`ClockVerdict::Suspicious`].
    pub suspicious: Millis,
}

impl SkewLimits {
    /// Limits for a venue reached over the public internet, which is every
    /// venue we currently talk to.
    ///
    /// - `tolerable = 250 ms`: round trips from a European or US host to the
    ///   Tokyo/Singapore region where most of these venues run are on the order
    ///   of 100–250 ms, so one-way transit of 50–125 ms is normal, and the venue
    ///   stamps the message before it queues it for the socket. Any reading
    ///   below 250 ms is therefore fully explained by transit and tells us
    ///   nothing at all about the clock. Flagging inside this band would be
    ///   reporting the speed of light as a fault.
    /// - `suspicious = 1500 ms`: six times the tolerable figure. No plausible
    ///   internet path adds 1.5 s of one-way delay to a live websocket, so at
    ///   that point one of two real problems exists — our NTP has drifted, or
    ///   the venue's publisher is queueing behind. Both invalidate age
    ///   arithmetic, and both are worth blocking on.
    ///
    /// `[assumption]` — both numbers are inferred from typical WAN behaviour and
    /// have never been measured from our own hosts to these venues. The first
    /// day of real capture should replace them with per-venue percentiles: the
    /// right `tolerable` is roughly the observed median lag, and the right
    /// `suspicious` is well above the observed p99.
    pub const WAN: SkewLimits = SkewLimits {
        tolerable: Millis::from_millis(250),
        suspicious: Millis::from_millis(1_500),
    };

    /// Limits for a co-located or same-region connection.
    ///
    /// - `tolerable = 25 ms`: with sub-millisecond transit, the latency term
    ///   almost vanishes, so a reading is nearly all clock offset. 25 ms is
    ///   loose enough to absorb host scheduling jitter and venue-side batching
    ///   while still catching a clock that has actually drifted.
    /// - `suspicious = 250 ms`: ten times tolerable, and an order of magnitude
    ///   more than any same-region path can produce.
    ///
    /// `[assumption]` — untested; we are not co-located anywhere. Provided so
    /// the WAN numbers do not get quietly reused in an environment where they
    /// would hide a real 200 ms drift.
    pub const COLOCATED: SkewLimits = SkewLimits {
        tolerable: Millis::from_millis(25),
        suspicious: Millis::from_millis(250),
    };

    /// True when `tolerable <= suspicious`, the only ordering in which the
    /// names mean anything.
    pub fn is_ordered(&self) -> bool {
        self.tolerable.millis() <= self.suspicious.millis()
    }
}

/// What a single exchange-versus-local timestamp reading looks like.
///
/// Each variant carries the magnitude that produced it, because the number is
/// what an operator needs to act; the label alone is not enough to decide
/// whether to resync a host or open a ticket with a venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockVerdict {
    /// Lag within the tolerable band. Consistent with normal transit; says
    /// nothing about the clock either way.
    Aligned,
    /// Exchange timestamp is behind our receive time by more than tolerable but
    /// less than suspicious. Ages computed from our clock are, if anything,
    /// slightly overstated — the safe direction.
    Lagging(Millis),
    /// Lag too large to be transit. Either our clock drifted or the venue's
    /// feed is queueing; both make age arithmetic meaningless.
    Suspicious(Millis),
    /// Exchange timestamp is *later* than our receive time. Physically
    /// impossible without clock error, and it means every age we compute is
    /// understated. See the module docs.
    ExchangeAhead(Millis),
}

impl ClockVerdict {
    /// True when this reading gives us no reason to distrust our age
    /// arithmetic.
    pub const fn is_aligned(self) -> bool {
        matches!(self, ClockVerdict::Aligned)
    }

    /// The signed reading in milliseconds, positive when the exchange clock
    /// reads later than ours.
    pub const fn skew(self) -> Millis {
        match self {
            ClockVerdict::Aligned => Millis::ZERO,
            // Lag: the exchange stamp is behind us, hence negative skew.
            // `saturating_neg` so an absurd magnitude cannot panic here.
            ClockVerdict::Lagging(ms) | ClockVerdict::Suspicious(ms) => {
                Millis::from_millis(ms.millis().saturating_neg())
            }
            ClockVerdict::ExchangeAhead(ms) => ms,
        }
    }
}

impl std::fmt::Display for ClockVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClockVerdict::Aligned => f.write_str("clock aligned"),
            ClockVerdict::Lagging(ms) => write!(f, "exchange stamp {} ms behind receive", ms.millis()),
            ClockVerdict::Suspicious(ms) => {
                write!(f, "exchange stamp {} ms behind receive (implausible)", ms.millis())
            }
            ClockVerdict::ExchangeAhead(ms) => {
                write!(f, "exchange stamp {} ms AHEAD of receive (our clock is behind)", ms.millis())
            }
        }
    }
}

/// Classify one exchange-versus-local timestamp reading.
///
/// Boundaries belong to the worse bucket, as everywhere in this crate: a lag of
/// exactly `tolerable` reads [`ClockVerdict::Lagging`], and exactly
/// `suspicious` reads [`ClockVerdict::Suspicious`].
///
/// Remember what the input is: a sum of clock offset and network latency that
/// cannot be decomposed from one sample. A `Lagging` verdict is not proof of a
/// bad clock, and an `Aligned` verdict is not proof of a good one — it only
/// means the reading was small enough that transit alone explains it.
pub fn assess_skew(exchange: WallTs, local: LocalTs, limits: SkewLimits) -> ClockVerdict {
    let skew = exchange.apparent_skew_from(local).millis();

    if skew > 0 {
        // Zero tolerance in this direction: see module docs. Understated ages
        // fail silently, and that is the failure we cannot afford.
        return ClockVerdict::ExchangeAhead(Millis::from_millis(skew));
    }

    // `saturating_neg` rather than `-skew`: a skew of `i64::MIN` ms is ~292
    // million years and cannot be a real reading, but it must not panic.
    let lag = skew.saturating_neg();

    if lag >= limits.suspicious.millis() {
        ClockVerdict::Suspicious(Millis::from_millis(lag))
    } else if lag >= limits.tolerable.millis() {
        ClockVerdict::Lagging(Millis::from_millis(lag))
    } else {
        ClockVerdict::Aligned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAN: SkewLimits = SkewLimits::WAN;

    fn ex(ms: i64) -> WallTs {
        WallTs::from_millis(ms)
    }
    fn lo(ms: i64) -> LocalTs {
        LocalTs::from_millis(ms)
    }

    #[test]
    fn foundation_skew_sign_convention() {
        // Pins the sign convention the rest of this module depends on.
        // Exchange stamped 1_000, we received at 1_500: the stamp is 500 ms
        // behind our receive clock, so skew = 1000 - 1500 = -500.
        assert_eq!(ex(1_000).apparent_skew_from(lo(1_500)).millis(), -500);
        // And the reverse: stamp 1_500, receive 1_000 -> +500.
        assert_eq!(ex(1_500).apparent_skew_from(lo(1_000)).millis(), 500);
    }

    #[test]
    fn presets_are_ordered() {
        assert!(WAN.is_ordered());
        assert!(SkewLimits::COLOCATED.is_ordered());
    }

    #[test]
    fn zero_reading_is_aligned() {
        assert_eq!(assess_skew(ex(1_000), lo(1_000), WAN), ClockVerdict::Aligned);
    }

    #[test]
    fn lag_one_ms_below_tolerable_is_aligned() {
        // tolerable = 250; lag = 1_249 - 1_000 = 249.
        assert_eq!(assess_skew(ex(1_000), lo(1_249), WAN), ClockVerdict::Aligned);
    }

    #[test]
    fn lag_exactly_on_tolerable_is_lagging() {
        // lag = 1_250 - 1_000 = 250, exactly on the boundary -> worse bucket.
        assert_eq!(
            assess_skew(ex(1_000), lo(1_250), WAN),
            ClockVerdict::Lagging(Millis::from_millis(250))
        );
    }

    #[test]
    fn lag_one_ms_below_suspicious_is_lagging() {
        // suspicious = 1_500; lag = 2_499 - 1_000 = 1_499.
        assert_eq!(
            assess_skew(ex(1_000), lo(2_499), WAN),
            ClockVerdict::Lagging(Millis::from_millis(1_499))
        );
    }

    #[test]
    fn lag_exactly_on_suspicious_is_suspicious() {
        // lag = 2_500 - 1_000 = 1_500.
        assert_eq!(
            assess_skew(ex(1_000), lo(2_500), WAN),
            ClockVerdict::Suspicious(Millis::from_millis(1_500))
        );
    }

    #[test]
    fn huge_lag_is_suspicious() {
        // lag = 61_000 - 1_000 = 60_000.
        assert_eq!(
            assess_skew(ex(1_000), lo(61_000), WAN),
            ClockVerdict::Suspicious(Millis::from_millis(60_000))
        );
    }

    #[test]
    fn one_ms_ahead_gets_its_own_variant() {
        // Stamp 1_001, received 1_000: impossible without clock error.
        let v = assess_skew(ex(1_001), lo(1_000), WAN);
        assert_eq!(v, ClockVerdict::ExchangeAhead(Millis::from_millis(1)));
        assert!(!v.is_aligned());
    }

    #[test]
    fn ahead_ignores_tolerable_band() {
        // 200 ms ahead is well inside `tolerable`, but tolerance only applies
        // to the lag direction. This must not read Aligned or Lagging.
        let v = assess_skew(ex(1_200), lo(1_000), WAN);
        assert_eq!(v, ClockVerdict::ExchangeAhead(Millis::from_millis(200)));
    }

    #[test]
    fn ahead_survives_a_looser_preset() {
        // Even the WAN preset, the loosest we ship, gives no ground here.
        let colo = assess_skew(ex(1_050), lo(1_000), SkewLimits::COLOCATED);
        let wan = assess_skew(ex(1_050), lo(1_000), WAN);
        assert_eq!(colo, ClockVerdict::ExchangeAhead(Millis::from_millis(50)));
        assert_eq!(wan, ClockVerdict::ExchangeAhead(Millis::from_millis(50)));
    }

    #[test]
    fn colocated_is_stricter_than_wan() {
        // lag = 100 ms: transit on a WAN link, a drifted clock in a colo.
        assert_eq!(assess_skew(ex(1_000), lo(1_100), WAN), ClockVerdict::Aligned);
        assert_eq!(
            assess_skew(ex(1_000), lo(1_100), SkewLimits::COLOCATED),
            ClockVerdict::Lagging(Millis::from_millis(100))
        );
    }

    #[test]
    fn skew_accessor_keeps_the_sign() {
        assert_eq!(ClockVerdict::Aligned.skew().millis(), 0);
        assert_eq!(
            ClockVerdict::Lagging(Millis::from_millis(300)).skew().millis(),
            -300
        );
        assert_eq!(
            ClockVerdict::ExchangeAhead(Millis::from_millis(300)).skew().millis(),
            300
        );
    }
}
