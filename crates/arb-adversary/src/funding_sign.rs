//! Funding sign and interval. The law: **a POSITIVE rate means LONGS PAY
//! SHORTS.** A flipped sign is not a rounding error, it is the other side of
//! the trade.

use arb_math::{annualise, funding_payment, net_apr_over_horizon, periods_to_breakeven, CarryLeg};
use arb_num::{Rate, RoundMode};
use arb_types::{
    DataQuality, FundingInterval, FundingSnapshot, LocalTs, Millis, PositionSide, WallTs,
};

use crate::support::{bps, linear_vkey, money, rate};

fn snapshot(rate_s: &str, interval: FundingInterval) -> FundingSnapshot {
    FundingSnapshot {
        vkey: linear_vkey(),
        rate: rate(rate_s),
        interval,
        next_funding_at: Some(WallTs::from_millis(28_800_000)),
        mark: None,
        index: None,
        exchange_ts: WallTs::from_millis(0),
        observed_at: LocalTs::from_millis(0),
        quality: DataQuality::SYNTHESIZED,
    }
}

fn leg(rate_s: &str, interval: FundingInterval, side: PositionSide) -> CarryLeg {
    CarryLeg {
        vkey: linear_vkey(),
        side,
        rate: rate(rate_s),
        interval,
        quality: DataQuality::SYNTHESIZED,
    }
}

#[test]
fn the_sign_law_itself() {
    // Positive rate, longs pay. A long's receipt is therefore NEGATIVE.
    assert_eq!(PositionSide::Long.funding_receipt_sign(), -1);
    assert_eq!(PositionSide::Short.funding_receipt_sign(), 1);
    assert_eq!(PositionSide::Long.opposite(), PositionSide::Short);
    assert_eq!(PositionSide::Short.opposite(), PositionSide::Long);
}

#[test]
fn a_long_pays_and_a_short_receives_when_the_rate_is_positive() {
    // rate +1 bp. Receipt rate for a long is -0.0001, for a short +0.0001.
    let snap = snapshot("0.0001", FundingInterval::EIGHT_HOURS);
    assert_eq!(
        snap.receipt_rate(PositionSide::Long).expect("ok"),
        rate("-0.0001")
    );
    assert_eq!(
        snap.receipt_rate(PositionSide::Short).expect("ok"),
        rate("0.0001")
    );
}

#[test]
fn a_negative_rate_reverses_who_pays() {
    // rate -3 bp: shorts pay longs. A long now RECEIVES 3 bp.
    let snap = snapshot("-0.0003", FundingInterval::EIGHT_HOURS);
    assert_eq!(
        snap.receipt_rate(PositionSide::Long).expect("ok"),
        rate("0.0003")
    );
    assert_eq!(
        snap.receipt_rate(PositionSide::Short).expect("ok"),
        rate("-0.0003")
    );
}

#[test]
fn cash_and_carry_earns_when_funding_is_positive() {
    // The canonical trade: long spot, SHORT perp. Positive funding must be a
    // receipt on the perp leg. If this ever inverts, the product recommends the
    // losing side of every funding row on the board.
    let short_perp = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Short);
    let per = short_perp.receipt_per_period().expect("ok");
    assert!(per.is_positive(), "short perp collects positive funding");
    assert_eq!(per, rate("0.0001"));
    // 100_000 notional * 0.0001 = +10 USDT arriving.
    assert_eq!(short_perp.payment(money("100000")).expect("ok"), money("10"));

    let long_perp = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Long);
    // ... and the mirror leg pays exactly the same 10 USDT out.
    assert_eq!(
        long_perp.payment(money("100000")).expect("ok"),
        money("-10")
    );
}

#[test]
fn funding_payment_is_signed_by_side_not_by_the_callers_minus_key() {
    // 250_000 notional at 7.5 bp. 250_000 * 0.00075 = 187.5 exactly.
    let n = money("250000");
    assert_eq!(
        funding_payment(n, rate("0.00075"), PositionSide::Short).expect("ok"),
        money("187.5")
    );
    assert_eq!(
        funding_payment(n, rate("0.00075"), PositionSide::Long).expect("ok"),
        money("-187.5")
    );
}

#[test]
fn a_negative_notional_is_refused_rather_than_absolute_valued() {
    // Direction lives in the side. A negative size reaching here is a sign
    // error upstream at exactly the place a receipt turns into a cost.
    assert!(funding_payment(
        money("-100000"),
        rate("0.0001"),
        PositionSide::Short
    )
    .is_err());
}

#[test]
fn eight_hourly_annualisation_is_exactly_one_thousand_and_ninety_five_periods() {
    // 31_536_000 / 28_800 = 1095. 0.0001 * 1095 = 0.1095 = 10.95% APR.
    let got = annualise(
        rate("0.0001"),
        FundingInterval::EIGHT_HOURS,
        RoundMode::REVENUE,
    )
    .expect("ok");
    assert_eq!(got, rate("0.1095"));
}

#[test]
fn assuming_eight_hours_for_an_hourly_instrument_is_exactly_an_eight_fold_error() {
    // 8760 / 1095 = 8. The same printed rate on an hourly instrument is eight
    // times the APR, and a ranking sorted on APR puts that row first.
    let hourly = annualise(rate("0.0001"), FundingInterval::ONE_HOUR, RoundMode::REVENUE)
        .expect("ok");
    let eight_hourly = annualise(
        rate("0.0001"),
        FundingInterval::EIGHT_HOURS,
        RoundMode::REVENUE,
    )
    .expect("ok");
    assert_eq!(hourly, rate("0.876"));
    assert_eq!(hourly.raw(), eight_hourly.raw() * 8);
}

#[test]
fn a_seven_hour_interval_annualises_on_the_exact_rational() {
    // 31_536_000 / 25_200 = 1251.428571... periods.
    // 0.0001 * 31_536_000 = 3153.6, / 25_200 = 0.125142857142857...
    // raw = 3_153_600_000_000_000 / 25_200 = 125_142_857_142.857 -> trunc.
    let seven_h = FundingInterval::from_secs(7 * 3_600).expect("7h");
    let exact = annualise(rate("0.0001"), seven_h, RoundMode::REVENUE).expect("ok");
    assert_eq!(exact.raw(), 125_142_857_142);

    // What a rounded period count would have produced: 1251 * 0.0001 = 0.1251.
    let rounded = rate("0.1251");
    // The gap is 0.428571 periods of 1 bp = 0.0000428571... = 0.4285 bps.
    // (The module doc calls this "4.29 bps"; it is 0.4286 bps. See report.)
    let gap = exact.checked_sub(rounded).expect("ok");
    assert_eq!(gap.raw(), 42_857_142);
    assert!(gap.is_positive(), "truncating the period count understates APR");
}

#[test]
fn breakeven_is_a_ceiling_not_a_rounding() {
    // 22 bp of round trip against a 1 bp settlement: 22 settlements. Twenty-one
    // do not cover it.
    assert_eq!(
        periods_to_breakeven(rate("0.0001"), bps("22")).expect("ok"),
        22
    );
    // 22 bp against 1.5 bp: 22 / 1.5 = 14.666..., so 15.
    assert_eq!(
        periods_to_breakeven(rate("0.00015"), bps("22")).expect("ok"),
        15
    );
    // Exact division must NOT be pushed to the next period: 20 / 2 = 10.
    assert_eq!(
        periods_to_breakeven(rate("0.0002"), bps("20")).expect("ok"),
        10
    );
}

#[test]
fn a_receipt_running_against_us_has_no_breakeven_count() {
    // A long paying funding never pays off a round trip by waiting. Returning a
    // large number here would invite a caller to render it as a long wait.
    assert!(periods_to_breakeven(rate("-0.0001"), bps("22")).is_err());
    assert!(periods_to_breakeven(Rate::ZERO, bps("22")).is_err());
    // Zero cost is not an error: nothing to pay off.
    assert_eq!(
        periods_to_breakeven(rate("0.0001"), bps("0")).expect("ok"),
        0
    );
}

#[test]
fn the_product_claim_one_settlement_of_ten_point_nine_five_percent_is_minus_two_hundred_and_thirty() {
    // annual receipt 0.1095 (10.95% APR, the number every other scanner shows).
    // Round trip 22 bp. Held one 8h settlement = 28_800 s.
    // annual cost = 0.0022 * 31_536_000 / 28_800 = 0.0022 * 1095 = 2.409.
    // net = 0.1095 - 2.409 = -2.2995 = -229.95%.
    let got = net_apr_over_horizon(rate("0.1095"), bps("22"), Millis::from_secs(28_800))
        .expect("ok");
    assert_eq!(got, rate("-2.2995"));

    // Held one year the same round trip costs exactly its 22 bp.
    let year = net_apr_over_horizon(rate("0.1095"), bps("22"), Millis::from_secs(31_536_000))
        .expect("ok");
    assert_eq!(year, rate("0.1073"));

    // Held 73 days (a fifth of a year) it costs 110 bp: 0.1095 - 0.011 = 0.0985.
    let seventy_three = net_apr_over_horizon(
        rate("0.1095"),
        bps("22"),
        Millis::from_secs(73 * 86_400),
    )
    .expect("ok");
    assert_eq!(seventy_three, rate("0.0985"));
}

#[test]
fn a_zero_or_negative_horizon_is_refused_not_treated_as_a_year() {
    assert!(net_apr_over_horizon(rate("0.1095"), bps("22"), Millis::ZERO).is_err());
    assert!(net_apr_over_horizon(rate("0.1095"), bps("22"), Millis::from_secs(-1)).is_err());
    // Sub-second horizons truncate to zero seconds and are refused rather than
    // producing a division by a rounded-down zero.
    assert!(net_apr_over_horizon(rate("0.1095"), bps("22"), Millis::from_millis(999)).is_err());
}

#[test]
fn annual_receipt_applies_the_sign_law_before_annualising() {
    // Long at +1 bp per 8h: -0.0001 * 1095 = -0.1095. Paying 10.95% a year.
    let long = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Long);
    assert_eq!(long.annual_receipt().expect("ok"), rate("-0.1095"));
    // Short at the same rate collects it.
    let short = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Short);
    assert_eq!(short.annual_receipt().expect("ok"), rate("0.1095"));
    // Perfect antisymmetry only because 1095 divides exactly; see rounding.rs
    // for the case where it does not.
    assert_eq!(
        long.annual_receipt().expect("ok").raw(),
        -short.annual_receipt().expect("ok").raw()
    );
}

#[test]
fn time_to_breakeven_exposes_the_interval_the_count_hides() {
    // 22 settlements at 8h = 176 h = 633_600 s = 7 days 8 h.
    let eight = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Short);
    assert_eq!(
        eight.time_to_breakeven(bps("22")).expect("ok").millis(),
        633_600_000
    );
    // The same 22 settlements on an hourly instrument is under a day.
    let hourly = leg("0.0001", FundingInterval::ONE_HOUR, PositionSide::Short);
    assert_eq!(
        hourly.time_to_breakeven(bps("22")).expect("ok").millis(),
        79_200_000
    );
}

#[test]
fn time_to_settlement_is_signed_and_absent_when_the_venue_did_not_say() {
    let snap = snapshot("0.0001", FundingInterval::EIGHT_HOURS);
    // next_funding_at = 28_800_000 ms. now = 28_000_000 -> 800_000 ms to go.
    assert_eq!(
        snap.time_to_settlement(WallTs::from_millis(28_000_000)),
        Some(Millis::from_millis(800_000))
    );
    // Past the settlement the figure goes negative rather than clamping: a
    // clamp at zero would hide that the rate in hand has already paid out.
    assert_eq!(
        snap.time_to_settlement(WallTs::from_millis(29_000_000)),
        Some(Millis::from_millis(-200_000))
    );
    let unknown = FundingSnapshot {
        next_funding_at: None,
        ..snapshot("0.0001", FundingInterval::EIGHT_HOURS)
    };
    assert_eq!(unknown.time_to_settlement(WallTs::from_millis(0)), None);
}

#[test]
fn a_zero_funding_interval_cannot_be_constructed() {
    // Otherwise annualisation divides by zero and every APR on the screen is
    // whatever the error path returns.
    assert!(FundingInterval::from_secs(0).is_err());
    assert_eq!(
        annualise(rate("0.0001"), FundingInterval::EIGHT_HOURS, RoundMode::REVENUE)
            .expect("ok"),
        rate("0.1095")
    );
}
