//! Venue trust: four detectors, one number, and the flags that carry the
//! verdict to the screen.
//!
//! # Why this is not an average
//!
//! The obvious way to combine four judgements is to average them. That is
//! exactly the dishonesty this crate exists to remove: an average lets three
//! healthy signals outvote one fatal one, and produces a comfortable 75 for a
//! venue whose order book is a guess. Fatal inputs therefore do not participate
//! in the arithmetic at all — they floor the score at
//! [`FLOOR_SCORE`] and the notes say which one did it.
//!
//! Two inputs are fatal:
//!
//! - [`Staleness::Dead`] — the observation carries no information about the
//!   current market. There is nothing to combine it with.
//! - [`SeqHealth::Unusable`] — the ladder is not the venue's ladder. Prices
//!   derived from it are fabrications, however fresh they are.
//!
//! Two more were added when the assumptions behind them were finally made
//! checkable, and both are fatal for the same reason: they say a *number we have
//! been doing arithmetic on is not the quantity we thought*, which no deduction
//! can express.
//!
//! - [`DepthSemantics::Contradicted`] — the feed did something an absolute-size
//!   feed cannot do, so the ladder's levels and sizes are not the venue's
//!   (`OPEN.md` D2).
//! - [`TsUnit::NotMillis`] — the venue's timestamps are not in milliseconds, so
//!   every age and every skew computed from this feed is wrong by a factor of a
//!   thousand or more (`OPEN.md` D4).
//!
//! Everything else is a deduction from 100, and the deductions are sized so that
//! **one** non-fatal defect at full strength still leaves a tradeable score,
//! while **two** independent defects do not. That is the judgement in one
//! sentence: bet on a venue with at most one thing wrong with it and nothing
//! fatal.
//!
//! Paint suspicion never floors and never blocks on its own, because
//! [`crate::painted`] produces inference, not measurement.

use arb_types::{BookSnapshot, QualityFlags, Venue};

use crate::clock::ClockVerdict;
use crate::epoch::{EpochUnit, TsUnit};
use crate::painted::{PaintReport, PaintSignal};
use crate::semantics::{DepthSemantics, SemanticsTell, WhyUnevaluated};
use crate::sequence::SeqHealth;
use crate::staleness::Staleness;

/// The score a venue with no detected defects gets. Not "the data is good" — see
/// the crate docs.
pub const MAX_SCORE: u8 = 100;

/// Where a fatal input puts the score. Zero, so that no combination of good news
/// can lift it and no ranking can accidentally float it up.
pub const FLOOR_SCORE: u8 = 0;

/// Minimum score at which we are willing to send an order.
///
/// 60 is not arbitrary: it is exactly `100 - PAINT_MAX_PENALTY`, the score left
/// by the single largest non-fatal deduction. So a venue whose *only* problem is
/// maximum paint suspicion is still tradeable, and any venue with two
/// independent problems is not. Raise this and we stop trading on venues with
/// one cosmetic issue; lower it and two simultaneous defects start looking
/// acceptable.
pub const MIN_TRADEABLE: u8 = 60;

/// Minimum score at which a venue may appear in the ranking at all, even
/// greyed out.
///
/// 40 leaves room to *display* a venue with two or three problems — an operator
/// wants to see that a row exists and why it is unusable — while keeping
/// anything below it out of a sorted list where a human might act on position
/// alone.
pub const MIN_RANKABLE: u8 = 40;

/// Deduction for [`Staleness::Warn`].
///
/// 10 points: the data is still usable, but every number computed from it comes
/// from a book we already know has moved on. Ten points is enough to put a
/// warned venue behind an otherwise identical fresh one in a ranking, and small
/// enough not to exclude it. Warn is a tiebreaker, not a veto.
pub const STALENESS_WARN_PENALTY: i32 = 10;

/// Deduction for [`Staleness::Stale`].
///
/// 55 points: more than half the score, deliberately larger than
/// `100 - MIN_TRADEABLE`, so stale data cannot be traded no matter how clean
/// everything else is. It is not fatal only because we still want the number on
/// screen to say *how* bad, and because a stale book plus a fresh funding
/// snapshot is a real state an operator needs to see.
pub const STALENESS_STALE_PENALTY: i32 = 55;

/// Deduction for [`ClockVerdict::Lagging`].
///
/// 5 points, near-nominal, because a lag reading is indistinguishable from
/// network transit (see [`crate::clock`]). The only consequence of a genuine lag
/// in this direction is that our ages are slightly *overstated*, which is the
/// safe direction. We deduct anything at all only because a lag that grows is an
/// early warning of a clock that is about to be a real problem.
pub const CLOCK_LAGGING_PENALTY: i32 = 5;

/// Deduction for [`ClockVerdict::Suspicious`].
///
/// 30 points: at this magnitude the reading is no longer explainable by transit,
/// so either our clock or the venue's publisher is broken, and in both cases
/// staleness arithmetic is unreliable. Combined with any second defect this
/// drops the venue below `MIN_TRADEABLE`, which is the intent — a suspect clock
/// alone is survivable, a suspect clock plus anything else is not.
pub const CLOCK_SUSPICIOUS_PENALTY: i32 = 30;

/// Base deduction for [`ClockVerdict::ExchangeAhead`].
///
/// 15 points before any magnitude scaling. A reading one millisecond into the
/// future is most likely timestamp quantisation, so the floor is modest; what
/// makes this direction dangerous is not its size but its silence, since it
/// makes every age *understated* and therefore fires no staleness flag.
pub const CLOCK_AHEAD_BASE_PENALTY: i32 = 15;

/// Cap on the per-millisecond addition for [`ClockVerdict::ExchangeAhead`].
///
/// 25 points on top of the base, one per millisecond. The scale is chosen so
/// that 1 ms ahead (plausibly quantisation) costs 16 while 25 ms or more ahead
/// costs the full 40: 25 ms cannot be quantisation, it is a real offset, and it
/// means every age we compute is understated by at least that much plus transit.
/// At 40 the venue needs everything else to be perfect to stay tradeable.
pub const CLOCK_AHEAD_MAX_EXTRA: i32 = 25;

/// Deduction for [`SeqHealth::Degraded`].
///
/// 25 points: a degraded feed means some of the levels we are pricing against
/// were reconstructed from a resync rather than observed, or that we have not
/// yet seen enough of the feed to know. A quarter of the score, so it cannot
/// block alone — a single gap in a long clean session should not stop trading —
/// but combined with staleness or paint suspicion it does.
pub const SEQ_DEGRADED_PENALTY: i32 = 25;

/// Maximum deduction from paint suspicion, at [`PaintReport::score`] 100.
///
/// 40 points: the largest non-fatal deduction, and exactly `100 -
/// MIN_TRADEABLE`, so a book that trips every paint signal is right at the edge
/// of tradeable and anything else pushes it over. Capped rather than fatal
/// because these signals are inference from one snapshot and have been validated
/// against nothing; treating a heuristic as proof would be the same mistake as
/// treating it as noise, in the other direction.
pub const PAINT_MAX_PENALTY: i32 = 40;

/// Deduction when no paint assessment was supplied.
///
/// 20 points: half of the maximum. "We did not look" is not "it was fine", and
/// it is not "it was painted" either — half the maximum is the honest midpoint
/// and it creates the right incentive, which is to always run the check.
pub const PAINT_UNASSESSED_PENALTY: i32 = 20;

/// Deduction for a crossed book.
///
/// 30 points: within a single venue, a bid at or above the ask cannot persist —
/// the matching engine would have filled it. So a crossed snapshot is either a
/// stitching bug in our own aggregation (we merged two update generations) or a
/// feed we are decoding wrong, and we cannot tell which from the snapshot. Both
/// mean the touch prices are not real prices, which is why this is heavy enough
/// that a crossed book plus any second defect is untradeable.
pub const CROSSED_PENALTY: i32 = 30;

/// Deduction when the caller reports depth too thin for the intended size.
///
/// 15 points: thin depth is a fact about the market rather than a defect in our
/// data, and the sizing logic downstream is what actually protects us from it.
/// The deduction exists so that ranking prefers a venue that can absorb the
/// order, not to block.
pub const THIN_DEPTH_PENALTY: i32 = 15;

/// Paint score at which [`QualityFlags::PAINTED_DEPTH`] is raised.
///
/// 50: with the heaviest single signal weighted 25, reaching 50 requires at
/// least two independent signals to have fired. Two independent tells is where
/// we stop calling it coincidence. Below 50 a single signal has too many
/// innocent explanations to justify raising a flag that may block trading
/// outright.
pub const PAINT_FLAG_MIN_SCORE: u8 = 50;

/// Deduction for circumstantial evidence that depth updates are not absolute
/// sizes.
///
/// 30 points, the same weight as [`CROSSED_PENALTY`], and by the same argument:
/// both say *the touch prices may not be real prices*. A crossed book says it
/// because a bid above an ask cannot persist; suspect depth semantics say it
/// because a feed whose deletes we never receive leaves levels in the ladder that
/// nobody is offering, which makes the spread read tighter than it is. That is
/// the favourable direction, so it is the direction ranking selects for.
///
/// Why not fatal: [`DepthSemantics::Suspect`] is an *absence* over a sample — no
/// delete seen, no delete landed — and an absence has innocent explanations
/// (a quiet instrument, a short window). Why not smaller: at 30 this plus any
/// second defect is untradeable, which is the same treatment a crossed book gets
/// and the same sentence the crate docs describe.
///
/// [`DepthSemantics::Contradicted`] is not on this scale. It is fatal, because it
/// is not an absence — it is the feed doing something an absolute-size feed
/// cannot do.
pub const DEPTH_SEMANTICS_SUSPECT_PENALTY: i32 = 30;

/// Deduction when depth semantics were not evaluated at all.
///
/// **Zero, deliberately, and this is the one place in this crate where "we did
/// not look" is not priced.** Recorded as a note so it is never silent, but it
/// costs nothing. Three reasons, in order of weight:
///
/// 1. Unlike a paint assessment, this one *cannot* be run on demand. It needs a
///    sample. Every fresh connection and every reconnect is legitimately
///    [`WhyUnevaluated::NoSampleYet`] for several minutes, and charging for that
///    would charge for reconnecting.
/// 2. Nothing in the workspace populates [`crate::semantics::DepthEvidence`]
///    yet, so a non-zero figure here would deduct from **every venue
///    immediately**. Stacked on the existing 20 for an absent paint report, a
///    venue with nothing whatever wrong with it would score 60 — exactly
///    [`MIN_TRADEABLE`] — and any further defect would stop all trading.
/// 3. That is a redistribution of the score budget, and the budget is shared
///    with every other detector. Choosing it unilaterally would also mean
///    rewriting exact-score assertions in `arb-adversary`, which is not this
///    crate's to rewrite.
///
/// So the honest reading of this zero is: **an open question, not a judgement.**
/// It is raised in `team/inbox/LEAD.md`. What is *not* deferred is the fatal
/// treatment of [`DepthSemantics::Contradicted`], which needs no budget
/// discussion because it floors rather than deducts.
pub const DEPTH_SEMANTICS_UNEVALUATED_PENALTY: i32 = 0;

/// Everything the four detectors found for one venue, bundled so the combining
/// rules live in one place instead of at every call site.
///
/// `staleness` should be the *worst* staleness across the inputs that matter for
/// the decision — a fresh funding rate does not rescue a stale book. Use
/// [`Staleness::worse_of`] to fold them before building this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueEvidence {
    /// Which venue this describes.
    pub venue: Venue,
    /// Worst staleness across the inputs the decision depends on.
    pub staleness: Staleness,
    /// Clock reading for this venue's feed.
    pub clock: ClockVerdict,
    /// Sequence health of this venue's book feed.
    pub sequence: SeqHealth,
    /// Paint assessment, or `None` if none was run. `None` is penalised: it
    /// means we did not look, which is not the same as finding nothing.
    pub paint: Option<PaintReport>,
    /// Best bid at or above best ask on a single venue. See [`is_crossed`].
    pub crossed: bool,
    /// Caller's verdict that displayed depth is too thin for the intended size.
    /// This crate cannot decide it: it depends on an order size we never see.
    pub thin_depth: bool,
    /// Whether this venue's depth updates really are absolute sizes
    /// (`OPEN.md` D2). Feed it
    /// [`crate::semantics::assess_depth_semantics`].
    ///
    /// Defaults to `Unevaluated(NoSampleYet)` in [`VenueEvidence::clean`], which
    /// is the truth about every venue in this workspace today: the detector
    /// exists and nothing feeds it yet.
    pub depth_semantics: DepthSemantics,
    /// What unit this venue's timestamps arrived in (`OPEN.md` D4). Feed it
    /// [`TsUnit::of`] at the parse boundary, on the value **as parsed**.
    ///
    /// Defaults to [`TsUnit::NoTimestamp`], which is a deliberate choice about
    /// which mistake is possible. A caller that forgets this field gets the same
    /// treatment as a venue that sent no timestamp — no penalty and no
    /// protection — rather than a free `Millis` that would assert the assumption
    /// this field exists to test.
    pub ts_unit: TsUnit,
}

impl VenueEvidence {
    /// Evidence with nothing wrong with it and a paint check that found nothing.
    /// Useful as a base for `..` updates in callers and tests.
    pub const fn clean(venue: Venue) -> Self {
        VenueEvidence {
            venue,
            staleness: Staleness::Fresh,
            clock: ClockVerdict::Aligned,
            sequence: SeqHealth::Healthy,
            paint: None,
            crossed: false,
            thin_depth: false,
            // Not `NoContraryEvidence`: nothing has been evaluated, and a
            // convenient default here would be the whole defect this field
            // exists to catch.
            depth_semantics: DepthSemantics::Unevaluated(WhyUnevaluated::NoSampleYet),
            ts_unit: TsUnit::NoTimestamp,
        }
    }
}

/// Why a venue lost points, in the caller's language.
///
/// Notes are an enum rather than strings so that a caller can react to them
/// programmatically and no formatting happens until something is displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustNote {
    /// The data is dead. Fatal.
    FatalDataDead,
    /// The ladder cannot be trusted. Fatal.
    FatalSequenceUnusable,
    /// The feed did something an absolute-size depth feed cannot do, so the
    /// ladder's levels and sizes are not the venue's. Fatal.
    FatalDepthSemantics(SemanticsTell),
    /// The venue's timestamps are not in milliseconds, so every age and skew
    /// computed from this feed is wrong by the unit's factor. Fatal.
    FatalTimestampUnit(EpochUnit),
    /// Age is past the warn or stale threshold.
    Aging(Staleness),
    /// Clock reading is not aligned.
    Clock(ClockVerdict),
    /// Sequence health is degraded.
    SequenceDegraded,
    /// Circumstantial evidence that depth updates are not absolute sizes.
    DepthSemanticsSuspect(SemanticsTell),
    /// Depth semantics were not evaluated. Carries no penalty — see
    /// [`DEPTH_SEMANTICS_UNEVALUATED_PENALTY`] — and exists so the gap is
    /// visible instead of silent.
    DepthSemanticsNotEvaluated(WhyUnevaluated),
    /// Paint suspicion at this score.
    PaintSuspicion(u8),
    /// No paint assessment was supplied.
    PaintNotAssessed,
    /// Bid at or above ask on one venue.
    CrossedBook,
    /// Depth too thin for the intended size, per the caller.
    ThinDepth,
}

impl std::fmt::Display for TrustNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustNote::FatalDataDead => f.write_str("FATAL: observation is dead, age carries no information"),
            TrustNote::FatalSequenceUnusable => {
                f.write_str("FATAL: sequence unusable, the ladder is not the venue's ladder")
            }
            TrustNote::FatalDepthSemantics(t) => write!(
                f,
                "FATAL: depth updates are not absolute sizes ({t}), \
                 the ladder's levels are not the venue's"
            ),
            TrustNote::FatalTimestampUnit(u) => write!(
                f,
                "FATAL: venue timestamps are not milliseconds, they look like {u}; \
                 every age computed from this feed is wrong"
            ),
            TrustNote::Aging(s) => write!(f, "age: {s}"),
            TrustNote::Clock(v) => write!(f, "{v}"),
            TrustNote::SequenceDegraded => {
                f.write_str("sequence degraded: some levels were reconstructed, not observed")
            }
            TrustNote::DepthSemanticsSuspect(t) => {
                write!(f, "depth semantics suspect: {t}")
            }
            TrustNote::DepthSemanticsNotEvaluated(w) => {
                write!(f, "depth semantics not evaluated: {}", w.tag())
            }
            TrustNote::PaintSuspicion(score) => write!(f, "paint suspicion score {score}/100"),
            TrustNote::PaintNotAssessed => f.write_str("depth was not assessed for painting"),
            TrustNote::CrossedBook => f.write_str("crossed book: bid at or above ask on one venue"),
            TrustNote::ThinDepth => f.write_str("displayed depth too thin for intended size"),
        }
    }
}

/// One venue's combined trustworthiness, plus the reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueTrust {
    /// Which venue.
    pub venue: Venue,
    /// `0..=100`. 100 means no defect was detected — not that the data is good.
    pub score: u8,
    /// Every deduction that was applied, and every fatal input.
    pub notes: Vec<TrustNote>,
}

impl VenueTrust {
    /// True when the score clears [`MIN_TRADEABLE`].
    pub const fn is_tradeable(&self) -> bool {
        self.score >= MIN_TRADEABLE
    }

    /// True when the score clears [`MIN_RANKABLE`].
    pub const fn is_rankable(&self) -> bool {
        self.score >= MIN_RANKABLE
    }

    /// True when a fatal input floored the score, as opposed to a pile of small
    /// deductions adding up to the same number. The distinction matters when
    /// deciding whether to retry or to disconnect.
    ///
    /// Matched exhaustively rather than with a wildcard: a wildcard would make a
    /// future fatal variant default to *not* fatal, and that default is the wrong
    /// way round for a predicate that gates trading.
    pub fn has_fatal(&self) -> bool {
        self.notes.iter().any(|n| match n {
            TrustNote::FatalDataDead
            | TrustNote::FatalSequenceUnusable
            | TrustNote::FatalDepthSemantics(_)
            | TrustNote::FatalTimestampUnit(_) => true,
            TrustNote::Aging(_)
            | TrustNote::Clock(_)
            | TrustNote::SequenceDegraded
            | TrustNote::DepthSemanticsSuspect(_)
            | TrustNote::DepthSemanticsNotEvaluated(_)
            | TrustNote::PaintSuspicion(_)
            | TrustNote::PaintNotAssessed
            | TrustNote::CrossedBook
            | TrustNote::ThinDepth => false,
        })
    }
}

/// Deduction for a clock reading.
fn clock_penalty(v: ClockVerdict) -> i32 {
    match v {
        ClockVerdict::Aligned => 0,
        ClockVerdict::Lagging(_) => CLOCK_LAGGING_PENALTY,
        ClockVerdict::Suspicious(_) => CLOCK_SUSPICIOUS_PENALTY,
        ClockVerdict::ExchangeAhead(ms) => {
            // One point per millisecond ahead, capped. `clamp` bounds are
            // literals, so it cannot panic, and the cast is safe after clamping.
            let extra = ms.abs().millis().clamp(0, i64::from(CLOCK_AHEAD_MAX_EXTRA)) as i32;
            CLOCK_AHEAD_BASE_PENALTY.saturating_add(extra)
        }
    }
}

/// Deduction for a paint report, rounded **up** so the penalty is never
/// understated by integer division.
fn paint_penalty(score: u8) -> i32 {
    let scaled = i32::from(score).saturating_mul(PAINT_MAX_PENALTY);
    // Ceiling division by 100.
    scaled.saturating_add(99) / 100
}

/// Combine the detectors into one score plus the reasons for it.
///
/// A fatal input floors the score at [`FLOOR_SCORE`] and short-circuits the
/// arithmetic, so no quantity of good news can lift it. Everything else is a
/// deduction from [`MAX_SCORE`]; the sum is clamped rather than allowed to go
/// negative, because "how far below zero" is not information anyone can use.
pub fn assess_venue(ev: &VenueEvidence) -> VenueTrust {
    let mut notes: Vec<TrustNote> = Vec::new();

    // ---- fatal inputs: recorded, then floored --------------------------------
    let mut fatal = false;
    if ev.staleness == Staleness::Dead {
        notes.push(TrustNote::FatalDataDead);
        fatal = true;
    }
    if ev.sequence == SeqHealth::Unusable {
        notes.push(TrustNote::FatalSequenceUnusable);
        fatal = true;
    }
    // D2 and D4. Both are fatal for the same reason and it is not severity: a
    // deduction says "this data is worse than we would like", and neither of
    // these is about quality. Each says a number we have been doing arithmetic on
    // is not the quantity we thought it was, so there is nothing left to grade.
    if let DepthSemantics::Contradicted(tell) = ev.depth_semantics {
        notes.push(TrustNote::FatalDepthSemantics(tell));
        fatal = true;
    }
    if let TsUnit::NotMillis(unit) = ev.ts_unit {
        notes.push(TrustNote::FatalTimestampUnit(unit));
        fatal = true;
    }

    let mut penalty: i32 = 0;

    match ev.staleness {
        Staleness::Fresh | Staleness::Dead => {}
        Staleness::Warn => {
            notes.push(TrustNote::Aging(Staleness::Warn));
            penalty = penalty.saturating_add(STALENESS_WARN_PENALTY);
        }
        Staleness::Stale => {
            notes.push(TrustNote::Aging(Staleness::Stale));
            penalty = penalty.saturating_add(STALENESS_STALE_PENALTY);
        }
    }

    if !ev.clock.is_aligned() {
        notes.push(TrustNote::Clock(ev.clock));
        penalty = penalty.saturating_add(clock_penalty(ev.clock));
    }

    if ev.sequence == SeqHealth::Degraded {
        notes.push(TrustNote::SequenceDegraded);
        penalty = penalty.saturating_add(SEQ_DEGRADED_PENALTY);
    }

    match &ev.paint {
        Some(report) if report.score > 0 => {
            notes.push(TrustNote::PaintSuspicion(report.score));
            penalty = penalty.saturating_add(paint_penalty(report.score));
        }
        Some(_) => {}
        None => {
            notes.push(TrustNote::PaintNotAssessed);
            penalty = penalty.saturating_add(PAINT_UNASSESSED_PENALTY);
        }
    }

    if ev.crossed {
        notes.push(TrustNote::CrossedBook);
        penalty = penalty.saturating_add(CROSSED_PENALTY);
    }

    if ev.thin_depth {
        notes.push(TrustNote::ThinDepth);
        penalty = penalty.saturating_add(THIN_DEPTH_PENALTY);
    }

    let score = if fatal {
        FLOOR_SCORE
    } else {
        let raw = i32::from(MAX_SCORE).saturating_sub(penalty);
        if raw <= i32::from(FLOOR_SCORE) {
            FLOOR_SCORE
        } else if raw >= i32::from(MAX_SCORE) {
            MAX_SCORE
        } else {
            // Safe: strictly between FLOOR_SCORE and MAX_SCORE.
            raw as u8
        }
    };

    VenueTrust {
        venue: ev.venue,
        score,
        notes,
    }
}

/// True when the best bid is at or above the best ask.
///
/// A book with only one side, or none, is not crossed — it is incomplete, which
/// is a different flag and someone else's.
///
/// Strictly, bid *above* ask is crossed and bid *equal to* ask is locked. Both
/// are impossible within one venue's matching engine, so both are treated the
/// same here: the touch prices are not real prices.
pub fn is_crossed(book: &BookSnapshot) -> bool {
    match (book.best_bid(), book.best_ask()) {
        (Some(bid), Some(ask)) => bid.price >= ask.price,
        _ => false,
    }
}

/// Map the detectors' verdicts onto [`QualityFlags`], which is how these
/// judgements reach the screen and the trading gate.
///
/// Deliberate choices, each with a consequence:
///
/// - `Staleness::Warn` does **not** raise `STALE`. Warn means "rank it lower",
///   and `STALE` may block; blocking on a 250 ms old book would stop most
///   trading for no measured reason. Stale and Dead both raise it.
/// - `ClockVerdict::Lagging` does **not** raise `CLOCK_SKEW`, because a lag
///   reading is indistinguishable from network transit. `Suspicious` and
///   `ExchangeAhead` both do.
/// - `SeqHealth::Degraded` **does** raise `SEQ_GAP`, not just `Unusable`. This
///   means a freshly connected feed, which is Degraded until `min_sample`
///   messages have been seen, carries `SEQ_GAP` and will be blocked if that flag
///   blocks. That is the conservative side and it is chosen knowingly: the
///   alternative is pricing an arb off levels that were reconstructed rather
///   than observed.
/// - `PAINTED_DEPTH` needs [`PAINT_FLAG_MIN_SCORE`], i.e. at least two
///   independent paint signals. One signal never raises it.
/// - `THIN_DEPTH` comes from either the caller's size-versus-depth judgement or
///   a [`PaintSignal::TouchTooThin`] finding, since a token touch is thin depth
///   whatever its cause.
///
/// Flags this crate never sets, because it cannot know them: `PARTIAL_BOOK`,
/// `BOOK_EMPTY`, `SIZE_EXCEEDS_DEPTH`, `FUNDING_INTERVAL_ASSUMED`,
/// `FEE_TIER_ASSUMED`, `PRICE_UNAVAILABLE`, `FUNDING_RATE_HISTORICAL`.
pub fn flags_for(ev: &VenueEvidence) -> QualityFlags {
    let mut flags = QualityFlags::NONE;

    if ev.staleness >= Staleness::Stale {
        flags.insert(QualityFlags::STALE);
    }

    match ev.clock {
        ClockVerdict::Suspicious(_) | ClockVerdict::ExchangeAhead(_) => {
            flags.insert(QualityFlags::CLOCK_SKEW);
        }
        ClockVerdict::Aligned | ClockVerdict::Lagging(_) => {}
    }

    if ev.sequence >= SeqHealth::Degraded {
        flags.insert(QualityFlags::SEQ_GAP);
    }

    if let Some(report) = &ev.paint {
        if report.is_suspicious_at(PAINT_FLAG_MIN_SCORE) {
            flags.insert(QualityFlags::PAINTED_DEPTH);
        }
        if report.contains(PaintSignal::TouchTooThin) {
            flags.insert(QualityFlags::THIN_DEPTH);
        }
    }

    if ev.thin_depth {
        flags.insert(QualityFlags::THIN_DEPTH);
    }

    if ev.crossed {
        flags.insert(QualityFlags::CROSSED_BOOK);
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_num::{Price, Qty};
    use arb_types::{
        DataQuality, InstrumentKey, Level, LocalTs, MarketKind, Millis, VenueKey,
    };

    fn ev() -> VenueEvidence {
        VenueEvidence {
            paint: Some(PaintReport {
                signals: Vec::new(),
                score: 0,
            }),
            ..VenueEvidence::clean(Venue::Binance)
        }
    }

    fn paint(score: u8, signals: Vec<PaintSignal>) -> Option<PaintReport> {
        Some(PaintReport { signals, score })
    }

    #[test]
    fn clean_evidence_scores_one_hundred() {
        let t = assess_venue(&ev());
        assert_eq!(t.score, 100);
        assert_eq!(t.notes, Vec::new());
        assert!(t.is_tradeable());
        assert!(!t.has_fatal());
    }

    #[test]
    fn unassessed_paint_is_not_free() {
        // None means we did not look: 100 - 20 = 80.
        let t = assess_venue(&VenueEvidence::clean(Venue::Bybit));
        assert_eq!(t.score, 80);
        assert!(t.notes.contains(&TrustNote::PaintNotAssessed));
    }

    #[test]
    fn unusable_sequence_floors_the_score() {
        // Everything else perfect. An average would give this venue 80/100.
        let e = VenueEvidence {
            sequence: SeqHealth::Unusable,
            ..ev()
        };
        let t = assess_venue(&e);
        assert_eq!(t.score, 0);
        assert!(t.has_fatal());
        assert!(!t.is_rankable());
        assert!(t.notes.contains(&TrustNote::FatalSequenceUnusable));
    }

    #[test]
    fn dead_staleness_floors_the_score() {
        let e = VenueEvidence {
            staleness: Staleness::Dead,
            ..ev()
        };
        let t = assess_venue(&e);
        assert_eq!(t.score, 0);
        assert!(t.has_fatal());
        assert!(t.notes.contains(&TrustNote::FatalDataDead));
    }

    #[test]
    fn a_fatal_input_beats_every_good_signal() {
        // Fresh, aligned, unpainted, uncrossed, deep — and awaiting resync.
        let e = VenueEvidence {
            staleness: Staleness::Fresh,
            clock: ClockVerdict::Aligned,
            sequence: SeqHealth::Unusable,
            paint: paint(0, Vec::new()),
            crossed: false,
            thin_depth: false,
            venue: Venue::Okx,
        };
        assert_eq!(assess_venue(&e).score, FLOOR_SCORE);
    }

    #[test]
    fn warn_staleness_is_a_tiebreaker_not_a_veto() {
        // 100 - 10 = 90, still tradeable.
        let e = VenueEvidence {
            staleness: Staleness::Warn,
            ..ev()
        };
        let t = assess_venue(&e);
        assert_eq!(t.score, 90);
        assert!(t.is_tradeable());
    }

    #[test]
    fn stale_alone_is_untradeable_but_still_visible() {
        // 100 - 55 = 45: below MIN_TRADEABLE 60, above MIN_RANKABLE 40.
        let e = VenueEvidence {
            staleness: Staleness::Stale,
            ..ev()
        };
        let t = assess_venue(&e);
        assert_eq!(t.score, 45);
        assert!(!t.is_tradeable());
        assert!(t.is_rankable());
        assert!(!t.has_fatal());
    }

    #[test]
    fn exchange_ahead_penalty_scales_with_magnitude() {
        // 1 ms ahead: 100 - (15 + 1) = 84.
        let one = VenueEvidence {
            clock: ClockVerdict::ExchangeAhead(Millis::from_millis(1)),
            ..ev()
        };
        assert_eq!(assess_venue(&one).score, 84);

        // 10 ms ahead: 100 - (15 + 10) = 75.
        let ten = VenueEvidence {
            clock: ClockVerdict::ExchangeAhead(Millis::from_millis(10)),
            ..ev()
        };
        assert_eq!(assess_venue(&ten).score, 75);

        // 25 ms ahead: 100 - (15 + 25) = 60, exactly at MIN_TRADEABLE.
        let cap = VenueEvidence {
            clock: ClockVerdict::ExchangeAhead(Millis::from_millis(25)),
            ..ev()
        };
        let t = assess_venue(&cap);
        assert_eq!(t.score, 60);
        assert!(t.is_tradeable());

        // 5000 ms ahead: the extra is capped, so still 100 - 40 = 60.
        let huge = VenueEvidence {
            clock: ClockVerdict::ExchangeAhead(Millis::from_millis(5_000)),
            ..ev()
        };
        assert_eq!(assess_venue(&huge).score, 60);
    }

    #[test]
    fn lagging_clock_is_nearly_free() {
        // 100 - 5 = 95. A lag reading is mostly network transit.
        let e = VenueEvidence {
            clock: ClockVerdict::Lagging(Millis::from_millis(400)),
            ..ev()
        };
        assert_eq!(assess_venue(&e).score, 95);
    }

    #[test]
    fn suspicious_clock_plus_degraded_sequence_is_untradeable() {
        // 100 - 30 - 25 = 45. Two independent defects, and that is the rule.
        let e = VenueEvidence {
            clock: ClockVerdict::Suspicious(Millis::from_millis(3_000)),
            sequence: SeqHealth::Degraded,
            ..ev()
        };
        let t = assess_venue(&e);
        assert_eq!(t.score, 45);
        assert!(!t.is_tradeable());
    }

    #[test]
    fn maximum_paint_suspicion_never_floors() {
        // 100 * 40 / 100 = 40 exactly, so 100 - 40 = 60: suspicion alone leaves
        // a venue at the edge of tradeable, because these heuristics are
        // inference and were validated against nothing.
        let e = VenueEvidence {
            paint: paint(100, Vec::new()),
            ..ev()
        };
        let t = assess_venue(&e);
        assert_eq!(t.score, 60);
        assert!(!t.has_fatal());
        assert!(t.is_tradeable());
    }

    #[test]
    fn paint_penalty_rounds_against_us() {
        // 45 * 40 = 1800, / 100 = 18 exactly -> 100 - 18 = 82.
        let exact = VenueEvidence {
            paint: paint(45, Vec::new()),
            ..ev()
        };
        assert_eq!(assess_venue(&exact).score, 82);

        // 10 * 40 = 400, / 100 = 4 exactly -> 96.
        let small = VenueEvidence {
            paint: paint(10, Vec::new()),
            ..ev()
        };
        assert_eq!(assess_venue(&small).score, 96);

        // 26 * 40 = 1040, / 100 = 10.4, rounded up to 11 -> 89. Truncation
        // would have given 90, i.e. a friendlier score than the evidence
        // supports.
        let rounded = VenueEvidence {
            paint: paint(26, Vec::new()),
            ..ev()
        };
        assert_eq!(assess_venue(&rounded).score, 89);
    }

    #[test]
    fn crossed_and_thin_stack() {
        // 100 - 30 - 15 = 55.
        let e = VenueEvidence {
            crossed: true,
            thin_depth: true,
            ..ev()
        };
        assert_eq!(assess_venue(&e).score, 55);
    }

    #[test]
    fn penalties_clamp_at_zero_without_a_fatal_input() {
        // 55 + 40 + 40 + 25 + 30 + 15 = 205 of deductions, no fatal input.
        let e = VenueEvidence {
            staleness: Staleness::Stale,
            clock: ClockVerdict::ExchangeAhead(Millis::from_millis(999)),
            sequence: SeqHealth::Degraded,
            paint: paint(100, Vec::new()),
            crossed: true,
            thin_depth: true,
            venue: Venue::Mexc,
        };
        let t = assess_venue(&e);
        assert_eq!(t.score, 0);
        // Floored by arithmetic, not by a fatal input: the operator should
        // expect this venue to recover, not to be disconnected.
        assert!(!t.has_fatal());
    }

    #[test]
    fn notes_carry_the_venue_through() {
        let e = VenueEvidence {
            venue: Venue::Gate,
            ..ev()
        };
        assert_eq!(assess_venue(&e).venue, Venue::Gate);
    }

    // ---- flags ------------------------------------------------------------

    #[test]
    fn flags_for_clean_evidence_is_empty() {
        assert!(flags_for(&ev()).is_empty());
        assert_eq!(flags_for(&ev()).bits(), 0);
    }

    #[test]
    fn flags_for_everything_wrong_is_exactly_six_bits() {
        // STALE 1<<0 = 1, SEQ_GAP 1<<1 = 2, CLOCK_SKEW 1<<2 = 4,
        // CROSSED_BOOK 1<<3 = 8, THIN_DEPTH 1<<4 = 16, PAINTED_DEPTH 1<<5 = 32.
        // 1 + 2 + 4 + 8 + 16 + 32 = 63.
        let e = VenueEvidence {
            staleness: Staleness::Stale,
            clock: ClockVerdict::Suspicious(Millis::from_millis(2_000)),
            sequence: SeqHealth::Degraded,
            paint: paint(60, vec![PaintSignal::TouchTooThin]),
            crossed: true,
            thin_depth: false,
            venue: Venue::Kucoin,
        };
        assert_eq!(flags_for(&e).bits(), 63);
    }

    #[test]
    fn warn_staleness_does_not_raise_stale() {
        let e = VenueEvidence {
            staleness: Staleness::Warn,
            ..ev()
        };
        assert!(flags_for(&e).is_empty());
    }

    #[test]
    fn dead_staleness_raises_stale() {
        let e = VenueEvidence {
            staleness: Staleness::Dead,
            ..ev()
        };
        assert_eq!(flags_for(&e).bits(), QualityFlags::STALE.bits());
    }

    #[test]
    fn lagging_clock_does_not_raise_clock_skew() {
        let lag = VenueEvidence {
            clock: ClockVerdict::Lagging(Millis::from_millis(400)),
            ..ev()
        };
        assert!(flags_for(&lag).is_empty());

        // One millisecond ahead does raise it: that direction understates every
        // age we compute, and nothing else will catch it.
        let ahead = VenueEvidence {
            clock: ClockVerdict::ExchangeAhead(Millis::from_millis(1)),
            ..ev()
        };
        assert_eq!(flags_for(&ahead).bits(), QualityFlags::CLOCK_SKEW.bits());
    }

    #[test]
    fn degraded_sequence_raises_seq_gap() {
        let deg = VenueEvidence {
            sequence: SeqHealth::Degraded,
            ..ev()
        };
        let un = VenueEvidence {
            sequence: SeqHealth::Unusable,
            ..ev()
        };
        assert_eq!(flags_for(&deg).bits(), QualityFlags::SEQ_GAP.bits());
        assert_eq!(flags_for(&un).bits(), QualityFlags::SEQ_GAP.bits());
    }

    #[test]
    fn painted_depth_flag_needs_two_signals_worth_of_score() {
        // 49 is one point short of PAINT_FLAG_MIN_SCORE.
        let under = VenueEvidence {
            paint: paint(49, vec![PaintSignal::UniformLevelSizes]),
            ..ev()
        };
        assert!(flags_for(&under).is_empty());

        // Exactly 50 raises it.
        let on = VenueEvidence {
            paint: paint(50, vec![PaintSignal::UniformLevelSizes]),
            ..ev()
        };
        assert_eq!(flags_for(&on).bits(), QualityFlags::PAINTED_DEPTH.bits());
    }

    #[test]
    fn touch_too_thin_raises_thin_depth_on_its_own() {
        // Score 20 is well under the paint flag threshold, but a token touch is
        // thin depth regardless of what caused it.
        let e = VenueEvidence {
            paint: paint(20, vec![PaintSignal::TouchTooThin]),
            ..ev()
        };
        assert_eq!(flags_for(&e).bits(), QualityFlags::THIN_DEPTH.bits());
    }

    #[test]
    fn caller_thin_depth_raises_thin_depth() {
        let e = VenueEvidence {
            thin_depth: true,
            ..ev()
        };
        assert_eq!(flags_for(&e).bits(), QualityFlags::THIN_DEPTH.bits());
    }

    #[test]
    fn unassessed_paint_raises_no_paint_flag() {
        // We did not look, so we have no evidence to flag. The score carries the
        // penalty instead; a flag would be a claim we cannot make.
        let e = VenueEvidence::clean(Venue::Htx);
        assert!(flags_for(&e).is_empty());
        assert_eq!(assess_venue(&e).score, 80);
    }

    // ---- crossed detection ------------------------------------------------

    fn lvl(price: &str, qty: &str) -> Level {
        Level::new(Price::parse(price).unwrap(), Qty::parse(qty).unwrap()).unwrap()
    }

    fn book(bids: Vec<Level>, asks: Vec<Level>) -> BookSnapshot {
        BookSnapshot::new(
            VenueKey::new(
                Venue::Binance,
                InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").unwrap(),
            ),
            bids,
            asks,
            Some(1),
            None,
            LocalTs::from_millis(0),
            // Invented from memory of API shapes, never seen on a network.
            DataQuality::SYNTHESIZED,
        )
        .unwrap()
    }

    #[test]
    fn uncrossed_book_is_not_crossed() {
        let b = book(vec![lvl("99.99", "1.0")], vec![lvl("100.00", "1.0")]);
        assert!(!is_crossed(&b));
    }

    #[test]
    fn locked_book_counts_as_crossed() {
        // bid == ask cannot persist inside one matching engine either.
        let b = book(vec![lvl("100.00", "1.0")], vec![lvl("100.00", "1.0")]);
        assert!(is_crossed(&b));
    }

    #[test]
    fn crossed_book_is_crossed() {
        let b = book(vec![lvl("100.01", "1.0")], vec![lvl("100.00", "1.0")]);
        assert!(is_crossed(&b));
    }

    #[test]
    fn one_sided_book_is_incomplete_not_crossed() {
        let bids_only = book(vec![lvl("99.99", "1.0")], Vec::new());
        let asks_only = book(Vec::new(), vec![lvl("100.00", "1.0")]);
        assert!(!is_crossed(&bids_only));
        assert!(!is_crossed(&asks_only));
    }
}
