//! Staleness. The failure mode named in the brief: **a stale quote that still
//! gets ranked**.
//!
//! Two independent gates decide that, and they are owned by different crates:
//! `arb-detect`'s trust score, and the `QualityFlags` carried on the snapshot
//! itself. A stale book must fail both. Where they disagree, a caller reading
//! only one of them ranks a price that no longer exists.
//!
//! Every age here is on the LOCAL clock — `now - observed_at`. The exchange
//! clock cannot be used for this (decision 008), which is why the last section
//! checks that a lying exchange stamp is caught by the skew detector instead of
//! quietly shrinking every age.

use arb_book::{BookState, SeqScheme};
use arb_detect::trust::{
    CLOCK_AHEAD_BASE_PENALTY, CLOCK_AHEAD_MAX_EXTRA, MIN_RANKABLE, MIN_TRADEABLE,
    PAINT_UNASSESSED_PENALTY, STALENESS_STALE_PENALTY, STALENESS_WARN_PENALTY,
};
use arb_detect::{
    age_of, assess_age, assess_skew, assess_venue, flags_for, ClockVerdict, SkewLimits, Staleness,
    StalenessLimits, TrustNote, VenueEvidence,
};
use arb_types::{DataQuality, LocalTs, Millis, Provenance, QualityFlags, Venue, WallTs};

use crate::support::{level, linear_vkey};

const BOOK: StalenessLimits = StalenessLimits::BOOK;
const FUNDING: StalenessLimits = StalenessLimits::FUNDING;

fn ms(v: i64) -> Millis {
    Millis::from_millis(v)
}

/// Evidence with every OTHER detector clean, so the score moves only with the
/// staleness verdict under test.
fn evidence(staleness: Staleness) -> VenueEvidence {
    VenueEvidence {
        staleness,
        ..VenueEvidence::clean(Venue::Binance)
    }
}

#[test]
fn age_is_now_minus_observed_and_nothing_else() {
    // Observed at 1_000 on our clock, now 1_600: the observation is 600 ms old.
    // A subtraction the other way round yields -600, which `assess_age` reads
    // as Dead — so an inverted `age_of` would fail loudly rather than silently,
    // and this test pins which way round it is.
    assert_eq!(age_of(LocalTs::from_millis(1_000), LocalTs::from_millis(1_600)), ms(600));
    assert_eq!(age_of(LocalTs::EPOCH, LocalTs::from_millis(2_000)), ms(2_000));
}

#[test]
fn every_staleness_boundary_belongs_to_the_worse_bucket() {
    // BOOK = warn 250, stale 500, dead 2000. At a boundary the threshold has
    // already been reached, so the worse verdict is the correct one: an age of
    // exactly 500 ms is Stale, not Warn. Derived from the definition of ">=",
    // not from the code: if being exactly on a threshold were acceptable, the
    // threshold would be one ulp higher.
    assert_eq!(assess_age(ms(0), BOOK), Staleness::Fresh);
    assert_eq!(assess_age(ms(249), BOOK), Staleness::Fresh);
    assert_eq!(assess_age(ms(250), BOOK), Staleness::Warn);
    assert_eq!(assess_age(ms(499), BOOK), Staleness::Warn);
    assert_eq!(assess_age(ms(500), BOOK), Staleness::Stale);
    assert_eq!(assess_age(ms(1_999), BOOK), Staleness::Stale);
    assert_eq!(assess_age(ms(2_000), BOOK), Staleness::Dead);
    assert_eq!(assess_age(ms(86_400_000), BOOK), Staleness::Dead);
}

#[test]
fn an_observation_from_the_future_is_dead_not_fresh() {
    // A negative age means our own clock arithmetic is broken, so the age we
    // just computed is worthless. `age < warn_after` — the naive comparison
    // this function exists to replace — returns Fresh for every negative value,
    // which is the single cheapest way to trade a quote of unknown vintage.
    assert_eq!(assess_age(ms(-1), BOOK), Staleness::Dead);
    assert_eq!(assess_age(ms(-10_000), BOOK), Staleness::Dead);
    assert_eq!(assess_age(ms(i64::MIN), BOOK), Staleness::Dead);
}

#[test]
fn the_worse_of_two_verdicts_wins_and_the_order_is_fresh_to_dead() {
    // A fresh funding rate does not rescue a stale book. Ord must run
    // Fresh < Warn < Stale < Dead so that `max` and `worse_of` agree.
    assert!(Staleness::Fresh < Staleness::Warn);
    assert!(Staleness::Warn < Staleness::Stale);
    assert!(Staleness::Stale < Staleness::Dead);
    assert_eq!(Staleness::Fresh.worse_of(Staleness::Stale), Staleness::Stale);
    assert_eq!(Staleness::Dead.worse_of(Staleness::Fresh), Staleness::Dead);
    assert_eq!(Staleness::Warn.worse_of(Staleness::Warn), Staleness::Warn);
}

#[test]
fn book_limits_and_funding_limits_are_not_interchangeable() {
    // 300_000 ms is five minutes. Against the funding limits that is exactly
    // Stale; against the book limits it is long Dead. Same age, two verdicts —
    // so a caller that reaches for the wrong constant gets a wrong answer with
    // no type error. Pinned because the two sets differ by 600x at the stale
    // line and a book judged on funding limits would read Fresh for four
    // minutes fifty-nine seconds.
    assert_eq!(assess_age(ms(300_000), FUNDING), Staleness::Stale);
    assert_eq!(assess_age(ms(300_000), BOOK), Staleness::Dead);
    assert_eq!(assess_age(ms(59_999), FUNDING), Staleness::Fresh);
    assert_eq!(assess_age(ms(59_999), BOOK), Staleness::Dead);
    assert!(BOOK.is_ordered() && FUNDING.is_ordered());
    assert_eq!(BOOK.stale_after.millis() * 600, FUNDING.stale_after.millis());
}

// ---------------------------------------------------------------------------
// Gate 1: the trust score. Gate 2: the flags on the snapshot. A stale quote
// must fail both, and the two must not disagree about what "rankable" means.
// ---------------------------------------------------------------------------

#[test]
fn a_stale_book_is_displayable_but_never_tradeable() {
    // Hand derivation. Every other detector clean, paint assessed and empty, so
    // the only deduction is staleness:
    //   score = MAX_SCORE - STALENESS_STALE_PENALTY = 100 - 55 = 45.
    //   45 >= MIN_RANKABLE (40)  -> it appears in the ranking, greyed out.
    //   45 <  MIN_TRADEABLE (60) -> no order may be sent.
    // The gap is deliberate and documented: an operator needs to see the row
    // and why it is unusable. It is pinned here because 55 is only five points
    // clear of the tradeable line, and any future penalty re-tune that lifts
    // the stale score to 60 makes a stale book tradeable with no other change.
    let ev = VenueEvidence {
        staleness: Staleness::Stale,
        paint: Some(arb_detect::PaintReport { signals: Vec::new(), score: 0 }),
        ..VenueEvidence::clean(Venue::Binance)
    };
    let t = assess_venue(&ev);
    assert_eq!(t.score, 45);
    assert_eq!(i32::from(t.score), 100 - STALENESS_STALE_PENALTY);
    assert!(t.is_rankable());
    assert!(!t.is_tradeable());
    assert!(!t.has_fatal());
    assert!(t.notes.contains(&TrustNote::Aging(Staleness::Stale)));
    // And the second gate agrees that it must not be traded.
    assert!(flags_for(&ev).contains(QualityFlags::STALE));
}

#[test]
fn the_stale_flag_blocks_trading_and_destroys_trustworthiness() {
    // The law, not the implementation: a quote we know is stale may never be
    // executed. Both gates are checked because a caller holds one or the other,
    // never reliably both.
    assert!(QualityFlags::STALE.is_blocking());
    let q = DataQuality::new(QualityFlags::STALE, Provenance::LiveMeasured);
    assert!(!q.is_trustworthy(), "a flagged quote is not trustworthy even when measured");
    assert!(!q.is_tradeable(), "a stale quote must never be executed");
    // Measured and unflagged is the only trustworthy combination.
    assert!(DataQuality::MEASURED.is_trustworthy());
    // Rule 7: a fixture-derived number stays untrustworthy for ever, and
    // combining cannot launder it.
    assert!(!DataQuality::SYNTHESIZED.is_trustworthy());
    let combined = DataQuality::MEASURED.combine(DataQuality::SYNTHESIZED);
    assert_eq!(combined.provenance, Provenance::SynthesizedUnverified);
    assert!(!combined.is_trustworthy());
}

#[test]
fn a_warn_book_ranks_behind_a_fresh_one_without_being_excluded() {
    // 100 - 10 = 90. Still tradeable, still rankable, but strictly below the
    // fresh venue's 100 — which is the whole point of a 10-point deduction: it
    // is a tiebreaker between two rows showing the same edge.
    let clean_paint = arb_detect::PaintReport { signals: Vec::new(), score: 0 };
    let warn = VenueEvidence {
        staleness: Staleness::Warn,
        paint: Some(clean_paint.clone()),
        ..VenueEvidence::clean(Venue::Binance)
    };
    let fresh = VenueEvidence {
        paint: Some(clean_paint),
        ..VenueEvidence::clean(Venue::Binance)
    };
    let (w, f) = (assess_venue(&warn), assess_venue(&fresh));
    assert_eq!(i32::from(w.score), 100 - STALENESS_WARN_PENALTY);
    assert_eq!(w.score, 90);
    assert_eq!(f.score, 100);
    assert!(w.score < f.score);
    assert!(w.is_tradeable() && w.is_rankable());
    // Warn deliberately does NOT raise STALE: blocking on a 250 ms old book
    // would stop most trading for no measured reason. Locked so that a future
    // "tighten the flags" change has to argue with this test first.
    assert!(!flags_for(&warn).contains(QualityFlags::STALE));
    assert!(flags_for(&warn).is_empty());
}

#[test]
fn dead_data_floors_the_score_no_matter_what_else_is_perfect() {
    let ev = evidence(Staleness::Dead);
    let t = assess_venue(&ev);
    assert_eq!(t.score, 0);
    assert!(!t.is_rankable(), "a dead price may not even be displayed in the ranking");
    assert!(!t.is_tradeable());
    assert!(t.has_fatal());
    assert!(t.notes.contains(&TrustNote::FatalDataDead));
    // Dead is worse than Stale, so it must raise the same blocking flag.
    assert!(flags_for(&ev).contains(QualityFlags::STALE));
}

#[test]
fn an_unassessed_paint_check_is_scored_as_a_defect_not_as_a_zero() {
    // `VenueEvidence::clean` leaves `paint: None`, and None means "we did not
    // look", which costs PAINT_UNASSESSED_PENALTY = 20. So the base score of a
    // venue built from `clean` is 80, not 100 — worth pinning because the
    // constructor's doc comment says "a paint check that found nothing", which
    // is a different thing and would score 100.
    let not_assessed = assess_venue(&VenueEvidence::clean(Venue::Binance));
    assert_eq!(i32::from(not_assessed.score), 100 - PAINT_UNASSESSED_PENALTY);
    assert!(not_assessed.notes.contains(&TrustNote::PaintNotAssessed));
    // Consequence worth stating: stale plus unassessed = 100 - 55 - 20 = 25,
    // which is below MIN_RANKABLE, so the row disappears entirely rather than
    // appearing greyed out.
    let stale_unassessed = assess_venue(&evidence(Staleness::Stale));
    assert_eq!(stale_unassessed.score, 25);
    assert!(!stale_unassessed.is_rankable());
}

// ---------------------------------------------------------------------------
// The laundering path: where a stale book acquires a fresh timestamp.
// ---------------------------------------------------------------------------

fn fresh_state() -> BookState {
    let mut st = BookState::new(
        linear_vkey(),
        SeqScheme::Monotonic,
        Provenance::RecordedFixture,
    );
    st.apply_snapshot(
        vec![level("100", "1")],
        vec![level("101", "1")],
        Some(10),
        Some(WallTs::from_millis(1_000)),
        LocalTs::from_millis(1_000),
    )
    .expect("a two-sided sorted snapshot is valid");
    st
}

#[test]
fn a_book_snapshot_may_not_be_stamped_fresher_than_its_newest_data() {
    // The book's last and only message was observed at t = 1_000. Nothing has
    // arrived since. At t = 60_000 the freshest datum in this book is 59_000 ms
    // old, so:
    //   age  = now - observed_at_of_newest_data = 60_000 - 1_000 = 59_000 ms
    //   59_000 >= dead_after (2_000)            -> Staleness::Dead
    // `BookState` already holds the correct value and exposes it as
    // `observed_at()`; `snapshot()` ignored it and stamped the snapshot with
    // whatever the caller passed, so `snapshot(now)` — the obvious call, and the
    // one the parameter name invited — produced a snapshot whose age was zero for
    // ever. Every downstream staleness check read Fresh on a book that had not
    // updated in a minute.
    //
    // Fixed by removing the parameter (decisions/018), so the call below no longer
    // passes `now`. The assertions are unchanged: they never named the mechanism,
    // only the age the snapshot must admit to.
    let st = fresh_state();
    let now = LocalTs::from_millis(60_000);

    // The truth is in the state, and it is available.
    assert_eq!(st.observed_at(), LocalTs::from_millis(1_000));
    assert_eq!(assess_age(age_of(st.observed_at(), now), BOOK), Staleness::Dead);

    // Fix-agnostic: refusing to emit a snapshot at all is an acceptable fix, so
    // an error here is a pass.
    match st.snapshot() {
        Err(_) => {}
        Ok(snap) => {
            let age = age_of(snap.observed_at, now);
            assert!(
                age.millis() >= 59_000,
                "snapshot claims an age of {} ms for data observed 59_000 ms ago",
                age.millis()
            );
            assert_eq!(assess_age(age, BOOK), Staleness::Dead);
        }
    }
}

#[test]
fn the_tracked_observation_time_advances_with_the_data_and_not_with_the_clock() {
    // Same book, but a delta really does arrive at t = 59_900. Now the book is
    // 100 ms old at t = 60_000 and Fresh is the correct verdict — which is why
    // the previous test is not asking for "always Dead", it is asking for the
    // age of the newest datum.
    let mut st = fresh_state();
    st.apply_delta(arb_book::Delta {
        seq: arb_book::SeqInput::Monotonic { id: 11 },
        bids: vec![(crate::support::price("100"), crate::support::qty("2"))],
        asks: Vec::new(),
        exchange_ts: Some(WallTs::from_millis(59_900)),
        observed_at: LocalTs::from_millis(59_900),
    })
    .expect("id 11 follows id 10");
    let now = LocalTs::from_millis(60_000);
    assert_eq!(st.observed_at(), LocalTs::from_millis(59_900));
    assert_eq!(age_of(st.observed_at(), now), ms(100));
    assert_eq!(assess_age(age_of(st.observed_at(), now), BOOK), Staleness::Fresh);
}

// ---------------------------------------------------------------------------
// The exchange clock. It cannot shorten an age — ages are local — but it is the
// only warning we get that a venue's timestamps are not milliseconds.
// ---------------------------------------------------------------------------

#[test]
fn the_skew_sign_convention_is_exchange_minus_local() {
    // An exchange stamp of 1_000 received at local 900 is 100 ms AHEAD of us.
    // If this sign inverted, every genuine lag would read as "ahead" and every
    // clock ahead would read as a lag inside the tolerable band, i.e. silence.
    assert_eq!(
        assess_skew(WallTs::from_millis(1_000), LocalTs::from_millis(900), SkewLimits::WAN),
        ClockVerdict::ExchangeAhead(ms(100))
    );
    // The ordinary direction — stamped before it reached us — inside the WAN
    // transit band is Aligned, because transit alone explains it.
    assert_eq!(
        assess_skew(WallTs::from_millis(900), LocalTs::from_millis(1_000), SkewLimits::WAN),
        ClockVerdict::Aligned
    );
    assert_eq!(ClockVerdict::ExchangeAhead(ms(100)).skew(), ms(100));
    // A lag is reported as a NEGATIVE skew: the accessor keeps the direction.
    assert_eq!(ClockVerdict::Lagging(ms(300)).skew(), ms(-300));
}

#[test]
fn lag_boundaries_belong_to_the_worse_bucket_too() {
    // WAN = tolerable 250, suspicious 1500. Exchange stamp at 0, local receive
    // at the lag under test.
    let at = |lag: i64| assess_skew(WallTs::EPOCH, LocalTs::from_millis(lag), SkewLimits::WAN);
    assert_eq!(at(249), ClockVerdict::Aligned);
    assert_eq!(at(250), ClockVerdict::Lagging(ms(250)));
    assert_eq!(at(1_499), ClockVerdict::Lagging(ms(1_499)));
    assert_eq!(at(1_500), ClockVerdict::Suspicious(ms(1_500)));
    // Zero tolerance the other way: one millisecond ahead gets its own variant
    // rather than being absorbed by the 250 ms transit band.
    assert_eq!(
        assess_skew(WallTs::from_millis(1), LocalTs::EPOCH, SkewLimits::WAN),
        ClockVerdict::ExchangeAhead(ms(1))
    );
}

/// Evidence whose only defect is the clock, with paint assessed so the score is
/// attributable to the clock alone.
fn clock_evidence(clock: ClockVerdict) -> VenueEvidence {
    VenueEvidence {
        clock,
        paint: Some(arb_detect::PaintReport { signals: Vec::new(), score: 0 }),
        ..VenueEvidence::clean(Venue::Binance)
    }
}

#[test]
fn a_venue_publishing_seconds_where_we_assume_millis_is_caught_as_suspicious() {
    // OPEN.md D4. True wall time 1_700_000_000_000 ms; the venue sends
    // 1_700_000_000 (seconds) and we parse it as millis. We receive it 50 ms
    // later, at local 1_700_000_000_050.
    //   skew = 1_700_000_000 - 1_700_000_000_050 = -1_698_300_000_050 ms
    //   lag  = 1_698_300_000_050 >= suspicious (1_500) -> Suspicious
    // 1000x wrong timestamps are therefore not silent. What they are also not is
    // blocking: Suspicious costs 30 points, leaving 70, which is above
    // MIN_TRADEABLE. The CLOCK_SKEW flag is the only thing standing between this
    // venue and an order.
    let verdict = assess_skew(
        WallTs::from_millis(1_700_000_000),
        LocalTs::from_millis(1_700_000_000_050),
        SkewLimits::WAN,
    );
    assert_eq!(verdict, ClockVerdict::Suspicious(ms(1_698_300_000_050)));
    let ev = clock_evidence(verdict);
    let t = assess_venue(&ev);
    assert_eq!(t.score, 70);
    assert!(t.score >= MIN_TRADEABLE, "a 1000x clock error does not block on score alone");
    assert!(flags_for(&ev).contains(QualityFlags::CLOCK_SKEW));
}

#[test]
fn a_venue_publishing_micros_leaves_the_score_exactly_on_the_tradeable_line() {
    // The dangerous direction of the same units bug: microseconds parsed as
    // millis puts the exchange stamp 1_698_299_999_999_950 ms in the future.
    //   penalty = CLOCK_AHEAD_BASE_PENALTY + min(ahead, CLOCK_AHEAD_MAX_EXTRA)
    //           = 15 + 25 = 40
    //   score   = 100 - 40 = 60 = MIN_TRADEABLE, and the gate is `>=`.
    // So the worst clock reading expressible still clears the tradeable bar with
    // nothing to spare. That is the documented intent — but it means the flag,
    // not the score, is what must stop the order.
    let verdict = assess_skew(
        WallTs::from_millis(1_700_000_000_000_000),
        LocalTs::from_millis(1_700_000_000_050),
        SkewLimits::WAN,
    );
    assert_eq!(verdict, ClockVerdict::ExchangeAhead(ms(1_698_299_999_999_950)));
    let ev = clock_evidence(verdict);
    let t = assess_venue(&ev);
    assert_eq!(
        i32::from(t.score),
        100 - (CLOCK_AHEAD_BASE_PENALTY + CLOCK_AHEAD_MAX_EXTRA)
    );
    assert_eq!(t.score, MIN_TRADEABLE);
    assert!(t.is_tradeable());
    assert!(t.score >= MIN_RANKABLE);
    assert!(flags_for(&ev).contains(QualityFlags::CLOCK_SKEW));
    // One more defect of any size takes it out. That is the whole margin.
    let plus_warn = VenueEvidence { staleness: Staleness::Warn, ..ev };
    assert!(!assess_venue(&plus_warn).is_tradeable());
}

#[test]
fn a_lagging_clock_is_the_safe_direction_and_is_priced_as_such() {
    // A lag overstates ages, which can only make data look worse than it is.
    // Five points, and no CLOCK_SKEW flag: a lag reading is indistinguishable
    // from network transit, and flagging it would block on the speed of light.
    let ev = clock_evidence(ClockVerdict::Lagging(ms(300)));
    assert_eq!(assess_venue(&ev).score, 95);
    assert!(!flags_for(&ev).contains(QualityFlags::CLOCK_SKEW));
    assert!(assess_venue(&ev).is_tradeable());
}
