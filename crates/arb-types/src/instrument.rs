//! Instruments: canonical identity, contract specification, and the notional
//! formulas that follow from them.
//!
//! # Why the notional formula lives here
//!
//! Converting a contract count into a notional is not strategy, it is the
//! *definition* of the instrument. Putting it in `arb-math` would mean every
//! consumer re-deriving it, and the linear/inverse distinction is exactly where
//! that goes wrong. `arb-math` owns the economics — fees, funding, APR, VWAP.
//! This module owns "what is one contract". See `decisions/003`.

use crate::strings::{Asset, Symbol};
use crate::venue::Venue;
use crate::TypeError;
use arb_num::{Money, NumError, Price, Qty, RoundMode};
use core::fmt;

/// What kind of market an instrument trades on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MarketKind {
    /// Spot: settlement is immediate delivery of the base asset.
    Spot,
    /// Linear perpetual: contract denominated in the base asset, margined and
    /// settled in a quote-like asset (usually USDT).
    LinearPerp,
    /// Inverse perpetual: contract denominated in the *quote* asset (usually
    /// USD), margined and settled in the *base* asset (usually BTC).
    InversePerp,
}

impl MarketKind {
    /// Short tag for table output.
    pub const fn tag(self) -> &'static str {
        match self {
            MarketKind::Spot => "spot",
            MarketKind::LinearPerp => "perp",
            MarketKind::InversePerp => "iperp",
        }
    }

    /// True for the two perpetual variants, which pay and receive funding.
    #[inline]
    pub const fn is_perp(self) -> bool {
        matches!(self, MarketKind::LinearPerp | MarketKind::InversePerp)
    }
}

impl fmt::Display for MarketKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// What one contract represents.
///
/// An enum rather than a bare number, because the two variants imply *different
/// notional formulas*. With a single `contract_size: Qty` field, sizing an
/// inverse BTCUSD position with the linear formula is off by a factor of the
/// price — roughly 68,000x — and nothing in the type system objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContractSize {
    /// One contract is this many units of the **base** asset.
    ///
    /// Spot is always `Qty::ONE`. Linear perps are usually `ONE` too, but not
    /// always: some venues quote ETH perps in 0.01 ETH contracts.
    BasePerContract(Qty),
    /// One contract is this many units of the **quote** asset.
    ///
    /// Inverse contracts only. Typical values are 1 USD or 100 USD per
    /// contract. Note that the notional in quote terms is then independent of
    /// price, and the *base* exposure is what moves.
    QuotePerContract(Money),
}

/// The unit an order book's ladder and a venue's lot size are denominated in.
///
/// # Why this is a declared field and not a derivation
///
/// Every function on [`ContractSpec`] that takes a `Qty` means a **count of
/// contracts**. An order book does not: venues quote linear and spot sizes in the
/// base asset and inverse sizes in contracts. Those two numbers differ by the
/// contract size — 100× on OKX's 0.01 BTC swap, 10 000× on Gate's 0.0001 — and
/// comparing one against the other is not a rounding difference, it is a wrong
/// answer.
///
/// It was a wrong answer this project shipped. `decisions/019` records it: a
/// contract count compared against a base-denominated ladder, which reached the
/// report as a margin column ranging from 4.69 to 261 447.99 against a single
/// 100 000 target. What made it survive review is that **Binance's contract size
/// is exactly one coin**, so on the venue anybody checks first the two units are
/// numerically identical and the bug looks like correct output.
///
/// The unit was inferrable from [`ContractSize`] — base for `BasePerContract`,
/// contracts for `QuotePerContract`. That inference is what this type replaces.
/// An inference is a claim about our own carefulness; a field is a claim about the
/// data, and only the second kind can be checked by a parser. Adapters are
/// expected to refuse a payload that does not state it rather than guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeUnit {
    /// Ladder and lot are in the **base** asset. Linear perps and spot.
    Base,
    /// Ladder and lot are in **contracts**, the same unit [`ContractSpec`]
    /// speaks. Inverse contracts.
    Contracts,
}

impl SizeUnit {
    /// The unit that [`ContractSize`] implies, for cross-checking a declared one.
    ///
    /// Deliberately **not** used to fill the field in. It exists so an adapter can
    /// assert that what a venue declared agrees with what its contract definition
    /// implies, and flag the instrument when they disagree — which is a real
    /// signal about the payload, not a chance to paper over it.
    pub const fn implied_by(size: ContractSize) -> Self {
        match size {
            ContractSize::BasePerContract(_) => SizeUnit::Base,
            ContractSize::QuotePerContract(_) => SizeUnit::Contracts,
        }
    }

    /// Short label for a report.
    pub const fn as_str(self) -> &'static str {
        match self {
            SizeUnit::Base => "base",
            SizeUnit::Contracts => "contracts",
        }
    }
}

impl core::fmt::Display for SizeUnit {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The tradeable parameters of one instrument at one venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContractSpec {
    /// Market type.
    pub kind: MarketKind,
    /// What one contract represents.
    pub size: ContractSize,
    /// Asset that profit, loss, and margin are denominated in.
    pub settle: Asset,
    /// Minimum price increment.
    pub tick_size: Price,
    /// What [`ContractSpec::lot_size`] and the order book's ladder are counted
    /// in. Declared by the venue, never inferred — see [`SizeUnit`].
    pub size_unit: SizeUnit,
    /// Minimum order size increment, in [`ContractSpec::size_unit`].
    ///
    /// Deliberately *not* always contracts: rounding a base-denominated size to a
    /// contract-denominated lot is wrong by the contract size. See `decisions/019`.
    pub lot_size: Qty,
    /// Minimum order notional, where the venue enforces one.
    pub min_notional: Option<Money>,
}

impl ContractSpec {
    /// Notional value, in the quote asset, of `contracts` at `price`.
    ///
    /// - Linear/spot: `contracts * base_per_contract * price`.
    /// - Inverse: `contracts * quote_per_contract`, independent of price.
    ///
    /// `mode` is explicit because both paths can drop a sub-unit remainder;
    /// callers computing a cost should pass [`RoundMode::COST`].
    pub fn notional(
        &self,
        contracts: Qty,
        price: Price,
        mode: RoundMode,
    ) -> Result<Money, NumError> {
        match self.size {
            ContractSize::BasePerContract(base_per) => {
                let base = contracts.mul_qty(base_per, mode)?;
                base.mul_price(price, mode)
            }
            ContractSize::QuotePerContract(quote_per) => {
                // Price deliberately unused: an inverse contract's quote
                // notional is fixed by the contract definition.
                contracts.mul_qty(quote_per.reinterpret_as_qty(), mode).map(Qty::reinterpret_as_money)
            }
        }
    }

    /// Exposure in the **base** asset of `contracts` at `price`.
    ///
    /// - Linear/spot: `contracts * base_per_contract`, independent of price.
    /// - Inverse: `contracts * quote_per_contract / price`.
    ///
    /// Note the symmetry with [`ContractSpec::notional`]: exactly one of the two
    /// is price-dependent, and which one flips with the contract type. This is
    /// the asymmetry the enum exists to force callers to confront.
    pub fn base_qty(&self, contracts: Qty, price: Price, mode: RoundMode) -> Result<Qty, NumError> {
        match self.size {
            ContractSize::BasePerContract(base_per) => contracts.mul_qty(base_per, mode),
            ContractSize::QuotePerContract(quote_per) => {
                let quote_notional = contracts
                    .mul_qty(quote_per.reinterpret_as_qty(), mode)
                    .map(Qty::reinterpret_as_money)?;
                quote_notional.div_price(price, mode)
            }
        }
    }

    /// A plain spot spec with unit contract size.
    ///
    /// Takes no `size_unit`: a spot book is denominated in the base asset at every
    /// venue we know of, because there is no contract to denominate it in. This is
    /// the one place the unit is not a claim about a payload. [факт, проверено —
    /// следует из определения спота, не из измерения API]
    pub fn spot(settle: Asset, tick_size: Price, lot_size: Qty) -> Self {
        Self {
            kind: MarketKind::Spot,
            size: ContractSize::BasePerContract(Qty::ONE),
            settle,
            tick_size,
            size_unit: SizeUnit::Base,
            lot_size,
            min_notional: None,
        }
    }

    /// The sanctioned constructor: builds a spec and checks its cross-field
    /// invariants.
    ///
    /// The fields are public because `tick_size`, `lot_size` and `min_notional`
    /// are read constantly and hiding them behind accessors buys nothing. That
    /// leaves struct-literal construction as an unchecked path, so
    /// [`ContractSpec::validate`] is public too: anything that builds a spec
    /// from venue data is expected to go through one of the two.
    ///
    /// `size_unit` is a required parameter and has no default. Defaulting it would
    /// reintroduce the inference that `decisions/019` identifies as the defect
    /// class: on Binance, where one contract is one coin, a wrong inference looks
    /// exactly right, and on Gate it is wrong by 10 000×. A caller that does not
    /// know the venue's order-size unit does not know enough to build a spec.
    pub fn new(
        kind: MarketKind,
        size: ContractSize,
        settle: Asset,
        tick_size: Price,
        size_unit: SizeUnit,
        lot_size: Qty,
        min_notional: Option<Money>,
    ) -> Result<Self, TypeError> {
        let spec = Self {
            kind,
            size,
            settle,
            tick_size,
            size_unit,
            lot_size,
            min_notional,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Rejects a spec whose contract size does not match its market kind.
    ///
    /// This is the check that makes the 68,000x error from
    /// [`ContractSize`]'s docs unrepresentable rather than merely discouraged.
    /// A linear or spot market with `QuotePerContract` would price its notional
    /// as price-independent; an inverse market with `BasePerContract` would
    /// multiply by price a second time. Both look plausible in a debugger.
    ///
    /// `tick_size` and `lot_size` must be strictly positive: a zero tick makes
    /// price rounding a division by zero, and a zero lot makes every size legal
    /// including sizes the venue will reject.
    ///
    /// # What this deliberately does not check
    ///
    /// It does **not** require `size_unit` to agree with what `size` implies via
    /// [`SizeUnit::implied_by`]. The two are independent facts and disagreement is
    /// legal: OKX's linear BTC swap has `BasePerContract(0.01)` and takes orders in
    /// **contracts**, so `Contracts` alongside `BasePerContract` is the correct
    /// description of a real venue. [предположение — восстановлено по памяти об
    /// API OKX, живой сетью не проверено]
    ///
    /// Rejecting that combination here would force adapters to lie about one of
    /// the two fields, which is the failure this field was added to end. A caller
    /// that wants the cross-check has [`SizeUnit::implied_by`] and should raise a
    /// quality flag on disagreement, not an error.
    pub fn validate(&self) -> Result<(), TypeError> {
        match (self.kind, self.size) {
            (MarketKind::Spot | MarketKind::LinearPerp, ContractSize::BasePerContract(base)) => {
                if !base.is_positive() {
                    return Err(TypeError::NonPositiveLevel);
                }
            }
            (MarketKind::InversePerp, ContractSize::QuotePerContract(quote)) => {
                if !quote.is_positive() {
                    return Err(TypeError::NonPositiveLevel);
                }
            }
            _ => return Err(TypeError::ContractSizeMismatch),
        }
        if !self.tick_size.is_positive() || !self.lot_size.is_positive() {
            return Err(TypeError::NonPositivePrice);
        }
        Ok(())
    }

    /// Checks that this spec settles in the asset its key claims.
    ///
    /// Two sources of truth for settlement is one too many: `InstrumentKey`
    /// carries `settle` because it is part of the cross-venue join identity,
    /// and `ContractSpec` carries it because margin is denominated in it. They
    /// must agree, and the only way to be sure is to ask.
    pub fn check_settle(&self, key: &InstrumentKey) -> Result<(), TypeError> {
        if self.settle == key.settle {
            Ok(())
        } else {
            Err(TypeError::SettleMismatch)
        }
    }
}

/// Venue-independent canonical identity of an instrument.
///
/// This is the join key for cross-venue comparison. It contains no venue and no
/// native symbol, precisely so that Binance's `BTCUSDT` and OKX's
/// `BTC-USDT-SWAP` collapse to the same key and can be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentKey {
    /// Base asset, e.g. `BTC`.
    pub base: Asset,
    /// Quote asset, e.g. `USDT`.
    pub quote: Asset,
    /// Market type. Part of the key: the BTC/USDT spot book and the BTC/USDT
    /// perp book are different instruments with different prices.
    pub kind: MarketKind,
    /// Settlement asset. Part of the key because `BTC/USD` settled in USDT and
    /// the same pair settled in BTC are genuinely different instruments with
    /// different risk.
    pub settle: Asset,
}

impl InstrumentKey {
    /// Construct a key.
    pub const fn new(base: Asset, quote: Asset, kind: MarketKind, settle: Asset) -> Self {
        Self {
            base,
            quote,
            kind,
            settle,
        }
    }

    /// Build from string codes, validating each.
    pub fn parse(
        base: &str,
        quote: &str,
        kind: MarketKind,
        settle: &str,
    ) -> Result<Self, TypeError> {
        Ok(Self::new(
            Asset::new(base)?,
            Asset::new(quote)?,
            kind,
            Asset::new(settle)?,
        ))
    }

    /// True when this instrument's key differs from `other` only by market kind
    /// — i.e. they are the spot and perp legs of the same underlying pair.
    #[inline]
    pub fn same_pair(&self, other: &InstrumentKey) -> bool {
        self.base == other.base && self.quote == other.quote
    }
}

impl fmt::Display for InstrumentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Settle is shown only when it is not the quote asset, which is the
        // common case; showing it always makes every spot row noisy.
        if self.settle == self.quote {
            write!(f, "{}/{}:{}", self.base, self.quote, self.kind.tag())
        } else {
            write!(
                f,
                "{}/{}:{}({})",
                self.base, self.quote, self.kind.tag(), self.settle
            )
        }
    }
}

/// A canonical instrument at a specific venue. The key used in per-venue maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VenueKey {
    /// Which venue.
    pub venue: Venue,
    /// Canonical instrument identity.
    pub key: InstrumentKey,
}

impl VenueKey {
    /// Construct.
    pub const fn new(venue: Venue, key: InstrumentKey) -> Self {
        Self { venue, key }
    }
}

impl fmt::Display for VenueKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.venue, self.key)
    }
}

/// Everything needed to trade one instrument at one venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Instrument {
    /// Venue plus canonical identity.
    pub vkey: VenueKey,
    /// The symbol string this venue expects in its API.
    pub native: Symbol,
    /// Tradeable parameters.
    pub spec: ContractSpec,
}

impl Instrument {
    /// Construct.
    pub const fn new(vkey: VenueKey, native: Symbol, spec: ContractSpec) -> Self {
        Self { vkey, native, spec }
    }

    /// Convenience accessor.
    #[inline]
    pub const fn venue(&self) -> Venue {
        self.vkey.venue
    }

    /// Convenience accessor.
    #[inline]
    pub const fn key(&self) -> InstrumentKey {
        self.vkey.key
    }
}

impl fmt::Display for Instrument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.vkey, self.native)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(s: &str) -> Asset {
        Asset::new(s).expect("test asset is valid")
    }

    fn linear_btc() -> ContractSpec {
        ContractSpec {
            kind: MarketKind::LinearPerp,
            size: ContractSize::BasePerContract(Qty::ONE),
            settle: asset("USDT"),
            tick_size: Price::parse("0.1").expect("ok"),
            size_unit: SizeUnit::Base,
            lot_size: Qty::parse("0.001").expect("ok"),
            min_notional: Some(Money::parse("5").expect("ok")),
        }
    }

    /// Inverse BTCUSD with 100 USD per contract — the Bitmex/Bybit-inverse
    /// convention. Value is unverified against any live API; the *formula* is
    /// what these tests pin down.
    fn inverse_btc_100usd() -> ContractSpec {
        ContractSpec {
            kind: MarketKind::InversePerp,
            size: ContractSize::QuotePerContract(Money::from_int(100).expect("ok")),
            settle: asset("BTC"),
            tick_size: Price::parse("0.5").expect("ok"),
            size_unit: SizeUnit::Contracts,
            lot_size: Qty::ONE,
            min_notional: None,
        }
    }

    #[test]
    fn linear_notional_scales_with_price() {
        // 2 contracts * 1 BTC * 68,000 = 136,000 USDT. By hand.
        let spec = linear_btc();
        let n = spec
            .notional(
                Qty::from_int(2).expect("ok"),
                Price::from_int(68_000).expect("ok"),
                RoundMode::Trunc,
            )
            .expect("ok");
        assert_eq!(n, Money::from_int(136_000).expect("ok"));
        // Base exposure is price-independent: 2 BTC.
        let b = spec
            .base_qty(
                Qty::from_int(2).expect("ok"),
                Price::from_int(68_000).expect("ok"),
                RoundMode::Trunc,
            )
            .expect("ok");
        assert_eq!(b, Qty::from_int(2).expect("ok"));
    }

    #[test]
    fn inverse_notional_is_price_independent() {
        // 3 contracts * 100 USD = 300 USD, at any price.
        let spec = inverse_btc_100usd();
        let contracts = Qty::from_int(3).expect("ok");
        for px in ["68000", "1", "250000"] {
            let n = spec
                .notional(contracts, Price::parse(px).expect("ok"), RoundMode::Trunc)
                .expect("ok");
            assert_eq!(
                n,
                Money::from_int(300).expect("ok"),
                "inverse notional must not vary with price {px}"
            );
        }
    }

    #[test]
    fn inverse_base_exposure_is_notional_over_price() {
        // 3 contracts * 100 USD = 300 USD at 68,000 -> 0.004411764705 BTC.
        // By hand: 300 / 68000 = 0.00441176470588... truncated at 12 places.
        let spec = inverse_btc_100usd();
        let b = spec
            .base_qty(
                Qty::from_int(3).expect("ok"),
                Price::from_int(68_000).expect("ok"),
                RoundMode::Trunc,
            )
            .expect("ok");
        assert_eq!(b, Qty::parse("0.004411764705").expect("ok"));
    }

    #[test]
    fn the_two_contract_types_disagree_by_a_factor_of_price() {
        // This is the bug the enum prevents, asserted numerically so the stakes
        // are on the record. Same contract count, same price, 68,000x apart.
        let contracts = Qty::from_int(1).expect("ok");
        let price = Price::from_int(68_000).expect("ok");
        let linear = ContractSpec {
            size: ContractSize::BasePerContract(Qty::ONE),
            ..linear_btc()
        };
        let inverse = ContractSpec {
            size: ContractSize::QuotePerContract(Money::from_int(1).expect("ok")),
            ..inverse_btc_100usd()
        };
        let ln = linear.notional(contracts, price, RoundMode::Trunc).expect("ok");
        let inv = inverse.notional(contracts, price, RoundMode::Trunc).expect("ok");
        assert_eq!(ln, Money::from_int(68_000).expect("ok"));
        assert_eq!(inv, Money::from_int(1).expect("ok"));
    }

    #[test]
    fn fractional_contract_size_is_honoured() {
        // ETH perp quoted in 0.01 ETH contracts: 10 contracts = 0.1 ETH.
        let spec = ContractSpec {
            kind: MarketKind::LinearPerp,
            size: ContractSize::BasePerContract(Qty::parse("0.01").expect("ok")),
            settle: asset("USDT"),
            tick_size: Price::parse("0.01").expect("ok"),
            size_unit: SizeUnit::Contracts,
            lot_size: Qty::ONE,
            min_notional: None,
        };
        let contracts = Qty::from_int(10).expect("ok");
        let price = Price::from_int(3_000).expect("ok");
        assert_eq!(
            spec.base_qty(contracts, price, RoundMode::Trunc).expect("ok"),
            Qty::parse("0.1").expect("ok")
        );
        // 0.1 ETH * 3000 = 300 USDT.
        assert_eq!(
            spec.notional(contracts, price, RoundMode::Trunc).expect("ok"),
            Money::from_int(300).expect("ok")
        );
    }

    #[test]
    fn notional_and_base_qty_are_consistent_for_both_types() {
        // `base_qty * price` must reproduce `notional`, but only to a tolerance
        // that scales with price — and that tolerance is worth stating exactly,
        // because guessing it is how a test gets quietly loosened later.
        //
        // Linear: base_qty is exact (no division), so the round trip is exact.
        // Inverse: base_qty = notional / price is truncated, losing up to one
        // ulp of Qty (1e-12). Multiplying back by price scales that error by
        // price, so the bound is `price * 1e-12`, not `1e-12`. At BTC prices
        // that is 6.8e-8 USD — six thousand times the naive bound.
        let price = Price::from_int(68_000).expect("ok");
        let contracts = Qty::from_int(7).expect("ok");
        let bound = price
            .mul_qty(Qty::EPSILON, RoundMode::AwayFromZero)
            .expect("ok");
        assert_eq!(bound, Money::parse("0.000000068").expect("ok"));

        for spec in [linear_btc(), inverse_btc_100usd()] {
            let n = spec.notional(contracts, price, RoundMode::Trunc).expect("ok");
            let b = spec.base_qty(contracts, price, RoundMode::Trunc).expect("ok");
            let rebuilt = b.mul_price(price, RoundMode::Trunc).expect("ok");
            let diff = n.checked_sub(rebuilt).expect("ok").checked_abs().expect("ok");
            assert!(
                diff <= bound,
                "{:?}: notional={n} rebuilt={rebuilt} diff={diff} exceeds {bound}",
                spec.kind
            );
            // Truncation must lose value, never create it.
            assert!(rebuilt <= n, "{:?}: round trip gained value", spec.kind);
        }

        // Pin the linear case as exactly exact, so a future change that
        // introduces a division there fails loudly instead of hiding under the
        // shared tolerance above.
        let lin = linear_btc();
        let ln = lin.notional(contracts, price, RoundMode::Trunc).expect("ok");
        let lb = lin.base_qty(contracts, price, RoundMode::Trunc).expect("ok");
        assert_eq!(lb.mul_price(price, RoundMode::Trunc).expect("ok"), ln);
        assert_eq!(ln, Money::from_int(476_000).expect("ok")); // 7 * 68000

        // And pin the inverse residual as the exact hand-computed value:
        // 700 / 68000 = 0.0102941176470588..., truncated -> 0.010294117647
        // 0.010294117647 * 68000 = 699.999999996, i.e. 4e-9 short of 700.
        let inv = inverse_btc_100usd();
        let ib = inv.base_qty(contracts, price, RoundMode::Trunc).expect("ok");
        assert_eq!(ib, Qty::parse("0.010294117647").expect("ok"));
        assert_eq!(
            ib.mul_price(price, RoundMode::Trunc).expect("ok"),
            Money::parse("699.999999996").expect("ok")
        );
    }

    #[test]
    fn spot_helper_has_unit_contract_size() {
        let spec = ContractSpec::spot(
            asset("USDT"),
            Price::parse("0.01").expect("ok"),
            Qty::parse("0.0001").expect("ok"),
        );
        assert_eq!(spec.kind, MarketKind::Spot);
        assert!(!spec.kind.is_perp());
        // 0.5 BTC at 68,000 = 34,000.
        assert_eq!(
            spec.notional(
                Qty::parse("0.5").expect("ok"),
                Price::from_int(68_000).expect("ok"),
                RoundMode::Trunc
            )
            .expect("ok"),
            Money::from_int(34_000).expect("ok")
        );
    }

    #[test]
    fn is_perp_covers_both_perp_variants() {
        assert!(MarketKind::LinearPerp.is_perp());
        assert!(MarketKind::InversePerp.is_perp());
        assert!(!MarketKind::Spot.is_perp());
    }

    #[test]
    fn settle_asset_participates_in_identity() {
        // Same pair, same kind, different settlement = different instrument.
        let usdt = InstrumentKey::parse("BTC", "USD", MarketKind::LinearPerp, "USDT").expect("ok");
        let coin = InstrumentKey::parse("BTC", "USD", MarketKind::InversePerp, "BTC").expect("ok");
        assert_ne!(usdt, coin);
        // And so does market kind.
        let spot = InstrumentKey::parse("BTC", "USDT", MarketKind::Spot, "USDT").expect("ok");
        let perp = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").expect("ok");
        assert_ne!(spot, perp);
        // But they are recognisably the same underlying pair.
        assert!(spot.same_pair(&perp));
        assert!(!spot.same_pair(&coin));
    }

    #[test]
    fn key_is_venue_independent_so_venues_join() {
        // The entire point: two venues, two native symbols, one key.
        let binance = Instrument::new(
            VenueKey::new(
                Venue::Binance,
                InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").expect("ok"),
            ),
            Symbol::new("BTCUSDT").expect("ok"),
            linear_btc(),
        );
        let okx = Instrument::new(
            VenueKey::new(
                Venue::Okx,
                InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").expect("ok"),
            ),
            Symbol::new("BTC-USDT-SWAP").expect("ok"),
            linear_btc(),
        );
        assert_eq!(binance.key(), okx.key());
        assert_ne!(binance.vkey, okx.vkey);
        assert_ne!(binance.native, okx.native);
        assert_eq!(binance.venue(), Venue::Binance);
    }

    #[test]
    fn display_is_readable_and_hides_redundant_settle() {
        let k = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").expect("ok");
        assert_eq!(k.to_string(), "BTC/USDT:perp");
        let inv = InstrumentKey::parse("BTC", "USD", MarketKind::InversePerp, "BTC").expect("ok");
        assert_eq!(inv.to_string(), "BTC/USD:iperp(BTC)");
        let vk = VenueKey::new(Venue::Bybit, k);
        assert_eq!(vk.to_string(), "bybit:BTC/USDT:perp");
    }

    #[test]
    fn invalid_asset_codes_are_rejected() {
        assert_eq!(
            InstrumentKey::parse("BTC-", "USDT", MarketKind::Spot, "USDT"),
            Err(TypeError::InvalidChar)
        );
        assert_eq!(
            InstrumentKey::parse("", "USDT", MarketKind::Spot, "USDT"),
            Err(TypeError::Empty)
        );
    }

    #[test]
    fn inverse_notional_at_zero_price_still_works_but_base_qty_fails() {
        // Inverse quote notional does not divide by price, so it survives a
        // zero price; base exposure cannot. Better an error than an infinity.
        let spec = inverse_btc_100usd();
        let c = Qty::from_int(1).expect("ok");
        assert!(spec.notional(c, Price::ZERO, RoundMode::Trunc).is_ok());
        assert_eq!(
            spec.base_qty(c, Price::ZERO, RoundMode::Trunc),
            Err(NumError::DivByZero)
        );
    }

    #[test]
    fn linear_declared_quote_per_contract_is_rejected() {
        // The 68,000x error, refused at construction. A linear perp whose size
        // is quote-denominated would report a price-independent notional.
        let got = ContractSpec::new(
            MarketKind::LinearPerp,
            ContractSize::QuotePerContract(Money::from_int(100).expect("ok")),
            asset("USDT"),
            Price::parse("0.1").expect("ok"),
            Qty::parse("0.001").expect("ok"),
            None,
        );
        assert_eq!(got, Err(TypeError::ContractSizeMismatch));
    }

    #[test]
    fn inverse_declared_base_per_contract_is_rejected() {
        let got = ContractSpec::new(
            MarketKind::InversePerp,
            ContractSize::BasePerContract(Qty::ONE),
            asset("BTC"),
            Price::parse("0.5").expect("ok"),
            Qty::ONE,
            None,
        );
        assert_eq!(got, Err(TypeError::ContractSizeMismatch));
    }

    #[test]
    fn spot_declared_quote_per_contract_is_rejected() {
        let got = ContractSpec::new(
            MarketKind::Spot,
            ContractSize::QuotePerContract(Money::ONE),
            asset("USDT"),
            Price::parse("0.01").expect("ok"),
            Qty::parse("0.0001").expect("ok"),
            None,
        );
        assert_eq!(got, Err(TypeError::ContractSizeMismatch));
    }

    #[test]
    fn matching_kind_and_size_is_accepted() {
        assert!(ContractSpec::new(
            MarketKind::LinearPerp,
            ContractSize::BasePerContract(Qty::ONE),
            asset("USDT"),
            Price::parse("0.1").expect("ok"),
            Qty::parse("0.001").expect("ok"),
            None,
        )
        .is_ok());
        assert!(ContractSpec::new(
            MarketKind::InversePerp,
            ContractSize::QuotePerContract(Money::from_int(100).expect("ok")),
            asset("BTC"),
            Price::parse("0.5").expect("ok"),
            Qty::ONE,
            None,
        )
        .is_ok());
    }

    #[test]
    fn zero_tick_or_zero_lot_is_rejected() {
        // A zero tick makes price rounding a division by zero; a zero lot makes
        // every size legal, including the ones the venue will bounce.
        let zero_tick = ContractSpec::new(
            MarketKind::LinearPerp,
            ContractSize::BasePerContract(Qty::ONE),
            asset("USDT"),
            Price::ZERO,
            Qty::parse("0.001").expect("ok"),
            None,
        );
        assert_eq!(zero_tick, Err(TypeError::NonPositivePrice));
        let zero_lot = ContractSpec::new(
            MarketKind::LinearPerp,
            ContractSize::BasePerContract(Qty::ONE),
            asset("USDT"),
            Price::parse("0.1").expect("ok"),
            Qty::ZERO,
            None,
        );
        assert_eq!(zero_lot, Err(TypeError::NonPositivePrice));
    }

    #[test]
    fn zero_contract_size_is_rejected() {
        // One contract representing nothing would make every notional zero,
        // which ranks as an infinitely cheap opportunity.
        let got = ContractSpec::new(
            MarketKind::LinearPerp,
            ContractSize::BasePerContract(Qty::ZERO),
            asset("USDT"),
            Price::parse("0.1").expect("ok"),
            Qty::parse("0.001").expect("ok"),
            None,
        );
        assert_eq!(got, Err(TypeError::NonPositiveLevel));
    }

    #[test]
    fn spot_helper_produces_a_valid_spec() {
        let spec = ContractSpec::spot(
            asset("USDT"),
            Price::parse("0.01").expect("ok"),
            Qty::parse("0.0001").expect("ok"),
        );
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn settle_disagreement_between_key_and_spec_is_caught() {
        // An inverse BTCUSD settles in BTC. A key claiming USDT settlement for
        // the same spec is two sources of truth, and one of them is wrong.
        let spec = inverse_btc_100usd();
        let agreeing =
            InstrumentKey::new(asset("BTC"), asset("USD"), MarketKind::InversePerp, asset("BTC"));
        assert_eq!(spec.check_settle(&agreeing), Ok(()));
        let disagreeing = InstrumentKey::new(
            asset("BTC"),
            asset("USD"),
            MarketKind::InversePerp,
            asset("USDT"),
        );
        assert_eq!(spec.check_settle(&disagreeing), Err(TypeError::SettleMismatch));
    }
}
