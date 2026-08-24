//! Seam tests for the private strategies.
//!
//! Two jobs. First: every `plan` function returns a *typed* refusal, not a
//! panic — this crate ships stubs, and a stub that panics is a landmine in the
//! one code path nobody exercised. Second: the refusal says what it is waiting
//! on, so the blocked-on strings cannot quietly go empty or stale.
//!
//! # About the fixtures below
//!
//! Every book, price, size, gas figure and timestamp in this file was invented
//! to be structurally plausible. None of it was captured from a live venue or
//! chain, and none of it has been checked against one. That is why each snapshot
//! carries `DataQuality::SYNTHESIZED`. They exercise validation paths; they are
//! evidence of nothing about the market.

use arb_num::{Bps, Money, Price, Qty, RoundMode};
use arb_private::dex_cex::{self, DexCexDirection, DexCexParams, DexQuote};
use arb_private::error::PrivateError;
use arb_private::listings::{self, ListingEvent, ListingKind, ListingParams};
use arb_private::quote_cover::{self, QuoteCoverParams};
use arb_private::BLOCKERS;
use arb_types::{
    Asset, BookSnapshot, ContractSpec, DataQuality, InstrumentKey, Level, LocalTs, MarketKind,
    Millis, Side, Symbol, Venue, VenueKey, WallTs,
};

// ---------------------------------------------------------------- fixtures --

fn vkey(venue: Venue, base: &str, quote: &str) -> VenueKey {
    let key = InstrumentKey::parse(base, quote, MarketKind::Spot, quote).unwrap();
    VenueKey::new(venue, key)
}

fn spot_spec(quote: &str) -> ContractSpec {
    // 0.01 tick, 0.001 lot: plausible shape for a major spot pair. INVENTED.
    ContractSpec::spot(
        Asset::new(quote).unwrap(),
        Price::parse("0.01").unwrap(),
        Qty::parse("0.001").unwrap(),
    )
}

fn book(venue: Venue, base: &str, quote: &str, bid: &str, ask: &str) -> BookSnapshot {
    let bids = vec![Level::new(Price::parse(bid).unwrap(), Qty::parse("1.5").unwrap()).unwrap()];
    let asks = vec![Level::new(Price::parse(ask).unwrap(), Qty::parse("1.5").unwrap()).unwrap()];
    BookSnapshot::new(
        vkey(venue, base, quote),
        bids,
        asks,
        None,
        None,
        LocalTs::from_millis(1_700_000_000_000),
        DataQuality::SYNTHESIZED,
    )
    .unwrap()
}

/// Tight book: 68_100.00 / 68_120.00. Spread 20.00, mid 68_110.00 exactly.
fn tight_book() -> BookSnapshot {
    book(Venue::Binance, "BTC", "USDT", "68100.00", "68120.00")
}

/// Wide book: 68_050.00 / 68_170.00. Spread 120.00, mid 68_110.00 exactly —
/// same mid as the tight book, so any edge here comes from the spread and not
/// from a level difference.
fn wide_book() -> BookSnapshot {
    book(Venue::Bybit, "BTC", "USDT", "68050.00", "68170.00")
}

fn quote_cover_params<'a>(
    quote_book: &'a BookSnapshot,
    cover_book: &'a BookSnapshot,
) -> QuoteCoverParams<'a> {
    QuoteCoverParams {
        quote_book,
        cover_book,
        quote_side: Side::Buy,
        quote_spec: spot_spec("USDT"),
        cover_spec: spot_spec("USDT"),
        show_size: Qty::parse("0.25").unwrap(),
        // 1 bp maker, 5 bps taker: plausible VIP-0 shape. INVENTED, not read
        // off any fee schedule.
        quote_maker_fee: Bps::from_whole(1).unwrap(),
        cover_taker_fee: Bps::from_whole(5).unwrap(),
        min_edge: Bps::from_whole(12).unwrap(),
    }
}

/// Announcement at t, trading one hour later.
/// 1_700_003_600_000 - 1_700_000_000_000 = 3_600_000 ms = 60 min exactly.
fn listing_event() -> ListingEvent {
    let announced_at = WallTs::from_millis(1_700_000_000_000);
    ListingEvent {
        vkey: vkey(Venue::Bybit, "ARB", "USDT"),
        native: Symbol::new("ARBUSDT").unwrap(),
        kind: ListingKind::AlreadyTradedElsewhere,
        reference: Some(vkey(Venue::Binance, "ARB", "USDT")),
        announced_at,
        trading_starts_at: announced_at.add_millis(Millis::from_millis(3_600_000)),
        observed_at: LocalTs::from_millis(1_700_000_000_250),
        quality: DataQuality::SYNTHESIZED,
    }
}

fn listing_params() -> ListingParams {
    ListingParams {
        hedge: Some(vkey(Venue::Binance, "ARB", "USDT")),
        max_size: Qty::parse("5000").unwrap(),
        max_notional: Money::parse("25000.00").unwrap(),
        // 750 ms is a placeholder that only has to be positive for validation
        // to pass. We have never measured our order round-trip on any venue.
        reaction_budget: Millis::from_millis(750),
    }
}

/// WETH/USDT pool quote for 2.0 WETH at an effective 3_000.00.
/// Gas 0.0021 ETH. All INVENTED.
fn dex_quote() -> DexQuote {
    DexQuote {
        pool: Symbol::new("UNIV3-WETH-USDT-500").unwrap(),
        base: Asset::new("WETH").unwrap(),
        quote: Asset::new("USDT").unwrap(),
        effective_price: Price::parse("3000.00").unwrap(),
        size: Qty::parse("2.0").unwrap(),
        gas_cost: Money::parse("0.0021").unwrap(),
        gas_asset: Asset::new("ETH").unwrap(),
        block_time: Millis::from_millis(12_000),
        observed_at: LocalTs::from_millis(1_700_000_000_000),
        quality: DataQuality::SYNTHESIZED,
    }
}

/// CEX book 3_004.00 / 3_006.00, mid 3_005.00 exactly. Buying the pool at
/// 3_000.00 and selling the bid at 3_004.00 is 4.00 gross per WETH, which is
/// 4/3000 = 0.001333... ~= 13.33 bps before fees, gas and MEV. Illustrative
/// only — nothing below asserts it, because the cost model that would compute
/// it does not exist yet.
fn cex_book() -> BookSnapshot {
    book(Venue::Binance, "WETH", "USDT", "3004.00", "3006.00")
}

fn dex_cex_params<'a>(dex: &'a DexQuote, cex_book: &'a BookSnapshot) -> DexCexParams<'a> {
    DexCexParams {
        dex,
        cex_book,
        direction: DexCexDirection::BuyDexSellCex,
        size: Qty::parse("2.0").unwrap(),
        cex_taker_fee: Bps::from_whole(5).unwrap(),
        gas_in_quote: Some(Price::parse("3000.00").unwrap()),
        // Any value here is invented; see DexCexParams::mev_haircut.
        mev_haircut: Some(Bps::from_whole(3).unwrap()),
        min_edge: Bps::from_whole(12).unwrap(),
    }
}

fn expect_blocked(err: PrivateError, strategy: &str) {
    assert!(
        err.is_not_implemented(),
        "expected NotImplemented, got: {err}"
    );
    assert_eq!(err.strategy(), Some(strategy));
    let blocked_on = err.blocked_on().unwrap();
    assert!(!blocked_on.is_empty(), "blocked_on must say something");
    // A one-word blocker is not a work item anybody can pick up.
    assert!(
        blocked_on.len() > 20,
        "blocked_on is too vague: {blocked_on}"
    );
}

// ------------------------------------------------------------------- tests --

#[test]
fn quote_cover_is_blocked_on_queue_position() {
    let wide = wide_book();
    let tight = tight_book();
    let params = quote_cover_params(&wide, &tight);
    let err = quote_cover::plan(&params).unwrap_err();
    expect_blocked(err, "quote_and_cover");
}

#[test]
fn listings_is_blocked_on_a_calendar_feed() {
    let err = listings::plan(&listing_event(), &listing_params()).unwrap_err();
    expect_blocked(err, "new_listing_capture");
}

#[test]
fn dex_cex_is_blocked_on_gas_and_mev() {
    let dex = dex_quote();
    let cex = cex_book();
    let params = dex_cex_params(&dex, &cex);
    let err = dex_cex::plan(&params).unwrap_err();
    expect_blocked(err, "dex_cex");
}

#[test]
fn display_names_the_strategy() {
    let wide = wide_book();
    let tight = tight_book();
    let dex = dex_quote();
    let cex = cex_book();

    let cases = [
        (
            quote_cover::plan(&quote_cover_params(&wide, &tight)).unwrap_err(),
            "quote_and_cover",
        ),
        (
            listings::plan(&listing_event(), &listing_params()).unwrap_err(),
            "new_listing_capture",
        ),
        (
            dex_cex::plan(&dex_cex_params(&dex, &cex)).unwrap_err(),
            "dex_cex",
        ),
    ];

    for (err, strategy) in cases {
        let text = err.to_string();
        assert!(
            text.contains(strategy),
            "Display must name the strategy: {text}"
        );
        assert!(
            text.contains("blocked on"),
            "Display must say what it is blocked on: {text}"
        );
    }
}

#[test]
fn blockers_table_matches_what_plan_returns() {
    let wide = wide_book();
    let tight = tight_book();
    let dex = dex_quote();
    let cex = cex_book();

    let live = [
        quote_cover::plan(&quote_cover_params(&wide, &tight)).unwrap_err(),
        listings::plan(&listing_event(), &listing_params()).unwrap_err(),
        dex_cex::plan(&dex_cex_params(&dex, &cex)).unwrap_err(),
    ];

    assert_eq!(BLOCKERS.len(), live.len());
    for (i, err) in live.iter().enumerate() {
        let (strategy, blocked_on) = BLOCKERS[i];
        assert_eq!(err.strategy(), Some(strategy));
        assert_eq!(err.blocked_on(), Some(blocked_on));
    }

    // Three distinct strategies with three distinct blockers. Identical strings
    // would mean somebody copy-pasted a stub and never described it.
    assert_ne!(BLOCKERS[0].0, BLOCKERS[1].0);
    assert_ne!(BLOCKERS[1].0, BLOCKERS[2].0);
    assert_ne!(BLOCKERS[0].0, BLOCKERS[2].0);
    assert_ne!(BLOCKERS[0].1, BLOCKERS[1].1);
    assert_ne!(BLOCKERS[1].1, BLOCKERS[2].1);
    assert_ne!(BLOCKERS[0].1, BLOCKERS[2].1);
}

/// The point of this crate today: nothing in it panics. Every degenerate input
/// we can think of, including the ones a stub would normally `todo!()` on, comes
/// back as an `Err`. If any of these panicked the test binary would abort rather
/// than fail an assertion, which is exactly the failure mode we are guarding.
#[test]
fn no_entry_point_panics_on_degenerate_input() {
    let wide = wide_book();
    let tight = tight_book();

    // Zero size.
    let mut p = quote_cover_params(&wide, &tight);
    p.show_size = Qty::ZERO;
    assert!(quote_cover::plan(&p).is_err());

    // Negative size.
    let mut p = quote_cover_params(&wide, &tight);
    p.show_size = Qty::parse("-1.0").unwrap();
    assert!(quote_cover::plan(&p).is_err());

    // Mismatched pairs: ETH book used to cover a BTC quote.
    let eth = book(Venue::Binance, "ETH", "USDT", "3004.00", "3006.00");
    assert!(quote_cover::plan(&quote_cover_params(&wide, &eth)).is_err());

    // Quoting the same book we cover on. Nonsense, but must not panic.
    assert!(quote_cover::plan(&quote_cover_params(&tight, &tight)).is_err());

    // Listing with every field emptied out in turn.
    let mut ev = listing_event();
    ev.kind = ListingKind::FirstListingAnywhere;
    assert!(listings::plan(&ev, &listing_params()).is_err());

    let mut ev = listing_event();
    ev.reference = None;
    assert!(listings::plan(&ev, &listing_params()).is_err());

    let mut lp = listing_params();
    lp.hedge = None;
    assert!(listings::plan(&listing_event(), &lp).is_err());

    let mut lp = listing_params();
    lp.max_size = Qty::ZERO;
    assert!(listings::plan(&listing_event(), &lp).is_err());

    let mut lp = listing_params();
    lp.max_notional = Money::ZERO;
    assert!(listings::plan(&listing_event(), &lp).is_err());

    let mut lp = listing_params();
    lp.reaction_budget = Millis::from_millis(-5);
    assert!(listings::plan(&listing_event(), &lp).is_err());

    // Trading start before the announcement: incoherent calendar.
    let mut ev = listing_event();
    ev.trading_starts_at = ev.announced_at.add_millis(Millis::from_millis(-1));
    assert!(listings::plan(&ev, &listing_params()).is_err());

    // DEX-CEX degenerate inputs.
    let cex = cex_book();
    let dq = dex_quote();

    let mut dp = dex_cex_params(&dq, &cex);
    dp.size = Qty::ZERO;
    assert!(dex_cex::plan(&dp).is_err());

    // Size the pool was not simulated for.
    let mut dp = dex_cex_params(&dq, &cex);
    dp.size = Qty::parse("3.0").unwrap();
    assert!(dex_cex::plan(&dp).is_err());

    let mut zero_price = dex_quote();
    zero_price.effective_price = Price::ZERO;
    assert!(dex_cex::plan(&dex_cex_params(&zero_price, &cex)).is_err());

    let mut zero_block = dex_quote();
    zero_block.block_time = Millis::ZERO;
    assert!(dex_cex::plan(&dex_cex_params(&zero_block, &cex)).is_err());

    // Reversed direction: takes the ask side instead of the bid.
    let mut dp = dex_cex_params(&dq, &cex);
    dp.direction = DexCexDirection::BuyCexSellDex;
    assert!(dex_cex::plan(&dp).is_err());
}

#[test]
fn quote_cover_needs_both_sides_of_the_quoting_book() {
    // Bids only. A resting quote is priced between the two touches, so a
    // one-sided book is not enough — and this must be MissingData, not
    // NotImplemented: the caller can fix it.
    let one_sided = BookSnapshot::new(
        vkey(Venue::Bybit, "BTC", "USDT"),
        vec![Level::new(
            Price::parse("68050.00").unwrap(),
            Qty::parse("1.5").unwrap(),
        )
        .unwrap()],
        Vec::new(),
        None,
        None,
        LocalTs::from_millis(1_700_000_000_000),
        DataQuality::SYNTHESIZED,
    )
    .unwrap();

    let tight = tight_book();
    let err = quote_cover::plan(&quote_cover_params(&one_sided, &tight)).unwrap_err();
    assert!(!err.is_not_implemented(), "got: {err}");
    assert!(matches!(err, PrivateError::MissingData { .. }));
}

#[test]
fn dex_cex_refuses_without_a_measured_mev_haircut() {
    let dex = dex_quote();
    let cex = cex_book();

    let mut params = dex_cex_params(&dex, &cex);
    params.mev_haircut = None;
    let err = dex_cex::plan(&params).unwrap_err();
    assert!(
        matches!(err, PrivateError::MissingData { .. }),
        "got: {err}"
    );
    assert!(err.to_string().contains("MEV"), "got: {err}");

    // Gas is paid in ETH, the pair quotes in USDT: without a conversion price
    // the gas leg cannot enter the edge at all.
    let mut params = dex_cex_params(&dex, &cex);
    params.gas_in_quote = None;
    assert!(matches!(
        dex_cex::plan(&params).unwrap_err(),
        PrivateError::MissingData { .. }
    ));
}

#[test]
fn leg_sides_are_opposites() {
    // A resting buy on the wide venue leaves us long there, so the cover is a
    // sell on the tight venue.
    let wide = wide_book();
    let tight = tight_book();
    let params = quote_cover_params(&wide, &tight);
    assert!(matches!(params.quote_side, Side::Buy));
    assert!(matches!(params.cover_side(), Side::Sell));

    // Buying on the pool means selling on the CEX, and a sell hits bids.
    let d = DexCexDirection::BuyDexSellCex;
    assert!(matches!(d.cex_side(), Side::Sell));
    assert!(matches!(d.dex_side(), Side::Buy));
    let d = DexCexDirection::BuyCexSellDex;
    assert!(matches!(d.cex_side(), Side::Buy));
    assert!(matches!(d.dex_side(), Side::Sell));
}

#[test]
fn listing_lead_time_is_exact() {
    // 1_700_003_600_000 - 1_700_000_000_000 = 3_600_000 ms.
    let ev = listing_event();
    assert_eq!(ev.lead_time().unwrap().millis(), 3_600_000);

    // Start one millisecond before the announcement: no usable lead time.
    let mut bad = listing_event();
    bad.trading_starts_at = bad.announced_at.add_millis(Millis::from_millis(-1));
    assert!(bad.lead_time().is_none());
}

/// The conversion the missing gas cost model would have to perform, done by
/// hand here so the numbers are on record: gas of 0.0021 ETH at 3_000.00
/// USDT/ETH is 0.0021 * 3000 = 6.30 USDT exactly. Exact, so `RoundMode::COST`
/// changes nothing — which is the only reason the mode is not load-bearing in
/// this vector.
///
/// It goes through `reinterpret_as_qty` because gas is a `Money` amount of ETH
/// and there is no `Money x Price` operation: the ETH amount has to be treated
/// as a quantity of ETH before it can be multiplied by a USDT-per-ETH price.
/// If DEX-CEX ships, that dimension crossing belongs in a named helper in
/// `arb-math`, not scattered at call sites.
#[test]
fn gas_converts_into_quote_asset_exactly() {
    let dex = dex_quote();
    let eth_qty = dex.gas_cost.reinterpret_as_qty();
    let in_usdt = eth_qty
        .mul_price(Price::parse("3000.00").unwrap(), RoundMode::COST)
        .unwrap();
    assert_eq!(in_usdt, Money::parse("6.30").unwrap());
}

#[test]
fn error_conversions_keep_the_cause() {
    let num_err = Price::MAX.checked_add(Price::ONE).unwrap_err();
    let converted = PrivateError::from(num_err);
    assert!(matches!(converted, PrivateError::Arithmetic(_)));
    assert!(converted.to_string().contains("arithmetic"));
    assert_eq!(converted.strategy(), None);
    assert_eq!(converted.blocked_on(), None);

    // Empty asset name is a domain error, not an arithmetic one.
    let type_err = Asset::new("").unwrap_err();
    let converted = PrivateError::from(type_err);
    assert!(matches!(converted, PrivateError::Type(_)));
    assert_eq!(converted.strategy(), None);
}
