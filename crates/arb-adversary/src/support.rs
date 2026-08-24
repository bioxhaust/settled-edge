//! Shared constructors. Panics are fine here: this is all `#[cfg(test)]`.

use arb_num::{Bps, Money, Price, Qty, Rate};
use arb_types::{
    Asset, BookSnapshot, ContractSize, ContractSpec, DataQuality, InstrumentKey, Level, LocalTs,
    MarketKind, SizeUnit, Venue, VenueKey,
};

pub fn price(s: &str) -> Price {
    Price::parse(s).expect("price parses")
}

pub fn qty(s: &str) -> Qty {
    Qty::parse(s).expect("qty parses")
}

pub fn money(s: &str) -> Money {
    Money::parse(s).expect("money parses")
}

pub fn rate(s: &str) -> Rate {
    Rate::parse(s).expect("rate parses")
}

/// `bps("12")` is twelve basis points, i.e. the rate 0.0012.
pub fn bps(s: &str) -> Bps {
    Bps::parse_bps(s).expect("bps parses")
}

pub fn asset(s: &str) -> Asset {
    Asset::new(s).expect("asset parses")
}

pub fn level(p: &str, q: &str) -> Level {
    Level::new(price(p), qty(q)).expect("level is valid")
}

pub fn vkey(kind: MarketKind, settle: &str) -> VenueKey {
    VenueKey::new(
        Venue::Binance,
        InstrumentKey::new(asset("BTC"), asset("USDT"), kind, asset(settle)),
    )
}

pub fn linear_vkey() -> VenueKey {
    vkey(MarketKind::LinearPerp, "USDT")
}

/// A book with the given levels, observed at t=0, marked measured so that
/// nothing in a quality flag can excuse a wrong number.
pub fn book(bids: Vec<Level>, asks: Vec<Level>) -> BookSnapshot {
    BookSnapshot::new(
        linear_vkey(),
        bids,
        asks,
        Some(1),
        None,
        LocalTs::from_millis(0),
        DataQuality::MEASURED,
    )
    .expect("book invariants hold")
}

/// Linear perp: one contract is `base_per` of BASE. OKX BTC-USDT-SWAP is 0.01.
pub fn linear_spec(base_per: &str, lot: &str) -> ContractSpec {
    ContractSpec::new(
        MarketKind::LinearPerp,
        ContractSize::BasePerContract(qty(base_per)),
        asset("USDT"),
        price("0.1"),
        SizeUnit::Base,
        qty(lot),
        None,
    )
    .expect("linear spec is valid")
}

/// Inverse perp: one contract is `quote_per` of QUOTE. OKX BTC-USD-SWAP is 100.
pub fn inverse_spec(quote_per: &str, lot: &str) -> ContractSpec {
    ContractSpec::new(
        MarketKind::InversePerp,
        ContractSize::QuotePerContract(money(quote_per)),
        asset("BTC"),
        price("0.1"),
        SizeUnit::Contracts,
        qty(lot),
        None,
    )
    .expect("inverse spec is valid")
}
