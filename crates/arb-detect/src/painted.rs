//! Painted depth: size that is displayed but will not be there when you take
//! it.
//!
//! # These heuristics were validated against nothing
//!
//! `[fact, verified]` — the thresholds in this module have been checked against
//! **zero** real order books. No capture, no replay, no fill comparison. Every
//! number below is a judgement written down by one engineer from a memory of
//! what order books look like, and the honest description of the module's
//! accuracy is "unknown".
//!
//! That is exactly why the output is a list of [`PaintSignal`]s and an integer
//! score instead of a boolean. You cannot prove painting from a single
//! snapshot — proof requires taking the liquidity and watching what happens, or
//! at minimum a time series showing size vanish as price approaches. What a
//! snapshot supports is "this ladder has properties that real ladders usually
//! do not", and that is all any function here claims.
//!
//! There is no `PaintReport::is_real` and there never will be. A confident
//! "depth is real" is far more dangerous than an admitted unknown: the first
//! sizes an order, the second declines to. A score of 0 means *no evidence of
//! painting was found*, which is not the same as *the depth is real*, and code
//! downstream must treat it that way.
//!
//! # What the signals cost when they are wrong
//!
//! A false positive costs one skipped opportunity. A false negative costs the
//! difference between the depth we sized against and the depth that actually
//! filled, on every leg. The thresholds are therefore set where an ordinary,
//! plausibly-healthy book does not trip them, but they are not set to be
//! reluctant: two independent signals is where we stop calling it coincidence.

use arb_num::{Bps, Price, Qty, RoundMode};
use arb_types::{BookSnapshot, Level, Side};

use crate::error::DetectError;

/// One structural property of a ladder that real ladders usually do not have.
///
/// None of these is proof. Each has at least one innocent explanation, noted on
/// the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaintSignal {
    /// The best level is a small fraction of the mean of the levels behind it.
    /// A real book decays with distance; a painted one has a token touch that
    /// moves the displayed best price for almost no capital.
    /// Innocent explanation: a genuine large order was just partially filled
    /// and the remainder is small.
    TouchTooThin,
    /// Most of the side's size sits far enough from the touch that it will
    /// never be hit, where displaying it costs the maker nothing.
    /// Innocent explanation: a real resting whale order well away from the mid.
    DepthConcentratedFar,
    /// Several consecutive levels carry byte-identical sizes. That is a script,
    /// not a market.
    /// Innocent explanation: several participants all sizing to the same round
    /// lot, which does happen on small-tick instruments.
    UniformLevelSizes,
    /// Nearly every level is an exact multiple of a round unit. Same tell as
    /// uniform sizes, from a different angle.
    /// Innocent explanation: a venue with a coarse lot size, where round sizes
    /// are the only sizes.
    RoundNumberSizes,
    /// Adjacent displayed levels are separated by far more than the tick, i.e.
    /// nothing at all is quoted across a wide span.
    /// Innocent explanation: a genuinely illiquid instrument.
    PriceGapsInLadder,
    /// One side of the book carries a large multiple of the other.
    /// Innocent explanation: real one-way flow, which is common right after
    /// news.
    OneSidedDepth,
}

impl PaintSignal {
    /// Points this signal contributes to [`PaintReport::score`].
    ///
    /// Weights, and why they differ:
    ///
    /// - `UniformLevelSizes = 25` — the strongest single tell available from one
    ///   snapshot. Independent participants do not arrive at byte-identical
    ///   sizes at four consecutive prices; a loop does.
    /// - `TouchTooThin = 20` and `DepthConcentratedFar = 20` — these two are the
    ///   ones that directly change what a taker pays, so they are weighted for
    ///   consequence as much as for evidence. Both describe a ladder whose
    ///   displayed shape misprices the cost of size.
    /// - `RoundNumberSizes = 15` — the same "a script wrote this" evidence as
    ///   uniform sizes, and partly correlated with it, so it is weighted lower
    ///   to avoid counting one fact twice. A book that trips both scores 40, not
    ///   50.
    /// - `PriceGapsInLadder = 10` and `OneSidedDepth = 10` — each has a common,
    ///   fully innocent explanation (an illiquid instrument; real one-way
    ///   flow), so neither should move the needle far on its own.
    ///
    /// The weights sum to exactly 100 so a book that trips everything scores
    /// 100, and any two of the three heaviest signals clear the flagging
    /// threshold used in [`crate::trust`].
    ///
    /// `[assumption]` — the relative ordering is defensible from first
    /// principles; the exact integers are not measured. What matters and is
    /// deliberate: no single signal reaches the flagging threshold alone.
    pub const fn weight(self) -> u8 {
        match self {
            PaintSignal::UniformLevelSizes => 25,
            PaintSignal::TouchTooThin => 20,
            PaintSignal::DepthConcentratedFar => 20,
            PaintSignal::RoundNumberSizes => 15,
            PaintSignal::PriceGapsInLadder => 10,
            PaintSignal::OneSidedDepth => 10,
        }
    }

    /// Short label for logs and screens.
    pub const fn tag(self) -> &'static str {
        match self {
            PaintSignal::TouchTooThin => "touch-too-thin",
            PaintSignal::DepthConcentratedFar => "depth-concentrated-far",
            PaintSignal::UniformLevelSizes => "uniform-level-sizes",
            PaintSignal::RoundNumberSizes => "round-number-sizes",
            PaintSignal::PriceGapsInLadder => "price-gaps-in-ladder",
            PaintSignal::OneSidedDepth => "one-sided-depth",
        }
    }
}

impl std::fmt::Display for PaintSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

/// Thresholds for the paint signals.
///
/// Two fields must come from the instrument rather than a constant:
/// `tick_size` and `round_unit`. Neither can be derived from a
/// [`BookSnapshot`], which carries no [`arb_types::ContractSpec`].
///
/// `[do not know]` — the digest exposes no accessor for `tick_size` or
/// `lot_size` on `ContractSpec`, only the constructor argument, so a caller
/// holding only an `Instrument` may not be able to read them back. Until such an
/// accessor exists, whoever built the spec has to pass these in. When
/// `tick_size` is left at [`Price::ZERO`] the ladder-gap check falls back to
/// `gap_max_bps`, which is a worse test because it cannot adapt to the
/// instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaintLimits {
    /// How many levels behind the touch form the comparison mean.
    pub touch_reference_levels: usize,
    /// The touch is "too thin" when `touch_qty * this < mean(reference)`.
    pub touch_thin_factor: i64,
    /// Distance from the touch beyond which size counts as "far".
    pub far_bps: Bps,
    /// Share of the side's size that must sit beyond `far_bps` to fire, as a
    /// fraction expressed in basis points (6000 bps = 60%).
    pub far_share_min: Bps,
    /// Consecutive identical sizes needed to fire.
    pub uniform_run_min: usize,
    /// A size is "round" when it is an exact multiple of this. Instrument
    /// specific; [`Qty::ZERO`] disables the check.
    pub round_unit: Qty,
    /// Minimum levels present before the round-number share means anything.
    pub round_min_levels: usize,
    /// Percentage of levels that must be round to fire.
    pub round_share_pct: u32,
    /// Instrument tick. [`Price::ZERO`] means unknown; the gap check then uses
    /// `gap_max_bps`.
    pub tick_size: Price,
    /// Ticks between adjacent levels tolerated before firing.
    pub gap_max_ticks: i64,
    /// Fallback gap threshold, in basis points of the touch price, used when
    /// `tick_size` is unknown.
    pub gap_max_bps: Bps,
    /// Ratio of fat side to thin side that fires, in basis points
    /// (40000 bps = 4.0x).
    pub one_sided_ratio_min: Bps,
}

impl PaintLimits {
    /// The shipped thresholds. Each one, and what happens on either side of it:
    ///
    /// - **`touch_reference_levels = 4`.** Four levels behind the touch is
    ///   enough that one odd size cannot dominate the mean, and shallow enough
    ///   that the mean still describes the part of the ladder a taker of
    ///   ordinary size actually reaches. Fewer and the mean is noise; more and
    ///   it starts averaging in depth that is irrelevant to the touch.
    /// - **`touch_thin_factor = 8`.** A real ladder decays; it does not fall off
    ///   a cliff at level one. A touch smaller than one eighth of the mean of
    ///   the next four levels is not a decay curve, it is a placeholder that
    ///   moves the displayed best price for almost no capital. Chosen so a
    ///   genuinely tapering book — touch around half the mean behind it — never
    ///   fires and only an order-of-magnitude discontinuity does. Lower it and
    ///   ordinary tapering books fire; raise it and a token touch stops being
    ///   detectable.
    /// - **`far_bps = 25`.** Twelve basis points is our edge, so size sitting
    ///   more than twice the edge away from the touch cannot participate in the
    ///   trade we are pricing: we would have consumed the whole edge before
    ///   reaching it. Below ~25 bps the "far" bucket starts including levels a
    ///   real fill would actually touch, which makes the signal fire on healthy
    ///   deep books.
    /// - **`far_share_min = 6000 bps` (60%).** A clear majority, not a bare one.
    ///   A real book can easily have 50/50 near/far split; at 60% and above the
    ///   ladder's centre of mass sits where it will never trade, which is where
    ///   a maker gets a depth chart for free.
    /// - **`uniform_run_min = 4`.** Two identical sizes is a coincidence, and on
    ///   a coarse-lot instrument three is too. Four consecutive byte-identical
    ///   sizes needs an explanation. Set to 3 and coarse-lot instruments fire
    ///   constantly; set to 6 and a short painted ladder slips through.
    /// - **`round_unit = 1.0`.** Deliberately generic and almost certainly wrong
    ///   for most instruments: the right unit is a multiple of the instrument's
    ///   lot size. `[assumption]` For a BTC perp, whole-unit sizes are a real
    ///   tell; for a token trading in millions of units it is meaningless. Set
    ///   this per instrument or the signal is noise.
    /// - **`round_min_levels = 4`.** With three levels, "all round" happens by
    ///   chance often enough to be worthless.
    /// - **`round_share_pct = 90`.** Requiring literally every level to be round
    ///   makes the signal defeatable by one random size, which costs a script
    ///   author nothing. 90% allows one irregular level in ten.
    /// - **`gap_max_ticks = 20`.** Venues quote in ticks and makers cluster at
    ///   adjacent ones, so a healthy ladder near the touch is contiguous or
    ///   close to it. Twenty ticks of nothing between two displayed levels means
    ///   that whole span is unquoted, which is what a ladder looks like when
    ///   someone placed a handful of orders to make a depth chart. Below about
    ///   five ticks, ordinary thin books fire constantly.
    /// - **`gap_max_bps = 25`.** Only used when the tick is unknown. On a liquid
    ///   instrument adjacent levels sit small fractions of a basis point apart,
    ///   so 25 bps is orders of magnitude past normal; on an illiquid alt with a
    ///   coarse tick, 25 bps gaps are ordinary, so this threshold deliberately
    ///   says nothing about those instruments. Prefer the tick path.
    /// - **`one_sided_ratio_min = 40000 bps` (4.0x).** Real books are routinely
    ///   2x lopsided and 3x is unremarkable after news. At 4x the imbalance is
    ///   large enough to be worth one signal's worth of suspicion, and this
    ///   signal is weighted lowest precisely because one-way flow is a real
    ///   thing that happens.
    ///
    /// `[assumption]` — all of it. See the module docs: nothing here has been
    /// checked against a real book.
    pub const CONSERVATIVE: PaintLimits = PaintLimits {
        touch_reference_levels: 4,
        touch_thin_factor: 8,
        // 25 bps at Bps scale 1e8.
        far_bps: Bps::from_raw(25 * 100_000_000),
        // 6000 bps == 60%.
        far_share_min: Bps::from_raw(6_000 * 100_000_000),
        uniform_run_min: 4,
        // 1.0 at Qty scale 1e12.
        round_unit: Qty::from_raw(1_000_000_000_000),
        round_min_levels: 4,
        round_share_pct: 90,
        // Unknown by default: no accessor exists to read it off the spec.
        tick_size: Price::ZERO,
        gap_max_ticks: 20,
        gap_max_bps: Bps::from_raw(25 * 100_000_000),
        // 40000 bps == 400% == 4.0x.
        one_sided_ratio_min: Bps::from_raw(40_000 * 100_000_000),
    };
}

/// What a single snapshot supports saying about one side of a ladder.
///
/// There is no field that means "real". `score == 0` means no evidence of
/// painting was found in this snapshot; it does not mean the depth will be there
/// when you take it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintReport {
    /// Signals that fired, in the order they were tested.
    pub signals: Vec<PaintSignal>,
    /// `0..=100`. 0 = no evidence of painting found, 100 = strong evidence.
    /// Integer arithmetic only: the sum of the fired signals' weights,
    /// saturated at 100.
    pub score: u8,
}

impl PaintReport {
    /// True when `signal` fired.
    pub fn contains(&self, signal: PaintSignal) -> bool {
        self.signals.iter().any(|s| *s == signal)
    }

    /// True when the score reaches `min`. Named for what it is: suspicion, not
    /// a finding.
    pub const fn is_suspicious_at(&self, min: u8) -> bool {
        self.score >= min
    }
}

/// Sum the sizes of a slice of levels.
fn sum_qty(levels: &[Level]) -> Result<Qty, DetectError> {
    let mut acc = Qty::ZERO;
    for l in levels {
        acc = acc.checked_add(l.qty)?;
    }
    Ok(acc)
}

/// Test one side of a book for the structural tells of painted depth.
///
/// `side` is the side you would *take*: `Side::Buy` assesses the asks you would
/// lift, via [`BookSnapshot::taking_levels`].
///
/// Signals that cannot be evaluated are skipped rather than guessed: a
/// three-level ladder gets no round-number verdict, and a two-level ladder gets
/// no touch-thinness verdict. A skipped signal lowers the score, so a very short
/// ladder can score 0 here — that is not a claim about its depth, it is the
/// absence of a test. Short ladders are the caller's `THIN_DEPTH` and
/// `PARTIAL_BOOK` problem, not this function's.
///
/// Every division rounds with [`RoundMode::COST`], i.e. in whichever direction
/// makes a signal more likely to fire.
pub fn assess_paint(
    book: &BookSnapshot,
    side: Side,
    limits: &PaintLimits,
) -> Result<PaintReport, DetectError> {
    if limits.touch_reference_levels < 2 {
        return Err(DetectError::UnusableLimits(
            "touch_reference_levels must be at least 2 for a mean to mean anything",
        ));
    }

    let levels = book.taking_levels(side);
    let touch = match levels.first() {
        Some(l) => l,
        // No levels at all is missing data, not painted data.
        None => return Err(DetectError::EmptySide),
    };
    let touch_price = touch.price;
    let mut signals: Vec<PaintSignal> = Vec::new();

    // ---- TouchTooThin ----------------------------------------------------
    // Compare the touch against the mean of the next N levels. The mean rounds
    // UP (COST), which makes the comparison marginally more likely to fire.
    let behind = levels.get(1..).unwrap_or(&[]);
    let take = behind.len().min(limits.touch_reference_levels);
    let refs = behind.get(..take).unwrap_or(&[]);
    if refs.len() >= 2 {
        let n = i64::try_from(refs.len())
            .map_err(|_| DetectError::UnusableLimits("reference level count exceeds i64"))?;
        let mean = sum_qty(refs)?.checked_div_int(n, RoundMode::COST)?;
        let scaled_touch = touch.qty.checked_mul_int(limits.touch_thin_factor)?;
        if scaled_touch < mean {
            signals.push(PaintSignal::TouchTooThin);
        }
    }

    // ---- DepthConcentratedFar --------------------------------------------
    let side_total = sum_qty(levels)?;
    if side_total.is_positive() {
        let far_edge = limits.far_bps.to_rate();
        let mut far = Qty::ZERO;
        for l in levels {
            // Distance from the touch as a fraction of the touch price. Rounded
            // away from zero so a level exactly on the edge counts as far.
            let diff = l.price.checked_sub(touch_price)?.checked_abs()?;
            let dist = diff.div_price(touch_price, RoundMode::COST)?;
            if dist > far_edge {
                far = far.checked_add(l.qty)?;
            }
        }
        // Share as a Rate, compared against the threshold's Rate form. `Bps` and
        // `Rate` share a raw integer, so `to_rate` is exact and free, and
        // comparing Rates avoids the fallible narrowing of `to_bps`.
        let share = far.div_qty(side_total, RoundMode::COST)?;
        if share >= limits.far_share_min.to_rate() {
            signals.push(PaintSignal::DepthConcentratedFar);
        }
    }

    // ---- UniformLevelSizes -----------------------------------------------
    let mut run: usize = 0;
    let mut longest_run: usize = 0;
    let mut prev: Option<Qty> = None;
    for l in levels {
        run = match prev {
            Some(p) if p == l.qty => run.saturating_add(1),
            _ => 1,
        };
        if run > longest_run {
            longest_run = run;
        }
        prev = Some(l.qty);
    }
    if longest_run >= limits.uniform_run_min {
        signals.push(PaintSignal::UniformLevelSizes);
    }

    // ---- RoundNumberSizes ------------------------------------------------
    // Roundness is only meaningful relative to a unit the caller supplies; a
    // zero unit disables the check rather than dividing by zero.
    if levels.len() >= limits.round_min_levels && limits.round_unit.is_positive() {
        let unit = limits.round_unit.raw();
        // Same-dimension modulus on raw integers: the digest exposes no
        // checked_rem, and the divisor is known non-zero here.
        let round_count = levels.iter().filter(|l| l.qty.raw() % unit == 0).count();
        let lhs = (round_count as u128).saturating_mul(100);
        let rhs = (levels.len() as u128).saturating_mul(u128::from(limits.round_share_pct));
        if lhs >= rhs {
            signals.push(PaintSignal::RoundNumberSizes);
        }
    }

    // ---- PriceGapsInLadder -----------------------------------------------
    let max_gap: Option<Price> = if limits.tick_size.is_positive() {
        Some(limits.tick_size.checked_mul_int(limits.gap_max_ticks)?)
    } else {
        None
    };
    let gap_edge = limits.gap_max_bps.to_rate();
    for window in levels.windows(2) {
        let (a, b) = match (window.first(), window.get(1)) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        let gap = b.price.checked_sub(a.price)?.checked_abs()?;
        let fired = match max_gap {
            Some(max) => gap > max,
            None => gap.div_price(touch_price, RoundMode::COST)? > gap_edge,
        };
        if fired {
            signals.push(PaintSignal::PriceGapsInLadder);
            break;
        }
    }

    // ---- OneSidedDepth ---------------------------------------------------
    // A property of the whole book, not of the side under test, so it fires
    // whichever side is the fat one.
    let other_total = sum_qty(book.taking_levels(side.opposite()))?;
    let (fat, thin) = if side_total >= other_total {
        (side_total, other_total)
    } else {
        (other_total, side_total)
    };
    if thin.is_zero() {
        if fat.is_positive() {
            signals.push(PaintSignal::OneSidedDepth);
        }
    } else {
        let ratio = fat.div_qty(thin, RoundMode::COST)?;
        if ratio >= limits.one_sided_ratio_min.to_rate() {
            signals.push(PaintSignal::OneSidedDepth);
        }
    }

    // Weights sum to 100 across all six signals, so the saturation below is
    // defence against a future weight change, not something reachable today.
    let total: u32 = signals.iter().map(|s| u32::from(s.weight())).sum();
    let score = if total > 100 { 100u8 } else { total as u8 };

    Ok(PaintReport { signals, score })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{
        DataQuality, InstrumentKey, LocalTs, MarketKind, Venue, VenueKey,
    };

    fn lvl(price: &str, qty: &str) -> Level {
        Level::new(Price::parse(price).unwrap(), Qty::parse(qty).unwrap()).unwrap()
    }

    fn vkey() -> VenueKey {
        VenueKey::new(
            Venue::Binance,
            InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").unwrap(),
        )
    }

    /// Every book in these tests was invented from memory of API shapes and has
    /// never been near a live network, so it is built `SYNTHESIZED`.
    fn book(bids: Vec<Level>, asks: Vec<Level>) -> BookSnapshot {
        BookSnapshot::new(
            vkey(),
            bids,
            asks,
            Some(1),
            None,
            LocalTs::from_millis(0),
            DataQuality::SYNTHESIZED,
        )
        .unwrap()
    }

    /// Shipped limits with the tick filled in, which is the intended usage.
    fn limits() -> PaintLimits {
        PaintLimits {
            tick_size: Price::parse("0.01").unwrap(),
            ..PaintLimits::CONSERVATIVE
        }
    }

    /// A plausible healthy ask ladder: sizes grow with distance, prices are
    /// contiguous at one tick, nothing is round, both sides are comparable.
    /// asks 0.5 + 0.8 + 1.2 + 1.5 + 2.0 = 6.0
    /// bids 0.6 + 0.9 + 1.3 + 1.2 + 1.5 = 5.5
    fn healthy_book() -> BookSnapshot {
        book(
            vec![
                lvl("99.99", "0.6"),
                lvl("99.98", "0.9"),
                lvl("99.97", "1.3"),
                lvl("99.96", "1.2"),
                lvl("99.95", "1.5"),
            ],
            vec![
                lvl("100.00", "0.5"),
                lvl("100.01", "0.8"),
                lvl("100.02", "1.2"),
                lvl("100.03", "1.5"),
                lvl("100.04", "2.0"),
            ],
        )
    }

    #[test]
    fn healthy_book_produces_no_signals() {
        // TouchTooThin: mean of next 4 = (0.8+1.2+1.5+2.0)/4 = 5.5/4 = 1.375;
        //   touch 0.5 * 8 = 4.0, and 4.0 > 1.375 -> silent.
        // DepthConcentratedFar: 25 bps of 100.00 = 0.25, so "far" starts past
        //   100.25; no level is out there -> far share 0% -> silent.
        // UniformLevelSizes: all five sizes differ -> longest run 1 -> silent.
        // RoundNumberSizes: only 2.0 is a multiple of 1.0 -> 1*100 = 100, and
        //   5*90 = 450, 100 < 450 -> silent.
        // PriceGapsInLadder: steps are 0.01 = 1 tick, max is 20 ticks = 0.20.
        // OneSidedDepth: 6.0 / 5.5 = 1.0909... which is under 4.0 -> silent.
        let r = assess_paint(&healthy_book(), Side::Buy, &limits()).unwrap();
        assert_eq!(r.signals, Vec::new());
        assert_eq!(r.score, 0);
    }

    #[test]
    fn healthy_book_is_also_clean_from_the_sell_side() {
        let r = assess_paint(&healthy_book(), Side::Sell, &limits()).unwrap();
        assert_eq!(r.signals, Vec::new());
        assert_eq!(r.score, 0);
    }

    #[test]
    fn touch_too_thin_fires() {
        // asks: 0.01 at the touch, then four levels of 1.0.
        // mean of next 4 = (1.0*4)/4 = 1.0 exactly.
        // 0.01 * 8 = 0.08, and 0.08 < 1.0 -> fires.
        // Bids sum 4.0 vs asks 4.01: ratio 1.0025, no OneSidedDepth.
        // Sizes 1.0 repeat four times, so UniformLevelSizes fires too and the
        // score is 20 + 25 = 45; this test asserts only the signal of interest.
        let b = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![
                lvl("100.00", "0.01"),
                lvl("100.01", "1.0"),
                lvl("100.02", "1.0"),
                lvl("100.03", "1.0"),
                lvl("100.04", "1.0"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(r.contains(PaintSignal::TouchTooThin));
    }

    #[test]
    fn touch_at_half_the_mean_does_not_fire() {
        // Tapering, not painted: touch 0.6, mean of next 4 = (1.0+1.2+1.4+1.6)/4
        // = 5.2/4 = 1.3. 0.6 * 8 = 4.8 > 1.3 -> silent.
        let b = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![
                lvl("100.00", "0.6"),
                lvl("100.01", "1.0"),
                lvl("100.02", "1.2"),
                lvl("100.03", "1.4"),
                lvl("100.04", "1.6"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(!r.contains(PaintSignal::TouchTooThin));
    }

    #[test]
    fn touch_thinness_is_skipped_on_a_two_level_ladder() {
        // One reference level is not a mean. Signal skipped, not guessed.
        let b = book(
            vec![lvl("99.99", "0.5"), lvl("99.98", "0.5")],
            vec![lvl("100.00", "0.001"), lvl("100.01", "1.0")],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(!r.contains(PaintSignal::TouchTooThin));
    }

    #[test]
    fn depth_concentrated_far_fires() {
        // asks: 1.0 at 100.00, 1.0 at 100.01, 4.0 at 101.00.
        // 25 bps of 100.00 = 0.25 -> far starts past 100.25, so only 101.00.
        // far = 4.0, total = 6.0, share = 4/6 = 0.6666... >= 0.60 -> fires.
        // TouchTooThin: mean of next 2 = (1.0+4.0)/2 = 2.5; 1.0*8 = 8 > 2.5.
        // The 0.99 hole between 100.01 and 101.00 is 99 ticks, so
        // PriceGapsInLadder fires as well; asserted separately below.
        let b = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![lvl("100.00", "1.0"), lvl("100.01", "1.0"), lvl("101.00", "4.0")],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(r.contains(PaintSignal::DepthConcentratedFar));
    }

    #[test]
    fn depth_just_under_the_share_threshold_does_not_fire() {
        // far = 2.5 (only the 101.00 level is past 100.25), total = 2.0 + 1.0 +
        // 2.5 = 5.5, share = 2.5/5.5 = 0.4545... which is under 0.60 -> silent.
        let b = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![lvl("100.00", "2.0"), lvl("100.01", "1.0"), lvl("101.00", "2.5")],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(!r.contains(PaintSignal::DepthConcentratedFar));
    }

    #[test]
    fn uniform_level_sizes_fires() {
        // Five levels of exactly 2.5: longest run 5 >= 4 -> fires.
        // 2.5 is not a multiple of 1.0, so RoundNumberSizes stays silent.
        // asks total 12.5, bids total 10.0 -> ratio 1.25, no OneSidedDepth.
        // Touch 2.5 vs mean 2.5: 2.5*8 = 20 > 2.5 -> no TouchTooThin.
        let b = book(
            vec![lvl("99.99", "5.0"), lvl("99.98", "5.0")],
            vec![
                lvl("100.00", "2.5"),
                lvl("100.01", "2.5"),
                lvl("100.02", "2.5"),
                lvl("100.03", "2.5"),
                lvl("100.04", "2.5"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert_eq!(r.signals, vec![PaintSignal::UniformLevelSizes]);
        assert_eq!(r.score, 25);
    }

    #[test]
    fn three_identical_sizes_do_not_fire() {
        // longest run 3 < uniform_run_min 4.
        let b = book(
            vec![lvl("99.99", "5.0"), lvl("99.98", "5.0")],
            vec![
                lvl("100.00", "2.5"),
                lvl("100.01", "2.5"),
                lvl("100.02", "2.5"),
                lvl("100.03", "3.5"),
                lvl("100.04", "4.5"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(!r.contains(PaintSignal::UniformLevelSizes));
    }

    #[test]
    fn round_number_sizes_fires() {
        // Sizes 1, 2, 3, 5, 8 are all multiples of 1.0: 5 round of 5 levels,
        // 5*100 = 500 >= 5*90 = 450 -> fires. All distinct, so no uniform run.
        // TouchTooThin: mean of next 4 = (2+3+5+8)/4 = 18/4 = 4.5;
        //   1.0 * 8 = 8.0 > 4.5 -> silent.
        // asks total 19.0, bids total 15.0 -> ratio 1.266..., no OneSidedDepth.
        let b = book(
            vec![lvl("99.99", "7.5"), lvl("99.98", "7.5")],
            vec![
                lvl("100.00", "1.0"),
                lvl("100.01", "2.0"),
                lvl("100.02", "3.0"),
                lvl("100.03", "5.0"),
                lvl("100.04", "8.0"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert_eq!(r.signals, vec![PaintSignal::RoundNumberSizes]);
        assert_eq!(r.score, 15);
    }

    #[test]
    fn one_irregular_size_in_five_still_fires() {
        // 4 round of 5 -> 4*100 = 400, threshold 5*90 = 450. 400 < 450 -> the
        // 90% rule needs 4.5 of 5, so a single irregular level in five is
        // enough to silence it. Documented behaviour, asserted so a future
        // change to round_share_pct is visible.
        let b = book(
            vec![lvl("99.99", "7.5"), lvl("99.98", "7.5")],
            vec![
                lvl("100.00", "1.0"),
                lvl("100.01", "2.0"),
                lvl("100.02", "3.0"),
                lvl("100.03", "5.0"),
                lvl("100.04", "8.5"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(!r.contains(PaintSignal::RoundNumberSizes));
    }

    #[test]
    fn round_unit_of_zero_disables_the_check() {
        let lim = PaintLimits {
            round_unit: Qty::ZERO,
            ..limits()
        };
        let b = book(
            vec![lvl("99.99", "7.5"), lvl("99.98", "7.5")],
            vec![
                lvl("100.00", "1.0"),
                lvl("100.01", "2.0"),
                lvl("100.02", "3.0"),
                lvl("100.03", "5.0"),
                lvl("100.04", "8.0"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &lim).unwrap();
        assert!(!r.contains(PaintSignal::RoundNumberSizes));
        assert_eq!(r.score, 0);
    }

    #[test]
    fn price_gaps_in_ladder_fires_on_ticks() {
        // 100.01 -> 100.50 is 0.49, which is 49 ticks of 0.01; the limit is 20
        // ticks = 0.20. 0.49 > 0.20 -> fires.
        // far check: far = 2.0 of total 4.5 = 44.4% < 60% -> silent.
        // Only 3 levels, so the round-number check is skipped.
        let b = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![lvl("100.00", "1.0"), lvl("100.01", "1.5"), lvl("100.50", "2.0")],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert_eq!(r.signals, vec![PaintSignal::PriceGapsInLadder]);
        assert_eq!(r.score, 10);
    }

    #[test]
    fn twenty_tick_gap_is_exactly_tolerated() {
        // 100.00 -> 100.20 is 0.20 = 20 ticks, and the test is `gap > max`, so
        // exactly 20 ticks does not fire. 21 ticks (0.21) does.
        let ok = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![lvl("100.00", "1.0"), lvl("100.20", "1.5"), lvl("100.21", "2.0")],
        );
        assert!(!assess_paint(&ok, Side::Buy, &limits())
            .unwrap()
            .contains(PaintSignal::PriceGapsInLadder));

        let bad = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![lvl("100.00", "1.0"), lvl("100.21", "1.5"), lvl("100.22", "2.0")],
        );
        assert!(assess_paint(&bad, Side::Buy, &limits())
            .unwrap()
            .contains(PaintSignal::PriceGapsInLadder));
    }

    #[test]
    fn unknown_tick_falls_back_to_bps() {
        // tick unknown. 100.00 -> 100.50 is 0.50, and 0.50/100.00 = 0.005 =
        // 50 bps, which is past the 25 bps fallback -> fires.
        let lim = PaintLimits {
            tick_size: Price::ZERO,
            ..PaintLimits::CONSERVATIVE
        };
        let b = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![lvl("100.00", "1.0"), lvl("100.50", "1.5"), lvl("100.51", "2.0")],
        );
        assert!(assess_paint(&b, Side::Buy, &lim)
            .unwrap()
            .contains(PaintSignal::PriceGapsInLadder));

        // 0.10 apart is 10 bps of 100.00, under the 25 bps fallback -> silent.
        let tight = book(
            vec![lvl("99.99", "2.0"), lvl("99.98", "2.0")],
            vec![lvl("100.00", "1.0"), lvl("100.10", "1.5"), lvl("100.20", "2.0")],
        );
        assert!(!assess_paint(&tight, Side::Buy, &lim)
            .unwrap()
            .contains(PaintSignal::PriceGapsInLadder));
    }

    #[test]
    fn one_sided_depth_fires() {
        // asks 1.5 + 2.0 + 2.5 + 1.7 + 2.3 = 10.0; bids 1.0 + 1.0 = 2.0.
        // 10.0 / 2.0 = 5.0 >= 4.0 -> fires.
        // TouchTooThin: mean of next 4 = (2.0+2.5+1.7+2.3)/4 = 8.5/4 = 2.125;
        //   1.5 * 8 = 12.0 > 2.125 -> silent.
        let b = book(
            vec![lvl("99.99", "1.0"), lvl("99.98", "1.0")],
            vec![
                lvl("100.00", "1.5"),
                lvl("100.01", "2.0"),
                lvl("100.02", "2.5"),
                lvl("100.03", "1.7"),
                lvl("100.04", "2.3"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert_eq!(r.signals, vec![PaintSignal::OneSidedDepth]);
        assert_eq!(r.score, 10);
    }

    #[test]
    fn one_sided_depth_fires_whichever_side_is_fat() {
        // Same book seen from the thin side: one-sidedness is a property of the
        // book, so the verdict does not depend on which side we asked about.
        let b = book(
            vec![lvl("99.99", "1.0"), lvl("99.98", "1.0")],
            vec![
                lvl("100.00", "1.5"),
                lvl("100.01", "2.0"),
                lvl("100.02", "2.5"),
                lvl("100.03", "1.7"),
                lvl("100.04", "2.3"),
            ],
        );
        assert!(assess_paint(&b, Side::Sell, &limits())
            .unwrap()
            .contains(PaintSignal::OneSidedDepth));
    }

    #[test]
    fn three_times_lopsided_does_not_fire() {
        // asks 6.0, bids 2.0 -> exactly 3.0x, under the 4.0x threshold.
        let b = book(
            vec![lvl("99.99", "1.0"), lvl("99.98", "1.0")],
            vec![
                lvl("100.00", "0.9"),
                lvl("100.01", "1.1"),
                lvl("100.02", "1.3"),
                lvl("100.03", "1.2"),
                lvl("100.04", "1.5"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(!r.contains(PaintSignal::OneSidedDepth));
    }

    #[test]
    fn exactly_four_times_lopsided_fires() {
        // asks 8.0, bids 2.0 -> 4.0x exactly. The boundary belongs to the worse
        // bucket, so it fires.
        let b = book(
            vec![lvl("99.99", "1.0"), lvl("99.98", "1.0")],
            vec![
                lvl("100.00", "1.4"),
                lvl("100.01", "1.5"),
                lvl("100.02", "1.6"),
                lvl("100.03", "1.7"),
                lvl("100.04", "1.8"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(r.contains(PaintSignal::OneSidedDepth));
    }

    #[test]
    fn empty_side_is_an_error_not_a_score() {
        let b = book(vec![lvl("99.99", "1.0")], Vec::new());
        assert_eq!(
            assess_paint(&b, Side::Buy, &limits()),
            Err(DetectError::EmptySide)
        );
    }

    #[test]
    fn degenerate_limits_are_refused() {
        let lim = PaintLimits {
            touch_reference_levels: 1,
            ..limits()
        };
        assert!(matches!(
            assess_paint(&healthy_book(), Side::Buy, &lim),
            Err(DetectError::UnusableLimits(_))
        ));
    }

    #[test]
    fn a_thoroughly_painted_book_scores_high() {
        // Touch 0.001 with four identical 10.0 levels parked 100+ bps away, and
        // nothing on the bid.
        // TouchTooThin: mean of next 4 = 40.0/4 = 10.0; 0.001*8 = 0.008 < 10.0.
        // DepthConcentratedFar: far = 40.0 of 40.001 -> 99.99% >= 60%.
        // UniformLevelSizes: run of 4 >= 4.
        // RoundNumberSizes: 10.0 x4 are round, 0.001 is not -> 4 of 5 -> 400 <
        //   450, so this one stays silent.
        // PriceGapsInLadder: 100.00 -> 101.00 is 100 ticks > 20.
        // OneSidedDepth: bid side has 0.5, 40.001/0.5 = 80x >= 4x.
        // Score = 20 + 20 + 25 + 10 + 10 = 85.
        let b = book(
            vec![lvl("99.00", "0.5")],
            vec![
                lvl("100.00", "0.001"),
                lvl("101.00", "10.0"),
                lvl("101.01", "10.0"),
                lvl("101.02", "10.0"),
                lvl("101.03", "10.0"),
            ],
        );
        let r = assess_paint(&b, Side::Buy, &limits()).unwrap();
        assert!(r.contains(PaintSignal::TouchTooThin));
        assert!(r.contains(PaintSignal::DepthConcentratedFar));
        assert!(r.contains(PaintSignal::UniformLevelSizes));
        assert!(r.contains(PaintSignal::PriceGapsInLadder));
        assert!(r.contains(PaintSignal::OneSidedDepth));
        assert!(!r.contains(PaintSignal::RoundNumberSizes));
        assert_eq!(r.score, 85);
    }

    #[test]
    fn weights_sum_to_one_hundred() {
        // Keeps the score ceiling honest: all six signals is exactly 100.
        let all = [
            PaintSignal::TouchTooThin,
            PaintSignal::DepthConcentratedFar,
            PaintSignal::UniformLevelSizes,
            PaintSignal::RoundNumberSizes,
            PaintSignal::PriceGapsInLadder,
            PaintSignal::OneSidedDepth,
        ];
        let total: u32 = all.iter().map(|s| u32::from(s.weight())).sum();
        assert_eq!(total, 100);
        // And no single signal can clear the trust-flag threshold alone.
        assert!(all.iter().all(|s| s.weight() < crate::trust::PAINT_FLAG_MIN_SCORE));
    }
}
