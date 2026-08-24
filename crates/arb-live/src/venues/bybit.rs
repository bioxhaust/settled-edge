#![forbid(unsafe_code)]
//! Bybit v5 public market data.
//!
//! The first adapter in this workspace that talks to a real exchange. Bybit is
//! first because it is the most complete of the three candidates: it publishes the
//! funding interval as a number rather than leaving it to be inferred, and it puts
//! every instrument filter — tick, lot, minimum notional — in one response.
//!
//! # Endpoints used
//!
//! All three are public and unauthenticated:
//!
//! | Purpose | Path |
//! |---|---|
//! | instrument specs | `/v5/market/instruments-info?category=linear` |
//! | funding + mark/index | `/v5/market/tickers?category=linear&symbol=…` |
//! | order book | `/v5/market/orderbook?category=linear&symbol=…&limit=…` |
//!
//! # Two things about Bybit's payloads that will bite a careless reader
//!
//! **Numbers arrive as strings, mostly.** Prices, sizes, rates and even timestamps
//! inside `result.list[0]` are quoted. But `retCode`, the envelope's `time`, the
//! book's `ts`/`u`/`seq`/`cts`, and — the odd one out — `fundingInterval` are bare
//! JSON numbers. [факт, проверено — три эндпоинта прочитаны живьём 2026-08-24]
//! This is exactly why [`crate::json::Json::decimal`] accepts both: a reader that
//! demanded one or the other would need a per-field table of which is which.
//!
//! **The funding interval is published twice, in two different units.**
//! `instruments-info` says `fundingInterval: 480` — **minutes**, not hours and not
//! seconds. `tickers` says `fundingIntervalHour: "8"`. They agree here, and this
//! adapter uses the minutes value because it is the finer-grained of the two: an
//! interval that is not a whole number of hours is representable in one and not in
//! the other. When both are present and disagree, the adapter has *chosen* between
//! two venue-published numbers, which is no longer a measurement, so it sets
//! [`QualityFlags::FUNDING_INTERVAL_ASSUMED`].
//!
//! Getting this field wrong is not a rounding error. `arb-math` annualises with
//! `rate * SECONDS_PER_YEAR / interval`, so reading 480 as hours instead of
//! minutes would divide the true APR by sixty.
//!
//! # Why the instrument list is fetched once, at construction
//!
//! [`ExchangeAdapter::book`] and [`ExchangeAdapter::funding`] are handed an
//! [`InstrumentKey`] and must produce a native symbol string. Deriving
//! `BTC` + `USDT` → `"BTCUSDT"` would work for most Bybit linear perps and would
//! be a guess; the venue's own `symbol` field is not a guess. So
//! [`BybitAdapter::connect`] reads `instruments-info` once and every later call
//! resolves through that list, returning [`VenueError::NotFound`] for a key Bybit
//! does not list.
//!
//! The cost is that the list is a snapshot: a symbol listed after construction is
//! invisible until the adapter is rebuilt. For a scanner that runs for minutes at
//! a time that is the right trade — new listings are not where the carry is, and
//! re-reading 600 instrument specs before every book would spend the rate limit on
//! data that changes weekly.
//!
//! # Only linear perpetuals
//!
//! `contractType` values other than `LinearPerpetual` are skipped, and the
//! `inverse` and `spot` categories are not requested at all. Inverse perps settle
//! in the base asset and take orders in quote-denominated contracts, which is a
//! different `ContractSize` and a different notional formula; admitting them here
//! on the strength of a shape that merely parses is how a size ends up wrong by
//! the price of Bitcoin. They are a separate, later change.
//!
//! # No credentials
//!
//! Nothing here signs a request. Fee tiers are therefore *assumed* at Bybit's
//! published non-VIP rate — see [`ASSUMED_MAKER_RATIO`].

use arb_num::{Bps, Money, Price, Qty, Rate};
use arb_types::{
    Asset, BookSnapshot, ContractSize, ContractSpec, DataQuality, FundingInterval, FundingSnapshot,
    Instrument, InstrumentKey, Level, MarketKind, QualityFlags, SizeUnit, Symbol, Venue, VenueKey,
    WallTs,
};
use arb_venue::{ExchangeAdapter, FeeSchedule, VenueError};

use crate::clock::Clock;
use crate::http::{Query, Transport};
use crate::json::Json;
use crate::venues::{transport_error, wire, wire_error};

/// Bybit's production API host.
pub const BASE_URL: &str = "https://api.bybit.com";

/// The only product category this adapter requests.
pub const CATEGORY: &str = "linear";

/// Deepest book Bybit's `linear` category will return per side.
///
/// A request for more than this is answered with this many, so the adapter clamps
/// and lets [`QualityFlags::PARTIAL_BOOK`] say that it did.
/// [предположение — из документации v5, границу живьём не проверяли]
pub const MAX_BOOK_DEPTH: usize = 500;

/// Instruments requested per page.
const INSTRUMENTS_PAGE_LIMIT: usize = 1000;

/// Hard stop on instrument pagination.
///
/// A venue that returns the same `nextPageCursor` forever would otherwise spin
/// until the process is killed. At 1000 per page this ceiling is far above the
/// real list size, so hitting it means the cursor protocol changed.
const MAX_INSTRUMENT_PAGES: usize = 8;

/// Maker fee assumed for Bybit USDT perpetuals: 0.02%.
///
/// Bybit's published non-VIP derivatives schedule. This is the *worst* tier —
/// there is none below it — which is the direction [`FeeSchedule::assumed`]
/// requires: assuming a better tier than we hold manufactures edge the engine
/// would then trade on, while assuming a worse one only costs us opportunity.
/// [предположение — восстановлено по памяти о тарифной странице Bybit; публичного
/// эндпоинта, раскрывающего тариф, нет, так что проверить это можно только
/// приватным ключом]
pub const ASSUMED_MAKER_RATIO: &str = "0.0002";

/// Taker fee assumed for Bybit USDT perpetuals: 0.055%. See
/// [`ASSUMED_MAKER_RATIO`].
pub const ASSUMED_TAKER_RATIO: &str = "0.00055";

/// Bybit `retCode`s where another attempt could plausibly succeed.
///
/// 10002 request expired, 10006 too many visits, 10016 internal server error,
/// 10018 IP rate limit. Everything else describes a request that is wrong rather
/// than unlucky.
/// [предположение — коды из документации v5, наблюдался живьём только 0]
const RETRYABLE_RET_CODES: [i64; 4] = [10002, 10006, 10016, 10018];

/// One listed instrument, plus the facts about it that [`ContractSpec`] has no
/// field for.
#[derive(Debug, Clone, Copy)]
struct Listed {
    instrument: Instrument,
    /// From `instruments-info`'s `fundingInterval`, converted from minutes.
    funding_interval: FundingInterval,
}

/// A read-only view of Bybit's public linear-perpetual market data.
///
/// Generic over [`Transport`] and [`Clock`] so the whole test suite runs offline
/// and deterministically: [`crate::http::StubTransport`] supplies captured
/// payloads and [`crate::clock::FixedClock`] supplies a stopped clock, so every
/// assertion below is on an exact integer rather than on a tolerance.
#[derive(Debug)]
pub struct BybitAdapter<T, C> {
    transport: T,
    clock: C,
    base_url: String,
    listed: Vec<Listed>,
}

impl<T: Transport, C: Clock> BybitAdapter<T, C> {
    /// Read Bybit's instrument list and build an adapter around it.
    ///
    /// One HTTPS round trip per page of instruments, at construction. Fails rather
    /// than degrading: an adapter with a half-read symbol table would answer
    /// [`VenueError::NotFound`] for instruments that exist, which reads as "Bybit
    /// does not list ETHUSDT" and is worse than a startup error.
    pub fn connect(transport: T, clock: C) -> Result<Self, VenueError> {
        Self::connect_to(BASE_URL, transport, clock)
    }

    /// [`BybitAdapter::connect`] against a specific host, for tests and for
    /// Bybit's testnet.
    pub fn connect_to(base_url: &str, transport: T, clock: C) -> Result<Self, VenueError> {
        let mut adapter = Self {
            transport,
            clock,
            base_url: base_url.trim_end_matches('/').to_string(),
            listed: Vec::new(),
        };
        adapter.listed = adapter.load_instruments()?;
        Ok(adapter)
    }

    /// How many instruments this adapter resolved at construction.
    pub fn listed_count(&self) -> usize {
        self.listed.len()
    }

    /// The funding interval Bybit publishes for `key`, in its own units.
    ///
    /// Exposed because it is the one instrument fact that
    /// [`ExchangeAdapter::instruments`] cannot carry — [`ContractSpec`] has no
    /// funding field — and a caller that wants to sanity-check an annualisation
    /// needs it.
    pub fn funding_interval(&self, key: &InstrumentKey) -> Option<FundingInterval> {
        self.find(key).ok().map(|l| l.funding_interval)
    }

    fn find(&self, key: &InstrumentKey) -> Result<&Listed, VenueError> {
        self.listed
            .iter()
            .find(|l| l.instrument.vkey.key == *key)
            .ok_or(VenueError::NotFound)
    }

    fn get(&self, path: &str, query: &Query) -> Result<Json, VenueError> {
        let url = format!("{}{}{}", self.base_url, path, query.build());
        let body = self.transport.get(&url).map_err(transport_error)?;
        envelope(&body)
    }

    fn load_instruments(&self) -> Result<Vec<Listed>, VenueError> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        for page in 0..MAX_INSTRUMENT_PAGES {
            let mut q = Query::new()
                .push("category", CATEGORY)
                .push("limit", &INSTRUMENTS_PAGE_LIMIT.to_string());
            if !cursor.is_empty() {
                q = q.push("cursor", &cursor);
            }
            let doc = self.get("/v5/market/instruments-info", &q)?;
            let (mut page_items, next) = parse_instruments(&doc)?;
            out.append(&mut page_items);

            if next.is_empty() {
                return Ok(out);
            }
            // A cursor that does not advance is a protocol change, not a long
            // list. Treating it as the end would silently truncate the symbol
            // table; erroring names the problem.
            if next == cursor {
                return Err(wire(format!(
                    "instruments-info returned the same nextPageCursor twice on page {}",
                    page + 1
                )));
            }
            cursor = next;
        }
        Err(wire(format!(
            "instruments-info did not terminate within {MAX_INSTRUMENT_PAGES} pages"
        )))
    }
}

impl<T: Transport, C: Clock> ExchangeAdapter for BybitAdapter<T, C> {
    fn venue(&self) -> Venue {
        Venue::Bybit
    }

    fn instruments(&self) -> Result<Vec<Instrument>, VenueError> {
        Ok(self.listed.iter().map(|l| l.instrument).collect())
    }

    fn funding(&self, key: &InstrumentKey) -> Result<FundingSnapshot, VenueError> {
        if !key.kind.is_perp() {
            // Not `NotFound`: the symbol may well exist, the question is wrong.
            // A zero rate here would be indistinguishable from a real zero.
            return Err(VenueError::Unsupported("funding on a non-perp instrument"));
        }
        let listed = *self.find(key)?;
        let q = Query::new()
            .push("category", CATEGORY)
            .push("symbol", listed.instrument.native.as_str());
        let doc = self.get("/v5/market/tickers", &q)?;
        let observed_at = self.clock.now()?;
        parse_funding(&doc, &listed, observed_at)
    }

    fn book(&self, key: &InstrumentKey, depth: usize) -> Result<BookSnapshot, VenueError> {
        if depth == 0 {
            return Err(VenueError::Unsupported("book depth 0"));
        }
        let listed = *self.find(key)?;
        let limit = depth.min(MAX_BOOK_DEPTH);
        let q = Query::new()
            .push("category", CATEGORY)
            .push("symbol", listed.instrument.native.as_str())
            .push("limit", &limit.to_string());
        let doc = self.get("/v5/market/orderbook", &q)?;
        let observed_at = self.clock.now()?;
        parse_book(&doc, listed.instrument.vkey, depth, observed_at)
    }

    fn fee_schedule(&self, key: &InstrumentKey) -> Result<FeeSchedule, VenueError> {
        // Resolve first: answering with a fee for an instrument Bybit does not
        // list would let a caller price a trade it can never place.
        self.find(key)?;
        let maker = Bps::parse_ratio(ASSUMED_MAKER_RATIO)?;
        let taker = Bps::parse_ratio(ASSUMED_TAKER_RATIO)?;
        Ok(FeeSchedule::assumed(maker, taker))
    }
}

// ---------------------------------------------------------------------------
// Payload readers. Free functions taking `&Json` so every one of them is
// testable against a captured body with no transport and no clock involved.
// ---------------------------------------------------------------------------

/// Parse a v5 body and reject a non-zero `retCode`.
///
/// Bybit answers **HTTP 200 with a non-zero `retCode`** for a bad request, so
/// skipping this check would hand a caller an empty `result` and no error. That is
/// the single most important line in this module: an empty book reads as "no
/// liquidity", which is a plausible market state, and a scanner would quietly
/// stop seeing a venue.
fn envelope(body: &str) -> Result<Json, VenueError> {
    let doc = Json::parse(body).map_err(wire_error)?;
    let code = int_field(&doc, "retCode")?;
    if code == 0 {
        return Ok(doc);
    }
    let msg = doc
        .get("retMsg")
        .and_then(|m| m.string().ok())
        .unwrap_or("<no retMsg>");
    let detail = format!("bybit retCode {code}: {msg}");
    if RETRYABLE_RET_CODES.contains(&code) {
        Err(VenueError::Transport {
            retryable: true,
            detail,
        })
    } else {
        Err(VenueError::Wire(detail))
    }
}

/// `result.list`, as an array.
fn list_of(doc: &Json) -> Result<&[Json], VenueError> {
    doc.field("result")
        .and_then(|r| r.field("list"))
        .and_then(|l| l.array())
        .map_err(wire_error)
}

/// An integer field, whether the venue quoted it or not.
fn int_field(node: &Json, key: &str) -> Result<i64, VenueError> {
    let text = node.field(key).and_then(|v| v.decimal()).map_err(wire_error)?;
    text.parse::<i64>()
        .map_err(|_| wire(format!("field {key:?} is not an integer: {text:?}")))
}

/// A decimal field as text, ready for `arb-num`.
fn dec_field<'a>(node: &'a Json, key: &str) -> Result<&'a str, VenueError> {
    node.field(key).and_then(|v| v.decimal()).map_err(wire_error)
}

/// A field that must be a JSON string: a symbol, an asset code, a status.
///
/// Deliberately stricter than [`dec_field`]. `decimal()` would accept `"Trading"`
/// and `"BTCUSDT"` happily, since it only hands the text back — so using it here
/// would mean a `status` that became a bare enum number, or a `symbol` that became
/// an integer id, parsed without complaint and then compared against a string it
/// could never equal. That failure would surface as "Bybit does not list BTCUSDT",
/// which is the least informative possible symptom.
fn str_field<'a>(node: &'a Json, key: &str) -> Result<&'a str, VenueError> {
    node.field(key).and_then(|v| v.string()).map_err(wire_error)
}

/// A decimal field that the venue may leave blank.
///
/// Bybit writes `""` rather than `null` for inapplicable fields (`basisRate` and
/// `predictedDeliveryPrice` on a perpetual, for instance), so absence has two
/// spellings and [`Json::is_absent`] knows both.
fn opt_dec_field<'a>(node: &'a Json, key: &str) -> Result<Option<&'a str>, VenueError> {
    match node.get(key) {
        None => Ok(None),
        Some(v) if v.is_absent() => Ok(None),
        Some(v) => v.decimal().map(Some).map_err(wire_error),
    }
}

/// Turn `instruments-info` into instrument specs, plus the next page cursor.
///
/// Symbols that are not tradeable linear perpetuals are skipped rather than
/// rejected: Bybit lists pre-launch and delisted contracts in the same response,
/// and a `status` of `PreLaunch` on one symbol is not a reason to fail the other
/// six hundred.
fn parse_instruments(doc: &Json) -> Result<(Vec<Listed>, String), VenueError> {
    let cursor = doc
        .field("result")
        .map_err(wire_error)?
        .get("nextPageCursor")
        .map(|c| c.string().map_err(wire_error))
        .transpose()?
        .unwrap_or("")
        .to_string();

    let mut out = Vec::new();
    for entry in list_of(doc)? {
        if str_field(entry, "status")? != "Trading" {
            continue;
        }
        if str_field(entry, "contractType")? != "LinearPerpetual" {
            continue;
        }

        let native = str_field(entry, "symbol")?;
        let base = str_field(entry, "baseCoin")?;
        let quote = str_field(entry, "quoteCoin")?;
        let settle = str_field(entry, "settleCoin")?;

        let key = InstrumentKey::parse(base, quote, MarketKind::LinearPerp, settle)?;
        let vkey = VenueKey::new(Venue::Bybit, key);

        let price_filter = entry.field("priceFilter").map_err(wire_error)?;
        let lot_filter = entry.field("lotSizeFilter").map_err(wire_error)?;

        let tick_size = Price::parse(dec_field(price_filter, "tickSize")?)?;
        let lot_size = Qty::parse(dec_field(lot_filter, "qtyStep")?)?;
        let min_notional = match opt_dec_field(lot_filter, "minNotionalValue")? {
            Some(text) => Some(Money::parse(text)?),
            None => None,
        };

        let spec = ContractSpec::new(
            MarketKind::LinearPerp,
            // Bybit's linear perps take order sizes in the base coin —
            // `minOrderQty` for BTCUSDT is 0.001 **BTC**, not 0.001 contracts —
            // so one contract is one coin and the order unit is `Base`.
            // [факт, проверено — minOrderQty/qtyStep = 0.001 при цене ~79k, что
            // согласуется только с базовыми единицами]
            ContractSize::BasePerContract(Qty::ONE),
            Asset::new(settle)?,
            tick_size,
            SizeUnit::Base,
            lot_size,
            min_notional,
        )?;

        let funding_minutes = int_field(entry, "fundingInterval")?;
        let funding_interval = interval_from_minutes(funding_minutes, native)?;

        out.push(Listed {
            instrument: Instrument::new(vkey, Symbol::new(native)?, spec),
            funding_interval,
        });
    }
    Ok((out, cursor))
}

/// `fundingInterval` is in **minutes**. See the module docs on why this is worth
/// its own function.
fn interval_from_minutes(minutes: i64, symbol: &str) -> Result<FundingInterval, VenueError> {
    let secs = minutes
        .checked_mul(60)
        .and_then(|s| u32::try_from(s).ok())
        .ok_or_else(|| {
            wire(format!(
                "{symbol}: fundingInterval {minutes} minutes is not a representable duration"
            ))
        })?;
    Ok(FundingInterval::from_secs(secs)?)
}

/// Turn one `tickers` entry into a funding snapshot.
fn parse_funding(
    doc: &Json,
    listed: &Listed,
    observed_at: arb_types::LocalTs,
) -> Result<FundingSnapshot, VenueError> {
    let exchange_ts = WallTs::from_millis(int_field(doc, "time")?);
    let entry = list_of(doc)?
        .first()
        .ok_or_else(|| wire("tickers returned an empty result.list"))?;

    // Guard against a symbol mix-up in either direction: a cached native symbol
    // that no longer means what we think, or a venue echoing the wrong row.
    let symbol = str_field(entry, "symbol")?;
    if symbol != listed.instrument.native.as_str() {
        return Err(wire(format!(
            "tickers answered for {symbol:?} but {:?} was requested",
            listed.instrument.native.as_str()
        )));
    }

    let mut flags = QualityFlags::NONE;

    // Bybit's ticker `fundingRate` is the rate that will apply at
    // `nextFundingTime`, not the last settled one, so FUNDING_RATE_HISTORICAL is
    // deliberately *not* set. [предположение — по документации v5; если это
    // неверно, каждая ставка здесь опаздывает на один интервал и знак может
    // отличаться, что и есть смысл того флага]
    let rate = Rate::parse(dec_field(entry, "fundingRate")?)?;

    // The interval comes from instruments-info, in minutes. The ticker publishes
    // the same fact in whole hours; when they disagree we have picked one, and a
    // pick is not a measurement.
    let interval = listed.funding_interval;
    if let Some(hours) = opt_dec_field(entry, "fundingIntervalHour")? {
        let hours: i64 = hours
            .parse()
            .map_err(|_| wire(format!("fundingIntervalHour is not an integer: {hours:?}")))?;
        if hours.checked_mul(3600) != Some(i64::from(interval.secs())) {
            flags.insert(QualityFlags::FUNDING_INTERVAL_ASSUMED);
        }
    }

    // `"0"` means no scheduled settlement, which is distinct from a blank field.
    let next_funding_at = match opt_dec_field(entry, "nextFundingTime")? {
        Some(text) => {
            let ms: i64 = text
                .parse()
                .map_err(|_| wire(format!("nextFundingTime is not an integer: {text:?}")))?;
            if ms == 0 {
                None
            } else {
                Some(WallTs::from_millis(ms))
            }
        }
        None => None,
    };

    let mark = match opt_dec_field(entry, "markPrice")? {
        Some(text) => Some(Price::parse(text)?),
        None => {
            // Funding settles against mark, so without it the receipt would be
            // priced off the wrong basis. PRICE_UNAVAILABLE is blocking, and that
            // is the correct severity here.
            flags.insert(QualityFlags::PRICE_UNAVAILABLE);
            None
        }
    };
    // Index is informational — nothing in the carry calculation multiplies by it —
    // so its absence is not flagged.
    let index = match opt_dec_field(entry, "indexPrice")? {
        Some(text) => Some(Price::parse(text)?),
        None => None,
    };

    Ok(FundingSnapshot {
        vkey: listed.instrument.vkey,
        rate,
        interval,
        next_funding_at,
        mark,
        index,
        exchange_ts,
        observed_at,
        quality: DataQuality::MEASURED.with_flags(flags),
    })
}

/// Turn an `orderbook` body into a book snapshot.
///
/// `requested_depth` is the caller's ask, not the clamped limit: the flag has to
/// reflect what the caller wanted, because it is the caller who will size against
/// the result.
fn parse_book(
    doc: &Json,
    vkey: VenueKey,
    requested_depth: usize,
    observed_at: arb_types::LocalTs,
) -> Result<BookSnapshot, VenueError> {
    let result = doc.field("result").map_err(wire_error)?;

    let symbol = str_field(result, "s")?;
    if symbol.is_empty() {
        return Err(wire("orderbook answered with an empty symbol"));
    }

    // `ts` is when the book was current; the envelope's `time` is when the
    // response was built. The former is the one to compare against our clock.
    let exchange_ts = WallTs::from_millis(int_field(result, "ts")?);
    let sequence = u64::try_from(int_field(result, "u")?).ok();

    let bids = levels(result, "b")?;
    let asks = levels(result, "a")?;

    let mut flags = QualityFlags::NONE;
    // Set when either side came back shallower than asked for — including the
    // case where the ask exceeded MAX_BOOK_DEPTH and was clamped. Depth past the
    // last level is then unknown, not absent, and a size priced against it is a
    // lower bound on cost.
    if bids.len() < requested_depth || asks.len() < requested_depth {
        flags.insert(QualityFlags::PARTIAL_BOOK);
    }

    Ok(BookSnapshot::new(
        vkey,
        bids,
        asks,
        sequence,
        Some(exchange_ts),
        observed_at,
        DataQuality::MEASURED.with_flags(flags),
    )?)
}

/// One side of a Bybit book: an array of `[price, size]` string pairs.
fn levels(result: &Json, key: &str) -> Result<Vec<Level>, VenueError> {
    let raw = result.field(key).and_then(|v| v.array()).map_err(wire_error)?;
    let mut out = Vec::with_capacity(raw.len());
    for (i, pair) in raw.iter().enumerate() {
        let pair = pair.array().map_err(wire_error)?;
        let (Some(p), Some(q)) = (pair.first(), pair.get(1)) else {
            return Err(wire(format!(
                "book side {key:?} level {i} is not a [price, size] pair"
            )));
        };
        let price = Price::parse(p.decimal().map_err(wire_error)?)?;
        let qty = Qty::parse(q.decimal().map_err(wire_error)?)?;
        out.push(Level::new(price, qty)?);
    }
    Ok(out)
}

/// Real Bybit responses, captured verbatim, for tests in this crate and above it.
///
/// # Why these are public
///
/// A crate that consumes [`BybitAdapter`] has to test its own handling of a Bybit
/// snapshot somehow, and the only alternatives to real captured bytes are inventing
/// a payload from memory of the API shape or reaching the network from a unit test.
/// This repository has already paid for the first: a whole round of fixtures was
/// written from recollection, and `FIXTURES-UNVERIFIED.md` exists because none of
/// it could be trusted. So the bytes that were actually measured are exported
/// rather than hidden behind `#[cfg(test)]`.
///
/// These are **not** market data. They are a fixed instant, useful only for
/// asserting that code handles a real payload shape. Anything built from them
/// carries [`arb_types::Provenance::LiveMeasured`] — which is accurate about where
/// the *bytes* came from and says nothing about whether the *prices* are current.
/// A caller that renders them into a report must not present that report as a
/// market view; the timestamps below are the tell.
pub mod captured {
    /// Captured verbatim from
    /// `GET /v5/market/instruments-info?category=linear&symbol=BTCUSDT`
    /// on 2026-08-24. Every key and every string/number distinction is as the
    /// venue sent it — including `fundingInterval` being the one bare number
    /// among the numeric fields, and `preListingInfo` being `null`.
    pub const INSTRUMENTS_BTCUSDT: &str = r#"{"retCode":0,"retMsg":"OK","result":{"category":"linear","list":[{"symbol":"BTCUSDT","contractType":"LinearPerpetual","status":"Trading","baseCoin":"BTC","quoteCoin":"USDT","launchTime":"1584230400000","deliveryTime":"0","deliveryFeeRate":"","priceScale":"2","leverageFilter":{"minLeverage":"1","maxLeverage":"150.00","leverageStep":"0.01"},"priceFilter":{"minPrice":"0.10","maxPrice":"1999999.80","tickSize":"0.10"},"lotSizeFilter":{"maxOrderQty":"1500.000","minOrderQty":"0.001","qtyStep":"0.001","postOnlyMaxOrderQty":"1500.000","maxMktOrderQty":"150.000","minNotionalValue":"5"},"unifiedMarginTrade":true,"fundingInterval":480,"settleCoin":"USDT","copyTrading":"both","upperFundingRate":"0.00333","lowerFundingRate":"-0.00333","isPreListing":false,"preListingInfo":null,"riskParameters":{"priceLimitRatioX":"0.01","priceLimitRatioY":"0.02"},"displayName":"","symbolType":"","forbidUplWithdrawal":false,"symbolId":5}],"nextPageCursor":""},"retExtInfo":{},"time":1787598168740}"#;

    /// Captured verbatim from `GET /v5/market/tickers?category=linear&symbol=BTCUSDT`
    /// on 2026-08-24. All 32 fields, including the eight that a perpetual leaves
    /// as `""`.
    pub const TICKERS_BTCUSDT: &str = r#"{"retCode":0,"retMsg":"OK","result":{"category":"linear","list":[{"symbol":"BTCUSDT","lastPrice":"79029.20","indexPrice":"79074.80","markPrice":"79033.51","prevPrice24h":"77277.90","price24hPcnt":"0.022662","highPrice24h":"79979.20","lowPrice24h":"76606.90","prevPrice1h":"78900.50","openInterest":"48013.618","openInterestValue":"3794684758.34","singleOpenInterest":"24006.809","singleOpenInterestValue":"1897342379.17","turnover24h":"7427125157.3043","volume24h":"94761.3330","fundingRate":"-0.00000869","nextFundingTime":"1787616000000","predictedDeliveryPrice":"","basisRate":"","deliveryFeeRate":"","deliveryTime":"0","ask1Size":"0.652","bid1Price":"79029.10","ask1Price":"79029.20","bid1Size":"1.928","basis":"","preOpenPrice":"","preQty":"","curPreListingPhase":"","fundingIntervalHour":"8","basisRateYear":"","fundingCap":"0.00333"}]},"retExtInfo":{},"time":1787598839605}"#;

    /// Captured verbatim from
    /// `GET /v5/market/orderbook?category=linear&symbol=BTCUSDT&limit=3`
    /// on 2026-08-24. Bids descending, asks ascending, as `BookSnapshot` requires.
    pub const ORDERBOOK_BTCUSDT: &str = r#"{"retCode":0,"retMsg":"OK","result":{"s":"BTCUSDT","a":[["78943.10","1.330"],["78943.20","0.002"],["78943.30","0.736"]],"b":[["78943.00","0.798"],["78942.90","0.002"],["78942.70","0.001"]],"ts":1787598550002,"u":14049475,"seq":793364192999,"cts":1787598550001},"retExtInfo":{},"time":1787598550203}"#;

    /// A moment after [`ORDERBOOK_BTCUSDT`]'s `ts`, so a test that uses these needs
    /// no wall clock at all. 500 ms of age, which every staleness limit in the
    /// workspace calls fresh.
    pub const OBSERVED_MS: i64 = 1_787_598_550_500;
}

#[cfg(test)]
mod tests {
    use super::captured::{
        INSTRUMENTS_BTCUSDT as INSTRUMENTS, OBSERVED_MS, ORDERBOOK_BTCUSDT as ORDERBOOK,
        TICKERS_BTCUSDT as TICKERS,
    };
    use super::*;
    use crate::clock::FixedClock;
    use crate::http::StubTransport;
    use arb_types::{LocalTs, Provenance};

    fn btc_usdt() -> InstrumentKey {
        InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").expect("key")
    }

    fn stub() -> StubTransport {
        StubTransport::new()
            .on("instruments-info", INSTRUMENTS)
            .on("market/tickers", TICKERS)
            .on("market/orderbook", ORDERBOOK)
    }

    fn adapter() -> BybitAdapter<StubTransport, FixedClock> {
        BybitAdapter::connect(stub(), FixedClock::at_millis(OBSERVED_MS)).expect("connect")
    }

    #[test]
    fn the_instrument_spec_is_read_and_not_inferred() {
        let a = adapter();
        assert_eq!(a.venue(), Venue::Bybit);
        let all = a.instruments().expect("instruments");
        assert_eq!(all.len(), 1);

        let i = all[0];
        assert_eq!(i.native.as_str(), "BTCUSDT");
        assert_eq!(i.vkey.venue, Venue::Bybit);
        assert_eq!(i.vkey.key, btc_usdt());
        assert_eq!(i.spec.kind, MarketKind::LinearPerp);

        // tickSize "0.10" at scale 1e12.
        assert_eq!(i.spec.tick_size.raw(), 100_000_000_000);
        // qtyStep "0.001".
        assert_eq!(i.spec.lot_size.raw(), 1_000_000_000);
        // minNotionalValue "5".
        assert_eq!(
            i.spec.min_notional.map(|m| m.raw()),
            Some(5_000_000_000_000)
        );
        assert_eq!(i.spec.settle, Asset::new("USDT").expect("asset"));

        // The pair that makes notional price-dependent exactly once.
        assert_eq!(i.spec.size, ContractSize::BasePerContract(Qty::ONE));
        assert_eq!(i.spec.size_unit, SizeUnit::Base);
    }

    #[test]
    fn four_hundred_and_eighty_is_minutes_and_a_naive_reading_would_be_sixty_times_wrong() {
        let a = adapter();
        let interval = a.funding_interval(&btc_usdt()).expect("interval");

        // 480 minutes == 8 hours == 28800 seconds.
        assert_eq!(interval.secs(), 28_800);
        assert_eq!(interval, FundingInterval::EIGHT_HOURS);
        assert_eq!(interval.periods_per_year_exact(), Some(1095));

        // What the two plausible misreadings would have produced. Neither is a
        // near miss: hours would be 60x too long and price the APR at a
        // sixtieth, seconds would be 60x too short.
        assert_ne!(interval.secs(), 480 * 3600);
        assert_ne!(interval.secs(), 480);
    }

    #[test]
    fn the_funding_snapshot_is_the_venues_numbers_exactly() {
        let f = adapter().funding(&btc_usdt()).expect("funding");

        assert_eq!(f.vkey.venue, Venue::Bybit);
        // "-0.00000869" at scale 1e12. A negative rate means shorts pay longs,
        // and it must survive with its sign: dropping it inverts which leg earns.
        assert_eq!(f.rate.raw(), -8_690_000);
        assert!(f.rate.is_negative());

        assert_eq!(f.interval, FundingInterval::EIGHT_HOURS);
        assert_eq!(
            f.next_funding_at,
            Some(WallTs::from_millis(1_787_616_000_000))
        );
        // markPrice "79033.51" and indexPrice "79074.80".
        assert_eq!(f.mark.map(|p| p.raw()), Some(79_033_510_000_000_000));
        assert_eq!(f.index.map(|p| p.raw()), Some(79_074_800_000_000_000));

        // The envelope's `time`, not a field inside the row.
        assert_eq!(f.exchange_ts, WallTs::from_millis(1_787_598_839_605));
        assert_eq!(f.observed_at, LocalTs::from_millis(OBSERVED_MS));

        // The whole point of this crate: a measured number, with no warnings.
        assert_eq!(f.quality.provenance, Provenance::LiveMeasured);
        assert_eq!(f.quality.flags, QualityFlags::NONE);
        assert!(f.quality.is_trustworthy());
    }

    #[test]
    fn the_two_published_intervals_agree_and_disagreement_is_flagged() {
        // They agree in the captured payload, so no flag.
        let f = adapter().funding(&btc_usdt()).expect("funding");
        assert!(!f
            .quality
            .flags
            .contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));

        // Now make the ticker claim 4 hours while instruments-info still says 480
        // minutes. Neither number is ours, so choosing between them is an
        // assumption and must say so.
        let disagreeing = TICKERS.replace(r#""fundingIntervalHour":"8""#, r#""fundingIntervalHour":"4""#);
        assert_ne!(disagreeing, TICKERS, "the replacement must actually apply");
        let a = BybitAdapter::connect(
            StubTransport::new()
                .on("instruments-info", INSTRUMENTS)
                .on("market/tickers", &disagreeing)
                .on("market/orderbook", ORDERBOOK),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect("connect");

        let f = a.funding(&btc_usdt()).expect("funding");
        assert!(f
            .quality
            .flags
            .contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));
        // The finer-grained source still wins.
        assert_eq!(f.interval.secs(), 28_800);
    }

    #[test]
    fn a_price_reaches_arb_num_without_ever_being_a_float() {
        let b = adapter().book(&btc_usdt(), 3).expect("book");

        let best_bid = b.best_bid().expect("bid");
        let best_ask = b.best_ask().expect("ask");
        // "78943.00" and "78943.10" at scale 1e12 — exact, and one tick apart.
        assert_eq!(best_bid.price.raw(), 78_943_000_000_000_000);
        assert_eq!(best_ask.price.raw(), 78_943_100_000_000_000);
        assert_eq!(
            best_ask.price.raw() - best_bid.price.raw(),
            100_000_000_000,
            "the spread must be exactly one tickSize"
        );
        // "0.798" and "1.330".
        assert_eq!(best_bid.qty.raw(), 798_000_000_000);
        assert_eq!(best_ask.qty.raw(), 1_330_000_000_000);
    }

    #[test]
    fn the_book_carries_the_venues_sequence_and_its_own_timestamp() {
        let b = adapter().book(&btc_usdt(), 3).expect("book");

        assert_eq!(b.vkey.venue, Venue::Bybit);
        assert_eq!(b.bids().len(), 3);
        assert_eq!(b.asks().len(), 3);
        // `u`, the update id — not `seq` and not `cts`.
        assert_eq!(b.sequence, Some(14_049_475));
        // `result.ts`, when the book was current; the envelope's `time` is 201ms
        // later and is not the right instant to measure skew against.
        assert_eq!(b.exchange_ts, Some(WallTs::from_millis(1_787_598_550_002)));
        assert_eq!(b.observed_at, LocalTs::from_millis(OBSERVED_MS));

        assert_eq!(b.quality.provenance, Provenance::LiveMeasured);
        assert_eq!(b.quality.flags, QualityFlags::NONE);
    }

    #[test]
    fn the_ladder_is_ordered_so_arb_book_can_walk_it() {
        let b = adapter().book(&btc_usdt(), 3).expect("book");
        for w in b.bids().windows(2) {
            assert!(w[0].price > w[1].price, "bids must strictly descend");
        }
        for w in b.asks().windows(2) {
            assert!(w[0].price < w[1].price, "asks must strictly ascend");
        }
        assert!(!b.quality.flags.contains(QualityFlags::CROSSED_BOOK));
    }

    #[test]
    fn asking_for_more_depth_than_came_back_flags_partial() {
        // The captured payload has three levels. Asking for five means depth past
        // the third is unknown rather than absent, and a size priced against it
        // is a lower bound on cost.
        let b = adapter().book(&btc_usdt(), 5).expect("book");
        assert_eq!(b.bids().len(), 3);
        assert!(b.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
        // Not blocking — a shallow book makes an estimate conservative, not false.
        assert!(!b.quality.flags.is_blocking());

        // Asking for exactly what is there does not flag.
        let exact = adapter().book(&btc_usdt(), 3).expect("book");
        assert!(!exact.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
    }

    #[test]
    fn a_depth_of_zero_is_a_wrong_question_not_an_empty_book() {
        assert_eq!(
            adapter().book(&btc_usdt(), 0),
            Err(VenueError::Unsupported("book depth 0"))
        );
    }

    #[test]
    fn spot_has_no_funding_and_says_so() {
        let a = adapter();
        let spot = InstrumentKey::parse("BTC", "USDT", MarketKind::Spot, "USDT").expect("key");
        // Reached before the symbol lookup: the question is wrong regardless of
        // whether Bybit lists a spot BTCUSDT, and a zero rate would be
        // indistinguishable from a real zero.
        assert_eq!(
            a.funding(&spot),
            Err(VenueError::Unsupported(
                "funding on a non-perp instrument"
            ))
        );
    }

    #[test]
    fn an_unlisted_symbol_is_not_found_rather_than_fetched() {
        let a = adapter();
        let doge = InstrumentKey::parse("DOGE", "USDT", MarketKind::LinearPerp, "USDT")
            .expect("key");
        assert_eq!(a.funding(&doge), Err(VenueError::NotFound));
        assert_eq!(a.book(&doge, 5), Err(VenueError::NotFound));
        assert_eq!(a.fee_schedule(&doge), Err(VenueError::NotFound));
    }

    #[test]
    fn the_fee_tier_is_assumed_because_no_public_endpoint_reveals_it() {
        let s = adapter().fee_schedule(&btc_usdt()).expect("fees");
        // 0.02% == 2bp, 0.055% == 5.5bp, at Bps scale 1e8.
        assert_eq!(s.maker.raw(), 200_000_000);
        assert_eq!(s.taker.raw(), 550_000_000);
        assert!(!s.tier_known);
        assert_eq!(s.flags(), QualityFlags::FEE_TIER_ASSUMED);
        // Entry on both legs, both taken: 11bp.
        assert_eq!(s.taker_both_legs().expect("bps").raw(), 1_100_000_000);
    }

    #[test]
    fn a_non_zero_ret_code_is_an_error_even_though_the_status_was_200() {
        // The failure this guard exists for: HTTP 200, empty result, and a caller
        // that would otherwise read "no liquidity" instead of "bad request".
        let body = r#"{"retCode":10001,"retMsg":"Not supported symbols","result":{},"retExtInfo":{},"time":1}"#;
        assert_eq!(
            envelope(body),
            Err(VenueError::Wire(
                "bybit retCode 10001: Not supported symbols".to_string()
            ))
        );
    }

    #[test]
    fn a_rate_limited_ret_code_is_retryable_and_a_bad_symbol_is_not() {
        let limited =
            r#"{"retCode":10006,"retMsg":"Too many visits","result":{},"retExtInfo":{},"time":1}"#;
        assert_eq!(
            envelope(limited),
            Err(VenueError::Transport {
                retryable: true,
                detail: "bybit retCode 10006: Too many visits".to_string(),
            })
        );

        let bad = r#"{"retCode":10001,"retMsg":"params error","result":{},"retExtInfo":{},"time":1}"#;
        match envelope(bad) {
            Err(VenueError::Wire(_)) => {}
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_ticker_for_the_wrong_symbol_is_refused() {
        // A symbol mix-up must not silently price ETH's funding as BTC's.
        let wrong = TICKERS.replace(r#""symbol":"BTCUSDT""#, r#""symbol":"ETHUSDT""#);
        assert_ne!(wrong, TICKERS);
        let a = BybitAdapter::connect(
            StubTransport::new()
                .on("instruments-info", INSTRUMENTS)
                .on("market/tickers", &wrong),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect("connect");

        match a.funding(&btc_usdt()) {
            Err(VenueError::Wire(detail)) => {
                assert!(detail.contains("ETHUSDT"), "detail: {detail}");
                assert!(detail.contains("BTCUSDT"), "detail: {detail}");
            }
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_mark_price_blocks_because_funding_settles_against_it() {
        let blank = TICKERS.replace(r#""markPrice":"79033.51""#, r#""markPrice":"""#);
        assert_ne!(blank, TICKERS);
        let a = BybitAdapter::connect(
            StubTransport::new()
                .on("instruments-info", INSTRUMENTS)
                .on("market/tickers", &blank),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect("connect");

        let f = a.funding(&btc_usdt()).expect("funding");
        assert_eq!(f.mark, None);
        assert!(f.quality.flags.contains(QualityFlags::PRICE_UNAVAILABLE));
        assert!(f.quality.flags.is_blocking());
        // Index alone is informational, so it is still read and still unflagged
        // on its own account.
        assert_eq!(f.index.map(|p| p.raw()), Some(79_074_800_000_000_000));
    }

    #[test]
    fn a_renamed_funding_field_fails_loudly_instead_of_defaulting_to_zero() {
        // The scenario `VenueError::Wire` exists for. A zero rate here would look
        // like a real market state and would be scanned as one.
        let renamed = TICKERS.replace(r#""fundingRate":"#, r#""funding_rate":"#);
        assert_ne!(renamed, TICKERS);
        let a = BybitAdapter::connect(
            StubTransport::new()
                .on("instruments-info", INSTRUMENTS)
                .on("market/tickers", &renamed),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect("connect");

        match a.funding(&btc_usdt()) {
            Err(VenueError::Wire(detail)) => {
                assert!(detail.contains("fundingRate"), "detail: {detail}");
            }
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_non_trading_or_non_perpetual_symbol_is_skipped_not_rejected() {
        // One tradeable perp, one halted perp, one dated future. The list must
        // still yield the tradeable one.
        let halted = INSTRUMENTS
            .replace(r#""symbol":"BTCUSDT""#, r#""symbol":"OLDUSDT""#)
            .replace(r#""status":"Trading""#, r#""status":"Closed""#)
            .replace(r#""baseCoin":"BTC""#, r#""baseCoin":"OLD""#);
        let dated = INSTRUMENTS
            .replace(r#""symbol":"BTCUSDT""#, r#""symbol":"BTC-28MAR26""#)
            .replace(
                r#""contractType":"LinearPerpetual""#,
                r#""contractType":"LinearFutures""#,
            );

        for body in [&halted, &dated] {
            let doc = Json::parse(body).expect("parse");
            let (listed, cursor) = parse_instruments(&doc).expect("instruments");
            assert!(listed.is_empty(), "should have been skipped: {body:.80}");
            assert_eq!(cursor, "");
        }

        // And the untouched payload still yields its one instrument, so the
        // filters above are not simply rejecting everything.
        let doc = Json::parse(INSTRUMENTS).expect("parse");
        assert_eq!(parse_instruments(&doc).expect("instruments").0.len(), 1);
    }

    #[test]
    fn a_cursor_that_does_not_advance_is_an_error_not_a_silent_truncation() {
        // A stub that always answers with the same non-empty cursor. Treating
        // that as the end of the list would drop instruments without saying so.
        let looping = INSTRUMENTS.replace(r#""nextPageCursor":"""#, r#""nextPageCursor":"page2""#);
        assert_ne!(looping, INSTRUMENTS);
        let err = BybitAdapter::connect(
            StubTransport::new().on("instruments-info", &looping),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect_err("must not loop forever");

        match err {
            VenueError::Wire(detail) => {
                assert!(detail.contains("nextPageCursor"), "detail: {detail}");
            }
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn every_url_this_adapter_builds_is_https_and_parameterised_safely() {
        // StubTransport enforces the HTTPS rule itself, so a successful call is
        // already proof of scheme. This pins the paths and the query shape.
        let a = adapter();
        a.funding(&btc_usdt()).expect("funding");
        a.book(&btc_usdt(), 3).expect("book");

        let seen = a.transport.requested();
        assert!(
            seen.iter().all(|u| u.starts_with("https://api.bybit.com/")),
            "unexpected host in {seen:?}"
        );
        assert!(seen
            .iter()
            .any(|u| u.contains("/v5/market/instruments-info?category=linear&limit=1000")));
        assert!(seen
            .iter()
            .any(|u| u.contains("/v5/market/tickers?category=linear&symbol=BTCUSDT")));
        assert!(seen
            .iter()
            .any(|u| u.contains("/v5/market/orderbook?category=linear&symbol=BTCUSDT&limit=3")));
    }

    #[test]
    fn a_depth_beyond_the_venue_cap_is_clamped_and_flagged() {
        let a = adapter();
        a.book(&btc_usdt(), MAX_BOOK_DEPTH + 1_000)
            .expect("book")
            .quality
            .flags
            .contains(QualityFlags::PARTIAL_BOOK)
            .then_some(())
            .expect("a clamped request must flag PARTIAL_BOOK");

        let seen = a.transport.requested();
        assert!(
            seen.iter().any(|u| u.ends_with("&limit=500")),
            "the request must be clamped to the venue cap, got {seen:?}"
        );
    }
}
