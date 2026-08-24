//! Liquidation: the price at which the position ends.
//!
//! # Four formulas, not one
//!
//! Linear and inverse contracts liquidate by different arithmetic, and the two
//! sides of each are not mirror images. Getting this wrong does not produce a
//! slightly optimistic number — it produces a number on the wrong side of the
//! entry, or a position that is closed at a price the operator was told was
//! safe.
//!
//! Write `r` for the margin as a fraction of the position (`1/leverage`) and `m`
//! for the maintenance margin rate. With `P` the entry price:
//!
//! | position       | liquidation price   | exists when |
//! |----------------|---------------------|-------------|
//! | linear long    | `P·(1−r)/(1−m)`     | `r < 1`     |
//! | linear short   | `P·(1+r)/(1+m)`     | always      |
//! | inverse long   | `P·(1+m)/(1+r)`     | always      |
//! | inverse short  | `P·(1−m)/(1−r)`     | `r < 1`     |
//!
//! Note that the inverse rows swap the roles of `r` and `m` and invert the
//! quotient. That is not a typo, and it has a consequence worth stating plainly:
//!
//! **A coin-margined long is liquidated sooner than a cash-margined long at the
//! same leverage.** At 4x with no maintenance margin, a linear long liquidates
//! 25% below entry and an inverse long liquidates 20% below entry. The collateral
//! of an inverse position is the asset whose price is falling, so it loses value
//! at exactly the moment it is needed. The tests in this module check both
//! numbers against hand arithmetic, because this asymmetry is the sort of thing
//! a scanner gets wrong once and then reports confidently forever.
//!
//! The mirror of that: a coin-margined short survives longer than a cash-margined
//! short, because its loss in coin terms is bounded as the price rises.
//!
//! # Two positions that cannot be liquidated by price alone
//!
//! [`liquidation_price`] returns `Option<Price>`, and `None` is a fact rather
//! than a failure. A linear long with margin at or above its notional cannot be
//! liquidated, because the price can only fall to zero and that loss is already
//! covered. An inverse short whose coin margin equals its base-denominated
//! exposure likewise cannot be, because its loss in coin terms converges as the
//! price rises. These two are mirror images, and both are legitimate states to
//! display. Collapsing them into an error, or into a liquidation price of zero,
//! would tell the operator something false about a real position.
//!
//! # What is assumed, and it is not small
//!
//! **Isolated margin, one-way mode, single position, no cross-collateral.** Cross
//! margin pools collateral across positions and gives a materially different and
//! usually later liquidation price; hedge mode holds both sides at once. Neither
//! is modelled here. A cross-margined account fed these numbers will be told it
//! is closer to liquidation than it is, which is the safe direction but is still
//! the wrong number. Tracked as `OPEN.md` A3.
//!
//! Also unmodelled: tiered maintenance margin, which rises with position size on
//! every venue in scope. A single `maintenance_rate` is only correct within one
//! tier. Feeding the tier for a smaller position than you hold gives a
//! liquidation price further away than the truth — the unsafe direction. Tracked
//! as `OPEN.md` A3 with A1 and A2.
//!
//! # Rounding
//!
//! Always toward the entry price, so the reported distance to liquidation is
//! never larger than the truth: a long's liquidation price rounds **up**, a
//! short's rounds **down**. [`distance_to_liquidation_bps`] then truncates, which
//! shrinks it once more. The operator is told the cliff is nearer than it is.

use crate::error::MathError;
use arb_num::{Bps, Money, Price, Rate, RoundMode};
use arb_types::{MarketKind, PositionSide};

/// Everything needed to locate the liquidation price of one position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiquidationInputs {
    /// Linear, inverse, or spot. Spot is rejected: see [`liquidation_price`].
    pub kind: MarketKind,
    /// Which side is held.
    pub side: PositionSide,
    /// Average entry price.
    pub entry: Price,
    /// Position notional **in the quote asset** at entry.
    ///
    /// For a linear contract this is `contracts * base_per_contract * entry`.
    /// For an inverse contract it is `contracts * quote_per_contract` and is
    /// independent of price. Use [`arb_types::ContractSpec::notional`] rather
    /// than computing it here.
    pub notional: Money,
    /// Margin posted, **in the settle asset**.
    ///
    /// That is the quote asset for a linear contract and the **base** asset for
    /// an inverse one. This field and [`LiquidationInputs::notional`] are
    /// therefore denominated in *different assets* for inverse contracts, which
    /// is exactly why the inverse formula is not the linear one.
    pub margin: Money,
    /// Maintenance margin rate, in `[0, 1)`. `0.005` is 0.5%.
    pub maintenance_rate: Rate,
}

impl LiquidationInputs {
    /// The rounding direction that moves the liquidation price toward entry.
    ///
    /// A long liquidates below entry, so rounding its price up brings the cliff
    /// nearer. A short liquidates above entry, so rounding down does. Both
    /// understate the distance, which is the only direction an operator can
    /// safely be wrong in.
    const fn mode(&self) -> RoundMode {
        match self.side {
            // Positive prices only, so away-from-zero is up.
            PositionSide::Long => RoundMode::COST,
            PositionSide::Short => RoundMode::REVENUE,
        }
    }

    /// Margin as a fraction of the position, the `r` of the module docs.
    ///
    /// Linear: `margin / notional`, both in quote.
    ///
    /// Inverse: `margin * entry / notional`. The margin is in the base asset, so
    /// it is converted to quote before the comparison. Skipping that conversion
    /// compares BTC against USD and yields a margin fraction wrong by the price
    /// itself.
    fn margin_fraction(&self) -> Result<Rate, MathError> {
        // Rounded down, so a position near the maintenance boundary is reported
        // as being on the dangerous side of it.
        let mode = RoundMode::REVENUE;
        match self.kind {
            MarketKind::Spot => Err(MathError::UnsupportedMarket(
                "spot has no liquidation price without a borrow model",
            )),
            MarketKind::LinearPerp => self
                .margin
                .div_money(self.notional, mode)
                .map_err(MathError::from),
            MarketKind::InversePerp => {
                // Base-denominated margin -> quote. `reinterpret_as_qty` is the
                // sanctioned crossing: the settle asset of an inverse contract
                // *is* the base asset, so this amount is a base quantity.
                let margin_quote = self
                    .margin
                    .reinterpret_as_qty()
                    .mul_price(self.entry, mode)?;
                margin_quote
                    .div_money(self.notional, mode)
                    .map_err(MathError::from)
            }
        }
    }

    /// Validate the inputs common to every market kind.
    fn check(&self) -> Result<(), MathError> {
        if !self.entry.is_positive() {
            return Err(MathError::NonPositivePrice);
        }
        if !self.notional.is_positive() {
            // A zero-notional position is not a position with infinite leverage;
            // it is a sizing bug upstream.
            return Err(MathError::ZeroSize);
        }
        if !self.margin.is_positive() {
            return Err(MathError::NonPositiveMargin);
        }
        if self.maintenance_rate.is_negative() || self.maintenance_rate >= Rate::ONE {
            // At exactly 1 the linear-long denominator is zero; above 1 it
            // changes sign and returns a plausible number on the wrong side of
            // the entry, which is worse than an error.
            return Err(MathError::MaintenanceRateOutOfRange);
        }
        Ok(())
    }

    /// Distance from entry to liquidation, in basis points.
    ///
    /// `None` when the position has no liquidation price. See
    /// [`distance_to_liquidation_bps`] for the rounding.
    pub fn distance_bps(&self) -> Result<Option<Bps>, MathError> {
        match liquidation_price(self)? {
            Some(liq) => distance_to_liquidation_bps(self.entry, liq).map(Some),
            None => Ok(None),
        }
    }
}

/// The price at which this position is liquidated, or `None` if it cannot be.
///
/// See the module docs for the four formulas, the two `None` cases, and the
/// isolated-margin assumption. Rejects spot with
/// [`MathError::UnsupportedMarket`]: a spot position is only liquidatable through
/// a borrow, and borrow cost is not modelled anywhere in this crate
/// (`OPEN.md` A1). Returning a confident number for spot would be worse than
/// refusing.
pub fn liquidation_price(inputs: &LiquidationInputs) -> Result<Option<Price>, MathError> {
    inputs.check()?;
    let r = inputs.margin_fraction()?;
    let m = inputs.maintenance_rate;

    // Uniform across all four cases: the margin fraction must strictly exceed
    // the maintenance rate, or the position is already below its requirement at
    // its own entry price and there is no future liquidation to report.
    if r <= m {
        return Err(MathError::AlreadyBelowMaintenance);
    }

    let mode = inputs.mode();
    // `k` is the multiple of the entry price at which liquidation occurs. Each
    // case is written to match the table in the module docs literally, so the
    // code can be checked against it line by line.
    let k = match (inputs.kind, inputs.side) {
        (MarketKind::Spot, _) => {
            return Err(MathError::UnsupportedMarket(
                "spot has no liquidation price without a borrow model",
            ))
        }

        // P*(1-r)/(1-m). Vanishes once margin covers the whole notional: the
        // price cannot fall below zero, so that loss is already funded.
        (MarketKind::LinearPerp, PositionSide::Long) => {
            if r >= Rate::ONE {
                return Ok(None);
            }
            let numer = Rate::ONE.checked_sub(r)?;
            let denom = Rate::ONE.checked_sub(m)?;
            numer.div_rate(denom, mode)?
        }

        // P*(1+r)/(1+m). Always exists: a short's loss is unbounded above.
        (MarketKind::LinearPerp, PositionSide::Short) => {
            let numer = Rate::ONE.checked_add(r)?;
            let denom = Rate::ONE.checked_add(m)?;
            numer.div_rate(denom, mode)?
        }

        // P*(1+m)/(1+r). Always exists, however large the margin: the collateral
        // is the falling asset, so more of it does not make the position safe,
        // only cheaper per contract.
        (MarketKind::InversePerp, PositionSide::Long) => {
            let numer = Rate::ONE.checked_add(m)?;
            let denom = Rate::ONE.checked_add(r)?;
            numer.div_rate(denom, mode)?
        }

        // P*(1-m)/(1-r). Vanishes once coin margin covers the base exposure: the
        // loss in coin terms converges as the price rises.
        (MarketKind::InversePerp, PositionSide::Short) => {
            if r >= Rate::ONE {
                return Ok(None);
            }
            let numer = Rate::ONE.checked_sub(m)?;
            let denom = Rate::ONE.checked_sub(r)?;
            numer.div_rate(denom, mode)?
        }
    };

    inputs
        .entry
        .mul_rate(k, mode)
        .map(Some)
        .map_err(MathError::from)
}

/// Distance from `entry` to `liquidation`, as a non-negative basis-point figure.
///
/// Truncated, so the reported distance is never larger than the truth. The sign
/// is dropped because the direction is already implied by the side, and a signed
/// distance invites a caller to compare a long's `-2500` against a short's
/// `+2500` and rank the long as more dangerous.
pub fn distance_to_liquidation_bps(entry: Price, liquidation: Price) -> Result<Bps, MathError> {
    if !entry.is_positive() {
        return Err(MathError::NonPositivePrice);
    }
    let magnitude = liquidation.checked_sub(entry)?.checked_abs()?;
    magnitude
        .div_price(entry, RoundMode::REVENUE)?
        .to_bps()
        .map_err(MathError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(s: &str) -> Price {
        Price::parse(s).expect("test price parses")
    }

    fn money(s: &str) -> Money {
        Money::parse(s).expect("test money parses")
    }

    fn rate(s: &str) -> Rate {
        Rate::parse(s).expect("test rate parses")
    }

    fn bps(s: &str) -> Bps {
        Bps::parse_bps(s).expect("test bps parses")
    }

    /// A position at entry 100 with notional 100, so the margin figure *is* the
    /// margin fraction as a percentage. Keeps the hand arithmetic checkable.
    fn pos(
        kind: MarketKind,
        side: PositionSide,
        margin: &str,
        maintenance: &str,
    ) -> LiquidationInputs {
        LiquidationInputs {
            kind,
            side,
            entry: price("100"),
            notional: money("100"),
            margin: money(margin),
            maintenance_rate: rate(maintenance),
        }
    }

    // -- linear -------------------------------------------------------------

    #[test]
    fn linear_long_at_ten_times_with_no_maintenance_margin() {
        // r = 10/100 = 0.1, m = 0.  p = 100 * (1-0.1)/(1-0) = 100 * 0.9 = 90.
        let p = pos(MarketKind::LinearPerp, PositionSide::Long, "10", "0");
        assert_eq!(liquidation_price(&p).expect("ok"), Some(price("90")));
        // |90 - 100| / 100 = 0.1 = 1000 bps.
        assert_eq!(p.distance_bps().expect("ok"), Some(bps("1000")));
    }

    #[test]
    fn linear_short_at_ten_times_with_no_maintenance_margin() {
        // r = 0.1, m = 0.  p = 100 * (1+0.1)/(1+0) = 110. Above entry.
        let p = pos(MarketKind::LinearPerp, PositionSide::Short, "10", "0");
        assert_eq!(liquidation_price(&p).expect("ok"), Some(price("110")));
        assert_eq!(p.distance_bps().expect("ok"), Some(bps("1000")));
    }

    #[test]
    fn linear_long_with_maintenance_margin_moves_the_cliff_closer() {
        // m = 0.005, and r chosen so the answer is exact:
        //   (1 - 0.1045) / (1 - 0.005) = 0.8955 / 0.995 = 0.9  (0.995*0.9=0.8955)
        //   p = 100 * 0.9 = 90.
        // The same 90 now costs 10.45 of margin instead of 10 — maintenance
        // margin is a real haircut, not a rounding detail.
        let p = pos(MarketKind::LinearPerp, PositionSide::Long, "10.45", "0.005");
        assert_eq!(liquidation_price(&p).expect("ok"), Some(price("90")));
    }

    #[test]
    fn linear_short_with_maintenance_margin() {
        // (1 + 0.1055) / (1 + 0.005) = 1.1055 / 1.005 = 1.1  (1.005*1.1=1.1055)
        // p = 110.
        let p = pos(MarketKind::LinearPerp, PositionSide::Short, "10.55", "0.005");
        assert_eq!(liquidation_price(&p).expect("ok"), Some(price("110")));
    }

    #[test]
    fn a_fully_cash_margined_linear_long_cannot_be_liquidated() {
        // r = 1: the price can only fall to zero, and that entire loss is
        // already covered by the margin. `None`, not zero, and not an error.
        let at = pos(MarketKind::LinearPerp, PositionSide::Long, "100", "0");
        assert_eq!(liquidation_price(&at).expect("ok"), None);
        assert_eq!(at.distance_bps().expect("ok"), None);

        let over = pos(MarketKind::LinearPerp, PositionSide::Long, "150", "0");
        assert_eq!(liquidation_price(&over).expect("ok"), None);
    }

    #[test]
    fn a_fully_margined_linear_short_still_has_a_cliff() {
        // A short's loss is unbounded above, so no amount of margin removes the
        // liquidation price. r = 1, m = 0: p = 100 * 2 / 1 = 200.
        let p = pos(MarketKind::LinearPerp, PositionSide::Short, "100", "0");
        assert_eq!(liquidation_price(&p).expect("ok"), Some(price("200")));
    }

    // -- inverse ------------------------------------------------------------

    #[test]
    fn an_inverse_long_liquidates_sooner_than_a_linear_long() {
        // THE asymmetry test.
        //
        // Both positions: entry 100, notional 100 quote, margin fraction 0.25
        // (4x), m = 0. For the inverse the margin is 0.25 of the BASE asset,
        // worth 0.25 * 100 = 25 quote, so r = 25/100 = 0.25 — the same fraction.
        //
        //   inverse long: p = 100 * (1+0)/(1+0.25) = 100/1.25 = 80
        //   linear  long: p = 100 * (1-0.25)/(1-0) = 100*0.75 = 75
        //
        // 80 is nearer to 100 than 75 is. The coin-margined long dies first,
        // because its collateral is the asset that is falling. A scanner that
        // uses the linear formula for a coin-margined position overstates the
        // safe distance by 500 bp here.
        let inverse = pos(MarketKind::InversePerp, PositionSide::Long, "0.25", "0");
        assert_eq!(liquidation_price(&inverse).expect("ok"), Some(price("80")));
        assert_eq!(inverse.distance_bps().expect("ok"), Some(bps("2000")));

        let linear = pos(MarketKind::LinearPerp, PositionSide::Long, "25", "0");
        assert_eq!(liquidation_price(&linear).expect("ok"), Some(price("75")));
        assert_eq!(linear.distance_bps().expect("ok"), Some(bps("2500")));
    }

    #[test]
    fn an_inverse_short_survives_longer_than_a_linear_short() {
        // The mirror. Margin fraction 0.5 (2x), m = 0.
        //   inverse short: p = 100 * (1-0)/(1-0.5) = 100/0.5 = 200
        //   linear  short: p = 100 * (1+0.5)/(1+0) = 150
        // The coin-margined short is safer, because its loss in coin terms
        // converges as the price rises.
        let inverse = pos(MarketKind::InversePerp, PositionSide::Short, "0.5", "0");
        assert_eq!(liquidation_price(&inverse).expect("ok"), Some(price("200")));
        assert_eq!(inverse.distance_bps().expect("ok"), Some(bps("10000")));

        let linear = pos(MarketKind::LinearPerp, PositionSide::Short, "50", "0");
        assert_eq!(liquidation_price(&linear).expect("ok"), Some(price("150")));
        assert_eq!(linear.distance_bps().expect("ok"), Some(bps("5000")));
    }

    #[test]
    fn a_fully_coin_margined_inverse_short_cannot_be_liquidated() {
        // r = 1. The mirror of the linear long case.
        let p = pos(MarketKind::InversePerp, PositionSide::Short, "1", "0");
        assert_eq!(liquidation_price(&p).expect("ok"), None);
    }

    #[test]
    fn an_inverse_long_always_has_a_cliff_however_much_margin_it_posts() {
        // r = 10 * 100/100 = 10.  p = 100 * 1/11 = 9.0909...
        //   1/11 raw = 1e24 / 11e12 = 90_909_090_909.09..., rounded UP (long)
        //   -> 90_909_090_910, and 100 * that = 9_090_909_091_000 exactly.
        // Ten times the position in collateral and the position is still
        // liquidatable. This is the fact that most distinguishes coin margin.
        let p = pos(MarketKind::InversePerp, PositionSide::Long, "10", "0");
        assert_eq!(
            liquidation_price(&p).expect("ok"),
            Some(Price::from_raw(9_090_909_091_000))
        );
    }

    #[test]
    fn the_inverse_margin_fraction_converts_the_base_margin_to_quote() {
        // If the conversion were skipped, 0.25 BTC would be compared against a
        // 100 USD notional as if it were 0.25 USD, giving r = 0.0025 — below any
        // plausible maintenance rate, so the position would be reported as
        // already liquidated. Entry 68_000, notional 68_000 quote, margin 0.25
        // base = 17_000 quote, r = 0.25.
        let p = LiquidationInputs {
            kind: MarketKind::InversePerp,
            side: PositionSide::Long,
            entry: price("68000"),
            notional: money("68000"),
            margin: money("0.25"),
            maintenance_rate: Rate::ZERO,
        };
        // p = 68_000 * 1/1.25 = 68_000 * 0.8 = 54_400.
        assert_eq!(liquidation_price(&p).expect("ok"), Some(price("54400")));
        assert_eq!(p.distance_bps().expect("ok"), Some(bps("2000")));
    }

    // -- rounding -----------------------------------------------------------

    #[test]
    fn a_long_rounds_its_liquidation_price_up_toward_entry() {
        // r = 0.1, m = 0.003.
        //   (1-0.1)/(1-0.003) = 0.9/0.997
        //     = 9e23 / 9.97e11 = 902_708_124_373.11935... raw
        //   rounded UP -> 902_708_124_374 -> 0.902708124374
        //   p = 100 * 0.902708124374 = 90.2708124374
        // The true value is 90.27081243731193..., so the reported cliff is
        // HIGHER, i.e. nearer to the entry of 100. The operator is told the
        // danger is closer than it is.
        let p = pos(MarketKind::LinearPerp, PositionSide::Long, "10", "0.003");
        let liq = liquidation_price(&p).expect("ok").expect("some");
        assert_eq!(liq, Price::from_raw(90_270_812_437_400));

        // |90.2708124374 - 100| / 100 = 0.097291875626 -> 972.91875626 bps.
        // The true distance is 972.918756268807 bps, so this is understated.
        assert_eq!(
            p.distance_bps().expect("ok"),
            Some(Bps::from_raw(97_291_875_626))
        );
    }

    #[test]
    fn a_short_rounds_its_liquidation_price_down_toward_entry() {
        // r = 0.1, m = 0.003.
        //   (1+0.1)/(1+0.003) = 1.1/1.003 = 1100/1003
        //     = 1_096_709_870_388.83349... raw, truncated -> 1_096_709_870_388
        //   p = 100 * 1.096709870388 = 109.6709870388
        // True value 109.670987038883..., so the reported cliff is LOWER, again
        // nearer to entry.
        let p = pos(MarketKind::LinearPerp, PositionSide::Short, "10", "0.003");
        assert_eq!(
            liquidation_price(&p).expect("ok"),
            Some(Price::from_raw(109_670_987_038_800))
        );
    }

    #[test]
    fn distance_is_never_larger_than_the_truth() {
        // Invariant rather than a fitted vector. For a sweep of margins and
        // maintenance rates, the truncated distance must never exceed the
        // distance implied by the unrounded ratio, which we bound by adding one
        // ulp back in.
        for margin in ["5", "10", "20", "33.3333", "50"] {
            for maint in ["0", "0.001", "0.004", "0.0125"] {
                for side in [PositionSide::Long, PositionSide::Short] {
                    let p = pos(MarketKind::LinearPerp, side, margin, maint);
                    let liq = liquidation_price(&p).expect("ok").expect("some");
                    match side {
                        PositionSide::Long => assert!(
                            liq < p.entry,
                            "long liquidation {liq} must be below entry {}",
                            p.entry
                        ),
                        PositionSide::Short => assert!(
                            liq > p.entry,
                            "short liquidation {liq} must be above entry {}",
                            p.entry
                        ),
                    }
                    let d = p.distance_bps().expect("ok").expect("some");
                    assert!(d.is_positive(), "distance must be positive, got {d}");
                }
            }
        }
    }

    #[test]
    fn more_margin_always_moves_the_cliff_further_away() {
        // Monotonicity. If the formula were inverted for a side, this fails.
        let mut previous: Option<Bps> = None;
        for margin in ["5", "10", "20", "40", "80"] {
            let p = pos(MarketKind::LinearPerp, PositionSide::Long, margin, "0.005");
            let d = p.distance_bps().expect("ok").expect("some");
            if let Some(prev) = previous {
                assert!(prev < d, "distance fell from {prev} to {d} at margin {margin}");
            }
            previous = Some(d);
        }
    }

    #[test]
    fn more_maintenance_margin_always_moves_the_cliff_closer() {
        let mut previous: Option<Bps> = None;
        for maint in ["0", "0.005", "0.01", "0.02", "0.04"] {
            let p = pos(MarketKind::LinearPerp, PositionSide::Long, "50", maint);
            let d = p.distance_bps().expect("ok").expect("some");
            if let Some(prev) = previous {
                assert!(prev > d, "distance rose from {prev} to {d} at maintenance {maint}");
            }
            previous = Some(d);
        }
    }

    // -- refusals -----------------------------------------------------------

    #[test]
    fn a_position_already_below_maintenance_has_no_future_liquidation() {
        // r = 0.004, m = 0.005. Liquidation is not ahead of us; it is overdue.
        let p = pos(MarketKind::LinearPerp, PositionSide::Long, "0.4", "0.005");
        assert_eq!(
            liquidation_price(&p),
            Err(MathError::AlreadyBelowMaintenance)
        );
        // The boundary is inclusive: exactly at the maintenance rate there is no
        // remaining buffer.
        let at = pos(MarketKind::LinearPerp, PositionSide::Long, "0.5", "0.005");
        assert_eq!(
            liquidation_price(&at),
            Err(MathError::AlreadyBelowMaintenance)
        );
        // One ulp above it does have a liquidation price — but one ulp of *what*
        // matters, and getting this wrong is how the boundary gets tested
        // sloppily. The comparison is against the margin *fraction*, which is
        // `margin / notional` rounded DOWN. On a notional of 100 it therefore
        // takes 100 ulps of margin to move the fraction by one ulp:
        let one_fraction_ulp = LiquidationInputs {
            margin: money("0.5").checked_add(Money::from_raw(100)).expect("ok"),
            ..at
        };
        assert!(liquidation_price(&one_fraction_ulp).expect("ok").is_some());

        // A single ulp of margin is discarded by that rounding and the position
        // is still reported as below maintenance. That is the safe direction —
        // a position a hundredth of an ulp above its requirement is told it is
        // in trouble — but it is a real quantisation of the boundary and it is
        // recorded here rather than discovered later.
        let one_margin_ulp = LiquidationInputs {
            margin: money("0.5").checked_add(Money::EPSILON).expect("ok"),
            ..at
        };
        assert_eq!(
            liquidation_price(&one_margin_ulp),
            Err(MathError::AlreadyBelowMaintenance)
        );
    }

    #[test]
    fn spot_is_refused_rather_than_guessed() {
        // A spot position is only liquidatable through a borrow, and borrow cost
        // is not modelled anywhere in this crate. A confident number here would
        // be worse than a refusal.
        let p = pos(MarketKind::Spot, PositionSide::Long, "10", "0");
        assert!(matches!(
            liquidation_price(&p),
            Err(MathError::UnsupportedMarket(_))
        ));
    }

    #[test]
    fn a_maintenance_rate_outside_the_unit_interval_is_refused() {
        // At exactly 1 the linear-long denominator is zero. Above 1 it changes
        // sign and produces a liquidation price on the wrong side of the entry,
        // which looks entirely plausible on a screen.
        let one = pos(MarketKind::LinearPerp, PositionSide::Long, "10", "1");
        assert_eq!(
            liquidation_price(&one),
            Err(MathError::MaintenanceRateOutOfRange)
        );
        let over = pos(MarketKind::LinearPerp, PositionSide::Long, "10", "1.5");
        assert_eq!(
            liquidation_price(&over),
            Err(MathError::MaintenanceRateOutOfRange)
        );
        let under = LiquidationInputs {
            maintenance_rate: rate("-0.001"),
            ..pos(MarketKind::LinearPerp, PositionSide::Long, "10", "0")
        };
        assert_eq!(
            liquidation_price(&under),
            Err(MathError::MaintenanceRateOutOfRange)
        );
    }

    #[test]
    fn degenerate_inputs_are_refused_individually() {
        let base = pos(MarketKind::LinearPerp, PositionSide::Long, "10", "0");

        let zero_entry = LiquidationInputs {
            entry: Price::ZERO,
            ..base
        };
        assert_eq!(liquidation_price(&zero_entry), Err(MathError::NonPositivePrice));

        let zero_notional = LiquidationInputs {
            notional: Money::ZERO,
            ..base
        };
        assert_eq!(liquidation_price(&zero_notional), Err(MathError::ZeroSize));

        let zero_margin = LiquidationInputs {
            margin: Money::ZERO,
            ..base
        };
        assert_eq!(
            liquidation_price(&zero_margin),
            Err(MathError::NonPositiveMargin)
        );

        let neg_margin = LiquidationInputs {
            margin: money("10").checked_neg().expect("ok"),
            ..base
        };
        assert_eq!(
            liquidation_price(&neg_margin),
            Err(MathError::NonPositiveMargin)
        );
    }

    #[test]
    fn distance_from_a_broken_entry_price_is_refused() {
        assert_eq!(
            distance_to_liquidation_bps(Price::ZERO, price("90")),
            Err(MathError::NonPositivePrice)
        );
    }

    #[test]
    fn distance_is_unsigned_so_the_two_sides_are_comparable() {
        // A long 10% below and a short 10% above are equally exposed, and the
        // table must be able to sort them against each other.
        assert_eq!(
            distance_to_liquidation_bps(price("100"), price("90")).expect("ok"),
            distance_to_liquidation_bps(price("100"), price("110")).expect("ok")
        );
        assert_eq!(
            distance_to_liquidation_bps(price("100"), price("90")).expect("ok"),
            bps("1000")
        );
    }
}
