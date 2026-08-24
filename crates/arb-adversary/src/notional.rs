//! Linear versus inverse. For BTC the two notionals differ by the price of
//! Bitcoin, roughly 68,000x. That is not a rounding error, it is a different
//! trade.

use arb_math::{contracts_for_notional, distance_to_liquidation_bps, liquidation_price,
               margin_required, round_down_to_lot, LiquidationInputs};
use arb_num::{Money, Price, Qty, RoundMode};
use arb_types::{ContractSize, ContractSpec, MarketKind, PositionSide};

use crate::support::{asset, bps, inverse_spec, linear_spec, money, price, qty, rate};

#[test]
fn one_contract_at_one_unit_differs_by_the_whole_price_between_kinds() {
    // Both specs say "one per contract". For the linear one that one is BASE,
    // so a contract is worth the price. For the inverse one it is QUOTE, so a
    // contract is worth one dollar, whatever the price.
    let lin = linear_spec("1", "1");
    let inv = inverse_spec("1", "1");
    let p = price("68000");
    let lin_notional = lin.notional(qty("1"), p, RoundMode::COST).expect("ok");
    let inv_notional = inv.notional(qty("1"), p, RoundMode::COST).expect("ok");
    assert_eq!(lin_notional, money("68000"));
    assert_eq!(inv_notional, money("1"));
    // The exact factor, stated so it cannot drift: it is the price itself.
    assert_eq!(lin_notional.raw(), inv_notional.raw() * 68_000);
}

#[test]
fn an_inverse_notional_does_not_depend_on_the_price_at_all() {
    // OKX BTC-USD-SWAP: 100 USD per contract. 100 contracts is 10_000 USD at
    // any price. If a price ever creeps into this formula the number moves by a
    // factor of tens of thousands and still looks plausible.
    let inv = inverse_spec("100", "1");
    let at_one = inv.notional(qty("100"), price("1"), RoundMode::COST).expect("ok");
    let at_btc = inv
        .notional(qty("100"), price("68000"), RoundMode::COST)
        .expect("ok");
    assert_eq!(at_one, money("10000"));
    assert_eq!(at_btc, money("10000"));
    assert_eq!(at_one, at_btc);
}

#[test]
fn a_linear_notional_is_contracts_times_size_times_price() {
    // OKX BTC-USDT-SWAP: 0.01 BTC per contract. 100 contracts = 1 BTC.
    // 1 BTC at 68_000 = 68_000 USDT.
    let lin = linear_spec("0.01", "1");
    assert_eq!(
        lin.notional(qty("100"), price("68000"), RoundMode::COST)
            .expect("ok"),
        money("68000")
    );
    // And the base exposure is price-independent for a linear contract.
    assert_eq!(
        lin.base_qty(qty("100"), price("68000"), RoundMode::REVENUE)
            .expect("ok"),
        qty("1")
    );
    assert_eq!(
        lin.base_qty(qty("100"), price("1"), RoundMode::REVENUE)
            .expect("ok"),
        qty("1")
    );
}

#[test]
fn an_inverse_base_exposure_is_quote_over_price() {
    // 100 contracts x 100 USD = 10_000 USD of exposure. At 68_000 that is
    // 10_000 / 68_000 = 0.147058823529411764... BTC.
    // raw = 10_000e12 * 1e12 / 68_000e12 = 1e28/6.8e16 = 147_058_823_529.411...
    // REVENUE truncates, so the base exposure is understated, not overstated.
    let inv = inverse_spec("100", "1");
    let got = inv
        .base_qty(qty("100"), price("68000"), RoundMode::REVENUE)
        .expect("ok");
    assert_eq!(got.raw(), 147_058_823_529);
    // The same exposure at half the price is twice the coin.
    let cheap = inv
        .base_qty(qty("100"), price("34000"), RoundMode::REVENUE)
        .expect("ok");
    assert_eq!(cheap.raw(), 294_117_647_058);
}

#[test]
fn contract_sizing_for_an_inverse_target_ignores_the_price() {
    // 10_000 USD target, 100 USD per contract -> 100 contracts. The price is
    // passed and must change nothing. A formula that divided by the price would
    // return 0.00147 contracts here and round to zero.
    let inv = inverse_spec("100", "1");
    let a = contracts_for_notional(money("10000"), price("1"), &inv).expect("ok");
    let b = contracts_for_notional(money("10000"), price("68000"), &inv).expect("ok");
    assert_eq!(a, qty("100"));
    assert_eq!(b, qty("100"));
}

#[test]
fn contract_sizing_for_a_linear_target_uses_the_price_once() {
    // 68_000 USDT at 68_000, 0.01 BTC per contract:
    // 68_000 / 68_000 = 1 BTC, / 0.01 = 100 contracts.
    let lin = linear_spec("0.01", "1");
    assert_eq!(
        contracts_for_notional(money("68000"), price("68000"), &lin).expect("ok"),
        qty("100")
    );
    // Halve the price and the same money buys twice the contracts.
    assert_eq!(
        contracts_for_notional(money("68000"), price("34000"), &lin).expect("ok"),
        qty("200")
    );
}

#[test]
fn contract_sizing_floors_so_the_realised_notional_never_exceeds_the_target() {
    // Target 1000 USDT at 68_000 with 0.01 BTC contracts and a 1-contract lot.
    // 1000 / 68_000 = 0.014705882352 BTC (truncated), / 0.01 = 1.4705882352
    // contracts, floored to the lot -> 1 contract.
    // 1 contract is 0.01 BTC = 680 USDT <= 1000. Two would be 1360 > 1000.
    let lin = linear_spec("0.01", "1");
    let n = contracts_for_notional(money("1000"), price("68000"), &lin).expect("ok");
    assert_eq!(n, qty("1"));
    let realised = lin
        .notional(n, price("68000"), RoundMode::COST)
        .expect("ok");
    assert_eq!(realised, money("680"));
    assert!(realised <= money("1000"));
}

#[test]
fn a_target_below_one_lot_is_an_error_not_a_silent_zero() {
    // A zero size multiplied back into a notional is a notional of nothing,
    // which ranks as a free trade.
    let lin = linear_spec("0.01", "1");
    assert!(contracts_for_notional(money("1"), price("68000"), &lin).is_err());
    assert!(contracts_for_notional(Money::ZERO, price("68000"), &lin).is_err());
    assert!(contracts_for_notional(money("-1000"), price("68000"), &lin).is_err());
}

#[test]
fn a_spec_cannot_pair_a_kind_with_the_wrong_size_denomination() {
    // This is the type-level defence against the 68_000x error. A linear spec
    // holding QuotePerContract, or an inverse holding BasePerContract, would
    // make every notional on the screen wrong by the price.
    assert!(ContractSpec::new(
        MarketKind::LinearPerp,
        ContractSize::QuotePerContract(money("100")),
        asset("USDT"),
        price("0.1"),
        qty("1"),
        None,
    )
    .is_err());
    assert!(ContractSpec::new(
        MarketKind::InversePerp,
        ContractSize::BasePerContract(qty("0.01")),
        asset("BTC"),
        price("0.1"),
        qty("1"),
        None,
    )
    .is_err());
    // Spot must be base-denominated too.
    assert!(ContractSpec::new(
        MarketKind::Spot,
        ContractSize::QuotePerContract(money("1")),
        asset("USDT"),
        price("0.01"),
        qty("0.0001"),
        None,
    )
    .is_err());
}

// ---------------------------------------------------------------------------
// The two holes around the 68,000x defence: a venue minimum nobody checks, and
// a spec that never went through the sanctioned constructor.
// ---------------------------------------------------------------------------

#[test]
fn a_venue_minimum_is_not_enforced_here_so_it_must_be_enforced_by_the_ranker() {
    // Binance USD-M BTCUSDT carries a MIN_NOTIONAL filter (the fixture recalls
    // it as 5 USDT; unverified). Set it deliberately high — 5_000 USDT — and ask
    // for a 1_000 USDT position.
    //
    // Hand derivation, all divisions truncating:
    //   1_000 / 68_000            = 0.014705882352 BTC   (0.0147058823529... trunc)
    //   0.014705882352 / 0.01     = 1.4705882352 contracts
    //   floor to a 1-contract lot = 1 contract
    //   realised notional         = 1 * 0.01 * 68_000 = 680 USDT
    //   680 < 5_000, so the venue would REJECT this order at submit time.
    //
    // `contracts_for_notional` returns it anyway, and that is documented,
    // deliberate behaviour (sizing.rs "# Not checked", OPEN.md): the caller needs
    // the size in order to explain why the row was dropped. The consequence is
    // that the minimum is checked NOWHERE until someone writes the check, and a
    // row that cannot be executed at all is displayed with a full edge. Pinned so
    // that the obligation is visible in a test and not only in prose.
    let lin = ContractSpec::new(
        MarketKind::LinearPerp,
        ContractSize::BasePerContract(qty("0.01")),
        asset("USDT"),
        price("0.1"),
        qty("1"),
        Some(money("5000")),
    )
    .expect("a linear spec with a minimum is valid");

    let n = contracts_for_notional(money("1000"), price("68000"), &lin).expect("ok");
    assert_eq!(n, qty("1"));
    let realised = lin.notional(n, price("68000"), RoundMode::COST).expect("ok");
    assert_eq!(realised, money("680"));

    // The spec carries the number, so the check is possible; nothing calls it.
    assert_eq!(lin.min_notional, Some(money("5000")));
    assert!(
        realised < money("5000"),
        "the size this function returns is below the venue minimum it was given"
    );
    // And the same call on a spec with no minimum is indistinguishable from the
    // outside: same size, same notional, no error either way. That is exactly why
    // a caller cannot infer the check was done.
    let no_min = linear_spec("0.01", "1");
    assert_eq!(
        contracts_for_notional(money("1000"), price("68000"), &no_min).expect("ok"),
        n
    );
}

#[test]
fn a_spec_built_by_struct_literal_reintroduces_the_whole_error() {
    // `ContractSpec::new` refuses a linear market denominated in quote (proved
    // above), but every field is `pub`, so a struct literal skips the check.
    // arb-types documents this and offers `validate()` as the remedy; nothing in
    // arb-math calls it, because `notional` and `contracts_for_notional` dispatch
    // on `size` alone and never look at `kind`.
    //
    // The mis-parse this models is the realistic one: a venue whose contract is
    // 1 BTC read as "1 unit of quote per contract".
    //   correct (base:1)  : 1 contract * 1 BTC * 68_000 = 68_000 USDT
    //   mis-parsed (quote:1): 1 contract * 1 USD         =      1 USDT
    // Factor: 68_000, the price itself. Both numbers look plausible in a
    // debugger; only one of them is a position.
    let honest = linear_spec("1", "1");
    let bypassed = ContractSpec {
        kind: MarketKind::LinearPerp,
        size: ContractSize::QuotePerContract(money("1")),
        settle: asset("USDT"),
        tick_size: price("0.1"),
        lot_size: qty("1"),
        min_notional: None,
    };

    // The sanctioned constructor would have refused this exact combination.
    assert!(bypassed.validate().is_err());
    assert!(honest.validate().is_ok());

    let right = honest.notional(qty("1"), price("68000"), RoundMode::COST).expect("ok");
    let wrong = bypassed.notional(qty("1"), price("68000"), RoundMode::COST).expect("ok");
    assert_eq!(right, money("68000"));
    assert_eq!(wrong, money("1"));
    assert_eq!(right.raw(), wrong.raw() * 68_000);

    // And the sizing function is wrong by the same factor in the other
    // direction: 68_000 USDT of target buys 1 contract honestly and 68_000
    // contracts through the bypassed spec.
    assert_eq!(
        contracts_for_notional(money("68000"), price("68000"), &honest).expect("ok"),
        qty("1")
    );
    assert_eq!(
        contracts_for_notional(money("68000"), price("68000"), &bypassed).expect("ok"),
        qty("68000")
    );
}

#[test]
fn lot_rounding_always_reduces_and_refuses_a_negative_size() {
    // 1.2345 against a 0.001 lot is 1.234, not 1.235.
    assert_eq!(round_down_to_lot(qty("1.2345"), qty("0.001")).expect("ok"), qty("1.234"));
    // Already on a lot boundary: unchanged.
    assert_eq!(round_down_to_lot(qty("2"), qty("0.5")).expect("ok"), qty("2"));
    // Direction is carried by PositionSide, so a negative magnitude is a bug.
    assert!(round_down_to_lot(qty("-1.5"), qty("0.001")).is_err());
    assert!(round_down_to_lot(qty("1.5"), Qty::ZERO).is_err());
}

#[test]
fn margin_is_never_understated() {
    // 1_000_000 at 3x. 1/3 = 0.333333333333_333..., which must round UP to
    // ...334, and then 1_000_000 * 0.333333333334 = 333_333.333334 rounds up
    // again. The true requirement is 333_333.333333_333..., so the reported
    // figure is above it. Two roundings, both against us: documented in
    // sizing.rs and OPEN.md C3.
    let got = margin_required(money("1000000"), rate("3")).expect("ok");
    assert_eq!(got.raw(), 333_333_333_334_000_000);
    assert!(got > money("333333.333333333333"));
    // Leverage of zero is a division by zero, not "no position".
    assert!(margin_required(money("1000000"), rate("0")).is_err());
    assert!(margin_required(money("1000000"), rate("-3")).is_err());
    assert!(margin_required(money("-1000000"), rate("3")).is_err());
}

/// Entry 100, notional 100 in QUOTE, so the margin number IS the margin
/// fraction as a percentage in both the linear and the inverse case.
fn pos(kind: MarketKind, side: PositionSide, margin: &str, maint: &str) -> LiquidationInputs {
    LiquidationInputs {
        kind,
        side,
        entry: price("100"),
        notional: money("100"),
        margin: money(margin),
        maintenance_rate: rate(maint),
    }
}

#[test]
fn a_coin_margined_long_liquidates_sooner_than_a_cash_margined_one() {
    // 4x leverage, r = 0.25, m = 0.
    // Linear long:  P(1-r)/(1-m) = 100 * 0.75 / 1    = 75  -> 2500 bps away.
    // Inverse long: P(1+m)/(1+r) = 100 * 1    / 1.25 = 80  -> 2000 bps away.
    // The inverse collateral is the falling asset, so it loses value exactly
    // when it is needed. Reporting 25% for a coin-margined long is a promise of
    // five percent of headroom that does not exist.
    let lin = pos(MarketKind::LinearPerp, PositionSide::Long, "25", "0");
    let inv = pos(MarketKind::InversePerp, PositionSide::Long, "0.25", "0");
    assert_eq!(liquidation_price(&lin).expect("ok"), Some(price("75")));
    assert_eq!(liquidation_price(&inv).expect("ok"), Some(price("80")));
    assert_eq!(lin.distance_bps().expect("ok"), Some(bps("2500")));
    assert_eq!(inv.distance_bps().expect("ok"), Some(bps("2000")));
}

#[test]
fn a_coin_margined_short_survives_longer_than_a_cash_margined_one() {
    // Linear short:  P(1+r)/(1+m) = 100 * 1.25 / 1    = 125.
    // Inverse short: P(1-m)/(1-r) = 100 * 1    / 0.75 = 133.333333333333...,
    // rounded DOWN (toward entry) to 133.3333333333 exactly:
    //   k    = trunc(1e24 / 0.75e12) = 1_333_333_333_333
    //   liq  = 100e12 * 1_333_333_333_333 / 1e12 = 133_333_333_333_300
    let lin = pos(MarketKind::LinearPerp, PositionSide::Short, "25", "0");
    let inv = pos(MarketKind::InversePerp, PositionSide::Short, "0.25", "0");
    assert_eq!(liquidation_price(&lin).expect("ok"), Some(price("125")));
    assert_eq!(
        liquidation_price(&inv).expect("ok").map(|p| p.raw()),
        Some(133_333_333_333_300)
    );
    assert_eq!(lin.distance_bps().expect("ok"), Some(bps("2500")));
    // 33.3333333333 / 100 truncated = 0.333333333333 = 3333.33333333 bps.
    assert_eq!(
        inv.distance_bps().expect("ok").map(|b| b.raw()),
        Some(333_333_333_333)
    );
}

#[test]
fn spot_has_no_liquidation_price_without_a_borrow_model() {
    let s = pos(MarketKind::Spot, PositionSide::Long, "25", "0");
    assert!(liquidation_price(&s).is_err());
}

#[test]
fn the_two_positions_that_cannot_be_liquidated_by_price_report_none() {
    // Linear long fully margined: r >= 1, price can only fall to zero.
    let lin = pos(MarketKind::LinearPerp, PositionSide::Long, "100", "0");
    assert_eq!(liquidation_price(&lin).expect("ok"), None);
    // Inverse short whose coin margin equals its base exposure.
    let inv = pos(MarketKind::InversePerp, PositionSide::Short, "1", "0");
    assert_eq!(liquidation_price(&inv).expect("ok"), None);
    // ... and the mirrors of each DO have one, so `None` is not a blanket
    // answer for a heavily margined position.
    let lin_short = pos(MarketKind::LinearPerp, PositionSide::Short, "100", "0");
    assert_eq!(liquidation_price(&lin_short).expect("ok"), Some(price("200")));
    let inv_long = pos(MarketKind::InversePerp, PositionSide::Long, "1", "0");
    assert_eq!(liquidation_price(&inv_long).expect("ok"), Some(price("50")));
}

#[test]
fn a_position_already_under_maintenance_is_an_error_not_a_price() {
    // r = 0.005, m = 0.005: the position is below its requirement at its own
    // entry price. There is no future liquidation to report.
    let p = pos(MarketKind::LinearPerp, PositionSide::Long, "0.5", "0.005");
    assert!(liquidation_price(&p).is_err());
    // And a maintenance rate at or above 1 is refused rather than returning a
    // plausible number on the wrong side of the entry.
    let bad = pos(MarketKind::LinearPerp, PositionSide::Long, "25", "1");
    assert!(liquidation_price(&bad).is_err());
}

#[test]
fn liquidation_distance_is_truncated_so_the_cliff_reads_nearer() {
    // Entry 100, liq 75.005: distance = 24.995/100 = 0.24995 = 2499.5 bps.
    assert_eq!(
        distance_to_liquidation_bps(price("100"), price("75.005")).expect("ok"),
        bps("2499.5")
    );
    // Sign is dropped: a short's 125 is the same 2500 bps as a long's 75.
    assert_eq!(
        distance_to_liquidation_bps(price("100"), price("125")).expect("ok"),
        bps("2500")
    );
    assert!(distance_to_liquidation_bps(Price::ZERO, price("75")).is_err());
}
