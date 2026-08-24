//! Depth. A buy lifts **asks**, a sell hits **bids**, and for any size that
//! matters the average is worse than the touch.

use arb_math::{executable_price, max_size_within_slippage, slippage_bps, SlippageBudget};
use arb_num::{Qty, RoundMode};
use arb_types::{QualityFlags, Side};

use crate::support::{book, bps, level, money, price, qty};

/// Asks are dear above the touch, bids are cheap below it, and the two ladders
/// are deliberately NOT mirror images: a function that walked the wrong side
/// would return 95 where 105 is correct, i.e. report a gain where there is a
/// cost.
fn asymmetric() -> arb_types::BookSnapshot {
    book(
        vec![level("100", "1"), level("90", "1")],
        vec![level("100", "1"), level("110", "1")],
    )
}

#[test]
fn taking_levels_maps_buy_to_asks() {
    let b = asymmetric();
    assert_eq!(b.taking_levels(Side::Buy), b.asks());
    assert_eq!(b.taking_levels(Side::Sell), b.bids());
    assert_eq!(b.touch(Side::Buy), Some(price("100")));
    assert_eq!(b.touch(Side::Sell), Some(price("100")));
    // Ladders are sorted the way the digest promises.
    assert_eq!(b.asks().first().map(|l| l.price), Some(price("100")));
    assert_eq!(b.bids().first().map(|l| l.price), Some(price("100")));
}

#[test]
fn a_buy_walks_asks_upward_and_a_sell_walks_bids_downward() {
    let b = asymmetric();
    // Buy 2: 1 @ 100 + 1 @ 110 = 210. VWAP = 105. Worse than the touch by 5.
    let buy = executable_price(&b, Side::Buy, qty("2")).expect("ok");
    assert_eq!(buy.notional, money("210"));
    assert_eq!(buy.vwap, price("105"));
    assert_eq!(buy.touch, price("100"));
    assert_eq!(buy.levels_consumed, 2);
    assert!(buy.complete);

    // Sell 2: 1 @ 100 + 1 @ 90 = 190. VWAP = 95. Also worse than the touch.
    let sell = executable_price(&b, Side::Sell, qty("2")).expect("ok");
    assert_eq!(sell.notional, money("190"));
    assert_eq!(sell.vwap, price("95"));
    assert_eq!(sell.touch, price("100"));

    // Both walks cost 500 bps, and BOTH are reported as a positive cost. A
    // signed figure whose meaning flipped with the side guarantees a sign error
    // in a caller summing two legs.
    assert_eq!(slippage_bps(buy.vwap, buy.touch, Side::Buy).expect("ok"), bps("500"));
    assert_eq!(
        slippage_bps(sell.vwap, sell.touch, Side::Sell).expect("ok"),
        bps("500")
    );
}

#[test]
fn the_touch_price_is_not_the_executable_price() {
    // The whole thesis in one assertion. Top of book says 100; two units cost
    // 105 each. A scanner that reports 100 overstates the edge by 500 bps,
    // which is forty times the twelve basis points we claim to trade.
    let b = asymmetric();
    let buy = executable_price(&b, Side::Buy, qty("2")).expect("ok");
    assert!(buy.vwap > buy.touch);
    let touch_notional = buy.touch.mul_qty(qty("2"), RoundMode::COST).expect("ok");
    assert_eq!(touch_notional, money("200"));
    assert_eq!(buy.notional, money("210"));
}

#[test]
fn a_buys_ulp_rounds_up_and_a_sells_ulp_rounds_down() {
    // Buy 3 against 1 @ 100 and 2 @ 101: notional 302, VWAP = 302/3
    // = 100.666666666666_666..., which must round UP to ...667: we never
    // under-report what we pay.
    let asks = book(vec![level("1", "1")], vec![level("100", "1"), level("101", "2")]);
    let buy = executable_price(&asks, Side::Buy, qty("3")).expect("ok");
    assert_eq!(buy.notional, money("302"));
    assert_eq!(buy.vwap.raw(), 100_666_666_666_667);

    // Sell 3 against 1 @ 100 and 2 @ 99: notional 298, VWAP = 298/3
    // = 99.333333333333_333..., which must round DOWN to ...333: we never
    // over-report what we receive.
    let bids = book(vec![level("100", "1"), level("99", "2")], vec![level("1000", "1")]);
    let sell = executable_price(&bids, Side::Sell, qty("3")).expect("ok");
    assert_eq!(sell.notional, money("298"));
    assert_eq!(sell.vwap.raw(), 99_333_333_333_333);
}

#[test]
fn a_partial_fill_is_flagged_and_never_priced_as_if_complete() {
    // Ask for 5 against 2 units of depth. The book cannot lie about this:
    // `filled` is 2, `complete` is false, SIZE_EXCEEDS_DEPTH is raised, and the
    // VWAP describes the 2 that filled, not the 5 that were asked for.
    let b = asymmetric();
    let e = executable_price(&b, Side::Buy, qty("5")).expect("ok");
    assert_eq!(e.requested, qty("5"));
    assert_eq!(e.filled, qty("2"));
    assert!(!e.complete);
    assert!(e.flags.contains(QualityFlags::SIZE_EXCEEDS_DEPTH));
    assert_eq!(e.shortfall().expect("ok"), qty("3"));
    assert_eq!(e.notional, money("210"));
    assert_eq!(e.vwap, price("105"));
}

#[test]
fn an_empty_side_is_an_error_not_a_touch_price() {
    // A one-sided book must not yield a buy price. Falling back to the other
    // side is exactly the failure `taking_levels` exists to prevent.
    let bids_only = book(vec![level("100", "1")], vec![]);
    assert!(executable_price(&bids_only, Side::Buy, qty("1")).is_err());
    assert!(executable_price(&bids_only, Side::Sell, qty("1")).is_ok());
    assert!(max_size_within_slippage(
        &bids_only,
        Side::Buy,
        SlippageBudget::new(bps("25")).expect("ok")
    )
    .is_err());
}

#[test]
fn a_zero_or_negative_size_is_refused() {
    let b = asymmetric();
    assert!(executable_price(&b, Side::Buy, Qty::ZERO).is_err());
    assert!(executable_price(&b, Side::Buy, qty("-1")).is_err());
}

#[test]
fn total_depth_uses_the_same_side_convention_as_taking_levels() {
    let b = book(
        vec![level("100", "1"), level("99", "2")],
        vec![level("101", "4"), level("102", "8")],
    );
    // `total_depth(side)` is indexed by the side of the ORDER, like
    // `taking_levels`: a buyer's depth is the ASK depth, 4 + 8 = 12. If these
    // two methods on one type ever disagree, every caller that uses both reads
    // one of them backwards and no type catches it.
    assert_eq!(b.total_depth(Side::Buy).expect("ok"), qty("12"));
    assert_eq!(b.total_depth(Side::Sell).expect("ok"), qty("3"));
    let taken: Qty = b
        .taking_levels(Side::Buy)
        .iter()
        .fold(Qty::ZERO, |a, l| a.checked_add(l.qty).expect("ok"));
    assert_eq!(taken, b.total_depth(Side::Buy).expect("ok"));
}

// ---------------------------------------------------------------------------
// Capacity: "the largest size whose AVERAGE fill price stays inside budget".
// ---------------------------------------------------------------------------

/// A contiguous one-unit-per-tick ladder, the ordinary shape of a real book.
/// Asks at `first`, `first+step`, ... for `n` levels, one unit each.
fn ramp(first: i64, step: i64, n: i64) -> arb_types::BookSnapshot {
    let mut asks = Vec::new();
    for i in 0..n {
        let p = format!("{}", first + step * i);
        asks.push(level(&p, "1"));
    }
    book(vec![level("1", "1")], asks)
}

#[test]
fn capacity_solves_for_the_average_not_for_the_deepest_qualifying_level() {
    // Ladder 100..104, one unit each. Budget 200 bps -> limit = 102.
    // Levels at or under 102 are 100, 101, 102: three units. The DEEPEST
    // QUALIFYING LEVEL answer is 3. The AVERAGE answer is larger, because the
    // cheap units subsidise the dear ones:
    //   avg(100..100+n-1) = 100 + (n-1)/2 <= 102  <=>  n <= 5.
    // n = 5: (100+101+102+103+104)/5 = 510/5 = 102 exactly, on the limit.
    // n = 6: 615/6 = 102.5, over it.
    // So the correct capacity is 5.0 units.
    let b = ramp(100, 1, 10);
    let budget = SlippageBudget::new(bps("200")).expect("ok");
    let got = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");

    // Independent confirmation that 5 fits and 6 does not, computed by the
    // crate's own executor rather than by me.
    let five = executable_price(&b, Side::Buy, qty("5")).expect("ok");
    assert_eq!(five.notional, money("510"));
    assert_eq!(five.vwap, price("102"));
    assert_eq!(
        slippage_bps(five.vwap, five.touch, Side::Buy).expect("ok"),
        bps("200")
    );
    let six = executable_price(&b, Side::Buy, qty("6")).expect("ok");
    assert_eq!(six.vwap, price("102.5"));
    assert_eq!(
        slippage_bps(six.vwap, six.touch, Side::Buy).expect("ok"),
        bps("250")
    );

    assert_eq!(
        got,
        qty("5"),
        "capacity must keep walking while the AVERAGE is inside the budget"
    );
}

#[test]
fn capacity_on_a_btc_scale_ladder_is_understated_by_one_hundred_and_sixty_nine_coins() {
    // Realistic shape: 1 BTC per dollar tick from 68_000 up. Budget 25 bps,
    // which is the crate's own `far_bps`, i.e. twice our stated edge.
    // limit = 68_000 * 1.0025 = 68_170.
    // avg(68_000..68_000+n-1) = 68_000 + (n-1)/2 <= 68_170 <=> n <= 341.
    // n = 341: avg = 68_170 exactly. n = 342: avg = 68_170.5, over.
    // Correct capacity: 341 BTC. Levels at or under the limit: 171.
    let b = ramp(68_000, 1, 400);
    let budget = SlippageBudget::new(bps("25")).expect("ok");
    let got = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");

    let at_limit = executable_price(&b, Side::Buy, qty("341")).expect("ok");
    assert_eq!(at_limit.vwap, price("68170"));
    assert_eq!(
        slippage_bps(at_limit.vwap, at_limit.touch, Side::Buy).expect("ok"),
        bps("25")
    );
    let over = executable_price(&b, Side::Buy, qty("342")).expect("ok");
    assert!(slippage_bps(over.vwap, over.touch, Side::Buy).expect("ok") > bps("25"));

    assert_eq!(got, qty("341"), "341 BTC fit inside 25 bps; anything less is capacity we own and cannot see");
}

#[test]
fn capacity_mirrors_on_the_sell_side() {
    // Bids 100, 99, ... one unit each. Budget 200 bps -> limit = 98.
    // avg(100 down to 100-(n-1)) = 100 - (n-1)/2 >= 98 <=> n <= 5.
    // n = 5: (100+99+98+97+96)/5 = 490/5 = 98 exactly.
    let mut bids = Vec::new();
    for i in 0..10 {
        bids.push(level(&format!("{}", 100 - i), "1"));
    }
    let b = book(bids, vec![level("1000", "1")]);
    let budget = SlippageBudget::new(bps("200")).expect("ok");
    let five = executable_price(&b, Side::Sell, qty("5")).expect("ok");
    assert_eq!(five.notional, money("490"));
    assert_eq!(five.vwap, price("98"));
    assert_eq!(
        slippage_bps(five.vwap, five.touch, Side::Sell).expect("ok"),
        bps("200")
    );
    assert_eq!(
        max_size_within_slippage(&b, Side::Sell, budget).expect("ok"),
        qty("5")
    );
}

#[test]
fn any_size_larger_than_the_reported_capacity_must_breach_the_budget() {
    // The same defect as the two tests above, stated without naming the right
    // answer. "Largest size within budget" has one strictly implied
    // consequence: for any s > cap, walking s breaches the budget. Otherwise
    // `cap` was not the largest.
    //
    // No hand-computed capacity appears here, so this test survives whatever
    // value a fix settles on. It only fails while a size strictly greater than
    // the reported capacity still fits — which is the definition of capacity
    // being understated.
    let b = ramp(100, 1, 10);
    let budget = SlippageBudget::new(bps("200")).expect("ok");
    let cap = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");
    let depth = b.total_depth(Side::Buy).expect("ok");
    assert!(cap < depth, "this ladder is deeper than any 200 bps budget can reach");

    let bigger = cap.checked_add(qty("1")).expect("ok");
    assert!(bigger <= depth, "the probe must be a size the book can actually fill");
    let walk = executable_price(&b, Side::Buy, bigger).expect("ok");
    assert!(walk.complete);
    let cost = slippage_bps(walk.vwap, walk.touch, Side::Buy).expect("ok");
    assert!(
        cost > budget.bps(),
        "{bigger} still walks at {cost} bps inside a {} bps budget, so the reported \
         capacity of {cap} was not the largest size that fits",
        budget.bps()
    );
}

#[test]
fn a_zero_budget_admits_exactly_the_touch_level() {
    // With no slippage allowed the only acceptable average is the touch itself,
    // so capacity is the size resting AT the touch and not one ulp more.
    let b = book(
        vec![level("1", "1")],
        vec![level("100", "2.5"), level("100.01", "50")],
    );
    let got = max_size_within_slippage(
        &b,
        Side::Buy,
        SlippageBudget::new(arb_num::Bps::ZERO).expect("ok"),
    )
    .expect("ok");
    assert_eq!(got, qty("2.5"));
}

#[test]
fn capacity_never_exceeds_what_the_book_holds() {
    // Budget so wide that every level qualifies: the answer is total depth, not
    // an extrapolation past the end of the ladder.
    let b = ramp(100, 1, 5);
    let got = max_size_within_slippage(
        &b,
        Side::Buy,
        SlippageBudget::new(bps("10000")).expect("ok"),
    )
    .expect("ok");
    assert_eq!(got, qty("5"));
    assert_eq!(got, b.total_depth(Side::Buy).expect("ok"));
}

#[test]
fn a_negative_slippage_budget_is_refused() {
    // A negative tolerance would compute a limit BETTER than the touch and
    // return zero capacity, which reads as "no depth" rather than "bad input".
    assert!(SlippageBudget::new(bps("-1")).is_err());
}
