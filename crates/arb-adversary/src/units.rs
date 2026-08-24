//! Units and scale. A ratio used as basis points is a 10,000x error; `Price`,
//! `Qty`, `Money` and `Rate` live at scale 1e12 while `Bps` lives at 1e8.
//!
//! Everything here is derived from the two definitions and nothing else:
//!   raw(Rate)  = ratio * 1e12
//!   raw(Bps)   = basis_points * 1e8 = (ratio * 1e4) * 1e8 = ratio * 1e12
//! so the two raws are the same integer for the same economic quantity, and any
//! place that multiplies or divides on the way between them is a bug.

use arb_num::{Bps, Money, Price, Qty, Rate, RoundMode, SCALE};
use arb_detect::PaintLimits;
use arb_types::{FundingInterval, SECONDS_PER_YEAR};

use crate::support::{bps, money, price, rate};

#[test]
fn one_basis_point_is_one_ten_thousandth() {
    // 1 bp = 0.0001. raw(Rate) = 0.0001 * 1e12 = 100_000_000.
    assert_eq!(Bps::ONE.raw(), 100_000_000);
    assert_eq!(Bps::ONE.to_rate().raw(), 100_000_000);
    assert_eq!(Bps::ONE.to_rate(), rate("0.0001"));
    // And the two spellings of the same thing agree.
    assert_eq!(Bps::from_whole(1).expect("1 bp"), Bps::ONE);
    assert_eq!(bps("1"), Bps::ONE);
    assert_eq!(Bps::parse_ratio("0.0001").expect("ratio"), Bps::ONE);
}

#[test]
fn a_ratio_parsed_as_basis_points_is_a_ten_thousand_fold_error() {
    // The failure this pins: someone writes `Bps::parse_bps("0.0012")` meaning
    // the ratio 0.0012 (= 12 bps). It is 0.0012 OF ONE BASIS POINT.
    let as_ratio = Bps::parse_ratio("0.0012").expect("ratio form");
    let as_bps = Bps::parse_bps("0.0012").expect("bps form");
    assert_eq!(as_ratio, bps("12"));
    assert_eq!(as_ratio.raw(), 12 * 100_000_000);
    assert_eq!(as_bps.raw(), 120_000);
    // Exactly 10_000x apart. If the two ever stop differing by this factor the
    // parse functions have been unified and one of them is now silently wrong.
    assert_eq!(as_ratio.raw(), as_bps.raw() * 10_000);
}

#[test]
fn twelve_basis_points_on_a_million_is_twelve_hundred() {
    // The edge we trade. 1_000_000 * 0.0012 = 1200 exactly, no rounding
    // involved, so any rounding mode must agree.
    let notional = money("1000000");
    let expected = money("1200");
    for mode in [
        RoundMode::Trunc,
        RoundMode::Floor,
        RoundMode::Ceil,
        RoundMode::AwayFromZero,
        RoundMode::HalfEven,
    ] {
        assert_eq!(
            notional.mul_rate(bps("12").to_rate(), mode).expect("ok"),
            expected,
            "12 bps of 1e6 is 1200 under every rounding mode"
        );
    }
}

#[test]
fn money_and_price_share_the_twelve_digit_scale() {
    assert_eq!(SCALE, 1_000_000_000_000);
    assert_eq!(Price::ONE.raw(), SCALE);
    assert_eq!(Qty::ONE.raw(), SCALE);
    assert_eq!(Money::ONE.raw(), SCALE);
    assert_eq!(Rate::ONE.raw(), SCALE);
    assert_eq!(Price::EPSILON.raw(), 1);
    // A BTC price. 68_000 * 1e12.
    assert_eq!(price("68000").raw(), 68_000_000_000_000_000);
}

#[test]
fn notional_of_one_btc_at_sixty_eight_thousand_is_sixty_eight_thousand() {
    // Price * Qty = Money, and the scale must be divided out exactly once.
    // 68_000e12 * 1e12 / 1e12 = 68_000e12.
    let got = price("68000")
        .mul_qty(Qty::ONE, RoundMode::COST)
        .expect("ok");
    assert_eq!(got, money("68000"));
    assert_eq!(got.raw(), 68_000_000_000_000_000);
}

#[test]
fn a_paint_ratio_threshold_is_a_ratio_not_a_spread() {
    // `one_sided_ratio_min` is documented as "40000 bps == 400% == 4.0x" and is
    // compared against `fat / thin`, a Rate. 40_000 bps = ratio 4.0, raw 4e12.
    // If it had been written as `Bps::from_whole(4)` the check would fire at
    // 0.0004x, i.e. always.
    let l = PaintLimits::CONSERVATIVE;
    assert_eq!(l.one_sided_ratio_min.to_rate(), rate("4"));
    assert_eq!(l.one_sided_ratio_min.to_rate().raw(), 4 * SCALE);
    // 6000 bps == 60%.
    assert_eq!(l.far_share_min.to_rate(), rate("0.6"));
    // 25 bps == 0.0025, twice our edge.
    assert_eq!(l.far_bps.to_rate(), rate("0.0025"));
    assert_eq!(l.gap_max_bps.to_rate(), rate("0.0025"));
}

#[test]
fn seconds_per_year_is_exactly_three_hundred_and_sixty_five_days() {
    assert_eq!(SECONDS_PER_YEAR, 365 * 24 * 60 * 60);
    assert_eq!(SECONDS_PER_YEAR, 31_536_000);
}

#[test]
fn exact_period_counts_are_exact_and_inexact_ones_are_refused() {
    // 31_536_000 / 28_800 = 1095. / 14_400 = 2190. / 7_200 = 4380. / 3_600 = 8760.
    assert_eq!(
        FundingInterval::EIGHT_HOURS.periods_per_year_exact(),
        Some(1095)
    );
    assert_eq!(
        FundingInterval::FOUR_HOURS.periods_per_year_exact(),
        Some(2190)
    );
    assert_eq!(FundingInterval::TWO_HOURS.periods_per_year_exact(), Some(4380));
    assert_eq!(FundingInterval::ONE_HOUR.periods_per_year_exact(), Some(8760));
    // 31_536_000 / 25_200 = 1251.428571... Not exact, so None: nobody gets to
    // round a period count.
    let seven_h = FundingInterval::from_secs(7 * 3600).expect("7h is a valid interval");
    assert_eq!(seven_h.periods_per_year_exact(), None);
    assert_eq!(FundingInterval::EIGHT_HOURS.secs(), 28_800);
    assert_eq!(FundingInterval::EIGHT_HOURS.as_millis().millis(), 28_800_000);
}

#[test]
fn rate_to_bps_is_a_pure_narrowing_with_no_rescaling() {
    // Same raw integer in both directions. A `* 10_000` or `/ 10_000` anywhere
    // in `to_bps` would show up here immediately.
    for s in ["0.0001", "0.0012", "0.1095", "-0.0022", "0"] {
        let r = rate(s);
        let b = r.to_bps().expect("fits i64");
        assert_eq!(i128::from(b.raw()), r.raw(), "rate {s} narrows without rescale");
        assert_eq!(Rate::from_bps(b), r, "and widens back exactly");
    }
}
