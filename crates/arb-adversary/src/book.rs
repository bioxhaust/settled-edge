//! Book maintenance. Three ways the ladder in memory stops being the venue's
//! ladder, and one word that means opposite things in two crates.
//!
//! The load-bearing assumption of `arb-book` is that a depth update carries the
//! ABSOLUTE size of a level, so `apply` replaces and never adds. A venue that
//! actually publishes increments would corrupt us silently — sizes drift by a
//! few percent, the touch still looks plausible, and twelve basis points of edge
//! becomes noise. That assumption cannot be tested here (it is a fact about the
//! venue, and nothing in this repository has ever spoken to a venue). What CAN
//! be tested is that the code implements the assumption it documents, so that a
//! measurement contradicting it has exactly one place to be fixed.

use arb_book::{BookState, Delta, Ladder, SeqFact, SeqInput, SeqScheme};
use arb_num::{Price, Qty};
use arb_types::{LocalTs, Provenance, QualityFlags, Side, WallTs};

use crate::support::{level, linear_vkey, price, qty};

fn state() -> BookState {
    let mut st = BookState::new(
        linear_vkey(),
        SeqScheme::Monotonic,
        Provenance::RecordedFixture,
    );
    st.apply_snapshot(
        vec![level("100", "1"), level("99", "2")],
        vec![level("101", "4"), level("102", "8")],
        Some(10),
        Some(WallTs::from_millis(1_000)),
        LocalTs::from_millis(1_000),
    )
    .expect("sorted two-sided snapshot");
    st
}

fn delta(id: u64, bids: Vec<(Price, Qty)>, asks: Vec<(Price, Qty)>) -> Delta {
    Delta {
        seq: SeqInput::Monotonic { id },
        bids,
        asks,
        exchange_ts: Some(WallTs::from_millis(2_000)),
        observed_at: LocalTs::from_millis(2_000),
    }
}

#[test]
fn a_level_update_is_absolute_and_never_cumulative() {
    // The assumption, implemented. Set 5, then set 3: the level is 3. If it were
    // 8 the crate would be treating the feed as incremental, and every size on
    // the screen would grow without bound while the prices stayed plausible.
    let mut l = Ladder::new(Side::Sell);
    l.apply(price("101"), qty("5")).expect("set");
    assert_eq!(l.best().map(|b| b.qty), Some(qty("5")));
    l.apply(price("101"), qty("3")).expect("replace");
    assert_eq!(l.best().map(|b| b.qty), Some(qty("3")));
    assert_eq!(l.len(), 1);
    assert_eq!(l.total_qty().expect("sum"), qty("3"));
}

#[test]
fn a_zero_size_deletes_and_a_negative_size_is_refused() {
    let mut l = Ladder::new(Side::Sell);
    l.apply(price("101"), qty("5")).expect("set");
    l.apply(price("101"), Qty::ZERO).expect("delete");
    assert!(l.is_empty());
    // Deleting a level we never held is normal traffic, not an error.
    l.apply(price("999"), Qty::ZERO).expect("no-op delete");
    assert!(l.is_empty());
    // A negative absolute size is meaningless. Clamping it to zero would delete
    // a level the venue never asked us to delete.
    assert!(l.apply(price("101"), qty("-1")).is_err());
    assert!(l.apply(Price::ZERO, qty("1")).is_err());
    assert!(l.apply(price("-1"), qty("1")).is_err());
}

#[test]
fn each_ladder_is_sorted_best_first_for_its_own_side() {
    let mut bids = Ladder::new(Side::Buy);
    for p in ["99", "101", "100"] {
        bids.apply(price(p), qty("1")).expect("set");
    }
    // A bid ladder is descending: the best bid is the HIGHEST price.
    assert_eq!(
        bids.levels().iter().map(|l| l.price).collect::<Vec<_>>(),
        vec![price("101"), price("100"), price("99")]
    );
    assert_eq!(bids.best().map(|l| l.price), Some(price("101")));

    let mut asks = Ladder::new(Side::Sell);
    for p in ["102", "100", "101"] {
        asks.apply(price(p), qty("1")).expect("set");
    }
    // An ask ladder is ascending: the best ask is the LOWEST price.
    assert_eq!(
        asks.levels().iter().map(|l| l.price).collect::<Vec<_>>(),
        vec![price("100"), price("101"), price("102")]
    );
    assert_eq!(asks.best().map(|l| l.price), Some(price("100")));
}

#[test]
fn the_word_side_means_opposite_things_in_arb_book_and_arb_types() {
    // `Ladder::new(Side::Buy)` is the ladder of RESTING BUY orders — the bids,
    // which is where WE sell. `BookSnapshot::taking_levels(Side::Buy)` is the
    // ladder OUR buy would take — the asks. Same enum, same variant, opposite
    // ladders, one crate apart.
    //
    // This is not a bug in either crate; each is self-consistent. It is pinned
    // because it is the exact shape of mistake that makes a scanner read the
    // spread backwards, and because a future "let us unify these" change would
    // silently invert one of them.
    let st = state();
    let snap = st.snapshot().expect("snapshot");

    assert_eq!(st.bids().side(), Side::Buy);
    assert_eq!(st.bids().best().map(|l| l.price), Some(price("100")));
    assert_eq!(snap.taking_levels(Side::Buy).first().map(|l| l.price), Some(price("101")));
    assert_eq!(snap.taking_levels(Side::Sell).first().map(|l| l.price), Some(price("100")));
    // Stated as the identity that must hold: the ladder a Buy TAKES is the
    // ladder built for Side::Sell.
    assert_eq!(snap.taking_levels(Side::Buy), snap.asks());
    assert_eq!(st.asks().side(), Side::Sell);
}

#[test]
fn depth_at_or_better_walks_the_ladder_a_taker_would_hit() {
    // Asks 101x4 and 102x8, bids 100x1 and 99x2.
    // A BUYER at a limit of 101 can take 4: the 102 level is worse than the
    // limit and everything behind it is worse still.
    let st = state();
    assert_eq!(st.depth_at_or_better(Side::Buy, price("101")).expect("ok"), qty("4"));
    assert_eq!(st.depth_at_or_better(Side::Buy, price("102")).expect("ok"), qty("12"));
    // A buyer at 100.99 can take nothing: there is no ask at or below it.
    assert_eq!(st.depth_at_or_better(Side::Buy, price("100.99")).expect("ok"), Qty::ZERO);
    // A SELLER at a limit of 100 can hit 1; at 99, both bids.
    assert_eq!(st.depth_at_or_better(Side::Sell, price("100")).expect("ok"), qty("1"));
    assert_eq!(st.depth_at_or_better(Side::Sell, price("99")).expect("ok"), qty("3"));
    assert_eq!(st.depth_at_or_better(Side::Sell, price("100.01")).expect("ok"), Qty::ZERO);
    // A non-positive limit is a broken input, not "everything".
    assert!(st.depth_at_or_better(Side::Buy, Price::ZERO).is_err());
}

// ---------------------------------------------------------------------------
// Sequence holes. A gap means the ladder is UNKNOWN, not merely late.
// ---------------------------------------------------------------------------

#[test]
fn a_gap_does_not_touch_the_ladder_and_marks_the_book_unknown() {
    // Snapshot at id 10, next expected id 11. Id 12 arrives: id 11 is a ladder
    // state we never saw, and it may have deleted the level we are about to
    // price against. Nothing in a later message will ever tell us so.
    let mut st = state();
    let fact = st
        .apply_delta(delta(12, vec![(price("100"), qty("999"))], Vec::new()))
        .expect("classified, not rejected");
    assert_eq!(fact, SeqFact::Gap { expected: 11, got: 12 });
    // The 999 must NOT be in the ladder: the delta was buffered, not applied.
    assert_eq!(st.bids().best().map(|l| l.qty), Some(qty("1")));
    assert!(st.awaiting_resync());
    assert_eq!(st.pending_len(), 1);
    assert_eq!(st.seq().gaps(), 1);
    // And a bare number cannot be produced while the ladder is unknown: a `Qty`
    // has nowhere to carry the doubt, so refusing is the only way not to launder
    // it into a confident figure.
    assert!(st.depth_at_or_better(Side::Buy, price("102")).is_err());
}

#[test]
fn a_gap_flags_the_snapshot_and_the_flag_survives_the_resync() {
    let mut st = state();
    st.apply_delta(delta(12, Vec::new(), Vec::new())).expect("gap");
    let during = st.snapshot().expect("snapshot");
    assert!(during.quality.flags.contains(QualityFlags::SEQ_GAP));

    // A clean resync repairs the ladder but not the venue's reputation: this
    // feed has demonstrated that it drops messages, and these levels were
    // rebuilt across a hole whose contents nobody ever saw.
    st.apply_snapshot(
        vec![level("100", "1")],
        vec![level("101", "4")],
        Some(20),
        Some(WallTs::from_millis(3_000)),
        LocalTs::from_millis(3_000),
    )
    .expect("resync");
    assert!(!st.awaiting_resync());
    let after = st.snapshot().expect("snapshot");
    assert!(
        after.quality.flags.contains(QualityFlags::SEQ_GAP),
        "one gap flags this book for the rest of its life, by design"
    );
    // The buffered delta was strictly older than the resync id, so it was
    // discarded rather than replayed over newer sizes.
    assert_eq!(st.discarded_pending(), 1);
    assert_eq!(st.pending_len(), 0);
}

#[test]
fn a_duplicate_and_a_late_message_both_leave_the_ladder_alone() {
    let mut st = state();
    // id 11 is the message we expect. Apply it: the bid at 100 becomes 7.
    assert_eq!(
        st.apply_delta(delta(11, vec![(price("100"), qty("7"))], Vec::new()))
            .expect("accepted"),
        SeqFact::Accepted
    );
    assert_eq!(st.bids().best().map(|l| l.qty), Some(qty("7")));

    // The venue re-sends id 11 carrying its own older size. Re-applying an
    // absolute size we have already applied is harmless here only because it is
    // the same message; the rule exists because a re-delivery of an OLDER
    // message would move the level backwards in time.
    assert_eq!(
        st.apply_delta(delta(11, vec![(price("100"), qty("1"))], Vec::new()))
            .expect("classified"),
        SeqFact::Duplicate
    );
    assert_eq!(st.bids().best().map(|l| l.qty), Some(qty("7")), "a duplicate must not be applied");
    assert!(!st.awaiting_resync(), "a duplicate is harmless and must not trigger a resync");

    // Now id 20 opens a gap (cursor stays at 11, high water mark 20), and id 15
    // turns up afterwards. 15 is past the id we expected but BEHIND one we have
    // already seen, so it is not the venue skipping forward — it is a late
    // delivery arriving under something that overtook it. Data we never applied
    // and cannot prove we may skip: the book stays unknown.
    st.apply_delta(delta(20, Vec::new(), Vec::new())).expect("gap");
    let fact = st
        .apply_delta(delta(15, vec![(price("100"), qty("55"))], Vec::new()))
        .expect("classified");
    assert_eq!(fact, SeqFact::OutOfOrder { expected: 12, got: 15 });
    assert_eq!(st.bids().best().map(|l| l.qty), Some(qty("7")));
    assert!(st.awaiting_resync());
    assert_eq!(st.seq().out_of_order(), 1);
    assert_eq!(st.seq().duplicates(), 1);
    assert_eq!(st.seq().gaps(), 1);
}

#[test]
fn even_a_perfectly_sequenced_delta_is_not_applied_while_the_book_is_unknown() {
    // Cursor at 10. Id 12 arrives and opens a gap; the cursor does NOT advance,
    // so the tracker still expects 11. Id 11 then turns up late, and from the
    // tracker's point of view it is exactly the next message — `SeqFact::Accepted`
    // is the honest classification of the MESSAGE.
    //
    // It must still not reach the ladder. The hole at 12 is unfilled, so the
    // ladder is unknown, and applying an absolute size on top of an unknown
    // ladder produces a book that is part observed and part invented with no
    // symptom. The fact describes the message; the decision belongs to the state
    // machine, and the two must not be confused.
    let mut st = state();
    st.apply_delta(delta(12, Vec::new(), Vec::new())).expect("gap");
    let fact = st
        .apply_delta(delta(11, vec![(price("100"), qty("777"))], Vec::new()))
        .expect("classified");
    assert_eq!(fact, SeqFact::Accepted);
    assert_eq!(
        st.bids().best().map(|l| l.qty),
        Some(qty("1")),
        "a delta must not be applied while the ladder is unknown, whatever its sequence says"
    );
    assert!(st.awaiting_resync());
    assert_eq!(st.pending_len(), 2, "both the gap opener and the late message are held for replay");
}

#[test]
fn a_delta_arriving_before_the_first_snapshot_is_buffered_not_applied() {
    // There is no base state for an absolute-size update to sit on, and the
    // snapshot's own id is the only honest origin for the cursor.
    let mut st = BookState::new(linear_vkey(), SeqScheme::Monotonic, Provenance::RecordedFixture);
    let fact = st
        .apply_delta(delta(500, vec![(price("100"), qty("1"))], Vec::new()))
        .expect("classified");
    assert_eq!(fact, SeqFact::Unknown);
    assert!(!st.initialized());
    assert_eq!(st.pending_len(), 1);
    assert!(st.seq().cursor().is_none(), "a pre-snapshot delta must not set the cursor");
    // And no number can be produced from a book that has never been observed.
    assert!(st.snapshot().is_err());
    assert!(st.depth_at_or_better(Side::Buy, price("101")).is_err());
}

#[test]
fn a_crossed_book_is_flagged_and_never_normalised() {
    // A bid at or above the ask cannot persist inside one matching engine, so
    // the touch prices are not real prices. Either we stitched two update
    // generations together or we are decoding the feed wrong, and the snapshot
    // cannot tell which — so both sides are reported as they are, flagged.
    let mut st = state();
    st.apply_delta(delta(11, vec![(price("103"), qty("1"))], Vec::new()))
        .expect("accepted");
    assert!(st.is_crossed());
    assert!(st.ensure_not_crossed().is_err());
    let snap = st.snapshot().expect("snapshot");
    assert!(snap.quality.flags.contains(QualityFlags::CROSSED_BOOK));
    assert_eq!(snap.best_bid().map(|l| l.price), Some(price("103")));
    assert_eq!(snap.best_ask().map(|l| l.price), Some(price("101")));
    assert!(arb_detect::is_crossed(&snap), "both crates must agree on what crossed means");
}

#[test]
fn a_full_refresh_with_two_sizes_at_one_price_is_refused() {
    // Two sizes at one price means one of them is wrong and we cannot know
    // which, so the refresh is refused rather than resolved by picking a winner.
    let mut st = BookState::new(linear_vkey(), SeqScheme::Monotonic, Provenance::RecordedFixture);
    assert!(st
        .apply_snapshot(
            vec![level("100", "1"), level("100", "2")],
            vec![level("101", "1")],
            Some(1),
            None,
            LocalTs::from_millis(1_000),
        )
        .is_err());
    // The failed refresh installed nothing at all.
    assert!(!st.initialized());
}

#[test]
fn provenance_is_carried_to_the_screen_and_never_upgraded() {
    // Rule 7. A book assembled from an invented fixture stays
    // SynthesizedUnverified no matter how much arithmetic happens on top of it,
    // because no arithmetic can turn a guess into a measurement.
    let mut st = BookState::new(
        linear_vkey(),
        SeqScheme::Monotonic,
        Provenance::SynthesizedUnverified,
    );
    st.apply_snapshot(
        vec![level("100", "1")],
        vec![level("101", "1")],
        Some(1),
        None,
        LocalTs::from_millis(1_000),
    )
    .expect("snapshot");
    let snap = st.snapshot().expect("snapshot");
    assert_eq!(snap.quality.provenance, Provenance::SynthesizedUnverified);
    assert!(!snap.quality.is_trustworthy());
    assert_eq!(
        Provenance::LiveMeasured.worst(Provenance::SynthesizedUnverified),
        Provenance::SynthesizedUnverified
    );
    assert_eq!(
        Provenance::RecordedFixture.worst(Provenance::LiveMeasured),
        Provenance::RecordedFixture
    );
}
