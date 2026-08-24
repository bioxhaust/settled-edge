//! Rounding. Every lossy operation must round AGAINST us. `RoundMode::COST`
//! rounds magnitude UP, `RoundMode::REVENUE` rounds magnitude toward zero, and
//! the honest choice is whichever makes the opportunity look worse.

use arb_math::{
    basis_bps, basis_ratio, convergence_capture, executable_price, fee_on_notional,
    funding_payment, margin_required, max_size_within_slippage, net_apr_over_horizon,
    slippage_bps, CarryLeg, FeeRates, FeeRole, FeeTable, RoundTrip, SlippageBudget, VolumeTier,
};
use arb_num::{mul_div, Bps, Money, Rate, RoundMode};
use arb_types::{DataQuality, FundingInterval, Millis, PositionSide, Side};

use crate::support::{book, bps, level, linear_vkey, money, price, qty, rate};

#[test]
fn cost_rounds_magnitude_up_and_revenue_rounds_it_toward_zero() {
    // 7/2 = 3.5 and -7/2 = -3.5. Pinned for both signs, because "round down" is
    // ambiguous between Floor and Trunc exactly where a receipt meets a cost.
    assert_eq!(mul_div(7, 1, 2, RoundMode::COST).expect("ok"), 4);
    assert_eq!(mul_div(-7, 1, 2, RoundMode::COST).expect("ok"), -4);
    assert_eq!(mul_div(7, 1, 2, RoundMode::REVENUE).expect("ok"), 3);
    assert_eq!(mul_div(-7, 1, 2, RoundMode::REVENUE).expect("ok"), -3);
    // And REVENUE is Trunc, NOT Floor: the two disagree by a whole unit on a
    // negative, which is the difference between a cost and a rebate.
    assert_eq!(mul_div(-7, 1, 2, RoundMode::Floor).expect("ok"), -4);
    assert_eq!(mul_div(-7, 1, 2, RoundMode::Trunc).expect("ok"), -3);
    assert_eq!(mul_div(-7, 1, 2, RoundMode::Ceil).expect("ok"), -3);
    // HalfEven is neither: it can round a cost DOWN. 5/2 -> 2, not 3.
    assert_eq!(mul_div(5, 1, 2, RoundMode::HalfEven).expect("ok"), 2);
}

#[test]
fn a_fee_on_a_non_zero_notional_is_never_reported_as_nothing() {
    // 1e-12 of notional at 1 bp is 1e-16, far below the 1e-12 scale. Rounded
    // against us that is one ulp, not zero: the venue keeps the fraction.
    assert_eq!(fee_on_notional(Money::EPSILON, bps("1")).expect("ok"), Money::EPSILON);
    // 100_000 at 5.5 bps = 55 exactly, no rounding to argue about.
    assert_eq!(fee_on_notional(money("100000"), bps("5.5")).expect("ok"), money("55"));
    // A negative rate is refused: a rebate treated as revenue would invert every
    // conservative decision in the crate.
    assert!(fee_on_notional(money("100000"), bps("-1")).is_err());
}

#[test]
fn an_identically_sized_receipt_and_cost_are_asymmetric_in_our_disfavour() {
    // notional 1e-12 at rate 0.5 -> exactly half an ulp of money.
    // Short (receipt): magnitude toward zero -> 0. We claim nothing.
    // Long  (cost):    magnitude up         -> 1 ulp. We owe a full unit.
    // The asymmetry must run this way. If the two were symmetric the engine
    // would be claiming half an ulp per settlement it does not receive.
    let n = Money::EPSILON;
    let receipt = funding_payment(n, rate("0.5"), PositionSide::Short).expect("ok");
    let cost = funding_payment(n, rate("0.5"), PositionSide::Long).expect("ok");
    assert_eq!(receipt, Money::ZERO);
    assert_eq!(cost.raw(), -1);
    assert!(cost.raw().abs() >= receipt.raw().abs());
}

#[test]
fn basis_truncates_toward_zero_for_a_premium_and_for_a_discount() {
    // mark 30001, index 30000: 1/30000 = 0.0000333333... -> 33_333_333 raw,
    // truncated, so the premium reads SMALLER than it is.
    assert_eq!(
        basis_ratio(price("30001"), price("30000"), RoundMode::REVENUE)
            .expect("ok")
            .raw(),
        33_333_333
    );
    // mark 29999, index 30000: -1/30000 -> -33_333_333, i.e. nearer zero than
    // the truth. Both directions shrink the claimed gap.
    assert_eq!(
        basis_ratio(price("29999"), price("30000"), RoundMode::REVENUE)
            .expect("ok")
            .raw(),
        -33_333_333
    );
    assert_eq!(basis_bps(price("30001"), price("30000")).expect("ok").raw(), 33_333_333);
    assert_eq!(basis_bps(price("29999"), price("30000")).expect("ok").raw(), -33_333_333);
    // A broken feed is an error, not a basis of infinity.
    assert!(basis_ratio(price("30001"), price("0"), RoundMode::REVENUE).is_err());
    assert!(basis_ratio(price("30001"), price("-1"), RoundMode::REVENUE).is_err());
}

#[test]
fn convergence_capture_shrinks_a_gain_and_grows_a_loss() {
    // entry 0.002, exit 0.001, notional 1e-12: moved = +0.001, product
    // = 1e-15 -> a gain of a thousandth of an ulp, reported as zero.
    let gain = convergence_capture(rate("0.002"), rate("0.001"), Money::EPSILON).expect("ok");
    assert_eq!(gain, Money::ZERO);
    // Reverse it: moved = -0.001, a loss of the same size, reported as a full
    // ulp out.
    let loss = convergence_capture(rate("0.001"), rate("0.002"), Money::EPSILON).expect("ok");
    assert_eq!(loss.raw(), -1);
    // On a size where the arithmetic is exact, the two are mirror images.
    assert_eq!(
        convergence_capture(rate("0.002"), rate("0.001"), money("1000000")).expect("ok"),
        money("1000")
    );
    assert_eq!(
        convergence_capture(rate("0.001"), rate("0.002"), money("1000000")).expect("ok"),
        money("-1000")
    );
    assert!(convergence_capture(rate("0.002"), rate("0.001"), money("-1")).is_err());
}

#[test]
fn an_unknown_fee_tier_resolves_to_the_worst_row_and_says_so() {
    let t = FeeTable::new(vec![
        VolumeTier { min_volume_30d: Money::ZERO, maker: bps("2"), taker: bps("5.5") },
        VolumeTier { min_volume_30d: money("1000000"), maker: bps("1.6"), taker: bps("4.5") },
        VolumeTier { min_volume_30d: money("5000000"), maker: bps("1.4"), taker: bps("4") },
    ])
    .expect("valid table");
    let unknown = t.resolve(None).expect("ok");
    assert_eq!(unknown.taker, bps("5.5"));
    assert!(!unknown.tier_known, "an assumed tier must reach the screen as assumed");
    // A known volume picks its row, and the threshold is INCLUSIVE.
    assert_eq!(t.resolve(Some(money("1000000"))).expect("ok").taker, bps("4.5"));
    assert_eq!(
        t.resolve(Some(money("999999.999999999999"))).expect("ok").taker,
        bps("5.5")
    );
    assert!(t.resolve(Some(money("5000000"))).expect("ok").tier_known);
}

#[test]
fn the_worst_tier_is_found_by_comparing_rates_not_by_taking_the_first_row() {
    // A promotional first row breaks the monotonicity a "take the first row"
    // implementation assumes. The worst taker here is in the MIDDLE.
    let t = FeeTable::new(vec![
        VolumeTier { min_volume_30d: Money::ZERO, maker: bps("0"), taker: bps("0") },
        VolumeTier { min_volume_30d: money("1000000"), maker: bps("3"), taker: bps("9") },
        VolumeTier { min_volume_30d: money("5000000"), maker: bps("1.4"), taker: bps("4") },
    ])
    .expect("valid table");
    let w = t.worst().expect("ok");
    assert_eq!(w.taker, bps("9"));
    assert_eq!(w.maker, bps("3"));
    assert!(!w.tier_known);
}

#[test]
fn an_ambiguous_fee_table_must_not_resolve_in_our_favour() {
    // Two rows at the SAME volume threshold. `FeeTable::new` accepts this
    // (`<=`, not `<`), and `resolve` keeps the LAST qualifying row, which by
    // table convention is the cheaper one. The honest answer for an ambiguous
    // table is the worse rate, exactly as for an unknown tier.
    //
    // Fix-agnostic: either the table is refused or the worse row wins.
    let built = FeeTable::new(vec![
        VolumeTier { min_volume_30d: Money::ZERO, maker: bps("2"), taker: bps("5.5") },
        VolumeTier { min_volume_30d: Money::ZERO, maker: bps("1"), taker: bps("3") },
    ]);
    match built {
        Err(_) => {}
        Ok(t) => {
            let r = t.resolve(Some(money("1000000"))).expect("resolves");
            assert_eq!(
                r.taker,
                bps("5.5"),
                "a duplicate threshold must resolve to the worse row, not the cheaper one"
            );
        }
    }
}

#[test]
fn a_round_trip_is_four_legs_not_one() {
    // 5.5 bps taker on all four legs = 22 bps. A "spread minus fees" display
    // that charges one leg understates the cost by a factor of four.
    let rates = FeeRates::new(bps("2"), bps("5.5"), true).expect("ok");
    assert_eq!(RoundTrip::ALL_TAKER.total_bps(rates).expect("ok"), bps("22"));
    // Mixed roles add the right rate per leg: 2 + 5.5 + 2 + 5.5 = 15.
    let mixed = RoundTrip {
        entry_a: FeeRole::Maker,
        entry_b: FeeRole::Taker,
        exit_a: FeeRole::Maker,
        exit_b: FeeRole::Taker,
    };
    assert_eq!(mixed.total_bps(rates).expect("ok"), bps("15"));
    assert!(FeeRates::new(bps("-1"), bps("5.5"), true).is_err());
    assert!(FeeRates::new(bps("2"), bps("-5.5"), true).is_err());
}

#[test]
fn a_negative_round_trip_cost_must_not_become_positive_annual_return() {
    // Every other entry point in arb-math refuses an out-of-domain negative:
    // `fee_on_notional`, `FeeRates::new` and `SlippageBudget::new` all do.
    // `net_apr_over_horizon` does not, and its COST rounding then grows the
    // magnitude of the negative cost, so the error runs in the FLATTERING
    // direction:
    //   annual_cost = -0.0022 * 31_536_000 / 28_800 = -2.409
    //   net = 0.1095 - (-2.409) = +2.5185 = +251.85% APR
    // A sign error one call upstream is rendered as the best row on the board.
    let got = net_apr_over_horizon(rate("0.1095"), bps("-22"), Millis::from_secs(28_800));
    match got {
        Err(_) => {}
        Ok(v) => panic!(
            "a negative round-trip cost was accepted and returned {v} \
             (expected a refusal, as everywhere else in this crate)"
        ),
    }
}

#[test]
fn a_fee_at_btc_scale_is_rounded_up_at_the_twelfth_place() {
    // The `Money::EPSILON` case above proves the direction at one ulp. This one
    // proves it where the money is, on a notional with sub-cent digits so that
    // the product is genuinely not representable.
    //
    // Hand derivation, exact integer arithmetic:
    //   notional = 68_123.456789012345      raw = 68_123_456_789_012_345
    //   rate     = 5.5 bps = 0.00055        raw =        550_000_000
    //   product  = 68_123_456_789_012_345 * 550_000_000 / 1e12
    //            = 68_123_456_789_012_345 * 55 / 1e5
    //   68_123_456_789_012_345 * 50 = 3_406_172_839_450_617_250
    //   68_123_456_789_012_345 *  5 =   340_617_283_945_061_725
    //                          * 55 = 3_746_790_123_395_678_975
    //   / 1e5                       =    37_467_901_233_956.78975
    // So the true fee is 37.46790123395678975 USDT. Twelve places cannot hold
    // it: it must land on ...956 or ...957. COST takes ...957, which is
    // 0.00000000000021025 USDT MORE than the venue will charge.
    //
    // The other direction would report a fee below the real one on every trade.
    // At our twelve basis points the per-trade amount is irrelevant; the
    // direction is not, because a systematic under-charge is indistinguishable
    // from an edge.
    let got = fee_on_notional(money("68123.456789012345"), bps("5.5")).expect("ok");
    assert_eq!(got.raw(), 37_467_901_233_957);
    assert!(got > money("37.467901233956"), "a fee is never rounded down");
}

#[test]
fn to_bps_refuses_a_value_too_large_for_i64_rather_than_wrapping_it() {
    // `Bps` is `i64` at 1e8 and `Rate` is `i128` at 1e12, and by decision 002 the
    // two share the same raw integer — so `to_bps` is a pure narrowing and the
    // only thing it can get wrong is the overflow.
    //
    // i64::MAX = 9_223_372_036_854_775_807, which as a ratio is 9_223_372.036...
    // (922,337,203.6...%). Absurd as a rate, and reachable by arithmetic: see
    // below. What matters is what happens ONE ulp past it. A checked conversion
    // errors. An `as i64` cast wraps to i64::MIN, turning the largest positive
    // ratio expressible into the largest NEGATIVE one — a cost of 9.2e6
    // displayed as a receipt of 9.2e6. That is not a precision loss, it is a
    // sign flip at the widest possible magnitude.
    let at_the_edge = Rate::from_raw(i128::from(i64::MAX));
    assert_eq!(at_the_edge.to_bps().expect("i64::MAX fits").raw(), i64::MAX);

    let one_past = Rate::from_raw(i128::from(i64::MAX) + 1);
    assert!(one_past.to_bps().is_err(), "one ulp past i64::MAX must error");
    // Symmetrically at the bottom: i64::MIN fits, one below does not.
    let bottom = Rate::from_raw(i128::from(i64::MIN));
    assert_eq!(bottom.to_bps().expect("i64::MIN fits").raw(), i64::MIN);
    assert!(Rate::from_raw(i128::from(i64::MIN) - 1).to_bps().is_err());

    // And it is reachable by ordinary arithmetic rather than only by `from_raw`:
    // a rate of 300_000 (30,000,000%) annualised over an 8h interval is
    // 300_000 * 1095 = 328_500_000, raw 3.285e20, which is 35x i64::MAX. A
    // nonsense input, but the failure mode must be an error and not a sign flip.
    let annual = arb_math::annualise(
        rate("300000"),
        arb_types::FundingInterval::EIGHT_HOURS,
        RoundMode::REVENUE,
    )
    .expect("the Rate itself is fine, i128 holds it");
    assert_eq!(annual.raw(), 328_500_000_000_000_000_000);
    assert!(annual.to_bps().is_err(), "narrowing it must fail loudly");
}

#[test]
fn the_conservative_annualisation_mode_flips_with_the_sign_of_the_receipt() {
    // The rule: shrink a receipt, grow a cost. Which of the two a funding number
    // is depends on the sign AFTER the sign law is applied, so the rounding mode
    // has to be chosen after it — not before, and not once for the module.
    //
    // A 7h interval is used because 31_536_000 / 25_200 is NOT an integer, so
    // the rounding actually bites. 8h would divide exactly and prove nothing.
    //
    // Hand derivation, rate +1 bp per 7h:
    //   |annual| = 0.0001 * 31_536_000 / 25_200
    //            = 3153.6 / 25_200 = 0.125142857142857142...
    //   raw      = 3_153_600_000_000_000 / 25_200 = 125_142_857_142.857142...
    //   SHORT receives (+): truncate toward zero -> +125_142_857_142
    //   LONG    pays   (-): away from zero       -> -125_142_857_143
    // The two magnitudes therefore differ by exactly one ulp, and the larger one
    // is the cost. Symmetric rounding here would credit a long with a tenth of a
    // femto-unit it does not have — trivial once, and the wrong direction
    // for ever.
    let seven_h = FundingInterval::from_secs(7 * 3_600).expect("7h is a valid interval");
    let short = CarryLeg {
        vkey: linear_vkey(),
        side: PositionSide::Short,
        rate: rate("0.0001"),
        interval: seven_h,
        quality: DataQuality::SYNTHESIZED,
    };
    let long = CarryLeg {
        side: PositionSide::Long,
        ..short
    };
    assert_eq!(short.annual_receipt().expect("ok").raw(), 125_142_857_142);
    assert_eq!(long.annual_receipt().expect("ok").raw(), -125_142_857_143);
    assert_eq!(
        long.annual_receipt().expect("ok").raw().abs(),
        short.annual_receipt().expect("ok").raw().abs() + 1,
        "the cost leg must be the larger magnitude, never the receipt leg"
    );

    // And the mirror: a NEGATIVE published rate makes the LONG the receiver, so
    // the asymmetry has to swap ends. If `against_us` looked at the published
    // rate instead of the post-sign-law receipt, this half would round the wrong
    // way and nothing in the 8h tests would notice.
    let neg_short = CarryLeg {
        rate: rate("-0.0001"),
        ..short
    };
    let neg_long = CarryLeg {
        side: PositionSide::Long,
        ..neg_short
    };
    assert_eq!(neg_long.annual_receipt().expect("ok").raw(), 125_142_857_142);
    assert_eq!(neg_short.annual_receipt().expect("ok").raw(), -125_142_857_143);
}

#[test]
fn a_net_apr_never_understates_the_cost_it_amortises() {
    // `net_apr_over_horizon` rounds the cost term COST regardless of the
    // receipt's sign, which is correct because a round trip is always money out.
    // Proved on a horizon where the scaling is inexact: 7 days = 604_800 s.
    //
    //   annual_cost = 0.0022 * 31_536_000 / 604_800
    //   0.0022 * 31_536_000 = 69_379.2       raw 69_379_200_000_000_000
    //   69_379.2 / 604_800  = 693_792 / 6_048_000 = 803 / 7000 exactly
    //                       = 0.114714285714285714...
    //                         raw 114_714_285_714.285714...
    //   COST (away from zero) -> 114_714_285_715, i.e. 11.4714285715%
    //   net = 0.1095 - 0.114714285715 = -0.005214285715
    // The true net is -0.005214285714285714..., so the reported figure is BELOW
    // it: the loss reads slightly larger than it is. That is the only acceptable
    // direction.
    //
    // The expectations below were CORRECTED, not the code: this test first
    // asserted a net of -5_206_349_207, from a long division of 69_379.2/604_800
    // that landed on 803/7000 - 1/126000 instead of 803/7000. The property the
    // test exists to defend is untouched, and it still fails if the cost term is
    // ever rounded toward zero. See decisions/018.
    let seven_days = Millis::from_secs(7 * 86_400);
    let got = net_apr_over_horizon(rate("0.1095"), bps("22"), seven_days).expect("ok");
    assert_eq!(got.raw(), -5_214_285_715);
    assert!(
        got < rate("-0.005214285714"),
        "amortised cost must never be rounded in our favour"
    );

    // Same horizon, but the receipt is itself a cost (a long paying funding).
    // net = -0.1095 - 0.114714285715 = -0.224214285715. Both terms negative, so
    // the sum must be the more negative of the two candidates.
    let paying = net_apr_over_horizon(rate("-0.1095"), bps("22"), seven_days).expect("ok");
    assert_eq!(paying.raw(), -224_214_285_715);
    // A zero-cost round trip leaves the receipt exactly as handed in: no
    // rounding is applied to a term that was not divided.
    assert_eq!(
        net_apr_over_horizon(rate("0.1095"), Bps::ZERO, seven_days).expect("ok"),
        rate("0.1095")
    );
}

#[test]
fn margin_times_leverage_is_never_below_the_notional() {
    // The invariant that makes `margin_required` safe: whatever the double
    // rounding does, posting the reported margin at the requested leverage must
    // support at least the requested notional. `recip(COST)` then `mul_rate(COST)`
    // both push up, so the invariant should hold with room to spare — but two
    // roundings in the same direction is also how a figure drifts, so the drift
    // is quantified rather than assumed.
    //
    // Worked case, 1_000_000 at 7x:
    //   1/7      = 1e24 / 7e12 = 142_857_142_857.142857...  -> COST 142_857_142_858
    //   margin   = 1e18 * 142_857_142_858 / 1e12 = 142_857_142_858_000_000
    //            = 142_857.142858
    //   true     = 1_000_000 / 7 = 142_857.142857142857142857...
    //   the closest representable value BELOW the truth, at scale 1e12, is
    //            142_857.142857142857    raw 142_857_142_857_142_857
    //   drift    = 142_857_142_858_000_000 - 142_857_142_857_142_857
    //            = 857_143 raw = 0.000000857143 USDT over-margined
    //   (the drift from the unrounded 1/7 is 0.00000085714285714..., of which
    //    857_143 raw is the representable ceiling — the safe side again)
    //   as a fraction of notional: 8.57e-13, i.e. 8.6e-9 bps. Six orders of
    //   magnitude below our twelve basis points, and on the safe side.
    //
    // The subtrahend and the expected drift were CORRECTED, not the code: this
    // test subtracted `money("142857.142857")` — the truth truncated at SIX
    // places, not twelve — while asserting the twelve-place drift, and asserted
    // 857_142 where the twelve-place difference is 857_143. The invariant it
    // defends held all along; `margin_required` is right. See decisions/018.
    let m7 = margin_required(money("1000000"), rate("7")).expect("ok");
    assert_eq!(m7.raw(), 142_857_142_858_000_000);
    assert!(m7 > money("142857.142857142857"));
    let drift = m7.checked_sub(money("142857.142857142857")).expect("ok");
    assert_eq!(drift.raw(), 857_143); // 0.000000857143 USDT

    // The invariant across a sweep, including fractional leverages where neither
    // the reciprocal nor the product is exact. `mul_rate(REVENUE)` is used to
    // reconstruct the supported notional so that the reconstruction itself cannot
    // be what rescues the check.
    for lev in ["1", "2", "3", "3.5", "5", "7", "7.5", "10", "12.5", "20", "100"] {
        for n in ["1", "1000", "1000000", "68123.456789012345", "0.000000000001"] {
            let notional = money(n);
            let leverage = rate(lev);
            let margin = margin_required(notional, leverage).expect("ok");
            let supported = margin.mul_rate(leverage, RoundMode::REVENUE).expect("ok");
            assert!(
                supported >= notional,
                "leverage {lev} on {n}: margin {margin} supports only {supported}"
            );
        }
    }
}

#[test]
fn reported_capacity_must_actually_fit_inside_the_budget() {
    // The contract of `max_size_within_slippage` is "the largest size whose
    // AVERAGE fill price stays inside the budget". So walking the book for
    // exactly the size it returns must not breach the budget.
    //
    // This book breaks it. Touch 100.000000000001, budget 50 bps:
    //   limit = trunc(100.000000000001 * 1.005) = 100.500000000001
    //   level 1 is within, so it is taken whole; its cost is accumulated with
    //   RoundMode::HalfEven, giving 10.000000000000 where the true product is
    //   10.0000000000001 and where `executable_price` (COST) will charge
    //   10.000000000001.
    //   The solve therefore uses a cheaper cost than execution will, and hands
    //   back 0.2, whose real VWAP is 100.500000000005 -> 50.00000001 bps.
    //
    // `max_size_within_slippage` documents "every rounding here is chosen to
    // UNDER-state capacity"; HalfEven is the one mode that can do the opposite.
    let b = book(
        vec![level("1", "1")],
        vec![level("100.000000000001", "0.1"), level("101", "10")],
    );
    let budget = SlippageBudget::new(bps("50")).expect("ok");
    let cap = max_size_within_slippage(&b, Side::Buy, budget).expect("ok");
    let walk = executable_price(&b, Side::Buy, cap).expect("ok");
    let actual = slippage_bps(walk.vwap, walk.touch, Side::Buy).expect("ok");
    assert!(
        actual <= budget.bps(),
        "capacity {cap} walks at {actual} bps against a {} bps budget",
        budget.bps()
    );
}

#[test]
fn slippage_is_non_negative_and_zero_at_the_touch() {
    // A fill entirely at the touch has cost nothing extra, on either side.
    let b = book(vec![level("100", "5")], vec![level("100.5", "5")]);
    let buy = executable_price(&b, Side::Buy, qty("2")).expect("ok");
    assert_eq!(slippage_bps(buy.vwap, buy.touch, Side::Buy).expect("ok"), Bps::ZERO);
    let sell = executable_price(&b, Side::Sell, qty("2")).expect("ok");
    assert_eq!(slippage_bps(sell.vwap, sell.touch, Side::Sell).expect("ok"), Bps::ZERO);
    // A zero or negative touch is a broken feed, not a slippage of infinity.
    assert!(slippage_bps(price("100"), price("0"), Side::Buy).is_err());
    let _ = Rate::ONE;
}
