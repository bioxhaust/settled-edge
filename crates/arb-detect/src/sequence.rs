//! Sequence health: did we actually see every update that built this ladder?
//!
//! Counters arrive as plain integers. This module deliberately knows nothing
//! about how a book is maintained — no dependency on the book crate — so the
//! two can change independently. Whoever applies the updates counts what
//! happened; this module decides what the counts mean.
//!
//! The distinction that matters here is between *defect count* and *defect
//! rate*. One gap in ten accepted messages and one gap in ten thousand are the
//! same integer and completely different situations: the first is a feed that
//! does not work, the second is a feed that hiccupped once during a normal
//! session. Every threshold below is therefore a ratio, and the ratios are
//! expressed as denominators (`1 gap per N accepted`) because that is the form
//! an operator can reason about out loud.

/// Ratio thresholds for feed health, each as "one defect per N accepted".
///
/// A larger denominator is a stricter requirement: `gap_healthy_denom = 10_000`
/// means the feed must do better than one gap per ten thousand messages to be
/// called healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqHealthLimits {
    /// Accepted messages required before any ratio is believable.
    pub min_sample: u64,
    /// Gaps: better than one per this many accepted to be Healthy.
    pub gap_healthy_denom: u64,
    /// Gaps: at or worse than one per this many accepted is Unusable.
    pub gap_unusable_denom: u64,
    /// Duplicates: better than one per this many accepted to be Healthy.
    /// Duplicates never reach Unusable — see [`assess_sequence`].
    pub duplicate_healthy_denom: u64,
    /// Out-of-order arrivals: better than one per this many to be Healthy.
    pub out_of_order_healthy_denom: u64,
    /// Out-of-order arrivals: at or worse than one per this many is Unusable.
    pub out_of_order_unusable_denom: u64,
    /// Resets tolerated before the publisher counts as unstable rather than
    /// merely restarted.
    pub reset_degraded_max: u64,
}

impl SeqHealthLimits {
    /// Limits for an incremental websocket depth feed, which is how we consume
    /// every venue.
    ///
    /// Each number, and what happens on either side of it:
    ///
    /// - **`min_sample = 200`.** A ratio over 10 accepted messages is not a
    ///   ratio, it is noise: one gap out of ten reads as a catastrophic 10%
    ///   defect rate when the real situation is "we connected two seconds ago".
    ///   At a typical 10 messages/second, 200 accepted is about twenty seconds
    ///   of feed — long enough for one bad reconnect to show up. Below this
    ///   sample we report Degraded even with zero defects, because a stream
    ///   that has not yet demonstrated it can hold sequence has not earned a
    ///   bet. Cost of that choice: roughly twenty seconds of missed
    ///   opportunity after every reconnect. Cost of the opposite choice:
    ///   trading the first ladder off a feed we have no evidence about.
    ///   Raising this number delays trading after reconnect; lowering it lets
    ///   small-sample noise decide.
    /// - **`gap_healthy_denom = 10_000`.** A gap means a ladder we never saw:
    ///   the levels are reconstructed from a resync, not observed. At better
    ///   than one in ten thousand, on a 10 msg/s feed, that is one gap every
    ///   ~17 minutes, each costing a few hundred milliseconds of resync. That
    ///   is an availability cost, not a correctness one, so it does not block.
    ///   Worse than this and reconstructed levels stop being rare enough to
    ///   ignore.
    /// - **`gap_unusable_denom = 100`.** At one gap per hundred messages the
    ///   same feed gaps roughly every ten seconds, which means we are
    ///   permanently inside or next to a resync window and the ladder we price
    ///   against is a guess most of the time. There is no size of edge that
    ///   survives pricing against a guessed ladder, so this blocks outright.
    /// - **`duplicate_healthy_denom = 100`.** A duplicate carries no new
    ///   information and, if updates are applied strictly-increasing-only,
    ///   cannot corrupt the ladder. It matters only as a symptom — usually a
    ///   feed multiplexer delivering the same frame twice. Hence the loose
    ///   threshold and hence duplicates are capped at Degraded and can never
    ///   make a feed Unusable on their own.
    /// - **`out_of_order_healthy_denom = 1_000`.** A late-arriving earlier
    ///   message is harmless if we discard it and corrupting if we apply it.
    ///   Since discarding is our own code's job, a few of these are tolerable —
    ///   ten times stricter than duplicates because they indicate real reorder
    ///   on the path, not just repetition.
    /// - **`out_of_order_unusable_denom = 10`.** One in ten messages arriving
    ///   out of order is not a market feed, it is a broken transport. Nothing
    ///   downstream can be trusted.
    /// - **`reset_degraded_max = 2`.** A reset means the venue restarted its
    ///   numbering and every ladder built before it was discarded. One is a
    ///   deployment on their side. Two is bad luck. Three in a single
    ///   accounting window is a publisher that will do it again while our order
    ///   is live, so the third makes the feed Unusable.
    ///
    /// `[assumption]` — every denominator here is a judgement, not a
    /// measurement. Nobody on this team has yet counted gaps per hour on a real
    /// venue socket. The 10 msg/s figure used to translate ratios into
    /// wall-clock intervals is also assumed. When we have a day of capture,
    /// these should be re-derived from observed distributions; until then they
    /// are set so that the *blocking* thresholds are an order of magnitude
    /// worse than anything a working feed should produce, which keeps false
    /// blocks unlikely while still catching a feed that is genuinely broken.
    pub const WS_INCREMENTAL: SeqHealthLimits = SeqHealthLimits {
        min_sample: 200,
        gap_healthy_denom: 10_000,
        gap_unusable_denom: 100,
        duplicate_healthy_denom: 100,
        out_of_order_healthy_denom: 1_000,
        out_of_order_unusable_denom: 10,
        reset_degraded_max: 2,
    };
}

/// What a feed reader observed while maintaining a ladder.
///
/// All counts are cumulative over whatever window the caller chose to keep.
/// This module does not care about the window length beyond
/// [`SeqHealthLimits::min_sample`]; if the caller resets these counters
/// periodically, ratios describe that period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeqCounters {
    /// Updates applied to the ladder in order.
    pub accepted: u64,
    /// Updates whose sequence had already been applied.
    pub duplicates: u64,
    /// Missing sequence numbers detected — each one is a ladder state we never
    /// saw.
    pub gaps: u64,
    /// Times the venue's numbering restarted from a lower base.
    pub resets: u64,
    /// Updates that arrived after a later-numbered update.
    pub out_of_order: u64,
    /// True while we are waiting for a fresh snapshot to rebuild the ladder.
    pub awaiting_resync: bool,
}

impl SeqCounters {
    /// Counters describing a feed that has applied `accepted` messages with no
    /// defects at all. Convenience for callers and tests.
    pub const fn clean(accepted: u64) -> Self {
        SeqCounters {
            accepted,
            duplicates: 0,
            gaps: 0,
            resets: 0,
            out_of_order: 0,
            awaiting_resync: false,
        }
    }
}

/// How much of the ladder we can claim to have actually observed.
///
/// Ordered worst-last, so `Ord`/`max` give "the worse of two verdicts".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SeqHealth {
    /// Enough sample, few enough defects: the ladder is what the venue sent.
    Healthy,
    /// Some of what we are pricing against was reconstructed rather than
    /// observed, or we have not seen enough of the feed to say.
    Degraded,
    /// The ladder cannot be trusted at all.
    Unusable,
}

impl SeqHealth {
    /// The worse of two verdicts.
    pub const fn worse_of(self, other: Self) -> Self {
        if (self as u8) >= (other as u8) {
            self
        } else {
            other
        }
    }

    /// Short label for logs and screens.
    pub const fn tag(self) -> &'static str {
        match self {
            SeqHealth::Healthy => "healthy",
            SeqHealth::Degraded => "degraded",
            SeqHealth::Unusable => "unusable",
        }
    }
}

impl std::fmt::Display for SeqHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

/// Classify one defect count against `accepted` using two ratio denominators.
///
/// `Unusable` when `count * unusable_denom >= accepted` (the ratio is at or
/// worse than `1/unusable_denom`), `Healthy` when
/// `count * healthy_denom < accepted` (strictly better than
/// `1/healthy_denom`), `Degraded` in between. Both boundaries land on the worse
/// side, which is the rule everywhere in this crate.
///
/// Arithmetic is `u128` and saturating: two `u64` counts multiplied cannot
/// overflow `u128`, and saturation would only ever push a count *up*, i.e.
/// toward the harsher verdict.
fn ratio_health(count: u64, accepted: u64, healthy_denom: u64, unusable_denom: u64) -> SeqHealth {
    if count == 0 {
        return SeqHealth::Healthy;
    }
    let accepted = u128::from(accepted);
    let count = u128::from(count);

    if count.saturating_mul(u128::from(unusable_denom)) >= accepted {
        return SeqHealth::Unusable;
    }
    if count.saturating_mul(u128::from(healthy_denom)) < accepted {
        SeqHealth::Healthy
    } else {
        SeqHealth::Degraded
    }
}

/// Combined sequence verdict: the worst of every component.
///
/// `awaiting_resync` is [`SeqHealth::Unusable`], full stop. There is no ratio
/// that makes an unknown ladder tradeable — while we are waiting for a
/// snapshot, the levels in memory are the ones from before the break, and the
/// only honest description of them is "wrong by an unknown amount". A perfect
/// gap ratio does not change that; it describes the past, and the resync flag
/// describes now.
///
/// Duplicates are capped at [`SeqHealth::Degraded`]: applied
/// strictly-increasing-only they cannot corrupt anything, so they are a symptom
/// to watch and never a reason to block on their own.
pub fn assess_sequence(c: &SeqCounters, limits: SeqHealthLimits) -> SeqHealth {
    if c.awaiting_resync {
        return SeqHealth::Unusable;
    }

    // Too little of the feed observed to compute a ratio. Not Healthy: absence
    // of evidence is not evidence of health.
    if c.accepted < limits.min_sample {
        return SeqHealth::Degraded;
    }

    let gaps = ratio_health(
        c.gaps,
        c.accepted,
        limits.gap_healthy_denom,
        limits.gap_unusable_denom,
    );

    // Duplicates cannot exceed Degraded, whatever the ratio says.
    let dups = ratio_health(
        c.duplicates,
        c.accepted,
        limits.duplicate_healthy_denom,
        // No unusable band: pass a denominator of 0 and clamp below, so the
        // intent stays visible at the call site rather than hidden in a helper.
        0,
    )
    .worse_of(SeqHealth::Healthy)
    .min(SeqHealth::Degraded);

    let ooo = ratio_health(
        c.out_of_order,
        c.accepted,
        limits.out_of_order_healthy_denom,
        limits.out_of_order_unusable_denom,
    );

    let resets = if c.resets == 0 {
        SeqHealth::Healthy
    } else if c.resets <= limits.reset_degraded_max {
        SeqHealth::Degraded
    } else {
        SeqHealth::Unusable
    };

    gaps.worse_of(dups).worse_of(ooo).worse_of(resets)
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: SeqHealthLimits = SeqHealthLimits::WS_INCREMENTAL;

    #[test]
    fn awaiting_resync_is_unusable_despite_perfect_counters() {
        // A million accepted messages and not one defect: still Unusable,
        // because the ladder in memory is not the venue's ladder right now.
        let c = SeqCounters {
            accepted: 1_000_000,
            awaiting_resync: true,
            ..SeqCounters::clean(1_000_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Unusable);
    }

    #[test]
    fn clean_feed_with_enough_sample_is_healthy() {
        // 200 accepted (exactly min_sample), zero defects.
        assert_eq!(assess_sequence(&SeqCounters::clean(200), L), SeqHealth::Healthy);
    }

    #[test]
    fn small_sample_is_degraded_even_when_clean() {
        // 199 < min_sample 200. Twenty seconds of feed not yet observed.
        assert_eq!(assess_sequence(&SeqCounters::clean(199), L), SeqHealth::Degraded);
        assert_eq!(assess_sequence(&SeqCounters::clean(0), L), SeqHealth::Degraded);
    }

    #[test]
    fn gap_ratio_one_better_than_healthy_boundary_is_healthy() {
        // 1 gap, 10_001 accepted. 1 * 10_000 = 10_000 < 10_001 -> Healthy.
        let c = SeqCounters {
            gaps: 1,
            ..SeqCounters::clean(10_001)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Healthy);
    }

    #[test]
    fn gap_ratio_exactly_on_healthy_boundary_is_degraded() {
        // 1 gap, 10_000 accepted. 1 * 10_000 = 10_000, not < 10_000, so the
        // boundary lands in the worse bucket.
        let c = SeqCounters {
            gaps: 1,
            ..SeqCounters::clean(10_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Degraded);
    }

    #[test]
    fn gap_ratio_one_better_than_unusable_boundary_is_degraded() {
        // 99 gaps, 10_000 accepted. 99 * 100 = 9_900 < 10_000 -> not Unusable.
        // 99 * 10_000 = 990_000, not < 10_000 -> not Healthy.
        let c = SeqCounters {
            gaps: 99,
            ..SeqCounters::clean(10_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Degraded);
    }

    #[test]
    fn gap_ratio_exactly_on_unusable_boundary_is_unusable() {
        // 100 gaps, 10_000 accepted. 100 * 100 = 10_000 >= 10_000 -> Unusable.
        let c = SeqCounters {
            gaps: 100,
            ..SeqCounters::clean(10_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Unusable);
    }

    #[test]
    fn gap_ratio_far_past_unusable() {
        // 500 gaps in 1_000 accepted: 500 * 100 = 50_000 >= 1_000.
        let c = SeqCounters {
            gaps: 500,
            ..SeqCounters::clean(1_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Unusable);
    }

    #[test]
    fn duplicates_never_reach_unusable() {
        // 900 duplicates in 1_000 accepted. As a gap ratio this would be
        // catastrophic; as duplicates it is capped at Degraded because
        // re-applying an already-applied sequence changes nothing.
        let c = SeqCounters {
            duplicates: 900,
            ..SeqCounters::clean(1_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Degraded);
    }

    #[test]
    fn duplicate_ratio_boundary() {
        // 1 duplicate, 101 accepted -> 1 * 100 = 100 < 101 -> Healthy ratio,
        // but 101 < min_sample 200 so the sample rule dominates: Degraded.
        let small = SeqCounters {
            duplicates: 1,
            ..SeqCounters::clean(101)
        };
        assert_eq!(assess_sequence(&small, L), SeqHealth::Degraded);

        // 1 duplicate, 1_000 accepted -> 100 < 1_000 -> Healthy.
        let big = SeqCounters {
            duplicates: 1,
            ..SeqCounters::clean(1_000)
        };
        assert_eq!(assess_sequence(&big, L), SeqHealth::Healthy);
    }

    #[test]
    fn out_of_order_unusable_boundary() {
        // 100 out-of-order in 1_000 accepted: 100 * 10 = 1_000 >= 1_000.
        let c = SeqCounters {
            out_of_order: 100,
            ..SeqCounters::clean(1_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Unusable);

        // 99 * 10 = 990 < 1_000 -> Degraded, not Unusable.
        let c = SeqCounters {
            out_of_order: 99,
            ..SeqCounters::clean(1_000)
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Degraded);
    }

    #[test]
    fn resets_step_from_degraded_to_unusable() {
        let two = SeqCounters {
            resets: 2,
            ..SeqCounters::clean(100_000)
        };
        let three = SeqCounters {
            resets: 3,
            ..SeqCounters::clean(100_000)
        };
        assert_eq!(assess_sequence(&two, L), SeqHealth::Degraded);
        assert_eq!(assess_sequence(&three, L), SeqHealth::Unusable);
    }

    #[test]
    fn worst_component_wins() {
        // Healthy gap ratio, healthy duplicates, but three resets.
        let c = SeqCounters {
            accepted: 1_000_000,
            duplicates: 10,
            gaps: 1,
            resets: 3,
            out_of_order: 0,
            awaiting_resync: false,
        };
        assert_eq!(assess_sequence(&c, L), SeqHealth::Unusable);
    }

    #[test]
    fn worse_of_picks_the_worse() {
        assert_eq!(
            SeqHealth::Healthy.worse_of(SeqHealth::Degraded),
            SeqHealth::Degraded
        );
        assert_eq!(
            SeqHealth::Unusable.worse_of(SeqHealth::Healthy),
            SeqHealth::Unusable
        );
        assert!(SeqHealth::Unusable > SeqHealth::Degraded);
    }
}
