//! The book state machine: snapshot, delta, gap, resync.
//!
//! One [`BookState`] per instrument per venue. It owns two [`Ladder`]s, a
//! [`SeqTracker`], and a bounded buffer of deltas it could not apply yet.
//!
//! # The recovery path is the whole difficulty
//!
//! Everything here is easy except one sequence of events, which is also the one
//! that happens every time a socket hiccups:
//!
//! 1. deltas are flowing and being applied;
//! 2. one goes missing — the ladder is now **unknown**, not late;
//! 3. deltas keep arriving while we ask REST for a fresh snapshot;
//! 4. the snapshot arrives, describing the book as of some id `S`;
//! 5. some buffered deltas are already inside the snapshot and some are not.
//!
//! Replaying a delta that the snapshot already contains writes an *older*
//! absolute size over a newer one, which does not error, does not look wrong,
//! and quietly misprices depth. Discarding a delta the snapshot does *not*
//! contain leaves a level stale forever. So step 5 is not a detail; it is the
//! reason this module exists, and [`BookState::apply_snapshot`] does it in one
//! explicit, commented block.

use arb_num::Qty;
use arb_types::{
    BookSnapshot, DataQuality, Level, LocalTs, Provenance, QualityFlags, Side, TypeError, VenueKey,
    WallTs,
};

use arb_num::Price;

use crate::error::BookError;
use crate::ladder::{Ladder, MAX_INPUT_LEVELS};
use crate::sequence::{SeqFact, SeqInput, SeqScheme, SeqTracker};

/// Most deltas we will hold while waiting for a snapshot, per book.
///
/// Why bounded at all: while a book is awaiting resync we buffer everything the
/// socket hands us. A venue that never sends a usable snapshot — down, rate
/// limiting us, or returning a depth endpoint that disagrees with its own
/// stream — would otherwise turn one broken instrument into an out-of-memory
/// kill for the whole engine, taking every healthy venue with it.
///
/// Why 256: the buffer only has to cover one REST snapshot round trip. That is
/// on the order of 100 ms, and a rate-limited retry pushes it to roughly a
/// second. A busy instrument can publish around 100 depth messages per second,
/// so one second is about 100 messages and 256 leaves better than 2x headroom.
/// For memory: a delta typically carries a couple of dozen `(Price, Qty)` pairs
/// at 32 bytes each, so about 1 KiB, so about 256 KiB per book in the worst
/// case — a few MiB across eight venues for one instrument, which is the shape
/// of the budget we want.
///
/// `[assumption]` — both the message rate and the snapshot latency come from
/// memory of venue documentation and were **never measured** against a live
/// socket or a real REST endpoint. Measure a recorded resync and replace this
/// number with what was measured.
///
/// Eviction is oldest-first, see [`BookState::apply_delta`] for why.
pub const MAX_PENDING: usize = 256;

/// One depth delta as a venue sent it: absolute size per level, zero deletes.
///
/// `bids` / `asks` are `(Price, Qty)` rather than `Level` on purpose: a `Level`
/// enforces a positive size, and a delta's whole vocabulary includes `qty == 0`
/// meaning "this level is gone".
#[derive(Debug, Clone)]
pub struct Delta {
    /// The message's sequencing fields, in the venue's own scheme.
    pub seq: SeqInput,
    /// Bid-side absolute sizes. `Qty::ZERO` deletes the level.
    pub bids: Vec<(Price, Qty)>,
    /// Ask-side absolute sizes. `Qty::ZERO` deletes the level.
    pub asks: Vec<(Price, Qty)>,
    /// What the exchange said the time was. `None` when it said nothing.
    pub exchange_ts: Option<WallTs>,
    /// When we received it, on our clock.
    pub observed_at: LocalTs,
}

/// A delta held back for replay after the next snapshot.
#[derive(Debug, Clone)]
pub struct PendingDelta {
    /// The delta exactly as it arrived, never edited. Editing buffered data is
    /// how a resync ends up applying something no venue ever sent.
    pub delta: Delta,
    /// Why it was held back. Kept because "buffered before the first snapshot"
    /// and "buffered because a gap opened mid-stream" are different failures
    /// with different fixes, and the buffer is where you look afterwards.
    pub reason: SeqFact,
}

/// Live order book for one instrument on one venue.
///
/// Fields are private: `initialized` and `awaiting_resync` are the two bits that
/// decide whether any number this struct produces means anything, and letting a
/// caller set them is letting a caller declare a broken book healthy.
#[derive(Debug)]
pub struct BookState {
    vkey: VenueKey,
    bids: Ladder,
    asks: Ladder,
    seq: SeqTracker,
    exchange_ts: Option<WallTs>,
    observed_at: LocalTs,
    initialized: bool,
    awaiting_resync: bool,
    pending: Vec<PendingDelta>,
    /// Never upgraded. See [`BookState::snapshot`].
    provenance: Provenance,
    /// Deltas evicted because the buffer was full.
    dropped_pending: u64,
    /// Deltas thrown away at snapshot time as superseded or unorderable.
    discarded_pending: u64,
    /// Replayed deltas that returned an error.
    replay_failures: u64,
}

impl BookState {
    /// Empty, uninitialised book. It produces no numbers until a snapshot
    /// arrives; every delta before that is buffered.
    pub fn new(vkey: VenueKey, scheme: SeqScheme, provenance: Provenance) -> Self {
        BookState {
            vkey,
            bids: Ladder::new(Side::Buy),
            asks: Ladder::new(Side::Sell),
            seq: SeqTracker::new(scheme),
            exchange_ts: None,
            // EPOCH, not "now": this crate has no clock (rule 5, no I/O), and a
            // book that has never been observed must not claim a fresh time.
            observed_at: LocalTs::EPOCH,
            initialized: false,
            awaiting_resync: false,
            pending: Vec::new(),
            provenance,
            dropped_pending: 0,
            discarded_pending: 0,
            replay_failures: 0,
        }
    }

    /// Install a full refresh and replay whatever it does not already contain.
    ///
    /// `seq` is the id the snapshot describes the book as of, or `None` for a
    /// venue that does not say.
    pub fn apply_snapshot(
        &mut self,
        bids: Vec<Level>,
        asks: Vec<Level>,
        seq: Option<u64>,
        exchange_ts: Option<WallTs>,
        observed_at: LocalTs,
    ) -> Result<(), BookError> {
        // Build both sides aside and swap them in together. `replace_all`
        // validates before it installs, so each side is individually
        // all-or-nothing, but installing bids and then failing on asks would
        // leave the book half new and half old with an error the caller may
        // well log and ignore. Two temporaries cost one allocation each.
        let mut new_bids = Ladder::new(Side::Buy);
        let mut new_asks = Ladder::new(Side::Sell);
        new_bids.replace_all(bids)?;
        new_asks.replace_all(asks)?;
        self.bids = new_bids;
        self.asks = new_asks;

        if let Some(id) = seq {
            self.seq.reset_to(id);
        }
        self.exchange_ts = exchange_ts;
        self.observed_at = observed_at;
        self.initialized = true;
        // Cleared BEFORE the replay below, because the replay goes through
        // `apply_delta`, and `apply_delta` refuses to mutate while this is set.
        // Clear it afterwards instead and the recovery quietly re-buffers
        // everything it was supposed to apply, forever.
        self.awaiting_resync = false;

        // ---- the replay -------------------------------------------------
        // Take the buffer out first. A replayed delta can legitimately
        // re-buffer itself (it may open a fresh gap), and it must land in a
        // clean buffer rather than in the collection being iterated.
        let pending = core::mem::take(&mut self.pending);
        let cursor = self.seq.cursor();
        for p in pending {
            match cursor {
                // Strictly AFTER the snapshot: the snapshot does not contain
                // it, so it must be applied, in arrival order, through the
                // ordinary path so that it is sequenced and validated exactly
                // like a live message.
                Some(c) if p.delta.seq.end_id() > c.last_id => {
                    if self.apply_delta(p.delta).is_err() {
                        // A replayed delta we cannot apply is data we have
                        // skipped, so the ladder can no longer be vouched for.
                        // Mark it: the deltas still to come will then buffer
                        // for the next snapshot instead of being applied on top
                        // of a hole.
                        self.replay_failures = self.replay_failures.saturating_add(1);
                        self.awaiting_resync = true;
                    }
                }
                // At or before the snapshot: entirely contained in the levels
                // we just installed. Replaying it would write this delta's
                // older absolute sizes over the snapshot's newer ones — no
                // error, no symptom, wrong depth. Discard.
                Some(_) => {
                    self.discarded_pending = self.discarded_pending.saturating_add(1);
                }
                // The snapshot carried no id, so we cannot order any buffered
                // delta against it and therefore cannot tell "already included"
                // from "still needed". Discarding is the only safe answer: the
                // alternative guesses, and guessing wrong writes stale sizes
                // into a fresh book. Counted, because it is data loss.
                None => {
                    self.discarded_pending = self.discarded_pending.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    /// Classify one delta and, if it is the message we were expecting, apply it.
    ///
    /// Returns the sequencing fact. An `Err` means the delta was rejected
    /// outright; the ladders are untouched and the book is marked as awaiting
    /// resync, because the tracker's cursor has already moved past a message
    /// the ladder does not contain.
    pub fn apply_delta(&mut self, d: Delta) -> Result<SeqFact, BookError> {
        if !self.initialized {
            // No base state for an absolute-size update to sit on. Note we do
            // NOT observe the sequence here: letting a pre-snapshot delta set
            // the cursor would make the tracker claim a position we never
            // applied, and the snapshot's own id is the only honest origin.
            self.buffer(d, SeqFact::Unknown);
            return Ok(SeqFact::Unknown);
        }
        let fact = self.seq.observe(d.seq);
        if self.awaiting_resync {
            // Already unknown. Keep the message for replay, touch nothing.
            self.buffer(d, fact);
            return Ok(fact);
        }
        match fact {
            SeqFact::Accepted => match self.apply_accepted(d) {
                Ok(()) => Ok(SeqFact::Accepted),
                Err(e) => {
                    self.awaiting_resync = true;
                    Err(e)
                }
            },
            // Already applied. Dropping is not laziness: re-applying an older
            // absolute size over a newer one moves a level backwards in time.
            SeqFact::Duplicate => Ok(SeqFact::Duplicate),
            SeqFact::SchemeMismatch => {
                // An adapter wired to the wrong scheme. Buffering is pointless
                // — it will mismatch identically on replay and only consume the
                // bounded buffer — but the ladder still cannot be vouched for,
                // because every message before this one was classified against
                // the wrong field.
                self.awaiting_resync = true;
                Ok(SeqFact::SchemeMismatch)
            }
            // Gap and Reset are the documented resync triggers. OutOfOrder and
            // Unknown join them deliberately: both describe a message carrying
            // data we did not apply and cannot prove we may skip, and the only
            // safe reading of "cannot prove" is that the ladder is unknown.
            SeqFact::Gap { .. }
            | SeqFact::Reset { .. }
            | SeqFact::OutOfOrder { .. }
            | SeqFact::Unknown => {
                self.awaiting_resync = true;
                self.buffer(d, fact);
                Ok(fact)
            }
        }
    }

    /// True while the ladder is unknown and a fresh snapshot is required.
    pub fn awaiting_resync(&self) -> bool {
        self.awaiting_resync
    }

    /// True once a snapshot has been applied.
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    /// Best bid at or above best ask.
    ///
    /// A one-sided or empty book is not crossed, merely empty — the two facts
    /// get separate flags because they have separate causes.
    pub fn is_crossed(&self) -> bool {
        match (self.bids.best(), self.asks.best()) {
            (Some(b), Some(a)) => b.price >= a.price,
            _ => false,
        }
    }

    /// Refuse to proceed on a crossed book.
    ///
    /// For callers that would rather stop than reason about which side is
    /// stale. The book itself only ever *flags* crossing: normalising it away
    /// would destroy the evidence that one side stopped updating.
    pub fn ensure_not_crossed(&self) -> Result<(), BookError> {
        if self.is_crossed() {
            return Err(BookError::CrossedAfterApply);
        }
        Ok(())
    }

    /// Freeze the current ladders into an `arb-types` snapshot.
    ///
    /// Every doubt this state machine holds is expressed as a flag rather than
    /// as an error, because a snapshot is the thing downstream code ranks and
    /// it must be able to see why a row is untrustworthy.
    ///
    /// The snapshot's `observed_at` is **not** a parameter. It is
    /// [`Self::observed_at`] — the local time the newest applied message was
    /// seen — and there is no way for a caller to supply anything else. This used
    /// to take an `observed_at: LocalTs`, and the obvious call, the one the
    /// parameter name invited, was `st.snapshot(now)`: that stamps a book which
    /// may not have updated in a minute with the current clock, so its age is
    /// zero for ever and every staleness predicate downstream reads `Fresh`. The
    /// state already held the truth and exposed it; the parameter's only function
    /// was to let a caller discard it. Age is `now - snapshot.observed_at`,
    /// computed by the reader against its own clock.
    pub fn snapshot(&self) -> Result<BookSnapshot, BookError> {
        if !self.initialized {
            return Err(BookError::NotInitialized);
        }
        let mut flags = QualityFlags::NONE;
        if self.bids.is_empty() || self.asks.is_empty() {
            flags.insert(QualityFlags::BOOK_EMPTY);
        }
        if self.is_crossed() {
            flags.insert(QualityFlags::CROSSED_BOOK);
        }
        // Sticky on purpose. `awaiting_resync` covers the live hole; `gaps() > 0`
        // keeps the mark after the resync, because a venue that has dropped a
        // message has demonstrated that it drops messages and this book's
        // levels were rebuilt across a hole we never saw the contents of. The
        // consequence is real and deliberate: one gap flags this book for the
        // rest of its life. Any decay or rate policy belongs to `arb-detect`,
        // which can read `seq()` for the counts; this crate owns no thresholds.
        if self.awaiting_resync || self.seq.gaps() > 0 {
            flags.insert(QualityFlags::SEQ_GAP);
        }
        if self.bids.truncated() || self.asks.truncated() {
            flags.insert(QualityFlags::PARTIAL_BOOK);
        }
        // Provenance is carried, never improved. A book assembled from an
        // invented fixture stays SynthesizedUnverified all the way to the
        // screen; there is no arithmetic anywhere that can turn a guess into a
        // measurement, so there is no code path here that raises it.
        let quality = DataQuality::new(flags, self.provenance);
        BookSnapshot::new(
            self.vkey,
            self.bids.levels().to_vec(),
            self.asks.levels().to_vec(),
            self.seq.cursor().map(|c| c.last_id),
            self.exchange_ts,
            self.observed_at,
            quality,
        )
        .map_err(BookError::from)
    }

    /// Cumulative size available at prices no worse than `limit`, for a taker.
    ///
    /// `side` is the side of the **taker's order**, so it selects the opposite
    /// ladder: a `Side::Buy` lifts **asks**, a `Side::Sell` hits **bids**. Get
    /// this backwards and every quote reads the spread with the wrong sign,
    /// which makes losing trades look like the best opportunities on the board.
    ///
    /// "No worse than" therefore also flips: a buyer accepts asks at or *below*
    /// `limit`, a seller accepts bids at or *above* it.
    ///
    /// Errors while awaiting resync rather than returning a number. A snapshot
    /// can carry `SEQ_GAP` alongside its levels; a bare `Qty` has nowhere to put
    /// the doubt, so the only way not to launder it is to refuse.
    pub fn depth_at_or_better(&self, side: Side, limit: Price) -> Result<Qty, BookError> {
        if !self.initialized {
            return Err(BookError::NotInitialized);
        }
        if self.awaiting_resync {
            return Err(BookError::AwaitingResync);
        }
        if !limit.is_positive() {
            return Err(BookError::Type(TypeError::NonPositivePrice));
        }
        let ladder = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };
        let mut acc = Qty::ZERO;
        for l in ladder.levels() {
            let acceptable = match side {
                Side::Buy => l.price <= limit,
                Side::Sell => l.price >= limit,
            };
            if !acceptable {
                // Ladders are sorted best-first, so the first unacceptable
                // level means every level behind it is worse too.
                break;
            }
            acc = acc.checked_add(l.qty)?;
        }
        Ok(acc)
    }

    /// The instrument this book is for.
    pub fn vkey(&self) -> VenueKey {
        self.vkey
    }

    /// Bid ladder, best first.
    pub fn bids(&self) -> &Ladder {
        &self.bids
    }

    /// Ask ladder, best first.
    pub fn asks(&self) -> &Ladder {
        &self.asks
    }

    /// Sequence facts observed so far.
    pub fn seq(&self) -> &SeqTracker {
        &self.seq
    }

    /// How trustworthy this book's inputs were. Never upgraded.
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// Exchange timestamp of the last message applied, if it carried one.
    pub fn exchange_ts(&self) -> Option<WallTs> {
        self.exchange_ts
    }

    /// Our own observation time of the last message applied.
    pub fn observed_at(&self) -> LocalTs {
        self.observed_at
    }

    /// Deltas currently held for replay.
    pub fn pending(&self) -> &[PendingDelta] {
        &self.pending
    }

    /// Number of deltas currently held for replay.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Deltas evicted because the buffer hit [`MAX_PENDING`]. Above zero means
    /// a resync took longer than the buffer covers.
    pub fn dropped_pending(&self) -> u64 {
        self.dropped_pending
    }

    /// Deltas discarded at snapshot time, as superseded or as unorderable
    /// against a snapshot that carried no id.
    pub fn discarded_pending(&self) -> u64 {
        self.discarded_pending
    }

    /// Replayed deltas that errored. Above zero means a resync did not complete
    /// cleanly and the book went straight back to awaiting resync.
    pub fn replay_failures(&self) -> u64 {
        self.replay_failures
    }

    /// Apply an accepted delta to both ladders.
    ///
    /// VALIDATE-FIRST, then mutate — every level on *both* sides is checked
    /// before either ladder is touched. Applying bids and then discovering a
    /// bad ask leaves the bid side advanced and the ask side stale: the caller
    /// sees one `Err`, probably logs it, and from then on prices against a book
    /// whose two halves are from different messages. Nothing later detects
    /// that, because both sides are individually well formed. So the only
    /// acceptable shape is all-or-nothing, and the cheapest way to get it
    /// without cloning two ladders is to make failure impossible before the
    /// first write.
    fn apply_accepted(&mut self, d: Delta) -> Result<(), BookError> {
        if d.bids.len() > MAX_INPUT_LEVELS || d.asks.len() > MAX_INPUT_LEVELS {
            return Err(BookError::LevelLimitExceeded);
        }
        for (price, qty) in d.bids.iter().chain(d.asks.iter()) {
            if !price.is_positive() {
                return Err(BookError::Type(TypeError::NonPositivePrice));
            }
            // Negative absolute size is meaningless, and clamping it to zero
            // would delete a level the venue never asked us to delete.
            if qty.is_negative() {
                return Err(BookError::NegativeQty);
            }
        }
        // Past this point every pair satisfies exactly the preconditions
        // `Ladder::apply` checks, so it cannot fail. The error arms below are
        // therefore unreachable today and exist so that a future failure mode
        // added to `Ladder::apply` degrades to "book marked unknown" instead of
        // "book silently half-applied". They are cheap; silence is not.
        for (price, qty) in &d.bids {
            self.bids.apply(*price, *qty)?;
        }
        for (price, qty) in &d.asks {
            self.asks.apply(*price, *qty)?;
        }
        // Taken as-is, including `None`: this field means "the exchange time of
        // the message we last applied". Keeping a previous timestamp when the
        // new message carried none would report a book as fresher than anything
        // the venue actually told us.
        self.exchange_ts = d.exchange_ts;
        self.observed_at = d.observed_at;
        Ok(())
    }

    /// Hold a delta for replay, evicting the oldest if the buffer is full.
    ///
    /// Oldest-first eviction, because the snapshot that ends the resync will be
    /// recent: the newest buffered deltas are the ones it will still need, and
    /// the oldest are precisely the ones it supersedes. Dropping the newest
    /// instead would throw away the only copies of the messages that matter.
    ///
    /// Eviction can leave a hole between the snapshot and the oldest retained
    /// delta, and that hole is not hidden: replay feeds every retained delta
    /// through the tracker, which reports the `Gap` and puts the book straight
    /// back into resync. Slow, loud, and correct.
    fn buffer(&mut self, delta: Delta, reason: SeqFact) {
        if self.pending.len() >= MAX_PENDING && !self.pending.is_empty() {
            // O(n) memmove. Acceptable: it happens only while a book is
            // already broken, and only once the buffer is full.
            self.pending.remove(0);
            self.dropped_pending = self.dropped_pending.saturating_add(1);
        }
        self.pending.push(PendingDelta { delta, reason });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{InstrumentKey, MarketKind, Venue};

    fn p(s: &str) -> Price {
        Price::parse(s).unwrap()
    }
    fn q(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }
    fn lvl(price: &str, qty: &str) -> Level {
        Level::new(p(price), q(qty)).unwrap()
    }
    fn vk() -> VenueKey {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::Spot, "USDT").unwrap();
        VenueKey::new(Venue::Binance, key)
    }
    fn mono(id: u64) -> SeqInput {
        SeqInput::Monotonic { id }
    }
    fn ts() -> LocalTs {
        LocalTs::from_millis(1_700_000_000_000)
    }
    fn ex() -> Option<WallTs> {
        Some(WallTs::from_millis(1_700_000_000_123))
    }

    /// Delta from string pairs, so every test vector is legible.
    fn delta(seq: SeqInput, bids: &[(&str, &str)], asks: &[(&str, &str)]) -> Delta {
        Delta {
            seq,
            bids: bids.iter().map(|(a, b)| (p(a), q(b))).collect(),
            asks: asks.iter().map(|(a, b)| (p(a), q(b))).collect(),
            exchange_ts: ex(),
            observed_at: ts(),
        }
    }

    /// Uninitialised monotonic book, measured provenance unless stated.
    fn fresh() -> BookState {
        BookState::new(vk(), SeqScheme::Monotonic, Provenance::LiveMeasured)
    }

    /// A two-a-side book: bids 99 @ 2 / 98 @ 1, asks 100 @ 3 / 101 @ 4.
    fn seeded(seq: Option<u64>) -> BookState {
        let mut b = fresh();
        b.apply_snapshot(
            vec![lvl("99", "2"), lvl("98", "1")],
            vec![lvl("100", "3"), lvl("101", "4")],
            seq,
            ex(),
            ts(),
        )
        .unwrap();
        b
    }

    #[test]
    fn delta_before_a_snapshot_is_buffered_and_unknown() {
        let mut b = fresh();
        let f = b.apply_delta(delta(mono(7), &[("99", "1")], &[])).unwrap();
        assert_eq!(f, SeqFact::Unknown);
        assert!(!b.initialized());
        assert_eq!(b.pending_len(), 1);
        // The ladder must not have moved, and the tracker must not have taken a
        // position from a message we never applied.
        assert!(b.bids().is_empty());
        assert!(b.seq().cursor().is_none());
        assert!(matches!(
            b.snapshot(),
            Err(BookError::NotInitialized)
        ));
    }

    #[test]
    fn snapshot_initialises_and_sets_the_cursor() {
        let b = seeded(Some(100));
        assert!(b.initialized());
        assert!(!b.awaiting_resync());
        assert_eq!(b.seq().cursor().unwrap().last_id, 100);
        let snap = b.snapshot().unwrap();
        assert_eq!(snap.sequence, Some(100));
        // 99 > 98 descending, 100 < 101 ascending.
        assert_eq!(snap.best_bid().unwrap().price.raw(), 99_000_000_000_000);
        assert_eq!(snap.best_ask().unwrap().price.raw(), 100_000_000_000_000);
        assert!(snap.quality.flags.is_empty());
    }

    #[test]
    fn accepted_delta_updates_both_sides_and_zero_deletes() {
        let mut b = seeded(Some(100));
        // 101 = 100 + 1, so this is the message we expect.
        // Bids: replace 99 with size 5, add 97 @ 6. Asks: delete 101, add 99.5.
        let f = b
            .apply_delta(delta(
                mono(101),
                &[("99", "5"), ("97", "6")],
                &[("101", "0"), ("99.5", "7")],
            ))
            .unwrap();
        assert_eq!(f, SeqFact::Accepted);
        let bids: Vec<(i128, i128)> = b
            .bids()
            .levels()
            .iter()
            .map(|l| (l.price.raw(), l.qty.raw()))
            .collect();
        // 99 @ 5, 98 @ 1 (untouched), 97 @ 6 — descending.
        assert_eq!(
            bids,
            vec![
                (99_000_000_000_000, 5_000_000_000_000),
                (98_000_000_000_000, 1_000_000_000_000),
                (97_000_000_000_000, 6_000_000_000_000),
            ]
        );
        let asks: Vec<(i128, i128)> = b
            .asks()
            .levels()
            .iter()
            .map(|l| (l.price.raw(), l.qty.raw()))
            .collect();
        // 101 deleted, 99.5 @ 7 inserted in front of 100 @ 3 — ascending.
        assert_eq!(
            asks,
            vec![
                (99_500_000_000_000, 7_000_000_000_000),
                (100_000_000_000_000, 3_000_000_000_000),
            ]
        );
        assert_eq!(b.seq().cursor().unwrap().last_id, 101);
    }

    #[test]
    fn duplicate_delta_does_not_mutate() {
        let mut b = seeded(Some(100));
        // id 100 == cursor: already applied.
        let f = b
            .apply_delta(delta(mono(100), &[], &[("100", "99")]))
            .unwrap();
        assert_eq!(f, SeqFact::Duplicate);
        // Still 3, not 99.
        assert_eq!(b.asks().best().unwrap().qty.raw(), 3_000_000_000_000);
        assert!(!b.awaiting_resync());
        assert_eq!(b.pending_len(), 0);
    }

    #[test]
    fn gap_sets_awaiting_resync_and_stops_mutating() {
        let mut b = seeded(Some(10));
        // expected 11, got 15.
        let f = b
            .apply_delta(delta(mono(15), &[("99", "50")], &[("100", "50")]))
            .unwrap();
        assert_eq!(
            f,
            SeqFact::Gap {
                expected: 11,
                got: 15
            }
        );
        assert!(b.awaiting_resync());
        assert_eq!(b.pending_len(), 1);
        // Ladders exactly as the snapshot left them: 99 @ 2 and 100 @ 3.
        assert_eq!(b.bids().best().unwrap().qty.raw(), 2_000_000_000_000);
        assert_eq!(b.asks().best().unwrap().qty.raw(), 3_000_000_000_000);
        // A further delta is buffered, not applied, even though 16 follows 15.
        let f2 = b
            .apply_delta(delta(mono(16), &[("99", "60")], &[]))
            .unwrap();
        assert!(matches!(f2, SeqFact::Gap { .. }));
        assert_eq!(b.pending_len(), 2);
        assert_eq!(b.bids().best().unwrap().qty.raw(), 2_000_000_000_000);
    }

    #[test]
    fn reset_marks_awaiting_resync_and_buffers() {
        let mut b = seeded(Some(5_000));
        // id 1 under RESET_LOW_ID with a cursor of 5_000 is a restart.
        let f = b
            .apply_delta(delta(mono(1), &[("99", "50")], &[]))
            .unwrap();
        assert_eq!(f, SeqFact::Reset { got: 1 });
        assert!(b.awaiting_resync());
        assert_eq!(b.pending_len(), 1);
        assert_eq!(b.bids().best().unwrap().qty.raw(), 2_000_000_000_000);
        assert_eq!(b.seq().resets(), 1);
    }

    #[test]
    fn scheme_mismatch_marks_the_book_unknown_and_is_not_buffered() {
        let mut b = seeded(Some(100));
        let d = delta(
            SeqInput::IdRange {
                first_id: 101,
                last_id: 105,
            },
            &[("99", "50")],
            &[],
        );
        assert_eq!(b.apply_delta(d).unwrap(), SeqFact::SchemeMismatch);
        assert!(b.awaiting_resync());
        // Not buffered: it would mismatch identically on replay.
        assert_eq!(b.pending_len(), 0);
        assert_eq!(b.bids().best().unwrap().qty.raw(), 2_000_000_000_000);
        assert_eq!(b.seq().scheme_mismatches(), 1);
    }

    #[test]
    fn snapshot_after_a_gap_carries_seq_gap() {
        let mut b = seeded(Some(10));
        let _ = b.apply_delta(delta(mono(15), &[], &[])).unwrap();
        let snap = b.snapshot().unwrap();
        assert!(snap.quality.flags.contains(QualityFlags::SEQ_GAP));
        assert!(!snap.quality.is_trustworthy());
        // And it stays flagged after recovery: the levels either side of the
        // hole were never reconciled and the venue has shown it drops data.
        b.apply_snapshot(
            vec![lvl("99", "2")],
            vec![lvl("100", "3")],
            Some(15),
            ex(),
            ts(),
        )
        .unwrap();
        assert!(!b.awaiting_resync());
        let snap2 = b.snapshot().unwrap();
        assert!(snap2.quality.flags.contains(QualityFlags::SEQ_GAP));
    }

    #[test]
    fn buffered_deltas_replay_in_arrival_order_after_a_snapshot() {
        let mut b = fresh();
        // Three deltas arrive before any snapshot. All touch ask 100, with
        // different sizes, so the final size proves which was applied last.
        let _ = b.apply_delta(delta(mono(90), &[], &[("100", "9")])).unwrap();
        let _ = b.apply_delta(delta(mono(101), &[], &[("100", "5")])).unwrap();
        let _ = b.apply_delta(delta(mono(102), &[], &[("100", "7")])).unwrap();
        assert_eq!(b.pending_len(), 3);

        b.apply_snapshot(
            vec![lvl("99", "2")],
            vec![lvl("100", "1")],
            Some(100),
            ex(),
            ts(),
        )
        .unwrap();

        // 90 <= 100 -> inside the snapshot -> discarded.
        // 101, 102 -> after it -> replayed in order, so 7 wins, not 5 and not 9.
        assert_eq!(b.asks().best().unwrap().qty.raw(), 7_000_000_000_000);
        assert_eq!(b.discarded_pending(), 1);
        assert_eq!(b.pending_len(), 0);
        assert_eq!(b.seq().cursor().unwrap().last_id, 102);
        assert!(!b.awaiting_resync());
        assert_eq!(b.seq().accepted(), 2);
    }

    #[test]
    fn stale_buffered_deltas_are_discarded_not_applied() {
        let mut b = fresh();
        // A pre-snapshot delta that would ADD a level the snapshot does not
        // have. If it were replayed, price 105 would appear out of nowhere.
        let _ = b.apply_delta(delta(mono(95), &[], &[("105", "8")])).unwrap();
        b.apply_snapshot(
            vec![lvl("99", "2")],
            vec![lvl("100", "1")],
            Some(100),
            ex(),
            ts(),
        )
        .unwrap();
        let prices: Vec<i128> = b.asks().levels().iter().map(|l| l.price.raw()).collect();
        assert_eq!(prices, vec![100_000_000_000_000]);
        assert_eq!(b.discarded_pending(), 1);
    }

    #[test]
    fn snapshot_without_a_sequence_discards_every_buffered_delta() {
        let mut b = fresh();
        let _ = b.apply_delta(delta(mono(101), &[], &[("105", "8")])).unwrap();
        let _ = b.apply_delta(delta(mono(102), &[], &[("106", "8")])).unwrap();
        b.apply_snapshot(vec![lvl("99", "2")], vec![lvl("100", "1")], None, ex(), ts())
            .unwrap();
        // Nothing to order them against, so nothing may be replayed.
        let prices: Vec<i128> = b.asks().levels().iter().map(|l| l.price.raw()).collect();
        assert_eq!(prices, vec![100_000_000_000_000]);
        assert_eq!(b.discarded_pending(), 2);
        assert!(b.seq().cursor().is_none());
        assert_eq!(b.snapshot().unwrap().sequence, None);
    }

    #[test]
    fn delta_that_fails_on_the_second_side_leaves_the_book_unchanged() {
        let mut b = seeded(Some(100));
        // Valid bid, then a negative ask size. Validate-first must reject the
        // whole message, so the bid must NOT have been applied.
        let bad = Delta {
            seq: mono(101),
            bids: vec![(p("99"), q("50"))],
            asks: vec![(p("100"), Qty::from_raw(-1))],
            exchange_ts: ex(),
            observed_at: ts(),
        };
        assert!(matches!(b.apply_delta(bad), Err(BookError::NegativeQty)));
        // Still 2 @ 99, not 50.
        assert_eq!(b.bids().best().unwrap().qty.raw(), 2_000_000_000_000);
        assert_eq!(b.asks().best().unwrap().qty.raw(), 3_000_000_000_000);
        // The cursor advanced over a message we refused, so the book must be
        // marked unknown rather than left quietly one message behind.
        assert!(b.awaiting_resync());
    }

    #[test]
    fn delta_with_a_non_positive_price_is_rejected_before_any_mutation() {
        let mut b = seeded(Some(100));
        let bad = Delta {
            seq: mono(101),
            bids: vec![(p("99"), q("50"))],
            asks: vec![(Price::ZERO, q("1"))],
            exchange_ts: ex(),
            observed_at: ts(),
        };
        assert!(matches!(b.apply_delta(bad), Err(BookError::Type(_))));
        assert_eq!(b.bids().best().unwrap().qty.raw(), 2_000_000_000_000);
    }

    #[test]
    fn snapshot_is_all_or_nothing_across_both_sides() {
        let mut b = seeded(Some(100));
        // Good bids, asks with a duplicated price. The bids must not land.
        let r = b.apply_snapshot(
            vec![lvl("90", "1")],
            vec![lvl("100", "1"), lvl("100", "2")],
            Some(200),
            ex(),
            ts(),
        );
        assert!(matches!(r, Err(BookError::DuplicatePrice)));
        // Original book intact: best bid 99 @ 2, best ask 100 @ 3, cursor 100.
        assert_eq!(b.bids().best().unwrap().price.raw(), 99_000_000_000_000);
        assert_eq!(b.bids().len(), 2);
        assert_eq!(b.asks().best().unwrap().qty.raw(), 3_000_000_000_000);
        assert_eq!(b.seq().cursor().unwrap().last_id, 100);
    }

    #[test]
    fn crossed_book_is_flagged_not_normalised() {
        let mut b = fresh();
        // Best bid 100.5, best ask 100 — crossed by 50 cents.
        b.apply_snapshot(
            vec![lvl("100.5", "2")],
            vec![lvl("100", "3")],
            Some(1),
            ex(),
            ts(),
        )
        .unwrap();
        assert!(b.is_crossed());
        let snap = b.snapshot().unwrap();
        assert!(snap.quality.flags.contains(QualityFlags::CROSSED_BOOK));
        // Both levels are still there, unmodified: nothing was "fixed".
        assert_eq!(snap.best_bid().unwrap().price.raw(), 100_500_000_000_000);
        assert_eq!(snap.best_ask().unwrap().price.raw(), 100_000_000_000_000);
        assert_eq!(snap.bids().len(), 1);
        assert_eq!(snap.asks().len(), 1);
    }

    #[test]
    fn ensure_not_crossed_refuses_only_when_crossed() {
        let b = seeded(Some(1));
        assert!(b.ensure_not_crossed().is_ok());
        let mut c = fresh();
        c.apply_snapshot(
            vec![lvl("100", "1")],
            vec![lvl("100", "1")],
            Some(1),
            ex(),
            ts(),
        )
        .unwrap();
        // Equal touch counts as crossed: there is no spread to cross.
        assert!(matches!(
            c.ensure_not_crossed(),
            Err(BookError::CrossedAfterApply)
        ));
    }

    #[test]
    fn depth_at_or_better_buy_walks_asks_and_cuts_mid_ladder() {
        let mut b = fresh();
        b.apply_snapshot(
            vec![lvl("99", "10")],
            vec![
                lvl("100", "1.5"),
                lvl("100.5", "2"),
                lvl("101", "3"),
                lvl("102", "4"),
            ],
            Some(1),
            ex(),
            ts(),
        )
        .unwrap();
        // Buying lifts asks. Limit 100.75 admits 100 and 100.5 and stops:
        // 1.5 + 2 = 3.5 exactly -> 3_500_000_000_000 at scale 1e12.
        assert_eq!(
            b.depth_at_or_better(Side::Buy, p("100.75")).unwrap().raw(),
            3_500_000_000_000
        );
        // Limit exactly on a level includes it: 1.5 + 2 + 3 = 6.5.
        assert_eq!(
            b.depth_at_or_better(Side::Buy, p("101")).unwrap().raw(),
            6_500_000_000_000
        );
        // Below the touch: nothing is available at that price or better.
        assert_eq!(
            b.depth_at_or_better(Side::Buy, p("99.99")).unwrap().raw(),
            0
        );
        // Above everything: 1.5 + 2 + 3 + 4 = 10.5.
        assert_eq!(
            b.depth_at_or_better(Side::Buy, p("500")).unwrap().raw(),
            10_500_000_000_000
        );
    }

    #[test]
    fn depth_at_or_better_sell_walks_bids_and_cuts_mid_ladder() {
        let mut b = fresh();
        b.apply_snapshot(
            vec![
                lvl("99", "2"),
                lvl("98.5", "1.25"),
                lvl("98", "4"),
                lvl("97", "10"),
            ],
            vec![lvl("100", "1")],
            Some(1),
            ex(),
            ts(),
        )
        .unwrap();
        // Selling hits bids. Limit 98.25 admits 99 and 98.5 and stops:
        // 2 + 1.25 = 3.25 exactly -> 3_250_000_000_000.
        assert_eq!(
            b.depth_at_or_better(Side::Sell, p("98.25")).unwrap().raw(),
            3_250_000_000_000
        );
        // Limit exactly on a level includes it: 2 + 1.25 + 4 = 7.25.
        assert_eq!(
            b.depth_at_or_better(Side::Sell, p("98")).unwrap().raw(),
            7_250_000_000_000
        );
        // Above the touch: no bid is that good.
        assert_eq!(
            b.depth_at_or_better(Side::Sell, p("99.01")).unwrap().raw(),
            0
        );
    }

    #[test]
    fn depth_at_or_better_reads_the_opposite_ladder_from_the_taker_side() {
        let mut b = fresh();
        // Deliberately lopsided: total bid size 6, total ask size 11. If the
        // sides were swapped the two assertions below would swap their values.
        b.apply_snapshot(
            vec![lvl("99", "6")],
            vec![lvl("100", "11")],
            Some(1),
            ex(),
            ts(),
        )
        .unwrap();
        assert_eq!(
            b.depth_at_or_better(Side::Buy, p("1000")).unwrap().raw(),
            11_000_000_000_000
        );
        assert_eq!(
            b.depth_at_or_better(Side::Sell, p("0.000000000001"))
                .unwrap()
                .raw(),
            6_000_000_000_000
        );
    }

    #[test]
    fn depth_at_or_better_refuses_while_awaiting_resync() {
        let mut b = seeded(Some(10));
        let _ = b.apply_delta(delta(mono(15), &[], &[])).unwrap();
        assert!(matches!(
            b.depth_at_or_better(Side::Buy, p("1000")),
            Err(BookError::AwaitingResync)
        ));
    }

    #[test]
    fn depth_at_or_better_refuses_before_initialisation() {
        let b = fresh();
        assert!(matches!(
            b.depth_at_or_better(Side::Buy, p("1000")),
            Err(BookError::NotInitialized)
        ));
    }

    #[test]
    fn depth_at_or_better_rejects_a_non_positive_limit() {
        let b = seeded(Some(1));
        assert!(matches!(
            b.depth_at_or_better(Side::Buy, Price::ZERO),
            Err(BookError::Type(_))
        ));
        assert!(matches!(
            b.depth_at_or_better(Side::Sell, Price::from_raw(-1)),
            Err(BookError::Type(_))
        ));
    }

    #[test]
    fn provenance_is_never_upgraded() {
        let mut b = BookState::new(
            vk(),
            SeqScheme::Monotonic,
            Provenance::SynthesizedUnverified,
        );
        // A flawless snapshot and a flawless delta. Clean data does not make an
        // invented fixture measured.
        b.apply_snapshot(
            vec![lvl("99", "2")],
            vec![lvl("100", "3")],
            Some(1),
            ex(),
            ts(),
        )
        .unwrap();
        assert_eq!(
            b.apply_delta(delta(mono(2), &[("99", "4")], &[])).unwrap(),
            SeqFact::Accepted
        );
        let snap = b.snapshot().unwrap();
        assert!(snap.quality.flags.is_empty());
        assert!(matches!(
            snap.quality.provenance,
            Provenance::SynthesizedUnverified
        ));
        assert!(!snap.quality.is_trustworthy());
        assert!(matches!(
            b.provenance(),
            Provenance::SynthesizedUnverified
        ));
    }

    #[test]
    fn max_pending_evicts_the_oldest_and_counts_it() {
        let mut b = fresh();
        let over = 3;
        // Ids 1 ..= MAX_PENDING + 3, buffered because the book is uninitialised.
        for id in 1..=(MAX_PENDING as u64 + over as u64) {
            let _ = b
                .apply_delta(delta(mono(id), &[], &[("100", "1")]))
                .unwrap();
        }
        assert_eq!(b.pending_len(), MAX_PENDING);
        assert_eq!(b.dropped_pending(), over as u64);
        // Oldest-first eviction, so ids 1, 2, 3 are gone and the buffer now
        // starts at 4 and ends at MAX_PENDING + 3.
        assert_eq!(b.pending().first().unwrap().delta.seq.end_id(), 4);
        assert_eq!(
            b.pending().last().unwrap().delta.seq.end_id(),
            MAX_PENDING as u64 + over as u64
        );
    }

    #[test]
    fn book_empty_flag_when_a_side_is_missing() {
        let mut b = fresh();
        b.apply_snapshot(vec![lvl("99", "2")], Vec::new(), Some(1), ex(), ts())
            .unwrap();
        let snap = b.snapshot().unwrap();
        assert!(snap.quality.flags.contains(QualityFlags::BOOK_EMPTY));
        // Empty is not crossed: different cause, different flag.
        assert!(!snap.quality.flags.contains(QualityFlags::CROSSED_BOOK));
        assert!(!b.is_crossed());
    }

    #[test]
    fn partial_book_flag_when_a_ladder_is_truncated() {
        use crate::ladder::MAX_LEVELS;
        let mut b = fresh();
        // MAX_LEVELS + 1 asks at 1000 ..= 1000 + MAX_LEVELS, one bid below them.
        let asks: Vec<Level> = (0..=MAX_LEVELS as i64)
            .map(|i| Level::new(Price::from_int(1000 + i).unwrap(), Qty::ONE).unwrap())
            .collect();
        b.apply_snapshot(vec![lvl("999", "1")], asks, Some(1), ex(), ts())
            .unwrap();
        assert_eq!(b.asks().len(), MAX_LEVELS);
        assert!(b.asks().truncated());
        let snap = b.snapshot().unwrap();
        assert!(snap.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
        // Depth from a truncated ladder is a lower bound: MAX_LEVELS levels of
        // Qty::ONE each, so exactly MAX_LEVELS * 1e12 raw.
        assert_eq!(
            b.depth_at_or_better(Side::Buy, p("1000000")).unwrap().raw(),
            MAX_LEVELS as i128 * 1_000_000_000_000
        );
    }

    #[test]
    fn snapshot_before_initialisation_is_an_error() {
        let b = fresh();
        assert!(matches!(b.snapshot(), Err(BookError::NotInitialized)));
        assert!(!b.initialized());
    }

    #[test]
    fn timestamps_follow_the_last_applied_message() {
        let mut b = seeded(Some(100));
        assert_eq!(b.exchange_ts().unwrap().millis(), 1_700_000_000_123);
        let d = Delta {
            seq: mono(101),
            bids: Vec::new(),
            asks: Vec::new(),
            exchange_ts: Some(WallTs::from_millis(1_700_000_009_000)),
            observed_at: LocalTs::from_millis(1_700_000_009_050),
        };
        assert_eq!(b.apply_delta(d).unwrap(), SeqFact::Accepted);
        assert_eq!(b.exchange_ts().unwrap().millis(), 1_700_000_009_000);
        assert_eq!(b.observed_at().millis(), 1_700_000_009_050);
        // A message with no exchange time clears the field rather than leaving
        // a stale one that would read as freshness.
        let d2 = Delta {
            seq: mono(102),
            bids: Vec::new(),
            asks: Vec::new(),
            exchange_ts: None,
            observed_at: LocalTs::from_millis(1_700_000_009_100),
        };
        assert_eq!(b.apply_delta(d2).unwrap(), SeqFact::Accepted);
        assert!(b.exchange_ts().is_none());
        assert_eq!(b.snapshot().unwrap().exchange_ts, None);
    }

    #[test]
    fn a_snapshot_is_stamped_with_the_age_of_its_newest_data_and_not_with_a_clock() {
        // The defect this pins used to be reachable through the signature:
        // `snapshot(observed_at)` let the caller stamp a book that had not updated
        // in a minute with the current time, so its age was zero for ever and
        // every staleness check downstream read Fresh. There is now no argument
        // to get wrong — the only value a snapshot can carry is the one the state
        // recorded when the newest message arrived.
        let mut b = seeded(Some(100));
        assert_eq!(b.snapshot().unwrap().observed_at, b.observed_at());
        assert_eq!(b.snapshot().unwrap().observed_at.millis(), ts().millis());

        // And it advances with the DATA, so this is not "always old" either: a
        // delta that really arrives moves the stamp forward by exactly its own
        // observation time.
        let d = Delta {
            seq: mono(101),
            bids: Vec::new(),
            asks: Vec::new(),
            exchange_ts: Some(WallTs::from_millis(1_700_000_009_000)),
            observed_at: LocalTs::from_millis(1_700_000_009_050),
        };
        assert_eq!(b.apply_delta(d).unwrap(), SeqFact::Accepted);
        assert_eq!(
            b.snapshot().unwrap().observed_at.millis(),
            1_700_000_009_050
        );

        // A rejected message must not move it. A duplicate sequence id changes
        // nothing about the book, so it must change nothing about its age —
        // otherwise a venue replaying old messages would keep a dead book fresh.
        let dup = Delta {
            seq: mono(101),
            bids: Vec::new(),
            asks: Vec::new(),
            exchange_ts: Some(WallTs::from_millis(1_700_000_020_000)),
            observed_at: LocalTs::from_millis(1_700_000_020_050),
        };
        let fact = b.apply_delta(dup).unwrap();
        assert_ne!(fact, SeqFact::Accepted);
        assert_eq!(
            b.snapshot().unwrap().observed_at.millis(),
            1_700_000_009_050,
            "a message that was not applied must not refresh the book's age"
        );
    }

    #[test]
    fn vkey_is_carried_through_to_the_snapshot() {
        let b = seeded(Some(1));
        let snap = b.snapshot().unwrap();
        assert_eq!(snap.vkey.venue.as_str(), b.vkey().venue.as_str());
        assert_eq!(snap.vkey.key.base.as_str(), "BTC");
    }

    #[test]
    fn deleting_the_last_level_of_a_side_raises_book_empty() {
        let mut b = fresh();
        b.apply_snapshot(
            vec![lvl("99", "2")],
            vec![lvl("100", "3")],
            Some(1),
            ex(),
            ts(),
        )
        .unwrap();
        assert!(b.snapshot().unwrap().quality.flags.is_empty());
        // Zero size deletes, leaving the ask side genuinely empty rather than
        // holding a phantom level at size zero.
        assert_eq!(
            b.apply_delta(delta(mono(2), &[], &[("100", "0")])).unwrap(),
            SeqFact::Accepted
        );
        assert!(b.asks().is_empty());
        let snap = b.snapshot().unwrap();
        assert!(snap.quality.flags.contains(QualityFlags::BOOK_EMPTY));
        assert_eq!(
            b.depth_at_or_better(Side::Buy, p("1000")).unwrap().raw(),
            0
        );
    }

    #[test]
    fn replay_that_reopens_a_gap_leaves_the_book_awaiting_resync() {
        let mut b = fresh();
        // Buffered 105 and 106: contiguous with each other but NOT with the
        // snapshot at 100, so the replay must detect the hole rather than paper
        // over it by applying them.
        let _ = b.apply_delta(delta(mono(105), &[], &[("100", "9")])).unwrap();
        let _ = b.apply_delta(delta(mono(106), &[], &[("100", "8")])).unwrap();
        b.apply_snapshot(
            vec![lvl("99", "2")],
            vec![lvl("100", "1")],
            Some(100),
            ex(),
            ts(),
        )
        .unwrap();
        assert!(b.awaiting_resync());
        // Neither buffered delta was applied: the ask is the snapshot's 1.
        assert_eq!(b.asks().best().unwrap().qty.raw(), 1_000_000_000_000);
        // Both are held for the next snapshot.
        assert_eq!(b.pending_len(), 2);
        assert!(b
            .snapshot()
            .unwrap()
            .quality
            .flags
            .contains(QualityFlags::SEQ_GAP));
    }
}
