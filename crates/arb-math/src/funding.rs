//! Funding: exact annualisation, signed settlements, and time-to-breakeven.
//!
//! # The lie this module exists to refuse
//!
//! Every funding scanner shows an APR. It computes it by taking the current
//! funding rate, multiplying by the number of settlements in a year, and
//! printing the result as a percentage. Two things are wrong with that number,
//! and they run in the same direction — they both make the opportunity look
//! better than it is.
//!
//! **First, it assumes the rate persists.** Funding rates mean-revert within
//! hours. A rate of 1 bp per 8h annualises to 10.95%, but nothing about the
//! current print says the next thirty settlements will look like this one.
//! [`annualise`] computes what it is asked to compute and its docs say plainly
//! that the persistence assumption is the caller's, not ours. Tracked as
//! `OPEN.md` A4.
//!
//! **Second, and worse, it ignores that you have to get in and out.** A carry
//! position pays four fee legs and crosses two spreads before it collects a
//! single settlement. Against a 22 bp round trip, a 1 bp settlement is not a
//! 10.95% return — it is a loss until the twenty-second settlement.
//! [`periods_to_breakeven`] reports that count, and
//! [`net_apr_over_horizon`] amortises the round trip over the holding period
//! you actually intend, which is the only annualisation that is not a fiction.
//!
//! The demonstration, computed in the tests of this module: an opportunity
//! displayed elsewhere as **+10.95% APR**, held for exactly one settlement, is
//! **−229.95% annualised**. Same rate, same fees, honest arithmetic. That sign
//! flip is the product.
//!
//! # Why annualisation never rounds a period count
//!
//! `SECONDS_PER_YEAR / interval` is exact for 8h (1095), 4h (2190) and 1h
//! (8760), and inexact for anything else. A venue on a 7h cycle has
//! 1251.428571… settlements per year. Rounding that to 1251 before multiplying
//! costs 4.29 bps on a 10.95% APR — a third of the edge we are hunting, thrown
//! away by a cast. Every function here scales by the exact rational
//! `rate * SECONDS_PER_YEAR / interval_secs`, with exactly one rounding at the
//! end, and [`arb_types::FundingInterval::periods_per_year_exact`] returns
//! `None` precisely so that nobody is tempted.
//!
//! # Sign
//!
//! This module never writes a minus sign next to the word funding. The sign law
//! lives in [`PositionSide::funding_receipt_sign`] and reaches here only through
//! [`FundingSnapshot::receipt_rate`] and [`CarryLeg::receipt_per_period`].

use crate::error::MathError;
use arb_num::{Bps, Money, Rate, RoundMode};
use arb_types::{
    DataQuality, FundingInterval, FundingSnapshot, Millis, PositionSide, VenueKey,
    SECONDS_PER_YEAR,
};

/// The rounding direction that makes a signed cash flow worse for us.
///
/// A funding number may be a receipt or a cost and the caller frequently does
/// not know which until the sign law has been applied. Once it has, the honest
/// direction is mechanical: shrink receipts, grow costs. Both branches happen to
/// equal "round toward negative infinity", but naming them by their economic
/// meaning is what stops the next reader from flipping one.
///
/// `pub(crate)` so [`crate::persistence`] applies the identical rule to a realised
/// rate. Two copies of this decision would be two places to get it backwards.
#[inline]
pub(crate) const fn against_us(value: Rate) -> RoundMode {
    if value.is_negative() {
        // Money leaving. Round the magnitude up so we never understate a cost.
        RoundMode::COST
    } else {
        // Money arriving. Round the magnitude down so we never overstate it.
        RoundMode::REVENUE
    }
}

/// Scale a per-settlement rate to a yearly rate, exactly.
///
/// `rate * SECONDS_PER_YEAR / interval_secs`, with a single rounding at the end.
/// See the module docs for why this is not `rate * periods_per_year`.
///
/// The `mode` is explicit because a bare funding rate carries no information
/// about whether it is good or bad for *us* — that depends on which side we
/// hold, which this function is not told. Once the side is known, prefer
/// [`CarryLeg::annual_receipt`], which applies the sign law and then picks the
/// conservative direction itself.
///
/// # The assumption in the output
///
/// The result is what the year would return **if this rate persisted for the
/// whole year**. It will not. Nothing in this crate can tell you how wrong that
/// is; [`periods_to_breakeven`] tells you how long you would have to be right
/// for it to matter.
pub fn annualise(
    rate_per_period: Rate,
    interval: FundingInterval,
    mode: RoundMode,
) -> Result<Rate, MathError> {
    // Multiply first: exact, no rounding possible. Then one division.
    let scaled = rate_per_period.checked_mul_int(SECONDS_PER_YEAR)?;
    let secs = i64::from(interval.secs());
    scaled.checked_div_int(secs, mode).map_err(MathError::from)
}

/// The signed cash amount one settlement moves, for a holder of `side`.
///
/// Positive is money arriving. The sign law is applied through
/// [`PositionSide::funding_receipt_sign`]; the rounding direction is then chosen
/// from the resulting sign, so a receipt is never overstated and a cost is never
/// understated.
///
/// A negative `notional` is rejected rather than absolute-valued: direction is
/// carried by `side`, and a negative size reaching here means a sign error
/// upstream at exactly the place where a receipt turns into a cost.
pub fn funding_payment(
    notional: Money,
    rate: Rate,
    side: PositionSide,
) -> Result<Money, MathError> {
    if notional.is_negative() {
        return Err(MathError::NegativeNotional);
    }
    let signed = rate.checked_mul_int(side.funding_receipt_sign())?;
    notional
        .mul_rate(signed, against_us(signed))
        .map_err(MathError::from)
}

/// How many settlements it takes for funding to pay off the round trip.
///
/// This is the number the rest of the market does not show. `receipt_per_period`
/// is the per-settlement receipt as a rate on notional, after the sign law;
/// `round_trip_cost` is every fee and spread cost of getting in and out, in
/// basis points of one leg's notional — see [`crate::fees::RoundTrip`] and
/// [`crate::execution::slippage_bps`].
///
/// Rounded **up**. Twenty-one settlements of 1 bp do not cover 22 bp of costs;
/// reporting 21 would claim breakeven one settlement before it happens, which at
/// 8h intervals is eight hours of being wrong about whether the trade is
/// profitable at all.
///
/// Returns [`MathError::NoBreakeven`] when the per-period receipt is zero or
/// runs against us — there is no such count, and returning a large number would
/// invite a caller to treat it as merely a long wait.
pub fn periods_to_breakeven(
    receipt_per_period: Rate,
    round_trip_cost: Bps,
) -> Result<u64, MathError> {
    if !receipt_per_period.is_positive() {
        return Err(MathError::NoBreakeven);
    }
    if !round_trip_cost.is_positive() {
        // Nothing to pay off. Not an error: a zero-cost round trip is a
        // legitimate, if unrealistic, input.
        return Ok(0);
    }
    // `Bps` and `Rate` share their raw integer by construction (decision 002),
    // so this comparison is exact and unit-safe once the receipt is narrowed.
    let receipt = receipt_per_period.to_bps()?;
    let cost = i128::from(round_trip_cost.raw());
    let per = i128::from(receipt.raw());
    // Ceiling division on positives. `per >= 1` here, so no overflow: cost is at
    // most i64::MAX and the numerator fits i128 with room to spare.
    let periods = cost
        .checked_add(per)
        .and_then(|n| n.checked_sub(1))
        .map(|n| n / per)
        .ok_or(MathError::Arithmetic(arb_num::NumError::Overflow))?;
    u64::try_from(periods).map_err(|_| MathError::Arithmetic(arb_num::NumError::Overflow))
}

/// Annual return net of a round trip amortised over the horizon you will hold.
///
/// `annual_receipt` is the annualised funding receipt from [`annualise`].
/// `round_trip_cost` is the one-off cost of entering and exiting, in basis
/// points of one leg's notional. `horizon` is how long the position is intended
/// to be held.
///
/// The cost is annualised too — `cost * SECONDS_PER_YEAR / horizon` — because a
/// fixed cost spread over a shorter holding period is a larger annual drag. That
/// single division is the difference between this crate and a spread display:
///
/// - held one year, a 22 bp round trip costs 22 bp of annual return;
/// - held 73 days, it costs 110 bp;
/// - held one 8h settlement, it costs 2409 bp, and a "+10.95% APR" row is
///   revealed as −229.95%.
///
/// The cost side rounds against us; the receipt is taken as given, so the result
/// is a lower bound on the honest figure.
///
/// A negative `round_trip_cost` is refused. It is out of domain — a round trip is
/// money out — and the refusal is not pedantry about types: this function
/// *subtracts* the annualised cost, so a negative cost is annualised into a large
/// negative number and then subtracted, i.e. **added** to the return. A −22 bp
/// cost over one 8h settlement turns a +10.95% row into +251.85%. The failure
/// therefore runs in the flattering direction and lands the corrupted row at the
/// top of the ranking, which is the one place an error is certain to be acted on.
/// [`fee_on_notional`](crate::fee_on_notional), [`FeeRates::new`](crate::FeeRates::new)
/// and [`SlippageBudget::new`](crate::SlippageBudget::new) all refuse a negative
/// rate already; this was the hole in that fence.
pub fn net_apr_over_horizon(
    annual_receipt: Rate,
    round_trip_cost: Bps,
    horizon: Millis,
) -> Result<Rate, MathError> {
    if round_trip_cost.is_negative() {
        return Err(MathError::NegativeFeeRate);
    }
    let horizon_secs = horizon.millis() / 1000;
    if horizon_secs <= 0 {
        return Err(MathError::NonPositiveHorizon);
    }
    let cost = Rate::from_bps(round_trip_cost);
    // Cost is money leaving, so its magnitude rounds up regardless of the
    // receipt's sign. Exact rational, one rounding, same as `annualise`.
    let annual_cost = cost
        .checked_mul_int(SECONDS_PER_YEAR)?
        .checked_div_int(horizon_secs, RoundMode::COST)?;
    annual_receipt
        .checked_sub(annual_cost)
        .map_err(MathError::from)
}

/// One leg of a carry position, with everything needed to price its funding.
///
/// Carries its own [`DataQuality`] so that combining two legs cannot lose a
/// warning: a spread computed from one measured and one invented input is
/// invented, and that has to survive all the way to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarryLeg {
    /// Which instrument at which venue.
    pub vkey: VenueKey,
    /// Which side of that instrument we hold.
    pub side: PositionSide,
    /// The venue's published rate, sign convention unchanged. Positive means
    /// longs pay shorts. Do not pre-negate it.
    pub rate: Rate,
    /// How often this instrument settles.
    pub interval: FundingInterval,
    /// Provenance and warnings for the rate and interval above.
    pub quality: DataQuality,
}

impl CarryLeg {
    /// Build a leg from an observed snapshot and the side we intend to hold.
    pub fn from_snapshot(snap: &FundingSnapshot, side: PositionSide) -> Self {
        Self {
            vkey: snap.vkey,
            side,
            rate: snap.rate,
            interval: snap.interval,
            quality: snap.quality,
        }
    }

    /// The signed per-settlement receipt rate. Positive is money arriving.
    ///
    /// The only sanctioned path from a published rate to a signed one in this
    /// crate.
    pub fn receipt_per_period(&self) -> Result<Rate, MathError> {
        self.rate
            .checked_mul_int(self.side.funding_receipt_sign())
            .map_err(MathError::from)
    }

    /// The signed receipt rate annualised, rounded against us.
    ///
    /// Unlike bare [`annualise`], the side is known here, so the conservative
    /// direction is known too and is not left to the caller.
    pub fn annual_receipt(&self) -> Result<Rate, MathError> {
        let per = self.receipt_per_period()?;
        annualise(per, self.interval, against_us(per))
    }

    /// The signed cash this leg's next settlement moves on `notional`.
    pub fn payment(&self, notional: Money) -> Result<Money, MathError> {
        funding_payment(notional, self.rate, self.side)
    }

    /// Settlements this leg alone needs to pay off `round_trip_cost`.
    pub fn periods_to_breakeven(&self, round_trip_cost: Bps) -> Result<u64, MathError> {
        periods_to_breakeven(self.receipt_per_period()?, round_trip_cost)
    }

    /// Wall-clock time those settlements take.
    ///
    /// The count alone hides the interval: 22 settlements is seven days on an 8h
    /// venue and under a day on an hourly one.
    pub fn time_to_breakeven(&self, round_trip_cost: Bps) -> Result<Millis, MathError> {
        let periods = self.periods_to_breakeven(round_trip_cost)?;
        let secs = i64::try_from(periods)
            .ok()
            .and_then(|p| p.checked_mul(i64::from(self.interval.secs())))
            .ok_or(MathError::Arithmetic(arb_num::NumError::Overflow))?;
        Ok(Millis::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_num::NumError;
    use arb_types::{Asset, InstrumentKey, MarketKind, Provenance, QualityFlags, Venue};

    fn rate(s: &str) -> Rate {
        Rate::parse(s).expect("test rate parses")
    }

    fn bps(s: &str) -> Bps {
        Bps::parse_bps(s).expect("test bps parses")
    }

    fn money(s: &str) -> Money {
        Money::parse(s).expect("test money parses")
    }

    fn hours(n: i64) -> Millis {
        Millis::from_secs(n * 3600)
    }

    fn days(n: i64) -> Millis {
        Millis::from_secs(n * 86_400)
    }

    fn leg(rate_s: &str, interval: FundingInterval, side: PositionSide) -> CarryLeg {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT")
            .unwrap_or(InstrumentKey::new(
                Asset::default(),
                Asset::default(),
                MarketKind::LinearPerp,
                Asset::default(),
            ));
        CarryLeg {
            vkey: VenueKey::new(Venue::Binance, key),
            side,
            rate: rate(rate_s),
            interval,
            quality: DataQuality::SYNTHESIZED,
        }
    }

    // -- annualisation ------------------------------------------------------

    #[test]
    fn annualise_eight_hourly_is_exact() {
        // 31_536_000 / 28_800 = 1095 settlements per year, exactly.
        // 0.0001 * 1095 = 0.1095 -> 10.95% APR.
        let got = annualise(rate("0.0001"), FundingInterval::EIGHT_HOURS, RoundMode::REVENUE)
            .expect("ok");
        assert_eq!(got, rate("0.1095"));
    }

    #[test]
    fn annualise_is_exact_so_the_mode_cannot_matter() {
        // When the division has no remainder, the two directions must agree.
        // If they ever disagree here, `checked_div_int` is rounding something it
        // should not be.
        let up = annualise(rate("0.0001"), FundingInterval::EIGHT_HOURS, RoundMode::COST)
            .expect("ok");
        let down = annualise(rate("0.0001"), FundingInterval::EIGHT_HOURS, RoundMode::REVENUE)
            .expect("ok");
        assert_eq!(up, down);
        assert_eq!(up, rate("0.1095"));
    }

    #[test]
    fn the_interval_error_is_a_factor_of_eight() {
        // 31_536_000 / 3_600 = 8760. 0.0001 * 8760 = 0.876 -> 87.6% APR.
        // The same rate on a 1h venue is EIGHT TIMES the 8h answer. Assuming
        // 8h for an hourly venue understates APR 8x; the reverse overstates it
        // 8x and puts the row at the top of the ranking. This is why
        // FUNDING_INTERVAL_ASSUMED exists as a flag.
        let hourly =
            annualise(rate("0.0001"), FundingInterval::ONE_HOUR, RoundMode::REVENUE).expect("ok");
        assert_eq!(hourly, rate("0.876"));

        let eight_hourly =
            annualise(rate("0.0001"), FundingInterval::EIGHT_HOURS, RoundMode::REVENUE)
                .expect("ok");
        assert_eq!(
            hourly,
            eight_hourly.checked_mul_int(8).expect("ok"),
            "1h must annualise to exactly 8x the 8h figure"
        );
    }

    #[test]
    fn four_and_two_hourly_match_hand_arithmetic() {
        // 4h: 31_536_000 / 14_400 = 2190. 0.0001 * 2190 = 0.219.
        // 2h: 31_536_000 /  7_200 = 4380. 0.0001 * 4380 = 0.438.
        assert_eq!(
            annualise(rate("0.0001"), FundingInterval::FOUR_HOURS, RoundMode::REVENUE)
                .expect("ok"),
            rate("0.219")
        );
        assert_eq!(
            annualise(rate("0.0001"), FundingInterval::TWO_HOURS, RoundMode::REVENUE).expect("ok"),
            rate("0.438")
        );
    }

    #[test]
    fn non_dividing_interval_keeps_the_precision_a_period_count_would_lose() {
        // A 7h venue: 31_536_000 / 25_200 = 1251.428571428571...
        //
        // Exact rational, computed by hand on the raw integers:
        //   raw(0.0001) = 100_000_000
        //   100_000_000 * 31_536_000 = 3_153_600_000_000_000
        //   3_153_600_000_000_000 / 25_200 = 125_142_857_142.857142...
        //   truncated -> 125_142_857_142 -> 0.125142857142
        let seven_h = FundingInterval::from_secs(7 * 3600).expect("ok");
        let exact = annualise(rate("0.0001"), seven_h, RoundMode::REVENUE).expect("ok");
        assert_eq!(exact, Rate::from_raw(125_142_857_142));

        // Rounding the period count first: 0.0001 * 1251 = 0.1251.
        let naive = rate("0.1251");
        let lost = exact.checked_sub(naive).expect("ok");
        // 125_142_857_142 - 125_100_000_000 = 42_857_142 raw = 0.000042857142
        // = 4.2857142 bps thrown away by a cast, on an edge of about 12 bps.
        assert_eq!(lost, Rate::from_raw(42_857_142));
        assert_eq!(lost.to_bps().expect("ok"), Bps::from_raw(42_857_142));

        // And the interval genuinely does not divide the year, which is why
        // `periods_per_year_exact` refuses to answer.
        assert_eq!(seven_h.periods_per_year_exact(), None);
    }

    #[test]
    fn annualising_a_negative_rate_keeps_the_sign() {
        assert_eq!(
            annualise(rate("-0.0001"), FundingInterval::EIGHT_HOURS, RoundMode::COST)
                .expect("ok"),
            rate("-0.1095")
        );
    }

    // -- settlements --------------------------------------------------------

    #[test]
    fn a_short_receives_when_the_rate_is_positive() {
        // 100_000 * 0.0001 = 10 USDT exactly. Positive rate, longs pay shorts.
        assert_eq!(
            funding_payment(money("100000"), rate("0.0001"), PositionSide::Short).expect("ok"),
            money("10")
        );
        assert_eq!(
            funding_payment(money("100000"), rate("0.0001"), PositionSide::Long).expect("ok"),
            money("-10")
        );
    }

    #[test]
    fn a_negative_rate_reverses_who_pays() {
        // The half of the sign law that a hand-written minus sign gets wrong.
        assert_eq!(
            funding_payment(money("100000"), rate("-0.0001"), PositionSide::Short).expect("ok"),
            money("-10")
        );
        assert_eq!(
            funding_payment(money("100000"), rate("-0.0001"), PositionSide::Long).expect("ok"),
            money("10")
        );
    }

    #[test]
    fn a_sub_ulp_receipt_is_dropped_and_a_sub_ulp_cost_is_charged() {
        // notional = 1e-12, rate = 1e-4, true product = 1e-16: a ten-thousandth
        // of one ulp. Identical magnitude on both sides, opposite treatment.
        //
        // Short (receipt): rounds down to zero. We do not book money we cannot
        // collect.
        assert_eq!(
            funding_payment(Money::EPSILON, rate("0.0001"), PositionSide::Short).expect("ok"),
            Money::ZERO
        );
        // Long (cost): rounds up in magnitude to one ulp. We do book money we
        // will owe.
        assert_eq!(
            funding_payment(Money::EPSILON, rate("0.0001"), PositionSide::Long).expect("ok"),
            Money::EPSILON.checked_neg().expect("ok")
        );
    }

    #[test]
    fn zero_rate_and_zero_notional_pay_nothing() {
        assert_eq!(
            funding_payment(money("100000"), Rate::ZERO, PositionSide::Short).expect("ok"),
            Money::ZERO
        );
        assert_eq!(
            funding_payment(Money::ZERO, rate("0.0001"), PositionSide::Short).expect("ok"),
            Money::ZERO
        );
    }

    #[test]
    fn negative_notional_is_refused_not_absolute_valued() {
        let neg = money("100000").checked_neg().expect("ok");
        assert_eq!(
            funding_payment(neg, rate("0.0001"), PositionSide::Short),
            Err(MathError::NegativeNotional)
        );
    }

    // -- breakeven ----------------------------------------------------------

    #[test]
    fn breakeven_against_a_four_leg_taker_round_trip() {
        // 1 bp per settlement against 22 bp of round trip (four taker legs at
        // 5.5 bp). 22 / 1 = 22 settlements exactly. At 8h that is 176 hours.
        assert_eq!(
            periods_to_breakeven(rate("0.0001"), bps("22")).expect("ok"),
            22
        );
    }

    #[test]
    fn breakeven_rounds_up_because_a_partial_settlement_pays_nothing() {
        // 1.5 bp per settlement, 22 bp cost. 22 / 1.5 = 14.666...
        //   14 settlements = 21.0 bp  < 22  -> still losing
        //   15 settlements = 22.5 bp >= 22  -> first period in profit
        assert_eq!(
            periods_to_breakeven(rate("0.00015"), bps("22")).expect("ok"),
            15
        );
    }

    #[test]
    fn an_exact_division_must_not_round_up() {
        // 2 bp per settlement, 22 bp cost. 11 * 2 = 22 exactly, so 11 is the
        // answer and 12 is an off-by-one that would cost eight hours of
        // opportunity on every row of the table.
        assert_eq!(
            periods_to_breakeven(rate("0.0002"), bps("22")).expect("ok"),
            11
        );
    }

    #[test]
    fn one_settlement_short_of_the_cost_still_takes_two() {
        // 12 bp per settlement against a 22 bp round trip: one settlement pays
        // 12 < 22, two pay 24 >= 22. This is our claimed edge measured against
        // our real costs, and it takes two settlements.
        assert_eq!(
            periods_to_breakeven(rate("0.0012"), bps("22")).expect("ok"),
            2
        );
    }

    #[test]
    fn no_breakeven_when_funding_runs_against_us_or_is_flat() {
        assert_eq!(
            periods_to_breakeven(rate("-0.0001"), bps("22")),
            Err(MathError::NoBreakeven)
        );
        assert_eq!(
            periods_to_breakeven(Rate::ZERO, bps("22")),
            Err(MathError::NoBreakeven)
        );
    }

    #[test]
    fn a_free_round_trip_breaks_even_immediately() {
        assert_eq!(periods_to_breakeven(rate("0.0001"), Bps::ZERO).expect("ok"), 0);
    }

    #[test]
    fn time_to_breakeven_exposes_the_interval() {
        // 22 settlements is a week on an 8h venue and under a day on an hourly
        // one. The count alone hides that; the duration does not.
        let eight = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Short);
        assert_eq!(eight.time_to_breakeven(bps("22")).expect("ok"), hours(176));

        // Hourly at the same per-settlement rate: 22 settlements = 22 hours.
        let hourly = leg("0.0001", FundingInterval::ONE_HOUR, PositionSide::Short);
        assert_eq!(hourly.time_to_breakeven(bps("22")).expect("ok"), hours(22));
    }

    // -- net APR over a real horizon ---------------------------------------

    #[test]
    fn over_a_full_year_the_round_trip_costs_its_face_value() {
        // 22 bp spread over 365 days is 22 bp of annual drag.
        // 0.1095 - 0.0022 = 0.1073 -> 10.73%.
        let got = net_apr_over_horizon(rate("0.1095"), bps("22"), days(365)).expect("ok");
        assert_eq!(got, rate("0.1073"));
    }

    #[test]
    fn over_seventy_three_days_the_round_trip_costs_five_times_as_much() {
        // 365 / 73 = 5 exactly. 22 bp * 5 = 110 bp = 0.011.
        // 0.1095 - 0.011 = 0.0985 -> 9.85%.
        let got = net_apr_over_horizon(rate("0.1095"), bps("22"), days(73)).expect("ok");
        assert_eq!(got, rate("0.0985"));
    }

    #[test]
    fn the_headline_number_is_a_catastrophic_loss_over_one_settlement() {
        // THE test of this module.
        //
        // A competitor displays this row as "+10.95% APR". Hold it for exactly
        // one 8h settlement and the truth is:
        //   round trip annualised = 0.0022 * (31_536_000 / 28_800)
        //                         = 0.0022 * 1095 = 2.409
        //   net = 0.1095 - 2.409 = -2.2995  ->  -229.95%
        //
        // Cross-check by cash flow: one settlement collects 1 bp against 22 bp
        // of costs, a 21 bp loss over 8h; 21 bp * 1095 = 22_995 bp = 229.95%.
        let got = net_apr_over_horizon(rate("0.1095"), bps("22"), hours(8)).expect("ok");
        assert_eq!(got, rate("-2.2995"));

        let by_cash_flow = rate("0.0021")
            .checked_mul_int(1095)
            .expect("ok")
            .checked_neg()
            .expect("ok");
        assert_eq!(got, by_cash_flow, "the two derivations must agree exactly");
    }

    #[test]
    fn net_apr_crosses_zero_exactly_at_the_breakeven_horizon() {
        // periods_to_breakeven says 22 settlements of 8h = 176 hours.
        // net_apr_over_horizon must therefore be exactly zero at 176 hours.
        //   0.0022 * 31_536_000 / 633_600 = 0.1095, and 0.1095 - 0.1095 = 0.
        // Two independent formulas agreeing to the ulp is the strongest check
        // available that neither has an off-by-one.
        let periods = periods_to_breakeven(rate("0.0001"), bps("22")).expect("ok");
        assert_eq!(periods, 22);
        let horizon = hours(176);
        assert_eq!(horizon.millis(), 633_600_000);

        let net = net_apr_over_horizon(rate("0.1095"), bps("22"), horizon).expect("ok");
        assert_eq!(net, Rate::ZERO);

        // One settlement earlier is still a loss, which is what "rounded up"
        // means: 168 hours = 21 settlements.
        let earlier = net_apr_over_horizon(rate("0.1095"), bps("22"), hours(168)).expect("ok");
        assert!(
            earlier.is_negative(),
            "21 settlements must not yet break even, got {earlier}"
        );
    }

    #[test]
    fn net_apr_improves_monotonically_with_a_longer_horizon() {
        // Invariant, not a fitted vector: amortising a fixed cost over more time
        // can only help. If this ever fails, the division is inverted.
        let mut previous: Option<Rate> = None;
        for h in [1_i64, 2, 8, 24, 176, 730, 4380, 8760] {
            let net = net_apr_over_horizon(rate("0.1095"), bps("22"), hours(h)).expect("ok");
            if let Some(prev) = previous {
                assert!(prev < net, "net APR fell from {prev} to {net} at {h}h");
            }
            previous = Some(net);
        }
    }

    #[test]
    fn the_cost_side_rounds_against_us() {
        // 7h horizon: 0.0022 * 31_536_000 / 25_200
        //   = 2_200_000_000 * 31_536_000 / 25_200
        //   = 69_379_200_000_000_000 / 25_200
        //   = 2_753_142_857_142.857142...
        // Rounded away from zero, the cost becomes 2_753_142_857_143, so the
        // reported net is one ulp WORSE than the true value. That direction is
        // the whole point.
        let net = net_apr_over_horizon(rate("0.1095"), bps("22"), hours(7)).expect("ok");
        let expected = rate("0.1095")
            .checked_sub(Rate::from_raw(2_753_142_857_143))
            .expect("ok");
        assert_eq!(net, expected);
    }

    #[test]
    fn a_zero_or_negative_horizon_is_refused() {
        assert_eq!(
            net_apr_over_horizon(rate("0.1095"), bps("22"), Millis::ZERO),
            Err(MathError::NonPositiveHorizon)
        );
        assert_eq!(
            net_apr_over_horizon(rate("0.1095"), bps("22"), hours(-1)),
            Err(MathError::NonPositiveHorizon)
        );
        // Sub-second horizons truncate to zero seconds and are refused rather
        // than dividing by zero.
        assert_eq!(
            net_apr_over_horizon(rate("0.1095"), bps("22"), Millis::from_millis(999)),
            Err(MathError::NonPositiveHorizon)
        );
    }

    #[test]
    fn a_negative_round_trip_cost_is_refused_before_it_becomes_a_return() {
        // Out of domain, and the direction of the failure is why it is checked
        // rather than tolerated. The cost is SUBTRACTED, so a negative one is
        // added:
        //   annual_cost = -0.0022 * 31_536_000 / 28_800 = -2.409
        //   net         = 0.1095 - (-2.409) = +2.5185, i.e. +251.85% APR
        // from a row whose true figure over one settlement is -229.95%. A sign
        // error one call upstream would be rendered as the best row on the board.
        assert_eq!(
            net_apr_over_horizon(rate("0.1095"), bps("-22"), hours(8)),
            Err(MathError::NegativeFeeRate)
        );
        // One ulp of Bps is enough to be refused; and zero is not negative, so a
        // genuinely free round trip still computes.
        assert_eq!(
            net_apr_over_horizon(rate("0.1095"), Bps::from_raw(-1), hours(8)),
            Err(MathError::NegativeFeeRate)
        );
        assert_eq!(
            net_apr_over_horizon(rate("0.1095"), Bps::ZERO, hours(8)).expect("ok"),
            rate("0.1095")
        );
    }

    // -- CarryLeg -----------------------------------------------------------

    #[test]
    fn a_short_leg_annualises_a_positive_rate_as_a_receipt() {
        let short = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Short);
        assert_eq!(short.receipt_per_period().expect("ok"), rate("0.0001"));
        assert_eq!(short.annual_receipt().expect("ok"), rate("0.1095"));
        assert_eq!(short.payment(money("100000")).expect("ok"), money("10"));
    }

    #[test]
    fn a_long_leg_annualises_the_same_rate_as_a_cost() {
        let long = leg("0.0001", FundingInterval::EIGHT_HOURS, PositionSide::Long);
        assert_eq!(long.receipt_per_period().expect("ok"), rate("-0.0001"));
        assert_eq!(long.annual_receipt().expect("ok"), rate("-0.1095"));
        assert_eq!(long.payment(money("100000")).expect("ok"), money("-10"));
        assert_eq!(
            long.periods_to_breakeven(bps("22")),
            Err(MathError::NoBreakeven)
        );
    }

    #[test]
    fn from_snapshot_takes_the_rate_and_quality_verbatim() {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT")
            .expect("key parses");
        let snap = FundingSnapshot {
            vkey: VenueKey::new(Venue::Bybit, key),
            rate: rate("0.0003"),
            interval: FundingInterval::EIGHT_HOURS,
            next_funding_at: None,
            mark: None,
            index: None,
            exchange_ts: arb_types::WallTs::from_millis(1_700_000_000_000),
            observed_at: arb_types::LocalTs::from_millis(1_700_000_000_040),
            quality: DataQuality::new(
                QualityFlags::FUNDING_INTERVAL_ASSUMED,
                Provenance::SynthesizedUnverified,
            ),
        };
        let l = CarryLeg::from_snapshot(&snap, PositionSide::Short);
        assert_eq!(l.rate, rate("0.0003"));
        assert_eq!(l.side, PositionSide::Short);
        // The interval was assumed, and that warning must survive into the leg
        // untouched — the leg is what the ranking sees.
        assert!(l
            .quality
            .flags
            .contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));
        assert_eq!(l.quality.provenance, Provenance::SynthesizedUnverified);
        // 0.0003 * 1095 = 0.3285 -> 32.85% APR.
        assert_eq!(l.annual_receipt().expect("ok"), rate("0.3285"));
    }

    #[test]
    fn a_leg_never_writes_its_own_minus_sign() {
        // Both sides of the same snapshot must be exact negations of each other.
        // If they are not, someone has applied the sign twice or not at all.
        for r in ["0.0001", "-0.0004", "0.000012", "0"] {
            let short = leg(r, FundingInterval::EIGHT_HOURS, PositionSide::Short);
            let long = leg(r, FundingInterval::EIGHT_HOURS, PositionSide::Long);
            assert_eq!(
                short.receipt_per_period().expect("ok"),
                long.receipt_per_period()
                    .expect("ok")
                    .checked_neg()
                    .expect("ok"),
                "sides disagree for rate {r}"
            );
        }
    }

    #[test]
    fn annualise_overflow_is_an_error_not_a_wrap() {
        // Rule 4: no panics. A rate near the representable maximum must return
        // Overflow rather than wrapping into a plausible small number.
        assert_eq!(
            annualise(Rate::MAX, FundingInterval::EIGHT_HOURS, RoundMode::REVENUE),
            Err(MathError::Arithmetic(NumError::Overflow))
        );
    }
}
