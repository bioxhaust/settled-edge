//! Does this venue's depth feed mean what we assumed it means?
//!
//! # The assumption under test
//!
//! Every ladder in this system is built on `OPEN.md` D2: a depth update carries
//! the **absolute size** at a price, and `qty == 0` deletes the level. Nobody has
//! confirmed that against a live socket for any venue. A venue publishing true
//! increments instead would corrupt the ladder with no error, no flag, and no
//! failing test.
//!
//! # Which way it hurts
//!
//! Reading increments as absolute sizes has two effects, and they are not equally
//! bad:
//!
//! - Sizes become "the last increment" rather than "the level", so depth is
//!   **understated**. That is the safe direction: we size too small and miss a
//!   trade.
//! - An increment feed has no use for `qty == 0` and does not send it, so
//!   **levels are never deleted**. Every price the venue ever mentioned stays in
//!   the ladder for ever, the touch becomes prices nobody is offering, and the
//!   spread reads **tighter** than it is.
//!
//! The second is the favourable direction, and ranking selects for it: a
//! permanently-too-tight spread is exactly the shape of a row that sorts to the
//! top of the board. Same failure as `OPEN.md` F1.
//!
//! # What this module is and is not
//!
//! It is the judgement half. `arb_book::semantics` counts; nothing there owns a
//! threshold. Here is where the sample sizes and the severities live, per
//! `decisions/011`. Counters arrive as plain integers, so this crate keeps no
//! dependency on the book crate — the same arrangement as
//! [`crate::sequence::SeqCounters`].
//!
//! It is **not** a proof of correctness. The strongest verdict available is
//! [`DepthSemantics::NoContraryEvidence`], and the name is deliberate: no tell
//! fired, which is a statement about our evidence and not about the venue.
//!
//! # Where the verdict comes from
//!
//! Two decisive tells and two circumstantial ones. The decisive ones need no
//! sample — one occurrence is enough, because each is something an absolute-size
//! feed *cannot* produce. The circumstantial ones are absences, and an absence
//! only means something over a large enough sample, which is what
//! [`DepthSemanticsLimits`] is for.

/// What a book reader counted while maintaining a ladder from this venue's feed.
///
/// Cumulative over whatever window the caller keeps. Every field maps one-to-one
/// onto an accessor of `arb_book::DeltaVocabulary`; `accepted` comes from
/// [`crate::sequence::SeqCounters`] and is used for one purpose only, described
/// on the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DepthEvidence {
    /// Sequenced messages applied to the ladder, from
    /// [`crate::sequence::SeqCounters::accepted`].
    ///
    /// Present so that "nobody wired the counters" can be told apart from "the
    /// feed is young". Both leave every other field at zero; only this one says
    /// how much feed went past while that happened.
    pub accepted: u64,
    /// `(price, qty)` pairs applied, counting each pair once.
    pub level_updates: u64,
    /// Pairs carrying `qty == 0` — the only way an absolute-size feed can say
    /// "this level is gone".
    pub zero_sized: u64,
    /// Pairs carrying `qty == 0` that removed a level the ladder was holding.
    pub zero_sized_deleted: u64,
    /// Pairs refused for carrying a **negative** size.
    pub negative_sized: u64,
    /// Snapshot reconciliations performed at sequence distance zero — the same
    /// book at the same position, where exact agreement is definitional.
    pub audits_at_same_position: u64,
    /// Of those, how many found a difference an absolute-size feed cannot
    /// produce.
    pub audits_contradicting: u64,
}

/// Sample sizes an *absence* has to clear before it counts as evidence.
///
/// Only the circumstantial tells consult these. A negative size or a failed
/// same-position reconciliation is decisive on its first occurrence and is not
/// gated by any sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepthSemanticsLimits {
    /// Level updates required before "this feed has never deleted a level" is
    /// evidence rather than a quiet market.
    pub min_updates: u64,
    /// Zero-sized updates required before "not one of them matched a price we
    /// held" is evidence rather than ordinary retransmission.
    pub min_deletes: u64,
}

impl DepthSemanticsLimits {
    /// Limits for an incremental websocket depth feed on an instrument liquid
    /// enough to arbitrage, which is the only kind we subscribe to.
    ///
    /// - **`min_updates = 20_000`.** The tell is an absence, so the question is
    ///   how much feed has to pass before absence is surprising. The argument
    ///   that does not need a measured delete rate is the touch: on an instrument
    ///   worth trading, the best bid and best ask move, and when a level's whole
    ///   size is taken or cancelled that level is *gone* — an absolute-size feed
    ///   has to say so with a zero. At the ~10 messages/second this crate already
    ///   assumes for [`crate::sequence::SeqHealthLimits::WS_INCREMENTAL`], and a
    ///   few `(price, qty)` pairs per message, 20 000 pairs is on the order of
    ///   five to ten minutes of feed. Ten minutes of a liquid book in which no
    ///   level was ever removed is not a market.
    ///
    ///   Cost of a larger number: an increment feed goes unnoticed for longer.
    ///   Cost of a smaller one: a genuinely quiet instrument at 04:00 gets
    ///   flagged, the flag gets ignored, and the detector is worth nothing. The
    ///   second cost is the one that destroys a detector, so this errs large.
    ///
    /// - **`min_deletes = 100`.** This tell fires when the venue *does* send
    ///   zeros and not one of them ever hit a price we hold. A few misses are
    ///   normal — venues re-send deletes, including for levels past the end of
    ///   our window. A hundred consecutive misses are not: with
    ///   `arb_book::MAX_LEVELS` at 4096 our window is not the explanation, so
    ///   the remaining ones are that our price keys do not match the venue's
    ///   (a scale or rounding defect in the adapter) or that these zeros are not
    ///   deletes at all. 100 is small because the tell is cheap to satisfy on a
    ///   healthy feed: the first delete that lands clears it for ever.
    ///
    /// `[assumption]` — both numbers are reasoned, not measured. Nobody has
    /// counted deletes per thousand updates on any venue socket, because there is
    /// no network to a venue in this workspace. What *is* pinned is the direction
    /// of the error: both tells are absences, so an under-sized limit produces
    /// false alarms and an over-sized one produces silence, and these are set on
    /// the silent side deliberately — see the cost note above. Replace both with
    /// observed distributions from the first recorded session.
    pub const WS_INCREMENTAL: DepthSemanticsLimits = DepthSemanticsLimits {
        min_updates: 20_000,
        min_deletes: 100,
    };
}

/// Which observation produced a non-clean verdict.
///
/// Carried by the verdict so a caller never has to re-derive it from the
/// counters, and so a log line says what to go and look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticsTell {
    /// The feed published a negative size. Decisive: an absolute size cannot be
    /// negative and a signed change can.
    ///
    /// It does not identify *which* misreading happened — a sign defect in the
    /// adapter looks identical — but under either explanation the arithmetic the
    /// ladder performed is not the arithmetic the venue intended.
    NegativeSizePublished,
    /// A snapshot at our own sequence position denied a level we hold, or gave a
    /// different size for one. Decisive: at the same position two exact-decimal
    /// descriptions of one book must be identical.
    SnapshotContradictedTheLadder,
    /// Over a large sample the feed has never once published `qty == 0`.
    /// Circumstantial: an absolute-size feed needs zero to express deletion, and
    /// an increment feed has no use for it.
    NoDeleteEverPublished,
    /// The feed publishes zeros, but not one has ever matched a price the ladder
    /// held. Circumstantial, and it points at the adapter as much as at the
    /// venue: most likely our prices and the venue's do not agree.
    DeletesNeverMatchAHeldPrice,
}

impl SemanticsTell {
    /// Short label for logs and screens.
    pub const fn tag(self) -> &'static str {
        match self {
            SemanticsTell::NegativeSizePublished => "negative size published",
            SemanticsTell::SnapshotContradictedTheLadder => "snapshot contradicted the ladder",
            SemanticsTell::NoDeleteEverPublished => "no delete ever published",
            SemanticsTell::DeletesNeverMatchAHeldPrice => "deletes never match a held price",
        }
    }
}

impl std::fmt::Display for SemanticsTell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

/// Why no verdict was reached.
///
/// Two states rather than one because they need different actions from whoever
/// reads them, and they are indistinguishable from the counters alone unless
/// [`DepthEvidence::accepted`] is supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WhyUnevaluated {
    /// Not enough feed has passed for an absence to mean anything. Expected on
    /// every fresh connection; it resolves itself by waiting.
    NoSampleYet,
    /// A great deal of feed has passed and no level update was ever recorded.
    ///
    /// A depth feed that applied thousands of messages carrying no `(price, qty)`
    /// pairs at all is not a depth feed, so the counters are almost certainly not
    /// plumbed in. Waiting will not fix this one, and it is the state the whole
    /// workspace is in until an adapter starts populating [`DepthEvidence`].
    CountersNotWired,
}

impl WhyUnevaluated {
    /// Short label for logs and screens.
    pub const fn tag(self) -> &'static str {
        match self {
            WhyUnevaluated::NoSampleYet => "no sample yet",
            WhyUnevaluated::CountersNotWired => "counters not wired",
        }
    }
}

/// What we can say about this venue's depth semantics.
///
/// Not `Ord`. A derived ordering would compare the payloads, and "is this tell
/// greater than that tell" has no meaning; [`Self::worse_of`] does the one
/// comparison that does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepthSemantics {
    /// Enough evidence to have caught the known tells, and none fired.
    ///
    /// The weakest useful statement, and the strongest one available. It does not
    /// say the semantics are absolute; it says we looked with the instruments we
    /// have and they registered nothing.
    NoContraryEvidence,
    /// No verdict. **Worse than [`Self::NoContraryEvidence`]**, per this crate's
    /// rule that "we did not look" is not "it was fine".
    Unevaluated(WhyUnevaluated),
    /// Circumstantial evidence that the semantics are not what we assumed.
    /// Costs points; does not block on its own.
    Suspect(SemanticsTell),
    /// The feed did something an absolute-size feed cannot do. The ladder's
    /// levels and sizes are not the venue's, so nothing derived from them is
    /// worth anything. Fatal in [`crate::trust::assess_venue`].
    Contradicted(SemanticsTell),
}

impl DepthSemantics {
    /// Rank, worst highest. Used only by [`Self::worse_of`].
    const fn severity(self) -> u8 {
        match self {
            DepthSemantics::NoContraryEvidence => 0,
            DepthSemantics::Unevaluated(_) => 1,
            DepthSemantics::Suspect(_) => 2,
            DepthSemantics::Contradicted(_) => 3,
        }
    }

    /// The worse of two verdicts. Ties keep `self`, so folding a sequence never
    /// upgrades anything.
    pub const fn worse_of(self, other: Self) -> Self {
        if self.severity() >= other.severity() {
            self
        } else {
            other
        }
    }

    /// The observation behind a [`Self::Suspect`] or [`Self::Contradicted`].
    pub const fn tell(self) -> Option<SemanticsTell> {
        match self {
            DepthSemantics::Suspect(t) | DepthSemantics::Contradicted(t) => Some(t),
            DepthSemantics::NoContraryEvidence | DepthSemantics::Unevaluated(_) => None,
        }
    }

    /// True when the ladder cannot be used at all.
    pub const fn is_fatal(self) -> bool {
        matches!(self, DepthSemantics::Contradicted(_))
    }

    /// True when there is circumstantial evidence against the semantics.
    pub const fn is_suspect(self) -> bool {
        matches!(self, DepthSemantics::Suspect(_))
    }

    /// Short label for logs and screens.
    pub const fn tag(self) -> &'static str {
        match self {
            DepthSemantics::NoContraryEvidence => "no contrary evidence",
            DepthSemantics::Unevaluated(w) => w.tag(),
            DepthSemantics::Suspect(t) | DepthSemantics::Contradicted(t) => t.tag(),
        }
    }
}

impl std::fmt::Display for DepthSemantics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DepthSemantics::NoContraryEvidence => f.write_str("depth semantics: no contrary evidence"),
            DepthSemantics::Unevaluated(w) => write!(f, "depth semantics not evaluated: {}", w.tag()),
            DepthSemantics::Suspect(t) => write!(f, "depth semantics suspect: {t}"),
            DepthSemantics::Contradicted(t) => write!(f, "depth semantics CONTRADICTED: {t}"),
        }
    }
}

/// Judge one venue's depth semantics from what its feed has done.
///
/// Tests run decisive-first, and the first hit returns, so the reported tell is
/// always the strongest available rather than whichever happened to be checked
/// last. Every ordering below is deliberate:
///
/// 1. A negative size, then a failed same-position reconciliation. Both are
///    proofs, so neither waits for a sample.
/// 2. The two absences, each gated on its own sample.
/// 3. A clean same-position reconciliation counts as looking, however short the
///    stream: it compared two independent descriptions of one book and they
///    matched exactly.
/// 4. Otherwise a large update sample counts as looking.
/// 5. Otherwise nothing was established, and that is said rather than implied.
pub fn assess_depth_semantics(
    ev: &DepthEvidence,
    limits: DepthSemanticsLimits,
) -> DepthSemantics {
    // ---- decisive: things an absolute-size feed cannot do ------------------
    if ev.negative_sized > 0 {
        return DepthSemantics::Contradicted(SemanticsTell::NegativeSizePublished);
    }
    if ev.audits_contradicting > 0 {
        return DepthSemantics::Contradicted(SemanticsTell::SnapshotContradictedTheLadder);
    }

    // ---- circumstantial: absences, each over its own sample ----------------
    if ev.zero_sized >= limits.min_deletes && ev.zero_sized_deleted == 0 {
        return DepthSemantics::Suspect(SemanticsTell::DeletesNeverMatchAHeldPrice);
    }
    if ev.level_updates >= limits.min_updates && ev.zero_sized == 0 {
        return DepthSemantics::Suspect(SemanticsTell::NoDeleteEverPublished);
    }

    // ---- did we look at all? ----------------------------------------------
    // A reconciliation at distance zero is the strongest instrument here, and one
    // clean pass of it is worth more than any amount of stream statistics: it
    // compared two independent exact-decimal descriptions of one book and found
    // them identical.
    if ev.audits_at_same_position > 0 {
        return DepthSemantics::NoContraryEvidence;
    }
    if ev.level_updates >= limits.min_updates {
        return DepthSemantics::NoContraryEvidence;
    }
    // Nothing established. Which flavour of nothing is worth distinguishing: a
    // feed that has applied a great many messages without recording a single
    // level update is not a quiet feed, it is an unplumbed counter.
    if ev.accepted >= limits.min_updates && ev.level_updates == 0 {
        return DepthSemantics::Unevaluated(WhyUnevaluated::CountersNotWired);
    }
    DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet)
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: DepthSemanticsLimits = DepthSemanticsLimits::WS_INCREMENTAL;

    /// A long, healthy sample: plenty of updates, deletes present and landing.
    fn healthy() -> DepthEvidence {
        DepthEvidence {
            accepted: 10_000,
            level_updates: 50_000,
            zero_sized: 4_000,
            zero_sized_deleted: 3_900,
            negative_sized: 0,
            audits_at_same_position: 0,
            audits_contradicting: 0,
        }
    }

    #[test]
    fn a_long_healthy_sample_finds_no_contrary_evidence() {
        assert_eq!(
            assess_depth_semantics(&healthy(), L),
            DepthSemantics::NoContraryEvidence
        );
    }

    #[test]
    fn an_empty_evidence_struct_is_not_a_clean_bill_of_health() {
        // The single most important assertion in this module. A zeroed struct is
        // what a caller that has wired nothing produces, and it must never read
        // as "we checked and it was fine".
        let v = assess_depth_semantics(&DepthEvidence::default(), L);
        assert_eq!(v, DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet));
        assert_ne!(v, DepthSemantics::NoContraryEvidence);
        assert_eq!(
            v.worse_of(DepthSemantics::NoContraryEvidence),
            v,
            "unevaluated must be the worse of the two"
        );
    }

    #[test]
    fn a_busy_feed_with_no_level_updates_is_reported_as_unwired() {
        // Thousands of messages applied and not one (price, qty) pair recorded.
        // A depth feed cannot do that; a caller that forgot the counters can.
        let ev = DepthEvidence {
            accepted: L.min_updates,
            ..DepthEvidence::default()
        };
        assert_eq!(
            assess_depth_semantics(&ev, L),
            DepthSemantics::Unevaluated(WhyUnevaluated::CountersNotWired)
        );
        // One message short of the bar, it is still just a young feed.
        let young = DepthEvidence {
            accepted: L.min_updates - 1,
            ..DepthEvidence::default()
        };
        assert_eq!(
            assess_depth_semantics(&young, L),
            DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet)
        );
    }

    #[test]
    fn one_negative_size_is_decisive_and_needs_no_sample() {
        let ev = DepthEvidence {
            negative_sized: 1,
            ..DepthEvidence::default()
        };
        let v = assess_depth_semantics(&ev, L);
        assert_eq!(
            v,
            DepthSemantics::Contradicted(SemanticsTell::NegativeSizePublished)
        );
        assert!(v.is_fatal());
        assert_eq!(v.tell(), Some(SemanticsTell::NegativeSizePublished));
    }

    #[test]
    fn one_failed_same_position_reconciliation_is_decisive() {
        let ev = DepthEvidence {
            audits_at_same_position: 1,
            audits_contradicting: 1,
            ..DepthEvidence::default()
        };
        assert_eq!(
            assess_depth_semantics(&ev, L),
            DepthSemantics::Contradicted(SemanticsTell::SnapshotContradictedTheLadder)
        );
    }

    #[test]
    fn a_negative_size_outranks_every_other_tell() {
        // All four tells available at once. The reported one must be the proof,
        // not whichever test happened to run last.
        let ev = DepthEvidence {
            accepted: 100_000,
            level_updates: 100_000,
            zero_sized: 500,
            zero_sized_deleted: 0,
            negative_sized: 1,
            audits_at_same_position: 5,
            audits_contradicting: 5,
        };
        assert_eq!(
            assess_depth_semantics(&ev, L),
            DepthSemantics::Contradicted(SemanticsTell::NegativeSizePublished)
        );
    }

    #[test]
    fn a_clean_reconciliation_does_not_excuse_a_contradicting_one() {
        // Nine passes and one failure. Under absolute-size semantics the failure
        // is impossible, so it is not outvoted — a ratio would be exactly the
        // wrong instrument here.
        let ev = DepthEvidence {
            audits_at_same_position: 10,
            audits_contradicting: 1,
            ..healthy()
        };
        assert!(assess_depth_semantics(&ev, L).is_fatal());
    }

    #[test]
    fn a_long_sample_with_no_delete_at_all_is_suspect() {
        let ev = DepthEvidence {
            accepted: 20_000,
            level_updates: L.min_updates,
            zero_sized: 0,
            zero_sized_deleted: 0,
            ..DepthEvidence::default()
        };
        let v = assess_depth_semantics(&ev, L);
        assert_eq!(
            v,
            DepthSemantics::Suspect(SemanticsTell::NoDeleteEverPublished)
        );
        assert!(v.is_suspect());
        assert!(!v.is_fatal(), "circumstantial evidence must not block alone");
    }

    #[test]
    fn a_short_sample_with_no_delete_is_unevaluated_not_suspect() {
        // The false alarm this sample floor exists to prevent: a quiet
        // instrument, or a feed that connected four seconds ago, has published no
        // deletes for entirely innocent reasons.
        let ev = DepthEvidence {
            accepted: 100,
            level_updates: L.min_updates - 1,
            ..DepthEvidence::default()
        };
        assert_eq!(
            assess_depth_semantics(&ev, L),
            DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet)
        );
    }

    #[test]
    fn the_sample_boundary_belongs_to_the_worse_bucket() {
        // Exactly at min_updates with no deletes is Suspect, not Unevaluated:
        // this crate's rule 1, boundaries land on the worse side.
        let at = DepthEvidence {
            level_updates: L.min_updates,
            ..DepthEvidence::default()
        };
        assert!(assess_depth_semantics(&at, L).is_suspect());
        // And exactly at min_deletes with none landing is Suspect too.
        let deletes = DepthEvidence {
            level_updates: 500,
            zero_sized: L.min_deletes,
            zero_sized_deleted: 0,
            ..DepthEvidence::default()
        };
        assert_eq!(
            assess_depth_semantics(&deletes, L),
            DepthSemantics::Suspect(SemanticsTell::DeletesNeverMatchAHeldPrice)
        );
    }

    #[test]
    fn one_delete_that_lands_clears_the_never_matched_tell_for_ever() {
        // The tell must be cheap to satisfy on a healthy feed, or it becomes
        // noise that gets ignored.
        let ev = DepthEvidence {
            level_updates: 500,
            zero_sized: 10_000,
            zero_sized_deleted: 1,
            ..DepthEvidence::default()
        };
        assert_eq!(
            assess_depth_semantics(&ev, L),
            DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet),
            "no tell fires, and a 500-update sample still establishes nothing"
        );
    }

    #[test]
    fn deletes_that_never_land_outrank_the_absence_of_deletes() {
        // Both absences cannot fire together — zero_sized cannot be both 0 and
        // >= min_deletes — but the ordering is asserted so a later edit that
        // loosens one of them cannot silently swap which tell is reported.
        let ev = DepthEvidence {
            level_updates: 100_000,
            zero_sized: 1_000,
            zero_sized_deleted: 0,
            ..DepthEvidence::default()
        };
        assert_eq!(
            assess_depth_semantics(&ev, L),
            DepthSemantics::Suspect(SemanticsTell::DeletesNeverMatchAHeldPrice)
        );
    }

    #[test]
    fn one_clean_reconciliation_beats_a_short_stream_sample() {
        // A single same-position comparison found two independent descriptions of
        // one book identical. That is real evidence, and it does not need ten
        // minutes of feed behind it.
        let ev = DepthEvidence {
            accepted: 3,
            level_updates: 12,
            zero_sized: 1,
            zero_sized_deleted: 1,
            negative_sized: 0,
            audits_at_same_position: 1,
            audits_contradicting: 0,
        };
        assert_eq!(
            assess_depth_semantics(&ev, L),
            DepthSemantics::NoContraryEvidence
        );
    }

    #[test]
    fn worse_of_orders_the_four_verdicts_and_never_upgrades() {
        let clean = DepthSemantics::NoContraryEvidence;
        let unev = DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet);
        let susp = DepthSemantics::Suspect(SemanticsTell::NoDeleteEverPublished);
        let bad = DepthSemantics::Contradicted(SemanticsTell::NegativeSizePublished);
        assert_eq!(clean.worse_of(unev), unev);
        assert_eq!(unev.worse_of(susp), susp);
        assert_eq!(susp.worse_of(bad), bad);
        assert_eq!(bad.worse_of(clean), bad);
        // Folding a whole set never lands better than the worst member.
        let folded = [clean, unev, susp, bad]
            .into_iter()
            .fold(clean, DepthSemantics::worse_of);
        assert_eq!(folded, bad);
    }

    #[test]
    fn every_verdict_says_something_different_out_loud() {
        let all = [
            DepthSemantics::NoContraryEvidence,
            DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet),
            DepthSemantics::Unevaluated(WhyUnevaluated::CountersNotWired),
            DepthSemantics::Suspect(SemanticsTell::NoDeleteEverPublished),
            DepthSemantics::Contradicted(SemanticsTell::NegativeSizePublished),
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a.to_string(), b.to_string());
            }
        }
        // The fatal case has to be unmissable in a log.
        assert!(all[4].to_string().contains("CONTRADICTED"));
    }
}
