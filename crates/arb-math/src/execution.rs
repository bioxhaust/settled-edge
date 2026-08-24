//! Execution against real depth: what your size actually fills at.
//!
//! This module is the difference between a scanner and a tool. The touch price
//! is the price of the last trade someone else did. Yours fills against the
//! ladder, and for any size that matters the average is worse than the touch.
//!
//! Three functions:
//!
//! - [`executable_price`] — walk the ladder for a given size, return the VWAP,
//!   the size actually available, and whether the book ran out.
//! - [`slippage_bps`] — the cost of that walk, as a non-negative number of basis
//!   points, for either side.
//! - [`max_size_within_slippage`] — invert the question: given a slippage
//!   budget, how much can we actually do? This is the *capacity* number, and it
//!   is the one a spread-only display cannot produce at all.
//!
//! # Direction, stated once
//!
//! A buyer lifts **asks**. A seller hits **bids**. This module never chooses a
//! side itself; it always goes through [`BookSnapshot::taking_levels`], which
//! encodes the mapping in `arb-types` so it cannot be re-derived incorrectly
//! here.
//!
//! # Rounding, stated once
//!
//! For a **buy**, notional and VWAP are costs, so both round with
//! [`RoundMode::COST`] — we never under-report what we pay. For a **sell**, they
//! are proceeds, so both round with [`RoundMode::REVENUE`] — we never
//! over-report what we receive. Slippage, being a cost either way, always rounds
//! [`RoundMode::COST`]. Capacity always rounds *down*, because over-stating how
//! much the book can absorb is how a position ends up half-filled.

use crate::error::MathError;
use arb_num::{Bps, Money, Price, Qty, Rate, RoundMode};
use arb_types::{BookSnapshot, QualityFlags, Side};

/// The rounding mode whose direction is conservative for `side`.
const fn conservative_mode(side: Side) -> RoundMode {
    match side {
        // Buying: the notional is money out. Overstate it.
        Side::Buy => RoundMode::COST,
        // Selling: the notional is money in. Understate it.
        Side::Sell => RoundMode::REVENUE,
    }
}

/// What a given size would actually do against the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Execution {
    /// Side we are taking (we lift asks when this is `Buy`).
    pub side: Side,
    /// Size we asked for.
    pub requested: Qty,
    /// Size the book could actually provide, at or before running out.
    pub filled: Qty,
    /// Gross notional of the fill, before fees. Signed positive either way: it
    /// is money paid on a buy and money received on a sell.
    pub notional: Money,
    /// Volume-weighted average price of the fill.
    pub vwap: Price,
    /// Best price on the side we took, for reference.
    pub touch: Price,
    /// How many levels the walk consumed, fully or partially.
    pub levels_consumed: usize,
    /// `true` when `filled == requested`.
    pub complete: bool,
    /// [`QualityFlags::SIZE_EXCEEDS_DEPTH`] when the book ran out. The caller
    /// must combine this with the book's own quality.
    pub flags: QualityFlags,
}

impl Execution {
    /// Size we could not fill.
    pub fn shortfall(&self) -> Result<Qty, MathError> {
        self.requested.checked_sub(self.filled).map_err(MathError::from)
    }
}

/// Walk the ladder and report what `size` really costs.
///
/// Returns [`MathError::EmptyBook`] when the side we need has no levels, and
/// [`MathError::ZeroSize`] for a zero or negative request. A partial fill is not
/// an error: it is a fact about the market, reported through `complete`,
/// `filled` and [`QualityFlags::SIZE_EXCEEDS_DEPTH`]. Turning it into an error
/// would tempt callers to fall back to the touch price, which is the failure
/// this module exists to prevent.
pub fn executable_price(
    book: &BookSnapshot,
    side: Side,
    size: Qty,
) -> Result<Execution, MathError> {
    if !size.is_positive() {
        return Err(MathError::ZeroSize);
    }
    let levels = book.taking_levels(side);
    let touch = match book.touch(side) {
        Some(p) => p,
        None => return Err(MathError::EmptyBook),
    };
    let mode = conservative_mode(side);

    let mut remaining = size;
    let mut filled = Qty::ZERO;
    let mut notional = Money::ZERO;
    let mut levels_consumed = 0usize;

    for level in levels {
        if !remaining.is_positive() {
            break;
        }
        let take = if level.qty < remaining {
            level.qty
        } else {
            remaining
        };
        notional = notional.checked_add(level.price.mul_qty(take, mode)?)?;
        filled = filled.checked_add(take)?;
        remaining = remaining.checked_sub(take)?;
        levels_consumed += 1;
    }

    // `filled` cannot be zero here: the side had at least one level, every level
    // has a positive quantity, and `size` is positive — so the first iteration
    // takes something. The guard is not defensive programming, it is the only
    // way to divide without risking `DivByZero` reaching the caller as an
    // arithmetic error when the real cause was an empty book.
    if !filled.is_positive() {
        return Err(MathError::EmptyBook);
    }

    let complete = !remaining.is_positive();
    let flags = if complete {
        QualityFlags::NONE
    } else {
        QualityFlags::SIZE_EXCEEDS_DEPTH
    };

    Ok(Execution {
        side,
        requested: size,
        filled,
        notional,
        vwap: notional.div_qty(filled, mode)?,
        touch,
        levels_consumed,
        complete,
        flags,
    })
}

/// Slippage of a fill against the touch, as a non-negative cost in basis points.
///
/// Defined as the fraction of value lost, which differs by side:
///
/// - Buy: `(vwap - touch) / touch` — the extra we paid.
/// - Sell: `(touch - vwap) / touch` — the shortfall in what we received.
///
/// Both are "how much worse than the touch", so both are positive for a normal
/// fill and zero for a fill entirely at the touch. Returning a signed number
/// whose meaning flipped with the side would guarantee a sign error in a caller
/// summing costs across two legs.
///
/// A negative result is possible only if `vwap` is *better* than the touch,
/// which cannot happen from a ladder walk and would indicate the caller passed
/// mismatched inputs. It is returned as-is rather than clamped, because silently
/// clamping would hide that bug.
pub fn slippage_bps(vwap: Price, touch: Price, side: Side) -> Result<Bps, MathError> {
    if !touch.is_positive() {
        return Err(MathError::NonPositivePrice);
    }
    let ratio = match side {
        // vwap/touch - 1
        Side::Buy => vwap
            .div_price(touch, RoundMode::COST)?
            .checked_sub(Rate::ONE)?,
        // 1 - vwap/touch
        Side::Sell => Rate::ONE.checked_sub(vwap.div_price(touch, RoundMode::REVENUE)?)?,
    };
    ratio.to_bps().map_err(MathError::from)
}

/// A slippage tolerance, in basis points.
///
/// A newtype because a bare `Bps` at a call site could equally be a fee, and the
/// two are added together often enough that swapping them is a live risk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlippageBudget(Bps);

impl SlippageBudget {
    /// Build a budget, rejecting a negative one.
    pub fn new(bps: Bps) -> Result<Self, MathError> {
        if bps.is_negative() {
            return Err(MathError::NegativeFeeRate);
        }
        Ok(Self(bps))
    }

    /// The tolerance.
    pub const fn bps(self) -> Bps {
        self.0
    }
}

/// The largest size whose **average** fill price stays inside `budget`.
///
/// Not the deepest level within the budget — the largest *cumulative* size whose
/// VWAP stays within it. Those are different numbers, and the difference is
/// always in the flattering direction: the deepest qualifying level is a larger
/// size than the VWAP constraint actually permits.
///
/// # The walk
///
/// Levels at or better than the limit price are taken whole; they cannot breach
/// an average that the limit bounds. Past that point the cheap fills already in
/// hand are a *subsidy*, and the walk must keep spending it: with cumulative size
/// `Q`, cumulative cost `C`, limit price `L` and level price `p` worse than `L`,
/// the largest additional `x` satisfying `(C + p·x)/(Q + x) <= L` (buy; the sell
/// case mirrors it) is `x = (L·Q - C)/(p - L)`.
///
/// If that `x` covers the whole level, the level is taken, **charged to `C`**, and
/// the walk continues — the next level may be affordable out of what is left of
/// the subsidy. Only a genuinely partial `x` ends the walk, because the average
/// has then landed exactly on the limit and nothing deeper can help.
///
/// Stopping at the first level worse than the limit, or taking it without adding
/// its cost, understates capacity without bound: on a one-dollar-tick BTC ladder
/// at 25 bps it reports 172 coins where 341 fit. Understated capacity is not a
/// safe error here — it is the size we can trade and cannot see.
///
/// # Rounding
///
/// Every rounding under-states capacity. `C` accumulates with the same
/// conservative mode [`executable_price`] will charge (`COST` on a buy, `REVENUE`
/// on a sell), because a solver whose cost model is cheaper than execution's hands
/// back a size that then breaches the budget by an ulp. `L·Q` rounds so that the
/// numerator shrinks — down on a buy, up on a sell — and `x` truncates. The limit
/// price itself is rounded to make the budget tighter rather than looser.
pub fn max_size_within_slippage(
    book: &BookSnapshot,
    side: Side,
    budget: SlippageBudget,
) -> Result<Qty, MathError> {
    let levels = book.taking_levels(side);
    let touch = match book.touch(side) {
        Some(p) => p,
        None => return Err(MathError::EmptyBook),
    };
    if !touch.is_positive() {
        return Err(MathError::NonPositivePrice);
    }
    let b = budget.bps().to_rate();

    // Worst average price we will accept.
    let limit = match side {
        // touch * (1 + b), rounded DOWN so the budget is not accidentally widened.
        Side::Buy => touch.mul_rate(Rate::ONE.checked_add(b)?, RoundMode::REVENUE)?,
        // touch * (1 - b), rounded UP so the budget is not accidentally widened.
        Side::Sell => touch.mul_rate(Rate::ONE.checked_sub(b)?, RoundMode::COST)?,
    };

    // What the fill will really cost, on the same side of every remainder as
    // `executable_price`. HalfEven here — the mode this used to use — is the one
    // mode that can round a cost DOWN, and a cost rounded down is capacity
    // rounded up.
    let cost_mode = conservative_mode(side);
    // `L·Q` sits on the other side of the subtraction from `C`, so the mode that
    // shrinks the numerator is the opposite one.
    let limit_q_mode = match side {
        Side::Buy => RoundMode::REVENUE,
        Side::Sell => RoundMode::COST,
    };

    let mut q = Qty::ZERO;
    let mut c = Money::ZERO;

    for level in levels {
        let within = match side {
            Side::Buy => level.price <= limit,
            Side::Sell => level.price >= limit,
        };
        if within {
            // Taking this level whole cannot breach the average, because every
            // price taken so far is at least as good as `limit`. Ladders are
            // sorted worsening, so these levels are a prefix of the walk.
            c = c.checked_add(level.price.mul_qty(level.qty, cost_mode)?)?;
            q = q.checked_add(level.qty)?;
            continue;
        }

        // This level is worse than the limit. Solve for the largest size at it
        // that keeps the average on the right side of the limit.
        let limit_times_q = limit.mul_qty(q, limit_q_mode)?;
        let (numerator, denominator) = match side {
            Side::Buy => (
                limit_times_q.checked_sub(c)?,
                level.price.checked_sub(limit)?,
            ),
            Side::Sell => (
                c.checked_sub(limit_times_q)?,
                limit.checked_sub(level.price)?,
            ),
        };
        if !numerator.is_positive() || !denominator.is_positive() {
            // The subsidy is spent: the average is already on the limit. Every
            // level after this one is worse, so stop.
            break;
        }
        let x = numerator.div_price(denominator, RoundMode::REVENUE)?;
        if x < level.qty {
            // A genuine partial fill. The average lands on the limit inside this
            // level, so no deeper level can be reached.
            if x.is_positive() {
                q = q.checked_add(x)?;
            }
            break;
        }
        // The subsidy covers this entire level. Take it, charge it, and keep
        // walking.
        c = c.checked_add(level.price.mul_qty(level.qty, cost_mode)?)?;
        q = q.checked_add(level.qty)?;
    }

    Ok(q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{
        Asset, ContractSpec, DataQuality, InstrumentKey, Level, LocalTs, MarketKind, Venue,
        VenueKey,
    };

    fn asset(s: &str) -> Asset {
        Asset::new(s).expect("asset")
    }

    fn vkey() -> VenueKey {
        VenueKey::new(
            Venue::Binance,
            InstrumentKey::new(
                asset("BTC"),
                asset("USDT"),
                MarketKind::LinearPerp,
                asset("USDT"),
            ),
        )
    }

    fn price(s: &str) -> Price {
        Price::parse(s).expect("price")
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).expect("qty")
    }

    fn money(s: &str) -> Money {
        Money::parse(s).expect("money")
    }

    fn bps(s: &str) -> Bps {
        Bps::parse_bps(s).expect("bps")
    }

    fn level(p: &str, q: &str) -> Level {
        Level::new(price(p), qty(q)).expect("level")
    }

    /// Asks 100 x 2, 101 x 3, 102 x 5. Total ask depth 10.
    /// One token bid below, so the book is not crossed.
    fn buy_book() -> BookSnapshot {
        BookSnapshot::new(
            vkey(),
            vec![level("99.9", "1")],
            vec![level("100", "2"), level("101", "3"), level("102", "5")],
            Some(1),
            None,
            LocalTs::from_millis(1_000),
            DataQuality::SYNTHESIZED,
        )
        .expect("book")
    }

    /// Bids 100 x 2, 99 x 2, 98 x 6. Total bid depth 10.
    fn sell_book() -> BookSnapshot {
        BookSnapshot::new(
            vkey(),
            vec![level("100", "2"), level("99", "2"), level("98", "6")],
            vec![level("100.1", "1")],
            Some(1),
            None,
            LocalTs::from_millis(1_000),
            DataQuality::SYNTHESIZED,
        )
        .expect("book")
    }

    // ---- executable_price -------------------------------------------------

    #[test]
    fn buying_four_walks_two_levels() {
        // 2 @ 100 = 200; 2 @ 101 = 202; total 402 for 4 units.
        // VWAP = 402 / 4 = 100.5 exactly.
        let e = executable_price(&buy_book(), Side::Buy, qty("4")).expect("ok");
        assert_eq!(e.filled, qty("4"));
        assert_eq!(e.notional, money("402"));
        assert_eq!(e.vwap, price("100.5"));
        assert_eq!(e.touch, price("100"));
        assert_eq!(e.levels_consumed, 2);
        assert!(e.complete);
        assert!(e.flags.is_empty());
    }

    #[test]
    fn buying_exactly_the_touch_size_does_not_slip() {
        // 2 @ 100 = 200. VWAP == touch == 100.
        let e = executable_price(&buy_book(), Side::Buy, qty("2")).expect("ok");
        assert_eq!(e.vwap, price("100"));
        assert_eq!(e.notional, money("200"));
        assert_eq!(e.levels_consumed, 1);
        assert_eq!(slippage_bps(e.vwap, e.touch, Side::Buy).expect("ok"), Bps::ZERO);
    }

    #[test]
    fn buying_more_than_the_book_reports_the_shortfall_not_an_error() {
        // 2*100 + 3*101 + 5*102 = 200 + 303 + 510 = 1013 for 10 units.
        // VWAP = 1013 / 10 = 101.3 exactly. Requested 12, short by 2.
        let e = executable_price(&buy_book(), Side::Buy, qty("12")).expect("ok");
        assert_eq!(e.filled, qty("10"));
        assert_eq!(e.notional, money("1013"));
        assert_eq!(e.vwap, price("101.3"));
        assert!(!e.complete);
        assert_eq!(e.shortfall().expect("ok"), qty("2"));
        assert!(e.flags.contains(QualityFlags::SIZE_EXCEEDS_DEPTH));
    }

    #[test]
    fn selling_four_walks_two_levels() {
        // 2 @ 100 = 200; 2 @ 99 = 198; total 398 for 4 units.
        // VWAP = 398 / 4 = 99.5 exactly.
        let e = executable_price(&sell_book(), Side::Sell, qty("4")).expect("ok");
        assert_eq!(e.notional, money("398"));
        assert_eq!(e.vwap, price("99.5"));
        assert_eq!(e.touch, price("100"));
        assert!(e.complete);
    }

    #[test]
    fn buying_takes_asks_and_selling_takes_bids() {
        // The single most common bug in this domain: reading depth from the
        // wrong side. A buy against `buy_book` must see 100, not 99.9.
        let buy = executable_price(&buy_book(), Side::Buy, qty("1")).expect("ok");
        assert_eq!(buy.touch, price("100"));
        let sell = executable_price(&sell_book(), Side::Sell, qty("1")).expect("ok");
        assert_eq!(sell.touch, price("100"));
        // And the reverse direction against the same book reads the other side.
        let other = executable_price(&buy_book(), Side::Sell, qty("1")).expect("ok");
        assert_eq!(other.touch, price("99.9"));
    }

    #[test]
    fn zero_size_is_an_error_not_a_zero_vwap() {
        assert_eq!(
            executable_price(&buy_book(), Side::Buy, Qty::ZERO),
            Err(MathError::ZeroSize)
        );
    }

    #[test]
    fn empty_side_is_an_error() {
        let one_sided = BookSnapshot::new(
            vkey(),
            vec![level("100", "1")],
            vec![],
            None,
            None,
            LocalTs::from_millis(1),
            DataQuality::SYNTHESIZED,
        )
        .expect("book");
        assert_eq!(
            executable_price(&one_sided, Side::Buy, qty("1")),
            Err(MathError::EmptyBook)
        );
    }

    #[test]
    fn partial_level_consumption_is_exact() {
        // Buy 3: 2 @ 100 = 200, then 1 of the 3 available @ 101 = 101.
        // Total 301 for 3. VWAP = 301/3 = 100.333333333333 (truncating the
        // repeating third at the 12th place, rounded AGAINST us to
        // 100.333333333334 because a buy VWAP is a cost).
        let e = executable_price(&buy_book(), Side::Buy, qty("3")).expect("ok");
        assert_eq!(e.notional, money("301"));
        assert_eq!(e.vwap, price("100.333333333334"));
        assert_eq!(e.levels_consumed, 2);
    }

    #[test]
    fn sell_vwap_rounds_the_other_way() {
        // Sell 3: 2 @ 100 = 200, 1 @ 99 = 99. Total 299 for 3.
        // 299/3 = 99.6666... and a sell VWAP is proceeds, so it truncates
        // DOWN to 99.666666666666 rather than up.
        let e = executable_price(&sell_book(), Side::Sell, qty("3")).expect("ok");
        assert_eq!(e.notional, money("299"));
        assert_eq!(e.vwap, price("99.666666666666"));
    }

    // ---- slippage ---------------------------------------------------------

    #[test]
    fn buy_slippage_is_hand_checkable() {
        // (100.5 - 100) / 100 = 0.005 = 50 bps.
        assert_eq!(
            slippage_bps(price("100.5"), price("100"), Side::Buy).expect("ok"),
            bps("50")
        );
    }

    #[test]
    fn sell_slippage_is_hand_checkable_and_also_positive() {
        // (100 - 99.5) / 100 = 0.005 = 50 bps. Same sign as the buy case.
        assert_eq!(
            slippage_bps(price("99.5"), price("100"), Side::Sell).expect("ok"),
            bps("50")
        );
    }

    #[test]
    fn slippage_on_a_realistic_price_is_still_exact() {
        // 68123.45 touch, 68157.51 VWAP: a move of 34.06 on a BTC-sized price,
        // which is where a scanner working in f64 starts losing digits it needs.
        //
        // Derived as exact integer arithmetic, not as decimal long division,
        // because the answer lands one third of the way between two ulps and a
        // decimal estimate cannot say which side of the boundary it is on.
        //
        //   ratio_raw = 68157.51e12 * 1e12 / 68123.45e12
        //             = 6_815_751e12 / 6_812_345          (common 1e10 cancels)
        //   and 6_812_345 * 1e12 = 6_812_345e12, so the fractional part is
        //             (6_815_751 - 6_812_345)e12 / 6_812_345
        //             = 3_406_000_000_000_000 / 6_812_345
        //   6_812_345 * 499_974_678 = 3_405_999_997_799_910
        //   remainder                =         2_200_090   (non-zero)
        //
        // So the true ratio is 1 + 499_974_678.32287.../1e12, i.e. the slippage
        // is 4.99974678322... bps and it is NOT representable: Bps has eight
        // decimals, so it must land on 4.99974678 or 4.99974679.
        //
        // A buy's VWAP is a cost, so `slippage_bps` rounds the ratio away from
        // zero and reports 4.99974679 — one ulp above the truth, in our
        // disfavour. Reporting 4.99974678 would understate the cost, which is
        // the direction that turns a real 12 bp edge into a claimed one.
        let got = slippage_bps(price("68157.51"), price("68123.45"), Side::Buy).expect("ok");
        assert_eq!(got, bps("4.99974679"));
        // And the truth it was rounded up from, to record the direction:
        assert!(got > bps("4.99974678"));
    }

    #[test]
    fn a_sell_understates_nothing_either() {
        // The mirror of the case above. A sell's VWAP is proceeds, so the ratio
        // 1 - vwap/touch is built from a quotient rounded DOWN, which makes the
        // slippage larger. Same prices, sides swapped:
        //   68089.39 / 68123.45: fractional part
        //     = -(68123.45 - 68089.39)e12 / 68123.45e12
        //     = -3_406_000_000_000_000 / 6_812_345 below 1
        //   so vwap/touch = 1 - 499_974_678.32287.../1e12, and truncating that
        //   quotient toward zero gives 1 - 499_974_679/1e12 ... i.e. the
        //   subtraction from ONE yields 499_974_679.
        let got = slippage_bps(price("68089.39"), price("68123.45"), Side::Sell).expect("ok");
        assert_eq!(got, bps("4.99974679"));
    }

    #[test]
    fn zero_touch_is_refused() {
        assert_eq!(
            slippage_bps(price("100"), Price::ZERO, Side::Buy),
            Err(MathError::NonPositivePrice)
        );
    }

    // ---- capacity ---------------------------------------------------------

    #[test]
    fn capacity_at_fifty_bps_is_four() {
        // Limit = 100 * 1.005 = 100.5.
        // Take all 2 @ 100 -> Q=2, C=200.
        // 101 > 100.5, so solve: x = (100.5*2 - 200) / (101 - 100.5)
        //                          = (201 - 200) / 0.5 = 1 / 0.5 = 2.
        // Total 4, and its VWAP is 402/4 = 100.5 = exactly the limit.
        let cap = max_size_within_slippage(
            &buy_book(),
            Side::Buy,
            SlippageBudget::new(bps("50")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("4"));
    }

    #[test]
    fn capacity_at_one_hundred_bps_is_seven() {
        // Limit = 100 * 1.01 = 101.
        // 100 <= 101: take 2   -> Q=2, C=200.
        // 101 <= 101: take 3   -> Q=5, C=503.
        // 102 > 101: x = (101*5 - 503) / (102 - 101) = (505 - 503)/1 = 2.
        // Total 7, VWAP = 707/7 = 101 = the limit.
        let cap = max_size_within_slippage(
            &buy_book(),
            Side::Buy,
            SlippageBudget::new(bps("100")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("7"));
    }

    #[test]
    fn capacity_is_the_vwap_constraint_not_the_deepest_level() {
        // At 100 bps the deepest level within the limit is 101, whose cumulative
        // size is 5. The VWAP constraint allows 7, because the cheap fills at
        // 100 subsidise a partial fill at 102. Getting this backwards in either
        // direction is a real error, so pin both numbers.
        let cap = max_size_within_slippage(
            &buy_book(),
            Side::Buy,
            SlippageBudget::new(bps("100")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("7"));
        let e = executable_price(&buy_book(), Side::Buy, cap).expect("ok");
        assert_eq!(e.vwap, price("101"));
        assert!(slippage_bps(e.vwap, e.touch, Side::Buy).expect("ok") <= bps("100"));
    }

    #[test]
    fn capacity_respects_the_budget_it_was_given() {
        // The invariant that matters: executing the reported capacity must not
        // exceed the budget. Check it across a sweep of budgets.
        for b in ["0", "1", "10", "25", "50", "75", "100", "150", "200"] {
            let budget = SlippageBudget::new(bps(b)).expect("ok");
            let cap = max_size_within_slippage(&buy_book(), Side::Buy, budget).expect("ok");
            if !cap.is_positive() {
                continue;
            }
            let e = executable_price(&buy_book(), Side::Buy, cap).expect("ok");
            let slip = slippage_bps(e.vwap, e.touch, Side::Buy).expect("ok");
            assert!(slip <= budget.bps(), "budget {b}: cap {cap} slipped {slip}");
        }
    }

    /// A one-unit-per-tick ladder: asks at `first`, `first+1`, ... `n` levels.
    fn ramp(first: i64, n: i64) -> BookSnapshot {
        let mut asks = Vec::new();
        for i in 0..n {
            asks.push(level(&format!("{}", first + i), "1"));
        }
        BookSnapshot::new(
            vkey(),
            vec![level("1", "1")],
            asks,
            Some(1),
            None,
            LocalTs::from_millis(1_000),
            DataQuality::SYNTHESIZED,
        )
        .expect("book")
    }

    #[test]
    fn capacity_keeps_spending_the_subsidy_across_several_dear_levels() {
        // Ramp 100..109, one unit each. Budget 200 bps -> limit = 102.
        // Three levels are at or under the limit, so the prefix is Q=3, C=303.
        // The cheap fills are then a subsidy that buys whole dear levels:
        //   103: L·Q = 306, num = 3, den = 1, x = 3 >= 1 -> whole. Q=4, C=406.
        //   104: L·Q = 408, num = 2, den = 2, x = 1 >= 1 -> whole. Q=5, C=510.
        //   105: L·Q = 510, num = 0                     -> subsidy spent, stop.
        // Capacity 5. Confirmed independently of the walk: the average of a
        // one-per-tick ramp is 100 + (n-1)/2, and 100 + (n-1)/2 <= 102 <=> n <= 5.
        //
        // A walk that stops at the first level worse than the limit reports 4.
        let b = ramp(100, 10);
        let cap = max_size_within_slippage(
            &b,
            Side::Buy,
            SlippageBudget::new(bps("200")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("5"));
        let e = executable_price(&b, Side::Buy, cap).expect("ok");
        assert_eq!(e.notional, money("510"));
        assert_eq!(e.vwap, price("102"));
        assert_eq!(
            slippage_bps(e.vwap, e.touch, Side::Buy).expect("ok"),
            bps("200")
        );
        // One unit more breaches it: 615/6 = 102.5, i.e. 250 bps.
        let over = executable_price(&b, Side::Buy, qty("6")).expect("ok");
        assert_eq!(over.vwap, price("102.5"));
        assert!(slippage_bps(over.vwap, over.touch, Side::Buy).expect("ok") > bps("200"));
    }

    #[test]
    fn capacity_on_a_btc_scale_ladder_reaches_three_hundred_and_forty_one() {
        // The same defect at a size that matters. One BTC per dollar tick from
        // 68_000, budget 25 bps -> limit = 68_170. Levels at or under the limit:
        // 171. Capacity: 68_000 + (n-1)/2 <= 68_170 <=> n <= 341, and n = 341
        // averages 68_170 exactly.
        //
        // 171 against 341 is not a rounding disagreement. It is 169 BTC of size
        // the book will absorb inside the budget and a stop-at-the-limit walk
        // cannot see — roughly 11.5 million dollars of capacity per venue leg.
        let b = ramp(68_000, 400);
        let cap = max_size_within_slippage(
            &b,
            Side::Buy,
            SlippageBudget::new(bps("25")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("341"));
        let e = executable_price(&b, Side::Buy, cap).expect("ok");
        assert_eq!(e.vwap, price("68170"));
        assert_eq!(slippage_bps(e.vwap, e.touch, Side::Buy).expect("ok"), bps("25"));
        let over = executable_price(&b, Side::Buy, qty("342")).expect("ok");
        assert!(slippage_bps(over.vwap, over.touch, Side::Buy).expect("ok") > bps("25"));
    }

    #[test]
    fn a_level_taken_whole_past_the_limit_is_charged_before_the_next_solve() {
        // Asks 100 x 2, 101 x 1, 200 x 100. Budget 50 bps -> limit 100.5.
        //   100: within.                                     Q=2,  C=200.
        //   101: L·Q = 201, num = 1, den = 0.5, x = 2 >= 1 -> whole. Q=3, C=301.
        //   200: L·Q = 301.5, num = 0.5, den = 99.5,
        //        x = 0.5/99.5 = 0.00502512562814...  -> truncated toward zero to
        //        0.005025125628, a genuine partial, so the walk ends.
        // Capacity = 3.005025125628.
        //
        // The charge on the 101 level is the only thing bounding this. Omit it
        // and the third solve sees num = 301.5 - 200 = 101.5 against the same
        // 99.5, so x = 1.020100502512 and the reported capacity is
        // 4.020100502512 — a size whose real average is 125.62, i.e. 2562 bps
        // against a 50 bps budget. Taking a level without paying for it is not a
        // small error; it is unbounded in the size of the level.
        let b = BookSnapshot::new(
            vkey(),
            vec![level("1", "1")],
            vec![level("100", "2"), level("101", "1"), level("200", "100")],
            Some(1),
            None,
            LocalTs::from_millis(1_000),
            DataQuality::SYNTHESIZED,
        )
        .expect("book");
        let budget = SlippageBudget::new(bps("50")).expect("ok");
        let cap = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");
        assert_eq!(cap, qty("3.005025125628"));
        let e = executable_price(&b, Side::Buy, cap).expect("ok");
        assert!(slippage_bps(e.vwap, e.touch, Side::Buy).expect("ok") <= budget.bps());
    }

    #[test]
    fn the_solve_charges_what_execution_will_charge_down_to_the_last_ulp() {
        // Asks 100.000000000001 x 0.1, then 101 x 10. Budget 50 bps.
        //   limit = trunc(100.000000000001 * 1.005) = 100.500000000001
        //   level 1 is within. Its true cost is 10.0000000000001, which twelve
        //   places cannot hold, so COST gives 10.000000000001 — the same figure
        //   `executable_price` will charge. Q = 0.1, C = 10.000000000001.
        //   level 2: L·Q = trunc(100.500000000001 * 0.1) = 10.05
        //            num = 0.049999999999, den = 0.499999999999
        //            x   = trunc(num/den)  = 0.099999999998
        //   capacity = 0.199999999998, whose real VWAP is 100.5 exactly, i.e.
        //   exactly 50 bps: on the budget, not through it.
        //
        // Accumulating C with `HalfEven` — the mode this used to use, and the one
        // mode that can round a cost DOWN — gives C = 10.000000000000, then
        // x = 0.1 and a capacity of 0.2 whose real VWAP is 100.500000000005:
        // 50.00000001 bps against a 50 bps budget. One ulp of cost in the solver
        // is enough to make the function return a size that violates the property
        // it is named after.
        let b = BookSnapshot::new(
            vkey(),
            vec![level("1", "1")],
            vec![level("100.000000000001", "0.1"), level("101", "10")],
            Some(1),
            None,
            LocalTs::from_millis(1_000),
            DataQuality::SYNTHESIZED,
        )
        .expect("book");
        let budget = SlippageBudget::new(bps("50")).expect("ok");
        let cap = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");
        assert_eq!(cap, qty("0.199999999998"));
        let e = executable_price(&b, Side::Buy, cap).expect("ok");
        assert_eq!(e.vwap, price("100.5"));
        assert_eq!(
            slippage_bps(e.vwap, e.touch, Side::Buy).expect("ok"),
            bps("50")
        );
    }

    #[test]
    fn capacity_across_a_budget_sweep_matches_the_closed_form() {
        // On a one-per-tick ramp the average of `n` units is `first + (n-1)/2`, so
        // the largest `n` inside a budget `b` satisfies `(n-1)/2 <= first*b`, i.e.
        // `n = 2*first*b + 1` whenever `2*first*b` is a whole number. Hand-computed
        // for a 100-base ramp, independently of the walk:
        //     0 bps -> 2*100*0     = 0 -> n = 1
        //    50 bps -> 2*100*0.005 = 1 -> n = 2
        //   100 bps -> 2               -> n = 3
        //   150 bps -> 3               -> n = 4
        //   200 bps -> 4               -> n = 5
        //   250 bps -> 5               -> n = 6
        //   400 bps -> 8               -> n = 9
        // Only budgets whose closed form lands on a whole number are listed, so
        // each expected capacity is exact and one *whole unit* more must breach —
        // which is what makes the second half of the check meaningful. Budgets
        // with a fractional answer are covered by the sweep below.
        let b = ramp(100, 12);
        for (s, n) in [
            ("0", "1"),
            ("50", "2"),
            ("100", "3"),
            ("150", "4"),
            ("200", "5"),
            ("250", "6"),
            ("400", "9"),
        ] {
            let budget = SlippageBudget::new(bps(s)).expect("ok");
            let cap = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");
            assert_eq!(cap, qty(n), "budget {s} bps");
            let fits = executable_price(&b, Side::Buy, cap).expect("ok");
            assert!(
                slippage_bps(fits.vwap, fits.touch, Side::Buy).expect("ok") <= budget.bps(),
                "budget {s}: reported capacity {cap} does not fit"
            );
            let more = cap.checked_add(qty("1")).expect("ok");
            let over = executable_price(&b, Side::Buy, more).expect("ok");
            assert!(over.complete, "the probe must be a size the ladder can fill");
            assert!(
                slippage_bps(over.vwap, over.touch, Side::Buy).expect("ok") > budget.bps(),
                "budget {s}: {more} also fits, so {cap} was not the largest"
            );
        }
    }

    #[test]
    fn a_fractional_capacity_still_fits_the_budget_it_names() {
        // The budgets left out of the closed-form table, because their answer is
        // not a whole number of units. 25 bps on the same ramp:
        //   limit = 100.25, level 100 taken whole -> Q=1, C=100
        //   101: num = 0.25, den = 0.75, x = 1/3 = 0.333333333333... truncated
        //   capacity = 1.333333333333
        // Truncation is why "one ulp more must breach" is NOT asserted here: the
        // real boundary sits inside an ulp, and `limit` is itself rounded tighter
        // than the budget, so a size one ulp past the reported capacity may still
        // be inside the true budget. Understating by less than one ulp of size is
        // the acceptable end of this trade-off; understating by 169 BTC is not.
        let b = ramp(100, 12);
        assert_eq!(
            max_size_within_slippage(&b, Side::Buy, SlippageBudget::new(bps("25")).expect("ok"))
                .expect("ok"),
            qty("1.333333333333")
        );
        for s in ["1", "10", "25", "33", "77", "123"] {
            let budget = SlippageBudget::new(bps(s)).expect("ok");
            let cap = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");
            let fits = executable_price(&b, Side::Buy, cap).expect("ok");
            assert!(
                slippage_bps(fits.vwap, fits.touch, Side::Buy).expect("ok") <= budget.bps(),
                "budget {s}: reported capacity {cap} does not fit"
            );
        }
    }

    #[test]
    fn sell_capacity_at_fifty_bps_is_four() {
        // Limit = 100 * (1 - 0.005) = 99.5.
        // 100 >= 99.5: take 2 -> Q=2, C=200.
        // 99 < 99.5: x = (200 - 99.5*2) / (99.5 - 99) = (200-199)/0.5 = 2.
        // Total 4, VWAP 398/4 = 99.5 = the limit.
        let cap = max_size_within_slippage(
            &sell_book(),
            Side::Sell,
            SlippageBudget::new(bps("50")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("4"));
    }

    #[test]
    fn sell_capacity_at_one_hundred_bps_is_six() {
        // Limit = 99.
        // 100 >= 99: take 2 -> Q=2, C=200.
        //  99 >= 99: take 2 -> Q=4, C=398.
        //  98 <  99: x = (398 - 99*4)/(99 - 98) = (398-396)/1 = 2.
        // Total 6, VWAP 594/6 = 99 = the limit.
        let cap = max_size_within_slippage(
            &sell_book(),
            Side::Sell,
            SlippageBudget::new(bps("100")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("6"));
    }

    #[test]
    fn zero_budget_gives_only_the_touch_level() {
        // With no tolerance at all, only fills at the touch price qualify.
        let cap = max_size_within_slippage(
            &buy_book(),
            Side::Buy,
            SlippageBudget::new(Bps::ZERO).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("2"));
    }

    #[test]
    fn a_budget_wider_than_the_book_returns_the_whole_book() {
        let cap = max_size_within_slippage(
            &buy_book(),
            Side::Buy,
            SlippageBudget::new(bps("10000")).expect("ok"),
        )
        .expect("ok");
        assert_eq!(cap, qty("10"));
    }

    #[test]
    fn negative_budget_is_refused() {
        let neg = bps("1").checked_neg().expect("ok");
        assert_eq!(SlippageBudget::new(neg), Err(MathError::NegativeFeeRate));
    }

    #[test]
    fn capacity_on_an_empty_side_is_an_error() {
        let one_sided = BookSnapshot::new(
            vkey(),
            vec![],
            vec![level("100", "1")],
            None,
            None,
            LocalTs::from_millis(1),
            DataQuality::SYNTHESIZED,
        )
        .expect("book");
        assert_eq!(
            max_size_within_slippage(
                &one_sided,
                Side::Sell,
                SlippageBudget::new(bps("50")).expect("ok")
            ),
            Err(MathError::EmptyBook)
        );
    }

    #[test]
    fn contract_spec_is_not_needed_here_but_the_types_line_up() {
        // A guard against drift: `Execution::notional` is a `Money` and so is
        // `ContractSpec::notional`, so a caller can compare a fill against a
        // contract-count notional without a reinterpret. If that stops being
        // true this test stops compiling, which is the point.
        let spec = ContractSpec::spot(asset("USDT"), price("0.01"), qty("0.0001"));
        let by_contracts = spec
            .notional(qty("2"), price("100"), RoundMode::COST)
            .expect("ok");
        let e = executable_price(&buy_book(), Side::Buy, qty("2")).expect("ok");
        assert_eq!(by_contracts, e.notional);
    }
}
