//! Evidence about whether a depth update really is an absolute size.
//!
//! # The assumption this module exists to make measurable
//!
//! Every `(price, qty)` pair in a [`crate::Delta`] is applied as **the new size
//! at that price**, and `qty == 0` means "this level is gone". That is `OPEN.md`
//! D2, and it is an `[assumption]`, not a fact: no socket has been read in this
//! workspace to confirm it, because there is no network to a venue here (charter
//! 1a).
//!
//! A venue that publishes true *increments* — "add 0.4 to whatever stands at
//! this price" — hands us numbers our code accepts without a single complaint.
//! No error, no flag, no failing test. The failure mode of D2 is silence.
//!
//! # Which direction the silence errs in
//!
//! Worth stating precisely, because charter 0 ranks errors by direction and not
//! by size. Reading increments as absolute sizes does two things:
//!
//! 1. Sizes become "the last increment" instead of "the level", so depth is
//!    understated. On its own that is the *unfavourable* direction — we would
//!    size too small and lose the trade, not the money.
//! 2. **Levels are never deleted.** An increment feed has no use for `qty == 0`
//!    and does not send it, so every price the venue ever mentioned stays in our
//!    ladder for ever. Our best bid and best ask become prices nobody is
//!    offering, and the spread we compute is *tighter* than the real one.
//!
//! Point 2 is the favourable direction, and it is the one ranking selects for. A
//! scanner fed a permanently-too-tight spread reports arbitrage that does not
//! exist and ranks it first. Same shape as `OPEN.md` F1. That is why these
//! counters exist.
//!
//! # Two independent sources of evidence
//!
//! Separate because they fail separately: the first needs only a stream and a lot
//! of it, the second needs a snapshot and is decisive when it is available.
//!
//! 1. [`DeltaVocabulary`] — what the stream has and has not said. An
//!    absolute-size feed **must** be able to express "this level is gone", and
//!    `qty == 0` is the only way it can. An increment feed cannot use zero for
//!    that: in an increment feed zero means "nothing changed" and is not worth a
//!    message. So a feed that has published a great many level updates and not
//!    one zero has not been speaking the language we assumed.
//! 2. [`SnapshotAudit`] — the ladder we built from deltas, compared against an
//!    authoritative snapshot of the same book. The crate docs call this the only
//!    real defence, and it costs nothing extra: every snapshot already installs
//!    an authoritative description of a book we are holding our own version of.
//!    Comparing the two before the swap is free.
//!
//! # What makes a comparison mean anything
//!
//! Almost nothing about a snapshot-versus-ladder comparison is interpretable by
//! default, and [`AuditScope`] exists to say so out loud. Two ladders describing
//! two different sequence positions differ for entirely healthy reasons, and a
//! detector that counted those differences would fire on every resync and would
//! therefore be switched off — which is worse than not having it.
//!
//! The interpretable quantity is the **sequence distance**: how far the snapshot's
//! id is ahead of our cursor. At distance zero the two descriptions are of the
//! same book at the same position, both parsed as exact decimals at the same
//! scale, with no rounding, timing or truncation between them inside the shared
//! price window — so under absolute-size semantics they must agree *exactly*, and
//! any disagreement at all is evidence. That zero is not a chosen tolerance; it
//! is what "the same position" means.
//!
//! # This module owns no thresholds
//!
//! It counts, and it reports the distance. "How much distance still permits a
//! verdict" and "how large a sample is enough" are human-chosen numbers, and by
//! `decisions/011` those live in `arb-detect`. Feed these counters to
//! `arb_detect::assess_depth_semantics`.

use arb_num::Qty;

use crate::ladder::Ladder;

/// Whether a [`SnapshotAudit`] is capable of saying anything, and against what.
///
/// The first applicable reason wins, ordered most fundamental first: a book with
/// no ladder cannot be compared whatever its ids say, and a ladder already known
/// to be broken cannot be compared however close the ids are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditScope {
    /// First snapshot on this book. There was no ladder to compare against.
    NotInitialized,
    /// The ladder was already known-broken — awaiting resync — when the snapshot
    /// arrived. Every disagreement is explained by the hole we already know
    /// about, so none of them is evidence about semantics.
    LadderUntrusted,
    /// The snapshot carried no sequence id, so the distance is unknowable and no
    /// disagreement count can be interpreted.
    NoSnapshotId,
    /// We hold levels but have sequenced nothing, so our own position is
    /// unknown. Same problem from the other side.
    NoCursor,
    /// The snapshot describes an **earlier** position than our ladder. Comparing
    /// would measure the levels we have legitimately changed since, which is a
    /// statement about our own progress and not about the venue's semantics.
    SnapshotBehind {
        /// Our cursor.
        ours: u64,
        /// The id the snapshot describes itself as of.
        theirs: u64,
    },
    /// Compared. `id_distance` is `snapshot_id - our_cursor`, and it is the
    /// number the disagreement counts must be judged against.
    ///
    /// Zero means same position, where absolute-size semantics require exact
    /// agreement. Above zero, some disagreement is expected — bounded by what
    /// the messages in the interval could have changed — and how much is
    /// tolerable is `arb-detect`'s call, not this crate's.
    Compared {
        /// Sequence ids between our cursor and the snapshot, inclusive of
        /// neither endpoint's content.
        id_distance: u64,
    },
}

impl AuditScope {
    /// True when a comparison was actually performed.
    pub const fn compared(self) -> bool {
        matches!(self, AuditScope::Compared { .. })
    }

    /// True when the comparison was of the same sequence position, which is the
    /// only case in which exact agreement is required.
    pub const fn same_position(self) -> bool {
        matches!(self, AuditScope::Compared { id_distance: 0 })
    }
}

/// One comparison of a delta-built ladder against an authoritative snapshot.
///
/// Counts are per book, both sides folded together: "does this venue mean
/// increments" is a question about the feed, not about a side.
///
/// Fields are private because a caller that can write `agreed` can declare a
/// broken feed healthy — the same reasoning as [`crate::BookState`]'s private
/// `awaiting_resync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotAudit {
    scope: AuditScope,
    compared: u32,
    agreed: u32,
    ours_larger: u32,
    ours_smaller: u32,
    only_ours_inside: u32,
    only_ours_outside: u32,
    only_theirs: u32,
    excess: Qty,
    excess_exact: bool,
}

impl SnapshotAudit {
    /// An audit that was not performed, carrying the reason.
    pub(crate) const fn not_compared(scope: AuditScope) -> Self {
        SnapshotAudit {
            scope,
            compared: 0,
            agreed: 0,
            ours_larger: 0,
            ours_smaller: 0,
            only_ours_inside: 0,
            only_ours_outside: 0,
            only_theirs: 0,
            excess: Qty::ZERO,
            excess_exact: true,
        }
    }

    /// Whether this audit can say anything, and against what distance.
    pub const fn scope(&self) -> AuditScope {
        self.scope
    }

    /// Prices present in both descriptions.
    pub const fn compared(&self) -> u32 {
        self.compared
    }

    /// Prices present in both, with identical sizes.
    pub const fn agreed(&self) -> u32 {
        self.agreed
    }

    /// Shared prices where our size was **larger** than the venue's — we were
    /// showing depth the venue does not have.
    pub const fn ours_larger(&self) -> u32 {
        self.ours_larger
    }

    /// Shared prices where our size was smaller than the venue's.
    pub const fn ours_smaller(&self) -> u32 {
        self.ours_smaller
    }

    /// Prices we hold that the snapshot does not, **within the price range the
    /// snapshot covers**.
    ///
    /// This is the count that matters. Inside its own window the snapshot is
    /// authoritative about which prices exist, so a price we hold and it does not
    /// is a level we failed to delete — the signature of a feed whose zeros we
    /// never received because it never sends any.
    pub const fn only_ours_inside(&self) -> u32 {
        self.only_ours_inside
    }

    /// Prices we hold beyond the snapshot's worst price.
    ///
    /// **Not evidence.** A depth endpoint returns a bounded window; levels past
    /// its edge are levels it declined to mention, not levels it denies. Counted
    /// separately for exactly that reason: folded into
    /// [`Self::only_ours_inside`] it would make every venue with a shallow depth
    /// endpoint read as an increment feed.
    pub const fn only_ours_outside(&self) -> u32 {
        self.only_ours_outside
    }

    /// Prices the snapshot has that we do not.
    ///
    /// Reported but **not used as evidence**: our own ladder truncates at
    /// [`crate::MAX_LEVELS`], so a deeper snapshot legitimately carries prices we
    /// dropped on purpose.
    pub const fn only_theirs(&self) -> u32 {
        self.only_theirs
    }

    /// Signed sum of `ours - theirs` over shared prices. Positive means we were
    /// showing more size than the venue. Valid only when
    /// [`Self::excess_exact`] is true.
    pub const fn excess(&self) -> Qty {
        self.excess
    }

    /// False when the size arithmetic overflowed and [`Self::excess`] was
    /// abandoned.
    ///
    /// A diagnostic that overflows must not fail the thing it is diagnosing:
    /// `apply_snapshot` is the recovery path, and refusing to recover because a
    /// counter would not sum turns a measurement into an outage. The count fields
    /// are unaffected — they are bounded by the ladder length — so an inexact
    /// `excess` costs precision, not the verdict.
    pub const fn excess_exact(&self) -> bool {
        self.excess_exact
    }

    /// Shared prices whose sizes disagreed.
    pub const fn disagreed(&self) -> u32 {
        self.ours_larger.saturating_add(self.ours_smaller)
    }

    /// True when this audit compared the **same sequence position** and still
    /// found a difference an absolute-size feed cannot produce.
    ///
    /// Not a threshold: at distance zero the two descriptions are of one book at
    /// one position and exact agreement is definitional, so the comparison value
    /// is zero because that is what "the same" means. Anything found here is
    /// either increment semantics or a parse defect, and both invalidate the
    /// ladder.
    pub const fn contradicts_at_same_position(&self) -> bool {
        self.scope.same_position()
            && (self.only_ours_inside > 0 || self.ours_larger > 0 || self.ours_smaller > 0)
    }
}

/// What kinds of level update this feed has actually published, cumulatively.
///
/// Monotonic for the life of the book. Saturating rather than wrapping: a counter
/// that wraps to zero reports "this feed has never deleted a level", which is the
/// exact false alarm this module exists to avoid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeltaVocabulary {
    level_updates: u64,
    zero_sized: u64,
    zero_sized_deleted: u64,
    negative_sized: u64,
    overwrote_larger: u64,
    overwrote_smaller: u64,
    overwrote_equal: u64,
    new_price: u64,
    audits_compared: u64,
    audits_at_same_position: u64,
    audits_contradicting: u64,
}

impl DeltaVocabulary {
    /// Level updates applied, counting each `(price, qty)` pair once.
    pub const fn level_updates(&self) -> u64 {
        self.level_updates
    }

    /// Pairs carrying `qty == 0`.
    ///
    /// Under the semantics this crate implements, this is the only way a venue can
    /// say "this level is gone". Zero of these across a large sample is the
    /// cheapest evidence available that the semantics are not what we assumed.
    pub const fn zero_sized(&self) -> u64 {
        self.zero_sized
    }

    /// Pairs carrying `qty == 0` that removed a level we were holding.
    ///
    /// Separate from [`Self::zero_sized`] because a zero for a price we never held
    /// is weaker evidence: venues re-send deletes, and a delete we cannot act on
    /// tells us about the venue's retransmission habits rather than about what its
    /// sizes mean.
    pub const fn zero_sized_deleted(&self) -> u64 {
        self.zero_sized_deleted
    }

    /// Pairs refused for carrying a negative size.
    ///
    /// Above zero is **proof** that the pair was not an absolute size: an absolute
    /// size cannot be negative and a signed change can. It does not prove which
    /// venue field was misread — an adapter sign defect reads identically — but
    /// either way the ladder's arithmetic is not the venue's.
    pub const fn negative_sized(&self) -> u64 {
        self.negative_sized
    }

    /// Updates that raised the size at a price we already held.
    pub const fn overwrote_larger(&self) -> u64 {
        self.overwrote_larger
    }

    /// Updates that lowered the size at a price we already held.
    ///
    /// An absolute-size feed produces these constantly: orders are cancelled and
    /// partially filled at every visible level. A long sample with none of them is
    /// not a market.
    pub const fn overwrote_smaller(&self) -> u64 {
        self.overwrote_smaller
    }

    /// Updates that restated the size we already held.
    pub const fn overwrote_equal(&self) -> u64 {
        self.overwrote_equal
    }

    /// Updates at a price the ladder did not hold.
    pub const fn new_price(&self) -> u64 {
        self.new_price
    }

    /// Updates that landed on a price we already held, in any direction.
    pub const fn overwrites(&self) -> u64 {
        self.overwrote_larger
            .saturating_add(self.overwrote_smaller)
            .saturating_add(self.overwrote_equal)
    }

    /// Snapshot audits in which a comparison was actually performed, at any
    /// sequence distance.
    pub const fn audits_compared(&self) -> u64 {
        self.audits_compared
    }

    /// Audits performed at sequence distance zero — the decisive subset, where
    /// exact agreement is definitional.
    pub const fn audits_at_same_position(&self) -> u64 {
        self.audits_at_same_position
    }

    /// Same-position audits that found a difference. Above zero contradicts
    /// absolute-size semantics.
    pub const fn audits_contradicting(&self) -> u64 {
        self.audits_contradicting
    }

    /// Record one applied `(price, qty)` pair.
    ///
    /// `held` is the size standing at that price **before** the update, or `None`
    /// if the ladder did not hold the price. Call before applying, or the
    /// classification describes the update against its own result.
    pub(crate) fn record_update(&mut self, held: Option<Qty>, incoming: Qty) {
        self.level_updates = self.level_updates.saturating_add(1);
        if incoming.is_zero() {
            self.zero_sized = self.zero_sized.saturating_add(1);
            if held.is_some() {
                self.zero_sized_deleted = self.zero_sized_deleted.saturating_add(1);
            }
            return;
        }
        match held {
            None => self.new_price = self.new_price.saturating_add(1),
            Some(prev) if incoming > prev => {
                self.overwrote_larger = self.overwrote_larger.saturating_add(1);
            }
            Some(prev) if incoming < prev => {
                self.overwrote_smaller = self.overwrote_smaller.saturating_add(1);
            }
            Some(_) => self.overwrote_equal = self.overwrote_equal.saturating_add(1),
        }
    }

    /// Record a pair refused for carrying a negative size.
    pub(crate) fn record_negative(&mut self) {
        self.negative_sized = self.negative_sized.saturating_add(1);
    }

    /// Fold a completed audit into the cumulative counters.
    ///
    /// An audit that was not performed moves nothing: the counters are read by a
    /// detector that cannot see the scope, so a scope-blind counter must only ever
    /// contain scope-checked events.
    pub(crate) fn record_audit(&mut self, audit: &SnapshotAudit) {
        if !audit.scope.compared() {
            return;
        }
        self.audits_compared = self.audits_compared.saturating_add(1);
        if !audit.scope.same_position() {
            return;
        }
        self.audits_at_same_position = self.audits_at_same_position.saturating_add(1);
        if audit.contradicts_at_same_position() {
            self.audits_contradicting = self.audits_contradicting.saturating_add(1);
        }
    }
}

/// Compare a delta-built ladder pair against an authoritative snapshot pair.
///
/// `scope` is decided by the caller, the only place that knows the ids and
/// whether the ladder was vouched for. When no comparison is warranted the
/// comparison is skipped outright rather than computed and ignored: a computed
/// number invites a later reader to use it.
pub(crate) fn audit_snapshot(
    scope: AuditScope,
    ours: (&Ladder, &Ladder),
    theirs: (&Ladder, &Ladder),
) -> SnapshotAudit {
    if !scope.compared() {
        return SnapshotAudit::not_compared(scope);
    }
    let mut audit = SnapshotAudit::not_compared(scope);
    audit_side(&mut audit, ours.0, theirs.0);
    audit_side(&mut audit, ours.1, theirs.1);
    audit
}

/// Fold one side's comparison into `audit`.
fn audit_side(audit: &mut SnapshotAudit, ours: &Ladder, theirs: &Ladder) {
    // The edge of the authoritative window. Our levels beyond it are not denied
    // by the snapshot, merely unmentioned. `levels()` is sorted best-first on
    // both sides, so the last entry is the worst price it covers.
    let their_edge = theirs.levels().last().map(|l| l.price);
    for l in ours.levels() {
        match theirs.qty_at(l.price) {
            Some(t) => {
                audit.compared = audit.compared.saturating_add(1);
                if l.qty == t {
                    audit.agreed = audit.agreed.saturating_add(1);
                } else if l.qty > t {
                    audit.ours_larger = audit.ours_larger.saturating_add(1);
                } else {
                    audit.ours_smaller = audit.ours_smaller.saturating_add(1);
                }
                if audit.excess_exact {
                    match l
                        .qty
                        .checked_sub(t)
                        .and_then(|d| audit.excess.checked_add(d))
                    {
                        Ok(sum) => audit.excess = sum,
                        // See `SnapshotAudit::excess_exact`: counts survive, the
                        // sum is abandoned, the snapshot still installs.
                        Err(_) => {
                            audit.excess_exact = false;
                            audit.excess = Qty::ZERO;
                        }
                    }
                }
            }
            None => {
                let inside = match their_edge {
                    // "Inside the window" is at a price the snapshot covers.
                    // `is_no_worse_than` is the ladder's own ordering, so this
                    // reads correctly for bids and asks without a `Side` match.
                    Some(edge) => ours.is_no_worse_than(l.price, edge),
                    // An empty snapshot side has no window: it covers nothing, so
                    // it denies nothing.
                    None => false,
                };
                if inside {
                    audit.only_ours_inside = audit.only_ours_inside.saturating_add(1);
                } else {
                    audit.only_ours_outside = audit.only_ours_outside.saturating_add(1);
                }
            }
        }
    }
    let theirs_only = theirs
        .levels()
        .iter()
        .filter(|l| ours.qty_at(l.price).is_none())
        .count();
    // Ladders cap at MAX_LEVELS so this conversion cannot lose anything today;
    // `try_from` with a saturating fallback means that if it ever could, the
    // count pins at the maximum rather than wrapping to something reassuring.
    audit.only_theirs = audit
        .only_theirs
        .saturating_add(u32::try_from(theirs_only).unwrap_or(u32::MAX));
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_num::Price;
    use arb_types::{Level, Side};

    fn q(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }

    fn p(s: &str) -> Price {
        Price::parse(s).unwrap()
    }

    fn ladder(side: Side, levels: &[(&str, &str)]) -> Ladder {
        let mut l = Ladder::new(side);
        l.replace_all(
            levels
                .iter()
                .map(|(pr, qy)| Level::new(p(pr), q(qy)).unwrap())
                .collect(),
        )
        .unwrap();
        l
    }

    const SAME: AuditScope = AuditScope::Compared { id_distance: 0 };

    #[test]
    fn a_fresh_vocabulary_claims_nothing() {
        let v = DeltaVocabulary::default();
        assert_eq!(v.level_updates(), 0);
        assert_eq!(v.zero_sized(), 0);
        assert_eq!(v.overwrites(), 0);
        assert_eq!(v.audits_compared(), 0);
        assert_eq!(v.audits_at_same_position(), 0);
    }

    #[test]
    fn a_zero_on_a_held_price_counts_as_a_delete_that_landed() {
        let mut v = DeltaVocabulary::default();
        v.record_update(Some(q("1.5")), Qty::ZERO);
        assert_eq!(v.zero_sized(), 1);
        assert_eq!(v.zero_sized_deleted(), 1);
        assert_eq!(v.level_updates(), 1);
        assert_eq!(v.overwrites(), 0, "a zero is not an overwrite in any direction");
    }

    #[test]
    fn a_zero_on_a_price_we_never_held_is_the_weaker_evidence() {
        let mut v = DeltaVocabulary::default();
        v.record_update(None, Qty::ZERO);
        assert_eq!(v.zero_sized(), 1);
        assert_eq!(
            v.zero_sized_deleted(),
            0,
            "a delete that hit nothing describes retransmission, not size semantics"
        );
    }

    #[test]
    fn overwrite_direction_is_measured_against_the_size_held_before() {
        let mut v = DeltaVocabulary::default();
        v.record_update(Some(q("1.0")), q("2.0"));
        v.record_update(Some(q("2.0")), q("0.5"));
        v.record_update(Some(q("0.5")), q("0.5"));
        v.record_update(None, q("3.0"));
        assert_eq!(v.overwrote_larger(), 1);
        assert_eq!(v.overwrote_smaller(), 1);
        assert_eq!(v.overwrote_equal(), 1);
        assert_eq!(v.new_price(), 1);
        assert_eq!(v.overwrites(), 3, "a new price is not an overwrite");
        assert_eq!(v.level_updates(), 4);
    }

    #[test]
    fn a_negative_size_is_counted_and_is_not_a_level_update() {
        let mut v = DeltaVocabulary::default();
        v.record_negative();
        assert_eq!(v.negative_sized(), 1);
        assert_eq!(
            v.level_updates(),
            0,
            "nothing was applied, so nothing may be counted as applied"
        );
    }

    #[test]
    fn an_uncomparable_scope_produces_no_comparison_at_all() {
        // Ladders that disagree about everything. The scope says the comparison
        // is meaningless, so no count may come out of it.
        let ours = ladder(Side::Buy, &[("100.0", "5.0")]);
        let theirs = ladder(Side::Buy, &[("100.0", "1.0")]);
        for scope in [
            AuditScope::NotInitialized,
            AuditScope::NoSnapshotId,
            AuditScope::NoCursor,
            AuditScope::LadderUntrusted,
            AuditScope::SnapshotBehind { ours: 9, theirs: 7 },
        ] {
            let a = audit_snapshot(scope, (&ours, &ours), (&theirs, &theirs));
            assert_eq!(a.scope(), scope);
            assert_eq!(a.compared(), 0, "{scope:?} must not produce a comparison");
            assert_eq!(a.disagreed(), 0);
            assert!(!a.contradicts_at_same_position());
        }
    }

    #[test]
    fn distance_above_zero_is_compared_but_never_a_contradiction() {
        // The snapshot is four ids ahead, so a difference is ordinary progress.
        // Counted, reported, and explicitly not called a contradiction — how much
        // distance excuses how much difference is arb-detect's threshold.
        let ours = ladder(Side::Buy, &[("100.0", "5.0")]);
        let theirs = ladder(Side::Buy, &[("100.0", "1.0")]);
        let e = Ladder::new(Side::Sell);
        let a = audit_snapshot(
            AuditScope::Compared { id_distance: 4 },
            (&ours, &e),
            (&theirs, &e),
        );
        assert_eq!(a.compared(), 1);
        assert_eq!(a.ours_larger(), 1);
        assert!(!a.contradicts_at_same_position());
        assert!(a.scope().compared());
        assert!(!a.scope().same_position());
    }

    #[test]
    fn identical_ladders_at_the_same_position_agree_exactly() {
        let bids = ladder(Side::Buy, &[("100.0", "1.0"), ("99.0", "2.0")]);
        let asks = ladder(Side::Sell, &[("101.0", "1.5"), ("102.0", "2.5")]);
        let bids2 = ladder(Side::Buy, &[("100.0", "1.0"), ("99.0", "2.0")]);
        let asks2 = ladder(Side::Sell, &[("101.0", "1.5"), ("102.0", "2.5")]);
        let a = audit_snapshot(SAME, (&bids, &asks), (&bids2, &asks2));
        assert_eq!(a.compared(), 4);
        assert_eq!(a.agreed(), 4);
        assert_eq!(a.disagreed(), 0);
        assert_eq!(a.only_ours_inside(), 0);
        assert_eq!(a.only_theirs(), 0);
        assert_eq!(a.excess(), Qty::ZERO);
        assert!(a.excess_exact());
        assert!(!a.contradicts_at_same_position());
    }

    #[test]
    fn a_level_the_venue_denies_inside_its_own_window_contradicts_the_assumption() {
        // We hold 99.5, which sits between two prices the snapshot does list, so
        // the window covers it and the silence is a denial. This is the signature
        // of a feed whose deletes we never received because it sends none.
        let ours = ladder(Side::Buy, &[("100.0", "1.0"), ("99.5", "4.0"), ("99.0", "2.0")]);
        let theirs = ladder(Side::Buy, &[("100.0", "1.0"), ("99.0", "2.0")]);
        let e_ours = Ladder::new(Side::Sell);
        let e_theirs = Ladder::new(Side::Sell);
        let a = audit_snapshot(SAME, (&ours, &e_ours), (&theirs, &e_theirs));
        assert_eq!(a.compared(), 2);
        assert_eq!(a.agreed(), 2);
        assert_eq!(a.only_ours_inside(), 1);
        assert_eq!(a.only_ours_outside(), 0);
        assert!(a.contradicts_at_same_position());
    }

    #[test]
    fn a_level_past_the_snapshots_edge_is_not_evidence_of_anything() {
        // The false-positive class the window guard exists for: a depth endpoint
        // returning fewer levels than we accumulated. If this counted, every
        // venue with a shallow depth window would read as an increment feed.
        let ours = ladder(Side::Buy, &[("100.0", "1.0"), ("99.0", "2.0"), ("98.0", "3.0")]);
        let theirs = ladder(Side::Buy, &[("100.0", "1.0"), ("99.0", "2.0")]);
        let e_ours = Ladder::new(Side::Sell);
        let e_theirs = Ladder::new(Side::Sell);
        let a = audit_snapshot(SAME, (&ours, &e_ours), (&theirs, &e_theirs));
        assert_eq!(a.only_ours_outside(), 1);
        assert_eq!(a.only_ours_inside(), 0);
        assert!(
            !a.contradicts_at_same_position(),
            "a shallow depth window must never read as broken semantics"
        );
    }

    #[test]
    fn the_ask_side_window_edge_is_the_highest_price_not_the_lowest() {
        // Asks ascend, so "inside the window" is at-or-below the last entry. Get
        // the comparator backwards and the two counts swap, which would turn
        // every truncated ask side into a contradiction.
        let ours = ladder(Side::Sell, &[("100.0", "1.0"), ("100.5", "4.0"), ("101.0", "2.0")]);
        let theirs = ladder(Side::Sell, &[("100.0", "1.0"), ("101.0", "2.0")]);
        let e_ours = Ladder::new(Side::Buy);
        let e_theirs = Ladder::new(Side::Buy);
        let a = audit_snapshot(SAME, (&e_ours, &ours), (&e_theirs, &theirs));
        assert_eq!(a.only_ours_inside(), 1, "100.5 is inside [100.0, 101.0]");
        assert_eq!(a.only_ours_outside(), 0);
    }

    #[test]
    fn a_size_we_overstate_is_recorded_with_its_direction_and_magnitude() {
        // The favourable-error direction: our ladder shows more depth than the
        // venue has. Excess = 5.0 - 1.0 = 4.0.
        let ours = ladder(Side::Buy, &[("100.0", "5.0")]);
        let theirs = ladder(Side::Buy, &[("100.0", "1.0")]);
        let e = Ladder::new(Side::Sell);
        let a = audit_snapshot(SAME, (&ours, &e), (&theirs, &e));
        assert_eq!(a.ours_larger(), 1);
        assert_eq!(a.ours_smaller(), 0);
        assert_eq!(a.excess(), q("4.0"));
        assert!(a.excess_exact());
        assert!(a.contradicts_at_same_position());
    }

    #[test]
    fn understating_a_size_gives_a_negative_excess() {
        let ours = ladder(Side::Buy, &[("100.0", "0.4")]);
        let theirs = ladder(Side::Buy, &[("100.0", "5.0")]);
        let e = Ladder::new(Side::Sell);
        let a = audit_snapshot(SAME, (&ours, &e), (&theirs, &e));
        assert_eq!(a.ours_smaller(), 1);
        assert_eq!(a.excess(), q("-4.6"));
        assert!(a.contradicts_at_same_position());
    }

    #[test]
    fn a_deeper_snapshot_is_reported_but_is_not_evidence() {
        let ours = ladder(Side::Buy, &[("100.0", "1.0")]);
        let theirs = ladder(Side::Buy, &[("100.0", "1.0"), ("99.0", "2.0")]);
        let e = Ladder::new(Side::Sell);
        let a = audit_snapshot(SAME, (&ours, &e), (&theirs, &e));
        assert_eq!(a.only_theirs(), 1);
        assert!(
            !a.contradicts_at_same_position(),
            "our own MAX_LEVELS truncation must not read as the venue's problem"
        );
    }

    #[test]
    fn only_same_position_audits_can_reach_the_contradiction_counter() {
        let ours = ladder(Side::Buy, &[("100.0", "5.0")]);
        let theirs = ladder(Side::Buy, &[("100.0", "1.0")]);
        let e = Ladder::new(Side::Sell);
        let mut v = DeltaVocabulary::default();

        let untrusted = audit_snapshot(AuditScope::LadderUntrusted, (&ours, &e), (&theirs, &e));
        v.record_audit(&untrusted);
        assert_eq!(v.audits_compared(), 0);
        assert_eq!(v.audits_at_same_position(), 0);
        assert_eq!(v.audits_contradicting(), 0);

        let far = audit_snapshot(
            AuditScope::Compared { id_distance: 7 },
            (&ours, &e),
            (&theirs, &e),
        );
        v.record_audit(&far);
        assert_eq!(v.audits_compared(), 1, "a distant comparison still happened");
        assert_eq!(v.audits_at_same_position(), 0);
        assert_eq!(
            v.audits_contradicting(),
            0,
            "distance excuses difference; only arb-detect may decide how much"
        );

        let same = audit_snapshot(SAME, (&ours, &e), (&theirs, &e));
        v.record_audit(&same);
        assert_eq!(v.audits_compared(), 2);
        assert_eq!(v.audits_at_same_position(), 1);
        assert_eq!(v.audits_contradicting(), 1);
    }

    #[test]
    fn an_empty_snapshot_side_denies_nothing() {
        // A venue returning an empty side says it has no data, not that our
        // levels are phantoms. Treating an empty window as a denial would turn
        // one bad response into a semantics verdict.
        let ours = ladder(Side::Buy, &[("100.0", "1.0"), ("99.0", "2.0")]);
        let theirs = Ladder::new(Side::Buy);
        let e_ours = Ladder::new(Side::Sell);
        let e_theirs = Ladder::new(Side::Sell);
        let a = audit_snapshot(SAME, (&ours, &e_ours), (&theirs, &e_theirs));
        assert_eq!(a.only_ours_inside(), 0);
        assert_eq!(a.only_ours_outside(), 2);
        assert!(!a.contradicts_at_same_position());
    }
}
