//! Sizing: how many contracts, how much margin, and what the venue will accept.
//!
//! Three small functions that sit between a target notional and an order the
//! venue will not reject. All three round **down**, so the position is never
//! larger than asked for and the margin is never smaller than required.
//!
//! # Why a contract count is not a quantity of coin
//!
//! Every venue in scope quotes size in contracts, and a contract is worth
//! `contract_size` of something — but *which* something flips with the contract
//! type. A linear BTC perp contract on OKX is 0.01 BTC; an inverse BTC perp
//! contract is 100 USD. Divide a 10,000 USD target by the wrong one and the
//! answer is off by the price of Bitcoin, roughly 68,000x. That is not a
//! rounding error, it is a different trade.
//!
//! [`contracts_for_notional`] therefore dispatches on [`ContractSize`] rather
//! than taking a single multiplier, and the inverse branch **does not use the
//! price at all**. There is a test asserting that the same target yields the same
//! contract count at 100 and at 68,000, because that is the only cheap way to
//! prove the price has not crept back into a formula it does not belong in.
//!
//! # Rounding, and where it is not free
//!
//! [`round_down_to_lot`] and [`contracts_for_notional`] floor: the resulting
//! order is at or below the target notional, never above. Overshooting a target
//! notional is worse than undershooting, because it silently changes the margin
//! requirement and the liquidation distance the operator was shown.
//!
//! [`margin_required`] rounds **up**, twice. It computes `1/leverage` and then
//! multiplies, and each step rounds away from zero, so the figure is at or above
//! the true requirement. The two roundings compound to at most about `2e-12`
//! relative — one ulp from the reciprocal, magnified by the notional, plus one
//! from the product. On a 1,000,000 USD notional that is a fifth of a cent. It is
//! documented rather than hidden (`OPEN.md` C3); removing it needs a
//! `Money::div_rate` in `arb-num`, which is frozen while this wave is in flight.

use crate::error::MathError;
use arb_num::{Money, Price, Qty, Rate, RoundMode};
use arb_types::{ContractSize, ContractSpec};

/// Floor `qty` to a whole multiple of `lot`.
///
/// Venues reject sizes that are not lot multiples, and they do not round for you.
/// This is the last step before an order size is quoted, and it always reduces:
/// a 1.2345 request against a 0.001 lot becomes 1.234, not 1.235.
///
/// Refuses a negative `qty` with [`MathError::NegativeSize`]. Sizes here are
/// magnitudes — direction lives in [`arb_types::PositionSide`] — and "round down"
/// is ambiguous for a negative number between floor and truncate, which disagree
/// by a whole lot.
///
/// A zero result is returned as zero rather than as an error: the caller knows
/// whether an empty order is a problem in its context, and
/// [`contracts_for_notional`] does treat it as one.
pub fn round_down_to_lot(qty: Qty, lot: Qty) -> Result<Qty, MathError> {
    if !lot.is_positive() {
        // `ContractSpec::validate` already rejects this, so reaching it means a
        // spec was built without validation. Refuse rather than divide by zero.
        return Err(MathError::ZeroSize);
    }
    if qty.is_negative() {
        return Err(MathError::NegativeSize);
    }
    // Both raws are non-negative here, so integer division is floor and the
    // product cannot exceed `qty.raw()`. Done on raws deliberately: `div_size`
    // would round the *ratio* to twelve places first, and a lot count with more
    // than twelve significant digits would then be off by one lot.
    let lots = qty.raw() / lot.raw();
    lots.checked_mul(lot.raw())
        .map(Qty::from_raw)
        .ok_or(MathError::Arithmetic(arb_num::NumError::Overflow))
}

/// Initial margin needed to hold `notional` at `leverage`.
///
/// `notional / leverage`, rounded up so the figure is never below what the venue
/// will demand. See the module docs for the size of the two-rounding error.
///
/// This is initial margin only. It is not the amount that keeps the position
/// alive: that is maintenance margin, and the price at which it binds is
/// [`crate::liquidation::liquidation_price`]. A position funded at exactly this
/// number is, by construction, one tick from trouble.
pub fn margin_required(notional: Money, leverage: Rate) -> Result<Money, MathError> {
    if notional.is_negative() {
        return Err(MathError::NegativeNotional);
    }
    if !leverage.is_positive() {
        // Zero leverage is not "no position", it is a division by zero.
        return Err(MathError::NonPositiveLeverage);
    }
    let inverse = leverage.recip(RoundMode::COST)?;
    notional
        .mul_rate(inverse, RoundMode::COST)
        .map_err(MathError::from)
}

/// How many contracts come closest to `target` notional without exceeding it.
///
/// Dispatches on [`ContractSize`], which is the only thing that determines the
/// formula:
///
/// - [`ContractSize::BasePerContract`] (linear perps and spot): the notional is
///   price-dependent, so `contracts = target / price / base_per_contract`.
/// - [`ContractSize::QuotePerContract`] (inverse perps): the notional is fixed in
///   quote per contract, so `contracts = target / quote_per_contract` and
///   **`price` is not used**. Passing a wrong price to an inverse instrument
///   changes nothing, by design.
///
/// The result is floored to `spec.lot_size`, and both divisions round down, so
/// the realised notional is at or below `target`.
///
/// # Not checked
///
/// `spec.min_notional` is deliberately **not** enforced. This function answers
/// "how many contracts fit in this much money", and a venue minimum is a reason
/// to drop the opportunity from the ranking, not a reason to fail the
/// arithmetic — the caller needs the size in order to explain *why* the row was
/// dropped. `arb-scan` must check it. Recorded in `OPEN.md`.
///
/// Returns [`MathError::ZeroSize`] when the target does not reach one lot, rather
/// than a silent zero that a caller would multiply back into a notional of
/// nothing and rank as a free trade.
pub fn contracts_for_notional(
    target: Money,
    price: Price,
    spec: &ContractSpec,
) -> Result<Qty, MathError> {
    if target.is_negative() {
        return Err(MathError::NegativeNotional);
    }
    if target.is_zero() {
        return Err(MathError::ZeroSize);
    }

    // Round down at every step: the realised notional must not exceed `target`.
    let mode = RoundMode::REVENUE;

    let raw_contracts = match spec.size {
        ContractSize::BasePerContract(base_per) => {
            if !price.is_positive() {
                return Err(MathError::NonPositivePrice);
            }
            if !base_per.is_positive() {
                return Err(MathError::ZeroSize);
            }
            // quote -> base -> contracts. `div_size` and not `div_qty`: dividing
            // an amount of base by base-per-contract yields a count, whereas
            // `div_qty` divides two amounts of the same thing and yields a
            // dimensionless ratio. See `decisions/014`.
            let base_units = target.div_price(price, mode)?;
            base_units.div_size(base_per, mode)?
        }
        ContractSize::QuotePerContract(quote_per) => {
            if !quote_per.is_positive() {
                return Err(MathError::ZeroSize);
            }
            // No price. An inverse contract's notional is fixed in quote, so the
            // count is a pure division of quote by quote-per-contract. The
            // reinterpretations cross Money into Qty because a *count* of
            // contracts is a quantity, not an amount of currency.
            target
                .reinterpret_as_qty()
                .div_size(quote_per.reinterpret_as_qty(), mode)?
        }
    };

    let contracts = round_down_to_lot(raw_contracts, spec.lot_size)?;
    if contracts.is_zero() {
        return Err(MathError::ZeroSize);
    }
    Ok(contracts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{Asset, MarketKind};

    fn price(s: &str) -> Price {
        Price::parse(s).expect("test price parses")
    }

    fn qty(s: &str) -> Qty {
        Qty::parse(s).expect("test qty parses")
    }

    fn money(s: &str) -> Money {
        Money::parse(s).expect("test money parses")
    }

    fn rate(s: &str) -> Rate {
        Rate::parse(s).expect("test rate parses")
    }

    fn asset(s: &str) -> Asset {
        Asset::new(s).expect("test asset parses")
    }

    /// A linear perp whose contract is `base_per` of the base asset.
    fn linear(base_per: &str, lot: &str) -> ContractSpec {
        ContractSpec {
            kind: MarketKind::LinearPerp,
            size: ContractSize::BasePerContract(qty(base_per)),
            settle: asset("USDT"),
            tick_size: price("0.1"),
            lot_size: qty(lot),
            min_notional: None,
        }
    }

    /// An inverse perp whose contract is `quote_per` of the quote asset.
    fn inverse(quote_per: &str, lot: &str) -> ContractSpec {
        ContractSpec {
            kind: MarketKind::InversePerp,
            size: ContractSize::QuotePerContract(money(quote_per)),
            settle: asset("BTC"),
            tick_size: price("0.1"),
            lot_size: qty(lot),
            min_notional: None,
        }
    }

    // -- lots ---------------------------------------------------------------

    #[test]
    fn a_size_is_floored_to_the_lot() {
        // 1.2345 against a 0.001 lot: 1234 whole lots, remainder 0.0005
        // discarded. Not 1.235 — a venue rejects the order rather than rounding
        // it for us, and rounding up would exceed the target notional.
        assert_eq!(
            round_down_to_lot(qty("1.2345"), qty("0.001")).expect("ok"),
            qty("1.234")
        );
    }

    #[test]
    fn an_exact_multiple_is_left_alone() {
        assert_eq!(
            round_down_to_lot(qty("1.234"), qty("0.001")).expect("ok"),
            qty("1.234")
        );
        assert_eq!(round_down_to_lot(qty("7"), qty("1")).expect("ok"), qty("7"));
    }

    #[test]
    fn a_size_below_one_lot_floors_to_zero_rather_than_erroring() {
        // Zero is the honest answer to "how many whole lots fit in 0.0009 when a
        // lot is 0.001". Whether that is a problem is the caller's judgement;
        // `contracts_for_notional` decides it is.
        assert_eq!(
            round_down_to_lot(qty("0.0009"), qty("0.001")).expect("ok"),
            Qty::ZERO
        );
        assert_eq!(round_down_to_lot(Qty::ZERO, qty("0.001")).expect("ok"), Qty::ZERO);
    }

    #[test]
    fn flooring_is_done_on_raw_lots_so_large_counts_are_exact() {
        // 1e12 lots of one ulp each. Rounding the *ratio* to twelve decimal
        // places first would lose the low digits and shift the answer by whole
        // lots; dividing the raws does not.
        let one = qty("1");
        assert_eq!(round_down_to_lot(one, Qty::EPSILON).expect("ok"), one);
        let awkward = Qty::from_raw(999_999_999_999);
        assert_eq!(round_down_to_lot(awkward, Qty::EPSILON).expect("ok"), awkward);
    }

    #[test]
    fn a_lot_larger_than_the_size_gives_nothing() {
        assert_eq!(round_down_to_lot(qty("0.5"), qty("1")).expect("ok"), Qty::ZERO);
    }

    #[test]
    fn a_non_positive_lot_is_refused_rather_than_dividing_by_zero() {
        assert_eq!(
            round_down_to_lot(qty("1"), Qty::ZERO),
            Err(MathError::ZeroSize)
        );
        let neg = qty("0.001").checked_neg().expect("ok");
        assert_eq!(round_down_to_lot(qty("1"), neg), Err(MathError::ZeroSize));
    }

    #[test]
    fn a_negative_size_is_refused_rather_than_interpreted() {
        // Truncation would give -1.234 and floor would give -1.235: a whole lot
        // apart, with no way to tell from the call site which was meant.
        let neg = qty("1.2345").checked_neg().expect("ok");
        assert_eq!(
            round_down_to_lot(neg, qty("0.001")),
            Err(MathError::NegativeSize)
        );
    }

    // -- margin -------------------------------------------------------------

    #[test]
    fn margin_at_ten_times_is_exact() {
        // 1/10 = 0.1 exactly, 10000 * 0.1 = 1000 exactly. Neither rounding bites.
        assert_eq!(
            margin_required(money("10000"), rate("10")).expect("ok"),
            money("1000")
        );
    }

    #[test]
    fn margin_at_one_times_is_the_whole_notional() {
        assert_eq!(
            margin_required(money("10000"), Rate::ONE).expect("ok"),
            money("10000")
        );
    }

    #[test]
    fn margin_at_three_times_rounds_up_by_the_documented_amount() {
        // 1/3 raw = 1e24/3e12 = 333_333_333_333.33..., rounded UP (COST) to
        //   333_333_333_334, i.e. 0.333333333334.
        // 10000 * that = 333_333_333_334 * 1e4 = 3_333_333_333_340_000 raw
        //   = 3333.33333334.
        // The true value is 3333.33333333333..., so we ask for 6.7e-9 more than
        // needed. That is the whole of the two-rounding error at this scale, and
        // it is on the safe side.
        assert_eq!(
            margin_required(money("10000"), rate("3")).expect("ok"),
            Money::from_raw(3_333_333_333_340_000)
        );
    }

    #[test]
    fn margin_never_falls_below_the_true_requirement() {
        // Invariant, not a fitted vector: margin * leverage >= notional, for any
        // leverage. If either rounding went the other way this fails.
        for lev in ["1", "2", "3", "7", "10", "12.5", "20", "75", "125"] {
            for notional in ["1", "100", "10000", "68122.5", "1000000"] {
                let n = money(notional);
                let l = rate(lev);
                let m = margin_required(n, l).expect("ok");
                let recovered = m.mul_rate(l, RoundMode::REVENUE).expect("ok");
                assert!(
                    recovered >= n,
                    "margin {m} at {lev}x covers only {recovered} of {n}"
                );
            }
        }
    }

    #[test]
    fn a_sub_ulp_margin_is_rounded_up_to_one_ulp_not_down_to_nothing() {
        // One ulp of notional at 125x is 8e-15, far below representable. It must
        // become one ulp of margin, because zero margin on a real position is a
        // number that cannot be true.
        assert_eq!(
            margin_required(Money::EPSILON, rate("125")).expect("ok"),
            Money::EPSILON
        );
    }

    #[test]
    fn zero_notional_needs_zero_margin() {
        assert_eq!(
            margin_required(Money::ZERO, rate("10")).expect("ok"),
            Money::ZERO
        );
    }

    #[test]
    fn degenerate_margin_inputs_are_refused() {
        assert_eq!(
            margin_required(money("10000"), Rate::ZERO),
            Err(MathError::NonPositiveLeverage)
        );
        let neg_lev = rate("10").checked_neg().expect("ok");
        assert_eq!(
            margin_required(money("10000"), neg_lev),
            Err(MathError::NonPositiveLeverage)
        );
        let neg_notional = money("10000").checked_neg().expect("ok");
        assert_eq!(
            margin_required(neg_notional, rate("10")),
            Err(MathError::NegativeNotional)
        );
    }

    // -- contract counts ----------------------------------------------------

    #[test]
    fn a_linear_contract_of_one_base_unit() {
        // 10000 USDT at 100 = 100 BTC of exposure; 1 BTC per contract = 100.
        assert_eq!(
            contracts_for_notional(money("10000"), price("100"), &linear("1", "0.001"))
                .expect("ok"),
            qty("100")
        );
    }

    #[test]
    fn a_linear_contract_of_a_hundredth_of_a_base_unit() {
        // Same 100 BTC of exposure, 0.01 BTC per contract = 10000 contracts.
        // The contract count is 100x the previous case for an identical position,
        // which is the entire reason `contract_size` cannot be assumed.
        assert_eq!(
            contracts_for_notional(money("10000"), price("100"), &linear("0.01", "1"))
                .expect("ok"),
            qty("10000")
        );
    }

    #[test]
    fn an_inverse_contract_count_does_not_depend_on_the_price() {
        // THE 68,000x guard test.
        //
        // An inverse contract is worth a fixed 10 USD, so 10000 USD is 1000
        // contracts at any price whatsoever. If the price leaked into this
        // formula, the count at 68,000 would be 680x the count at 100 and the
        // position would be off by the price of Bitcoin.
        let spec = inverse("10", "1");
        let at_100 = contracts_for_notional(money("10000"), price("100"), &spec).expect("ok");
        let at_btc = contracts_for_notional(money("10000"), price("68000"), &spec).expect("ok");
        assert_eq!(at_100, qty("1000"));
        assert_eq!(at_btc, qty("1000"));
        assert_eq!(at_100, at_btc);
    }

    #[test]
    fn an_inverse_contract_of_a_hundred_quote_units() {
        // OKX's recalled BTC-USD swap: ctVal = 100 USD.
        // 10000 / 100 = 100 contracts.
        assert_eq!(
            contracts_for_notional(money("10000"), price("68122.5"), &inverse("100", "1"))
                .expect("ok"),
            qty("100")
        );
    }

    #[test]
    fn using_the_linear_formula_on_an_inverse_spec_would_be_wrong_by_the_price() {
        // Records the size of the mistake this dispatch exists to prevent.
        // Inverse, ctVal = 100 USD, target 10000, price 68122.5:
        //   correct: 10000 / 100                = 100 contracts
        //   wrong:   10000 / 68122.5 / 100      = 0.001468... contracts
        // A factor of 68122.5. The wrong answer would also floor to zero against
        // any sane lot size and be silently dropped, which is how it survives
        // unnoticed until it does not.
        let correct =
            contracts_for_notional(money("10000"), price("68122.5"), &inverse("100", "1"))
                .expect("ok");
        assert_eq!(correct, qty("100"));

        let as_if_linear =
            contracts_for_notional(money("10000"), price("68122.5"), &linear("100", "0.000001"));
        // 10000/68122.5 = 0.146794... base, /100 = 0.00146794... contracts.
        assert_eq!(as_if_linear.expect("ok"), qty("0.001467"));
    }

    #[test]
    fn an_inexact_division_floors_and_then_floors_to_the_lot() {
        // 10000 / 3 = 3333.333333333333... base units, truncated at twelve
        // places to 3333.333333333333, then floored to a 0.001 lot = 3333.333.
        assert_eq!(
            contracts_for_notional(money("10000"), price("3"), &linear("1", "0.001")).expect("ok"),
            qty("3333.333")
        );
    }

    #[test]
    fn the_realised_notional_never_exceeds_the_target() {
        // The invariant that matters. Round-trips the contract count back through
        // `ContractSpec::notional`, rounding UP, and still requires it to be at
        // or below the target. Covers both contract types and awkward prices.
        let specs = [
            linear("1", "0.001"),
            linear("0.01", "1"),
            linear("0.0001", "1"),
            inverse("10", "1"),
            inverse("100", "1"),
        ];
        for spec in &specs {
            for p in ["3", "100", "1234.5", "68122.5"] {
                for t in ["1000", "10000", "12345.6789", "999999.999999"] {
                    let target = money(t);
                    let px = price(p);
                    let contracts = match contracts_for_notional(target, px, spec) {
                        Ok(c) => c,
                        // Below one lot for this spec and price; nothing to check.
                        Err(MathError::ZeroSize) => continue,
                        Err(e) => panic!("unexpected error {e} for {p}/{t}"),
                    };
                    let realised = spec
                        .notional(contracts, px, RoundMode::COST)
                        .expect("notional");
                    assert!(
                        realised <= target,
                        "{contracts} contracts realise {realised}, over target {target} \
                         at price {px}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_target_below_one_lot_is_an_error_not_a_silent_zero() {
        // 0.0005 USDT at 100 is 0.000005 BTC, well under a 0.001 lot. Returning
        // zero here would let a caller compute a notional of nothing, divide by
        // it, and rank the result.
        assert_eq!(
            contracts_for_notional(money("0.0005"), price("100"), &linear("1", "0.001")),
            Err(MathError::ZeroSize)
        );
        assert_eq!(
            contracts_for_notional(money("5"), price("100"), &inverse("10", "1")),
            Err(MathError::ZeroSize)
        );
    }

    #[test]
    fn degenerate_sizing_inputs_are_refused() {
        let spec = linear("1", "0.001");
        assert_eq!(
            contracts_for_notional(Money::ZERO, price("100"), &spec),
            Err(MathError::ZeroSize)
        );
        let neg = money("10000").checked_neg().expect("ok");
        assert_eq!(
            contracts_for_notional(neg, price("100"), &spec),
            Err(MathError::NegativeNotional)
        );
        assert_eq!(
            contracts_for_notional(money("10000"), Price::ZERO, &spec),
            Err(MathError::NonPositivePrice)
        );
    }

    #[test]
    fn a_broken_price_is_only_an_error_where_the_price_is_used() {
        // Deliberate asymmetry, stated as a test so it cannot be "fixed" by
        // adding a uniform price check. An inverse count does not consult the
        // price, so a zero price is not a reason to fail — and pretending
        // otherwise would hide that the two formulas differ.
        assert_eq!(
            contracts_for_notional(money("10000"), Price::ZERO, &linear("1", "0.001")),
            Err(MathError::NonPositivePrice)
        );
        assert_eq!(
            contracts_for_notional(money("10000"), Price::ZERO, &inverse("10", "1")).expect("ok"),
            qty("1000")
        );
    }

    #[test]
    fn a_spot_spec_sizes_like_a_linear_one() {
        // Spot carries `BasePerContract(1)`, so the count is the base quantity
        // itself. Dispatch is on `ContractSize`, not `MarketKind`, precisely so
        // this needs no separate branch.
        let spot = ContractSpec::spot(asset("USDT"), price("0.01"), qty("0.00001"));
        assert_eq!(
            contracts_for_notional(money("10000"), price("68122.5"), &spot).expect("ok"),
            // 10000 / 68122.5 = 0.146794... -> floored to a 0.00001 lot.
            qty("0.14679")
        );
    }

    #[test]
    fn min_notional_is_deliberately_not_enforced_here() {
        // Documented behaviour, tested so a later "helpful" check does not slip
        // in silently. The caller needs the size in order to explain why the row
        // was dropped; failing the arithmetic destroys that explanation.
        // `arb-scan` owns the minimum.
        let spec = ContractSpec {
            min_notional: Some(money("5000")),
            ..linear("1", "0.001")
        };
        let contracts =
            contracts_for_notional(money("100"), price("100"), &spec).expect("no minimum check");
        assert_eq!(contracts, qty("1"));
        let realised = spec.notional(contracts, price("100"), RoundMode::COST).expect("ok");
        assert!(realised < money("5000"), "the caller can see it is too small");
    }
}
