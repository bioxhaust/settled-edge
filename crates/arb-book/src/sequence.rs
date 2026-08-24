//! Sequence tracking for the three incompatible schemes venues use.
//!
//! # The three facts that matter, and why they are different
//!
//! * **Duplicate** — a message we have already applied, re-delivered. Harmless.
//!   Drop it and carry on. Absolute-size deltas make re-application *look*
//!   idempotent, but it is not: an older absolute size written over a newer one
//!   moves a level backwards in time. So we drop rather than re-apply.
//! * **Gap** — a message is missing and nothing will ever tell us what it said.
//!   The missing delta may have deleted the level we are about to price
//!   against. The ladder is **UNKNOWN**, not merely late. There is no catch-up:
//!   the only cure is a fresh snapshot.
//! * **Reset** — the venue restarted its own stream and began numbering again.
//!   Our cursor now refers to a sequence space that no longer exists. Same cure
//!   as a gap, different cause, and worth counting separately because a venue
//!   that restarts twenty times an hour is a different problem from a venue
//!   that drops one message an hour.
//!
//! This module reports facts and owns **no** policy. It never decides that a
//! venue is untradeable; `arb-detect` does. The only judgement encoded here is
//! how to classify a *single* observation, and the two constants that require a
//! judgement ([`RESET_BACKWARD_JUMP`], [`RESET_LOW_ID`]) carry their reasoning
//! where they are defined.
//!
//! Nothing here touches money, so nothing here needs fixed point: sequence ids
//! are `u64` counters and are compared exactly.

/// How far *backwards* an id must jump before we read it as a venue stream
/// restart rather than a stale re-delivery.
///
/// The two readings need different cures (drop the message vs. throw the ladder
/// away), so guessing wrong is expensive in both directions, and the only
/// evidence a single message gives us is the size of the jump.
///
/// Why one million: a backward jump is a duplicate when some buffer replayed
/// old messages at us, and re-delivery buffers are small — a reconnect or a
/// server-side retransmit window covers seconds, not hours. At 100 updates per
/// second, which is fast for a single instrument, one million messages is
/// roughly 2.8 hours of stream. No venue buffers 2.8 hours of depth deltas in
/// order to replay them, so a jump that large cannot be re-delivery; the
/// counter itself must have been re-based.
///
/// `[assumption]` — the per-instrument update rates and the re-delivery window
/// sizes behind that arithmetic came from memory of venue documentation and
/// were **never measured** against a live socket. If someone records a stream
/// and measures the largest genuine backward jump, replace this number and say
/// what was measured.
pub const RESET_BACKWARD_JUMP: u64 = 1_000_000;

/// An id at or below this, arriving while our cursor is already above it, is a
/// restart from zero.
///
/// [`RESET_BACKWARD_JUMP`] alone misses the common case: a venue that restarts
/// early in our session jumps back to 1 from a cursor of only a few thousand,
/// which is nowhere near a million. But "id 3 arrived while we are at 5000" has
/// only one plausible explanation, because id 3 was published seconds after the
/// stream began and no buffer holds a message that long.
///
/// Why 1024: the window has to be wide enough to catch a restart no matter
/// which of the first few messages of the new epoch reaches us first (venues
/// differ on whether numbering starts at 0, 1, or some small nonce), and narrow
/// enough that we are certainly past it before a real duplicate could arrive.
/// A live instrument publishes 1024 updates in the first few seconds after
/// connect, so a cursor above 1024 means the low window is long gone.
///
/// The unavoidable blind spot: while our own cursor is still *inside* this
/// window, a restart and a duplicate are indistinguishable from one message,
/// and we choose the harmless reading (duplicate — drop it). The real defence
/// there is not this constant, it is that a venue adapter which reconnects
/// calls [`SeqTracker::reset_to`] with the new snapshot's id explicitly rather
/// than hoping the classifier notices.
pub const RESET_LOW_ID: u64 = 1_024;

/// Which sequencing scheme a venue's depth stream uses.
///
/// Three schemes, three incompatible notions of "next". This enum existing is
/// the documentation: there is no single `sequence: u64` that models all of
/// them, and pretending otherwise is how a resync silently applies a delta from
/// the wrong side of a hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeqScheme {
    /// One id per message, expected to increase by exactly one.
    Monotonic,
    /// Each message names the id of the message it must follow. Self-linking,
    /// so a hole is detectable from the *new* message alone.
    PrevIdChain,
    /// Each message covers a closed range of ids `[first_id, last_id]`. Ranges
    /// are contiguous, so the next range starts one past our cursor.
    IdRange,
}

/// The sequencing fields carried by one depth message.
///
/// Deliberately mirrors [`SeqScheme`] one-to-one so that feeding the wrong
/// shape is a value we can classify rather than a field we silently ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeqInput {
    /// A single id.
    Monotonic { id: u64 },
    /// This message's id plus the id it chains from.
    PrevIdChain { prev_id: u64, id: u64 },
    /// A closed, inclusive range of ids.
    IdRange { first_id: u64, last_id: u64 },
}

impl SeqInput {
    /// The scheme this input shape belongs to.
    pub const fn scheme(self) -> SeqScheme {
        match self {
            SeqInput::Monotonic { .. } => SeqScheme::Monotonic,
            SeqInput::PrevIdChain { .. } => SeqScheme::PrevIdChain,
            SeqInput::IdRange { .. } => SeqScheme::IdRange,
        }
    }

    /// Lowest id this message claims to carry.
    pub const fn start_id(self) -> u64 {
        match self {
            SeqInput::Monotonic { id } => id,
            SeqInput::PrevIdChain { id, .. } => id,
            SeqInput::IdRange { first_id, .. } => first_id,
        }
    }

    /// Highest id this message claims to carry.
    ///
    /// This is the value to compare against a snapshot's id when deciding
    /// whether a buffered delta is stale: a message whose *end* is at or below
    /// the snapshot is entirely contained in it.
    pub const fn end_id(self) -> u64 {
        match self {
            SeqInput::Monotonic { id } => id,
            SeqInput::PrevIdChain { id, .. } => id,
            SeqInput::IdRange { last_id, .. } => last_id,
        }
    }
}

/// What one observation was.
///
/// Every variant is a statement about the *message*, never a decision about the
/// venue. The caller decides what to do; see the module docs for why Duplicate,
/// Gap and Reset must not be collapsed into "bad message".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqFact {
    /// Exactly the message we were expecting. Apply it.
    Accepted,
    /// Already applied. Drop it; the ladder is still good.
    Duplicate,
    /// Arrived after a message that supersedes part of it: the transport
    /// reordered or interleaved deliveries. Distinct from [`SeqFact::Gap`]
    /// because it blames the path, not the venue — but just as unusable, since
    /// we cannot apply a message out of order against absolute sizes.
    ///
    /// `expected` is the value the scheme required in the field it keys on,
    /// `got` is what arrived there.
    OutOfOrder { expected: u64, got: u64 },
    /// We missed data. The ladder is unknown until a fresh snapshot.
    ///
    /// `expected` / `got` as for [`SeqFact::OutOfOrder`]: `Monotonic` keys on
    /// `id` and expects `last + 1`, `PrevIdChain` keys on `prev_id` and expects
    /// `last`, `IdRange` keys on `first_id` and expects `last + 1`.
    Gap { expected: u64, got: u64 },
    /// The venue re-based its sequence space. `got` is the first id of the new
    /// epoch as far as we can tell.
    Reset { got: u64 },
    /// The input's shape does not match the tracker's scheme — a `PrevIdChain`
    /// message fed to a `Monotonic` tracker, say. That is a bug in the venue
    /// adapter, not a stream anomaly, and coercing it (reading `id` and
    /// ignoring `prev_id`) would hide a whole misconfigured feed behind
    /// plausible-looking books.
    SchemeMismatch,
    /// Not classifiable. Either there is no cursor yet in the caller's sense,
    /// or the message's own fields are self-contradictory (a range whose end
    /// precedes its start). No claim is made either way.
    Unknown,
}

/// The last position we actually accepted.
///
/// "Accepted" is load-bearing: the cursor never advances over data we did not
/// apply, so after a gap it deliberately keeps pointing at the last good
/// message. That is what makes a gap sticky instead of self-healing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeqCursor {
    /// First id of the accepted message. Equal to `last_id` for
    /// [`SeqScheme::Monotonic`] and [`SeqScheme::PrevIdChain`]; the range start
    /// for [`SeqScheme::IdRange`].
    pub first_id: u64,
    /// Highest id we have applied.
    pub last_id: u64,
}

/// Cumulative observation counts. Facts only, no thresholds.
#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    accepted: u64,
    duplicates: u64,
    gaps: u64,
    resets: u64,
    out_of_order: u64,
    unknown: u64,
    scheme_mismatches: u64,
}

/// Classifies depth messages for one instrument on one venue.
#[derive(Debug)]
pub struct SeqTracker {
    scheme: SeqScheme,
    cursor: Option<SeqCursor>,
    counters: Counters,
    /// Highest id we have ever *seen*, applied or not.
    ///
    /// Not part of the cursor on purpose — seeing an id is not accepting it.
    /// It exists so that a forward discontinuity can be split into "the venue
    /// skipped ahead" (gap) and "something we already saw overtook this"
    /// (out of order), which are different bugs to chase.
    high_seen: Option<u64>,
}

impl SeqTracker {
    /// Fresh tracker with no cursor. The first message establishes the cursor.
    pub fn new(scheme: SeqScheme) -> Self {
        SeqTracker {
            scheme,
            cursor: None,
            counters: Counters::default(),
            high_seen: None,
        }
    }

    /// Which scheme this tracker enforces.
    pub fn scheme(&self) -> SeqScheme {
        self.scheme
    }

    /// Last accepted position, or `None` before the first accepted message.
    pub fn cursor(&self) -> Option<SeqCursor> {
        self.cursor
    }

    /// Messages accepted and therefore applied.
    pub fn accepted(&self) -> u64 {
        self.counters.accepted
    }

    /// Messages we had already applied.
    pub fn duplicates(&self) -> u64 {
        self.counters.duplicates
    }

    /// Gap *observations*, which is not the same as distinct holes.
    ///
    /// The cursor does not advance over unapplied data, so while a hole is open
    /// every further message is also a gap and this climbs once per message
    /// until a snapshot repositions the cursor. Read it as "messages that
    /// arrived against a broken cursor", and note that the number therefore
    /// also measures how slow our resync was. `arb-detect` owns any rate or
    /// decay policy over it.
    pub fn gaps(&self) -> u64 {
        self.counters.gaps
    }

    /// Venue stream restarts we detected. Does not include
    /// [`SeqTracker::reset_to`] calls, which are our own recovery.
    pub fn resets(&self) -> u64 {
        self.counters.resets
    }

    /// Messages that arrived behind something we had already seen.
    pub fn out_of_order(&self) -> u64 {
        self.counters.out_of_order
    }

    /// Messages whose own sequencing fields contradicted themselves.
    pub fn unknown(&self) -> u64 {
        self.counters.unknown
    }

    /// Messages fed in the wrong shape for this tracker's scheme. Any value
    /// above zero is an adapter bug, not a venue problem.
    pub fn scheme_mismatches(&self) -> u64 {
        self.counters.scheme_mismatches
    }

    /// Reposition the cursor onto a freshly received snapshot.
    ///
    /// Not counted as a [`SeqFact::Reset`]: `resets()` is evidence about the
    /// venue, and our own recovery is not evidence about the venue. The
    /// counters are cumulative facts and are never cleared here — a book that
    /// has been resynced still had a hole in its past.
    pub fn reset_to(&mut self, id: u64) {
        self.cursor = Some(SeqCursor {
            first_id: id,
            last_id: id,
        });
        self.note_seen(id);
    }

    /// Classify one message and, if it is acceptable, advance the cursor.
    pub fn observe(&mut self, input: SeqInput) -> SeqFact {
        if input.scheme() != self.scheme {
            self.counters.scheme_mismatches = self.counters.scheme_mismatches.saturating_add(1);
            // Do NOT record the ids: they belong to a different sequence space
            // than the one this tracker is following, so remembering them would
            // poison every later comparison.
            return SeqFact::SchemeMismatch;
        }
        let fact = match input {
            SeqInput::Monotonic { id } => self.observe_monotonic(id),
            SeqInput::PrevIdChain { prev_id, id } => self.observe_prev_id_chain(prev_id, id),
            SeqInput::IdRange { first_id, last_id } => self.observe_id_range(first_id, last_id),
        };
        // A self-contradictory message tells us nothing about where the stream
        // is, so it does not move the high-water mark either.
        if fact != SeqFact::Unknown {
            self.note_seen(input.end_id());
        }
        fact
    }

    fn observe_monotonic(&mut self, id: u64) -> SeqFact {
        let Some(cursor) = self.cursor else {
            // Nothing to compare against. The honest reading of the first
            // message is "this is where we start".
            self.accept(id, id);
            return SeqFact::Accepted;
        };
        let last = cursor.last_id;
        // `saturating_add` rather than `+`: a cursor at u64::MAX is absurd, but
        // library code does not get to overflow-panic on absurd input.
        let expected = last.saturating_add(1);
        if id == expected {
            self.accept(id, id);
            return SeqFact::Accepted;
        }
        if id > expected {
            if self.high_seen.is_some_and(|h| id < h) {
                // We have already seen a HIGHER id than this one, so this
                // message is not the venue skipping forward — it is a late
                // delivery arriving under something that overtook it.
                self.counters.out_of_order = self.counters.out_of_order.saturating_add(1);
                return SeqFact::OutOfOrder { expected, got: id };
            }
            self.counters.gaps = self.counters.gaps.saturating_add(1);
            return SeqFact::Gap { expected, got: id };
        }
        // id <= last from here.
        if looks_like_restart(last, id) {
            self.counters.resets = self.counters.resets.saturating_add(1);
            return SeqFact::Reset { got: id };
        }
        // With a pure +1 counter, an id at or below the cursor is by
        // construction one we already applied: "out of order" and "duplicate"
        // are the same fact, and the fact is harmless.
        self.counters.duplicates = self.counters.duplicates.saturating_add(1);
        SeqFact::Duplicate
    }

    fn observe_prev_id_chain(&mut self, prev_id: u64, id: u64) -> SeqFact {
        let Some(cursor) = self.cursor else {
            self.accept(id, id);
            return SeqFact::Accepted;
        };
        let last = cursor.last_id;
        if looks_like_restart(last, id) {
            self.counters.resets = self.counters.resets.saturating_add(1);
            return SeqFact::Reset { got: id };
        }
        if prev_id == last {
            if id > last {
                self.accept(id, id);
                return SeqFact::Accepted;
            }
            // Chains from our cursor but does not advance past it, so there is
            // nothing in it we have not already applied. Malformed rather than
            // dangerous; take the harmless reading.
            self.counters.duplicates = self.counters.duplicates.saturating_add(1);
            return SeqFact::Duplicate;
        }
        if id <= last {
            self.counters.duplicates = self.counters.duplicates.saturating_add(1);
            return SeqFact::Duplicate;
        }
        if prev_id > last {
            // It chains from a message we never received: the hole is between
            // our cursor and this message's parent.
            self.counters.gaps = self.counters.gaps.saturating_add(1);
            return SeqFact::Gap {
                expected: last,
                got: prev_id,
            };
        }
        // prev_id < last but id > last: this message forks off a position we
        // have already moved past, yet claims new ids. Its parent's state is
        // not the state we hold, so applying it would overwrite newer sizes
        // with older ones.
        self.counters.out_of_order = self.counters.out_of_order.saturating_add(1);
        SeqFact::OutOfOrder {
            expected: last,
            got: prev_id,
        }
    }

    fn observe_id_range(&mut self, first_id: u64, last_id: u64) -> SeqFact {
        if last_id < first_id {
            // A range that ends before it starts is not a stream anomaly, it is
            // a decoder or venue bug. We refuse to invent an interpretation.
            self.counters.unknown = self.counters.unknown.saturating_add(1);
            return SeqFact::Unknown;
        }
        let Some(cursor) = self.cursor else {
            self.accept(first_id, last_id);
            return SeqFact::Accepted;
        };
        let last = cursor.last_id;
        if looks_like_restart(last, last_id) {
            // Report the range's START as the new epoch's position: that is
            // where the venue began numbering again.
            self.counters.resets = self.counters.resets.saturating_add(1);
            return SeqFact::Reset { got: first_id };
        }
        if last_id <= last {
            self.counters.duplicates = self.counters.duplicates.saturating_add(1);
            return SeqFact::Duplicate;
        }
        let expected = last.saturating_add(1);
        if first_id == expected {
            self.accept(first_id, last_id);
            return SeqFact::Accepted;
        }
        if first_id <= last {
            // Straddles the cursor: part re-delivered, part new. Ranges in this
            // scheme are contiguous, so a straddle cannot happen in a healthy
            // stream and we will not guess which half to trust.
            self.counters.out_of_order = self.counters.out_of_order.saturating_add(1);
            return SeqFact::OutOfOrder {
                expected,
                got: first_id,
            };
        }
        self.counters.gaps = self.counters.gaps.saturating_add(1);
        SeqFact::Gap {
            expected,
            got: first_id,
        }
    }

    fn accept(&mut self, first_id: u64, last_id: u64) {
        self.cursor = Some(SeqCursor { first_id, last_id });
        self.counters.accepted = self.counters.accepted.saturating_add(1);
    }

    fn note_seen(&mut self, id: u64) {
        self.high_seen = Some(match self.high_seen {
            Some(h) if h >= id => h,
            _ => id,
        });
    }
}

/// Does a non-forward id look like the venue re-based its counter?
///
/// See [`RESET_BACKWARD_JUMP`] and [`RESET_LOW_ID`] for why these two tests and
/// not one.
fn looks_like_restart(last: u64, got: u64) -> bool {
    if got > last {
        return false;
    }
    if got <= RESET_LOW_ID && last > RESET_LOW_ID {
        return true;
    }
    // `got <= last` here, so this cannot underflow.
    last - got >= RESET_BACKWARD_JUMP
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(id: u64) -> SeqInput {
        SeqInput::Monotonic { id }
    }
    fn chain(prev_id: u64, id: u64) -> SeqInput {
        SeqInput::PrevIdChain { prev_id, id }
    }
    fn range(first_id: u64, last_id: u64) -> SeqInput {
        SeqInput::IdRange { first_id, last_id }
    }

    // ---- Monotonic ----------------------------------------------------------

    #[test]
    fn monotonic_first_message_establishes_the_cursor() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        assert!(t.cursor().is_none());
        assert_eq!(t.observe(mono(7)), SeqFact::Accepted);
        assert_eq!(t.cursor().unwrap().last_id, 7);
        assert_eq!(t.accepted(), 1);
    }

    #[test]
    fn monotonic_accepts_contiguous_ids() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        for id in 10..=13 {
            assert_eq!(t.observe(mono(id)), SeqFact::Accepted);
        }
        // 10, 11, 12, 13 -> four accepts, cursor at 13.
        assert_eq!(t.accepted(), 4);
        assert_eq!(t.cursor().unwrap().last_id, 13);
        assert_eq!(t.gaps(), 0);
        assert_eq!(t.duplicates(), 0);
    }

    #[test]
    fn monotonic_id_at_or_below_cursor_is_a_duplicate() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(500);
        // Same id again: the exact message we last applied, re-delivered.
        assert_eq!(t.observe(mono(500)), SeqFact::Duplicate);
        // Strictly older, but only 100 back and 400 is above RESET_LOW_ID, so
        // neither reset test fires. With a +1 counter an older id is data we
        // have already applied, so Duplicate is the whole truth.
        assert_eq!(t.observe(mono(400)), SeqFact::Duplicate);
        assert_eq!(t.duplicates(), 2);
        assert_eq!(t.resets(), 0);
        // A duplicate never moves the cursor.
        assert_eq!(t.cursor().unwrap().last_id, 500);
    }

    #[test]
    fn monotonic_forward_skip_is_a_gap() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(10);
        // expected 11, got 14: ids 11, 12, 13 are gone for good.
        assert_eq!(
            t.observe(mono(14)),
            SeqFact::Gap {
                expected: 11,
                got: 14
            }
        );
        assert_eq!(t.gaps(), 1);
    }

    #[test]
    fn monotonic_cursor_does_not_advance_over_a_gap() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(10);
        let _ = t.observe(mono(14));
        assert_eq!(t.cursor().unwrap().last_id, 10);
        // 15 follows 14, but we never applied 14, so relative to the cursor
        // the hole is still open: expected 11, got 15.
        assert_eq!(
            t.observe(mono(15)),
            SeqFact::Gap {
                expected: 11,
                got: 15
            }
        );
        assert_eq!(t.gaps(), 2);
        assert_eq!(t.accepted(), 0);
    }

    #[test]
    fn monotonic_large_backward_jump_is_a_reset_not_a_duplicate() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(5_000_000);
        // 5_000_000 - 3_500_000 = 1_500_000 >= RESET_BACKWARD_JUMP (1_000_000).
        assert_eq!(
            t.observe(mono(3_500_000)),
            SeqFact::Reset { got: 3_500_000 }
        );
        assert_eq!(t.resets(), 1);
        assert_eq!(t.duplicates(), 0);
        // A reset does not reposition us: only a snapshot may do that.
        assert_eq!(t.cursor().unwrap().last_id, 5_000_000);
    }

    #[test]
    fn monotonic_backward_jump_just_under_the_threshold_is_a_duplicate() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(5_000_000);
        // 5_000_000 - 4_000_001 = 999_999, one short of RESET_BACKWARD_JUMP,
        // and 4_000_001 is far above RESET_LOW_ID. Boundary belongs to the
        // harmless reading.
        assert_eq!(t.observe(mono(4_000_001)), SeqFact::Duplicate);
        assert_eq!(t.resets(), 0);
    }

    #[test]
    fn monotonic_low_id_after_a_high_cursor_is_a_reset() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(5_000);
        // 5_000 - 1 = 4_999, nowhere near RESET_BACKWARD_JUMP, but id 1 with a
        // cursor of 5_000 is a restart from zero: RESET_LOW_ID catches it.
        assert_eq!(t.observe(mono(1)), SeqFact::Reset { got: 1 });
        // The documented blind spot: inside the low window we cannot tell, so
        // we take the harmless reading.
        let mut u = SeqTracker::new(SeqScheme::Monotonic);
        u.reset_to(5);
        assert_eq!(u.observe(mono(1)), SeqFact::Duplicate);
    }

    #[test]
    fn monotonic_late_id_under_a_seen_id_is_out_of_order() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(10);
        // Sees 20 (gap), so high_seen = 20 while the cursor stays at 10.
        assert_eq!(
            t.observe(mono(20)),
            SeqFact::Gap {
                expected: 11,
                got: 20
            }
        );
        // 15 is forward of the cursor but behind something already delivered:
        // the transport reordered, the venue did not skip.
        assert_eq!(
            t.observe(mono(15)),
            SeqFact::OutOfOrder {
                expected: 11,
                got: 15
            }
        );
        assert_eq!(t.out_of_order(), 1);
        assert_eq!(t.gaps(), 1);
    }

    // ---- PrevIdChain -------------------------------------------------------

    #[test]
    fn chain_accepts_when_prev_matches_the_cursor() {
        let mut t = SeqTracker::new(SeqScheme::PrevIdChain);
        assert_eq!(t.observe(chain(0, 100)), SeqFact::Accepted);
        // Chain ids need not be contiguous: 100 -> 137 is fine so long as the
        // link is stated.
        assert_eq!(t.observe(chain(100, 137)), SeqFact::Accepted);
        assert_eq!(t.observe(chain(137, 138)), SeqFact::Accepted);
        assert_eq!(t.cursor().unwrap().last_id, 138);
        assert_eq!(t.accepted(), 3);
        assert_eq!(t.gaps(), 0);
    }

    #[test]
    fn chain_gap_when_the_parent_was_never_seen() {
        let mut t = SeqTracker::new(SeqScheme::PrevIdChain);
        t.reset_to(100);
        // Chains from 150, we are at 100: everything between is lost.
        assert_eq!(
            t.observe(chain(150, 151)),
            SeqFact::Gap {
                expected: 100,
                got: 150
            }
        );
        assert_eq!(t.gaps(), 1);
        assert_eq!(t.cursor().unwrap().last_id, 100);
    }

    #[test]
    fn chain_duplicate_when_the_id_is_behind_the_cursor() {
        let mut t = SeqTracker::new(SeqScheme::PrevIdChain);
        t.reset_to(100);
        assert_eq!(t.observe(chain(99, 100)), SeqFact::Duplicate);
        // A message that links from our cursor but does not advance past it
        // carries nothing new either.
        assert_eq!(t.observe(chain(100, 100)), SeqFact::Duplicate);
        assert_eq!(t.duplicates(), 2);
        assert_eq!(t.accepted(), 0);
    }

    #[test]
    fn chain_out_of_order_when_the_parent_is_already_behind_us() {
        let mut t = SeqTracker::new(SeqScheme::PrevIdChain);
        t.reset_to(100);
        // Parent 80 is behind our cursor, yet the message claims id 120: it
        // was built on state we have already replaced.
        assert_eq!(
            t.observe(chain(80, 120)),
            SeqFact::OutOfOrder {
                expected: 100,
                got: 80
            }
        );
        assert_eq!(t.out_of_order(), 1);
        assert_eq!(t.cursor().unwrap().last_id, 100);
    }

    #[test]
    fn chain_restart_is_a_reset() {
        let mut t = SeqTracker::new(SeqScheme::PrevIdChain);
        t.reset_to(9_000);
        // A restarted OKX-style chain opens with prev_id 0, id 1. 1 is inside
        // RESET_LOW_ID and 9_000 is well past it.
        assert_eq!(t.observe(chain(0, 1)), SeqFact::Reset { got: 1 });
        assert_eq!(t.resets(), 1);
    }

    // ---- IdRange -----------------------------------------------------------

    #[test]
    fn id_range_accepts_contiguous_ranges() {
        let mut t = SeqTracker::new(SeqScheme::IdRange);
        assert_eq!(t.observe(range(10, 14)), SeqFact::Accepted);
        // Next range must start at 15 = 14 + 1.
        assert_eq!(t.observe(range(15, 15)), SeqFact::Accepted);
        assert_eq!(t.observe(range(16, 40)), SeqFact::Accepted);
        let c = t.cursor().unwrap();
        assert_eq!((c.first_id, c.last_id), (16, 40));
        assert_eq!(t.accepted(), 3);
    }

    #[test]
    fn id_range_gap_when_the_next_range_skips() {
        let mut t = SeqTracker::new(SeqScheme::IdRange);
        t.reset_to(14);
        // expected 15, got 20: ids 15..19 never arrived.
        assert_eq!(
            t.observe(range(20, 25)),
            SeqFact::Gap {
                expected: 15,
                got: 20
            }
        );
        assert_eq!(t.gaps(), 1);
    }

    #[test]
    fn id_range_duplicate_when_the_whole_range_is_behind() {
        let mut t = SeqTracker::new(SeqScheme::IdRange);
        t.reset_to(100);
        // last_id 100 <= cursor 100: fully contained in what we applied.
        assert_eq!(t.observe(range(95, 100)), SeqFact::Duplicate);
        assert_eq!(t.observe(range(80, 90)), SeqFact::Duplicate);
        assert_eq!(t.duplicates(), 2);
    }

    #[test]
    fn id_range_straddling_the_cursor_is_out_of_order() {
        let mut t = SeqTracker::new(SeqScheme::IdRange);
        t.reset_to(100);
        // 95..=110 overlaps ids we applied (95..100) and ids we have not
        // (101..110). Contiguous ranges cannot do this; refuse to guess.
        assert_eq!(
            t.observe(range(95, 110)),
            SeqFact::OutOfOrder {
                expected: 101,
                got: 95
            }
        );
        assert_eq!(t.out_of_order(), 1);
        assert_eq!(t.cursor().unwrap().last_id, 100);
    }

    #[test]
    fn id_range_restart_is_a_reset_reporting_the_new_start() {
        let mut t = SeqTracker::new(SeqScheme::IdRange);
        t.reset_to(4_000);
        // New epoch's first range is 1..=20; 20 is inside RESET_LOW_ID.
        assert_eq!(t.observe(range(1, 20)), SeqFact::Reset { got: 1 });
        assert_eq!(t.resets(), 1);
    }

    #[test]
    fn id_range_reversed_range_is_unknown_and_moves_nothing() {
        let mut t = SeqTracker::new(SeqScheme::IdRange);
        t.reset_to(100);
        assert_eq!(t.observe(range(120, 110)), SeqFact::Unknown);
        assert_eq!(t.unknown(), 1);
        assert_eq!(t.cursor().unwrap().last_id, 100);
        // The bogus range must not count as progress of any kind: the next
        // range is still classified against the cursor of 100.
        assert_eq!(
            t.observe(range(105, 106)),
            SeqFact::Gap {
                expected: 101,
                got: 105
            }
        );
    }

    // ---- Cross-cutting -----------------------------------------------------

    #[test]
    fn wrong_input_shape_is_a_scheme_mismatch_for_every_scheme() {
        let mut m = SeqTracker::new(SeqScheme::Monotonic);
        assert_eq!(m.observe(chain(1, 2)), SeqFact::SchemeMismatch);
        assert_eq!(m.observe(range(1, 2)), SeqFact::SchemeMismatch);

        let mut c = SeqTracker::new(SeqScheme::PrevIdChain);
        assert_eq!(c.observe(mono(1)), SeqFact::SchemeMismatch);
        assert_eq!(c.observe(range(1, 2)), SeqFact::SchemeMismatch);

        let mut r = SeqTracker::new(SeqScheme::IdRange);
        assert_eq!(r.observe(mono(1)), SeqFact::SchemeMismatch);
        assert_eq!(r.observe(chain(1, 2)), SeqFact::SchemeMismatch);

        assert_eq!(m.scheme_mismatches(), 2);
        // A mismatch is never coerced into a position.
        assert!(m.cursor().is_none());
        assert_eq!(m.accepted(), 0);
    }

    #[test]
    fn scheme_is_reported_back() {
        assert_eq!(
            SeqTracker::new(SeqScheme::Monotonic).scheme(),
            SeqScheme::Monotonic
        );
        assert_eq!(
            SeqTracker::new(SeqScheme::PrevIdChain).scheme(),
            SeqScheme::PrevIdChain
        );
        assert_eq!(
            SeqTracker::new(SeqScheme::IdRange).scheme(),
            SeqScheme::IdRange
        );
    }

    #[test]
    fn reset_to_repositions_without_counting_a_venue_reset() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        t.reset_to(10);
        let _ = t.observe(mono(99)); // gap, cursor frozen at 10
        assert_eq!(t.gaps(), 1);
        t.reset_to(99);
        assert_eq!(t.cursor().unwrap().last_id, 99);
        assert_eq!(t.resets(), 0);
        // Counters are cumulative history and are NOT cleared by recovery.
        assert_eq!(t.gaps(), 1);
        // And the stream continues cleanly from the new position.
        assert_eq!(t.observe(mono(100)), SeqFact::Accepted);
    }

    #[test]
    fn input_accessors_report_the_span_of_each_shape() {
        assert_eq!(mono(5).start_id(), 5);
        assert_eq!(mono(5).end_id(), 5);
        assert_eq!(chain(4, 5).start_id(), 5);
        assert_eq!(chain(4, 5).end_id(), 5);
        assert_eq!(range(4, 9).start_id(), 4);
        assert_eq!(range(4, 9).end_id(), 9);
        assert_eq!(range(4, 9).scheme(), SeqScheme::IdRange);
    }

    #[test]
    fn counters_are_independent() {
        let mut t = SeqTracker::new(SeqScheme::Monotonic);
        // Cursor deliberately above RESET_LOW_ID (1_024) so that the low-id
        // reset test below is able to fire at all.
        t.reset_to(2_000);
        let _ = t.observe(mono(2_001)); // accepted: 2_000 + 1
        let _ = t.observe(mono(2_001)); // duplicate: equals the cursor
        let _ = t.observe(mono(2_010)); // gap: expected 2_002, high_seen -> 2_010
        let _ = t.observe(mono(1)); // reset: 1 <= 1_024 < cursor 2_001
        let _ = t.observe(chain(1, 2)); // scheme mismatch: wrong shape
        let _ = t.observe(mono(2_005)); // out of order: > 2_002 but < seen 2_010
        assert_eq!(t.accepted(), 1);
        assert_eq!(t.duplicates(), 1);
        assert_eq!(t.gaps(), 1);
        assert_eq!(t.resets(), 1);
        assert_eq!(t.scheme_mismatches(), 1);
        assert_eq!(t.out_of_order(), 1);
        assert_eq!(t.unknown(), 0);
        // Only the accept moved the cursor.
        assert_eq!(t.cursor().unwrap().last_id, 2_001);
    }
}
