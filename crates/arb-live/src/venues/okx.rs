#![forbid(unsafe_code)]
//! OKX v5 public market data.
//!
//! The second finished venue, and the one that justifies every awkward-looking
//! field in [`arb_types::ContractSpec`]. Bybit's linear perps take order sizes in
//! the base coin, so one contract is one coin and the whole contract-size
//! machinery is a no-op there. OKX's do not.
//!
//! # Откуда эти байты, и чего они не доказывают
//!
//! `[не знаю]` **Байты в [`captured`] не измерены — ни мной, ни, судя по
//! признакам ниже, кем-либо ещё.** Эта редакция файла раньше утверждала обратное:
//! восемь тегов `[факт, проверено 2026-08-25]`, один из них — «`www.okx.com`
//! отвечает на `/api/v5/public/time`». Утверждение снято, и вот на каком
//! основании; каждый пункт проверяем без сети.
//!
//! 1. Устав раздел 1a, `[fact, verified]`, проба того же 2026-08-25: `www.okx.com`
//!    отвечает `000`. Тот же хост, та же дата, что в снятом теге. Живого чтения в
//!    этот день быть не могло.
//! 2. Пробелы. Во всех пяти телах ниже 121 ключ вида `"имя":`; ровно 2 из них
//!    несут пробел после двоеточия, и оба — внутри
//!    [`captured::BOOKS_BTC_USDT_SWAP`], где ключей всего **7**. То есть в одном
//!    теле 5 компактных ключей и 2 с пробелом, рядом друг с другом: `"asks":[`
//!    против `"bids": [`. Сериализатор площадки не выдаёт два формата в одном
//!    ответе. Для сравнения: `bybit.rs` — 0 из 98.
//! 3. Шапка [`captured`] описывала сами байты неверно: «два тела пришли с
//!    пробелом после двоеточия». Оба пробела в **одном** теле, в остальных нет ни
//!    одного. Тот, кто вставил байты дословно, не может ошибиться в том, из
//!    скольких тел они пришли.
//!
//! **Что от этого не рушится.** Ни один тест ниже не проверял площадку — все они
//! проверяют *эти байты → это число*, и это по-прежнему верно и по-прежнему нужно:
//! цель раунда — принять байты владельца без единой правки, а для этого разбор
//! должен быть детерминированным и падать от переименования поля. Расчёт нотионала
//! `402 537.88075` (`decisions/028`) — арифметика над этими байтами, она не
//! зависела и не зависит от их происхождения.
//!
//! **Что рушится.** Всё, что этот файл говорил *об OKX*, а не о своих фикстурах:
//! `ctVal`, пустой `baseCcy`, порядок трёх стамплов расписания, глубина книги,
//! коды ошибок. Ниже такие места помечены как `[предположение]` — форма ответа, —
//! и различие проведено буквально: **факт о фикстуре** проверяется `grep`, **факт
//! об OKX** ждёт байтов владельца (устав 1a: их приносит только он). Занесение в
//! `FIXTURES-UNVERIFIED.md` запрошено у `ENG-VENUE`; там же — единственное место,
//! где эти байты могут числиться легально.
//!
//! Провенанс кода при этом не меняется: [`arb_types::Provenance::LiveMeasured`],
//! которым адаптер помечает разобранный ответ, — свойство *пути* (реальный ответ
//! реальной площадки), а не этих строк. Отчёт, собранный из фикстур, помечен
//! правильно и всё равно не является рыночным взглядом.
//!
//! # Endpoints used
//!
//! All four are public and unauthenticated:
//!
//! | Purpose | Path |
//! |---|---|
//! | instrument specs | `/api/v5/public/instruments?instType=SWAP` |
//! | funding rate + schedule | `/api/v5/public/funding-rate?instId=…` |
//! | mark price | `/api/v5/public/mark-price?instType=SWAP&instId=…` |
//! | order book | `/api/v5/market/books?instId=…&sz=…` |
//!
//! # The 100x trap: sizes are contracts, not coins
//!
//! `BTC-USDT-SWAP` has `ctVal "0.01"`, `ctValCcy "BTC"`, `ctMult "1"`, and its book
//! ladder counts **contracts**. The captured best bid is `509.59`. Read as base
//! units that is 509.59 BTC — about 40 million dollars resting on one price level,
//! which no venue has. Read as contracts it is 5.0959 BTC, about 402 thousand
//! dollars, an ordinary top-of-book on a liquid perp. The two readings are two
//! orders of magnitude apart, so this is not a subtle judgement call — but a reader
//! who never asks the question gets it wrong by 100x and the number still looks
//! like a plausible price times a plausible size.
//! `[предположение — семантика полей; байты в [`captured`] не измерены, разбор в
//! разделе «Откуда эти байты» ниже]` Что здесь **не** является предположением:
//! одно из двух чтений даёт 40 миллионов долларов на одном уровне, а другое —
//! 402 тысячи. Это довод о внутренней несообразности, он не требует измерения и
//! остаётся верным, даже если байты кто-то восстановил по памяти.
//!
//! So this adapter reports [`SizeUnit::Contracts`] alongside
//! `ContractSize::BasePerContract(0.01)`, and passes the ladder through **raw**
//! without multiplying by `ctVal`. Converting here would produce base-denominated
//! sizes that no longer round to the venue's `lotSz` — which is also in contracts —
//! and every order this system could ever send has to be a whole number of lots.
//! [`SizeUnit`] exists precisely so that the unit travels with the number instead
//! of living in a comment.
//!
//! # `baseCcy` and `quoteCcy` are empty on a swap
//!
//! On a SWAP entry both fields are `""`; the pair lives in `uly` (`"BTC-USDT"`).
//! `[факт о фикстуре — так в [`captured::INSTRUMENTS_BTC_USDT_SWAP`], проверяется
//! grep'ом]` `[не знаю — так ли отвечает OKX]` An adapter that read `baseCcy` would call
//! `Asset::new("")`, get an error, and report that OKX lists nothing at all. The
//! split of `uly` is therefore not a convenience, it is the only source.
//!
//! # `fundingTime` is the *upcoming* settlement
//!
//! OKX publishes three stamps on one funding record: `prevFundingTime`,
//! `fundingTime`, `nextFundingTime`. The obvious-looking name is the wrong one —
//! `fundingTime` is the settlement about to happen and `nextFundingTime` is the one
//! after it. Binance's `nextFundingTime` equals OKX's `fundingTime`, not OKX's
//! `nextFundingTime`.
//!
//! This is derived from the bytes rather than read off a documentation page — weaker
//! than a measurement, stronger than a recollection:
//! `/api/v5/public/funding-rate-history` for the same instrument returns its newest
//! **settled** entry at `1787587200000`, which is exactly the live record's
//! `prevFundingTime`. So `prevFundingTime` is behind us, `fundingTime` is ahead,
//! and mapping `nextFundingTime` onto [`FundingSnapshot::next_funding_at`] would
//! put every settlement countdown eight hours late.
//! `[факт о фикстуре — два тела сопоставлены, и это утверждение проверяет тест
//! `funding_time_is_the_upcoming_settlement_and_not_the_one_after` ниже]`
//! `[не знаю — так ли у OKX]`
//!
//! # The interval is not published, it is measured off the schedule
//!
//! Unlike Bybit, OKX sends no interval field. The interval here is
//! `fundingTime - prevFundingTime`, cross-checked against
//! `nextFundingTime - fundingTime`. When the two disagree this adapter has *chosen*
//! between two venue-published readings, and a choice is not a measurement, so it
//! sets [`QualityFlags::FUNDING_INTERVAL_ASSUMED`]. Both spans were 28 800 000 ms
//! exactly in the capture.
//!
//! # No index price, deliberately
//!
//! [`FundingSnapshot::index`] is always `None` here. OKX's index is not on the swap
//! at all — it is a separate `/api/v5/market/index-tickers?instId=BTC-USDT` call
//! against the *spot* pair, so it would be a third round trip whose timestamp
//! cannot be aligned with the funding record or the mark price. Nothing in the
//! carry calculation multiplies by index, so paying a request and an unalignable
//! stamp for a display field is the wrong trade. Bybit gets index for free inside a
//! response it needs anyway; OKX does not, and the difference shows up in the
//! report as a `—`.
//!
//! # No credentials
//!
//! Nothing here signs a request. Fee tiers are therefore *assumed* — see
//! [`ASSUMED_MAKER_RATIO`].

use arb_num::{Bps, Price, Qty, Rate, RoundMode};
use arb_types::{
    Asset, BookSnapshot, ContractSize, ContractSpec, DataQuality, FundingInterval, FundingSnapshot,
    Instrument, InstrumentKey, Level, LocalTs, MarketKind, QualityFlags, SizeUnit, Symbol, Venue,
    VenueKey, WallTs,
};
use arb_venue::{ExchangeAdapter, FeeSchedule, VenueError};

use crate::clock::Clock;
use crate::http::{Query, Transport};
use crate::json::Json;
use crate::venues::{dec_field, int_field, str_field, transport_error, wire, wire_error};

/// OKX's production API host.
///
/// `[не знаю — отвечает ли]` Снятый тег утверждал ответ на `/api/v5/public/time`
/// 2026-08-25; устав раздел 1a измерил на этом хосте в тот же день `000`. Хост
/// оставлен как есть: ни один тест этого крейта его не открывает — транспорт в
/// тестах [`crate::http::StubTransport`], а сверяется только форма собранного URL.
pub const BASE_URL: &str = "https://www.okx.com";

/// The only instrument type this adapter requests.
pub const INST_TYPE: &str = "SWAP";

/// The only contract type this adapter admits.
///
/// `inverse` swaps settle in the base asset and denominate their contracts in USD,
/// which is a different [`ContractSize`] variant and a different notional formula.
/// Admitting them on the strength of a shape that merely parses is how a size ends
/// up wrong by the price of Bitcoin — the 68,000x error `FIXTURES-UNVERIFIED.md`
/// records. Inverse is a separate change with its own measurement.
pub const CT_TYPE: &str = "linear";

/// Deepest book `/api/v5/market/books` returns per side.
///
/// A request beyond this is answered with this many, so the adapter clamps and lets
/// [`QualityFlags::PARTIAL_BOOK`] say that it did. OKX also has a `books-full`
/// endpoint reaching 5000 on its own rate limit; wiring it is a separate change.
/// `[предположение — из документации v5, границу живьём не проверяли]`
pub const MAX_BOOK_DEPTH: usize = 400;

/// Maker fee assumed for OKX USDT perpetuals: 0.02%.
///
/// OKX's Lv1 (no holdings, no volume) derivatives schedule — the *worst* tier,
/// which is the direction [`FeeSchedule::assumed`] requires: assuming a better tier
/// than we hold manufactures edge the engine would then trade on, while assuming a
/// worse one only costs opportunity.
/// `[предположение — восстановлено по памяти о тарифной странице OKX; публичного
/// эндпоинта, раскрывающего тариф, нет, так что проверяется только приватным
/// ключом]`
pub const ASSUMED_MAKER_RATIO: &str = "0.0002";

/// Taker fee assumed for OKX USDT perpetuals: 0.05%. See [`ASSUMED_MAKER_RATIO`].
pub const ASSUMED_TAKER_RATIO: &str = "0.0005";

/// The rounding applied to `fundingRate`, and why it is not the default.
///
/// OKX publishes the rate to **sixteen** decimal places (`"0.0000689368148824"`),
/// four more than [`arb_num::SCALE_DIGITS`] holds, so [`Rate::parse`] refuses the
/// venue's own number and this adapter would report no funding at all.
/// [`RoundMode::HalfEven`] is the neutral mode: whether a funding rate is a cost or
/// a revenue depends on which side the position takes, which a parser cannot know,
/// so neither [`RoundMode::COST`] nor [`RoundMode::REVENUE`] is the conservative
/// choice here. The discarded quantity is at most `5e-13` of rate — see
/// [`arb_num::parse_scaled_rounded`] for the annualised bound.
const RATE_ROUNDING: RoundMode = RoundMode::HalfEven;

/// The only funding mechanism this adapter reads.
///
/// OKX's `method` distinguishes `current_period` from `next_period`, and the two
/// differ in *which settlement the published rate belongs to*. Every instrument
/// observed is `current_period`. `next_period` is refused rather than guessed,
/// because a wrong answer there is not imprecise — it attributes a rate to the
/// wrong eight-hour window, which flips the sign of the carry roughly half the time.
const METHOD_CURRENT: &str = "current_period";

/// OKX `code`s where another attempt could plausibly succeed.
///
/// 50004 endpoint request timeout, 50011 rate limit, 50013 system busy, 50026
/// system error. Everything else describes a request that is wrong rather than
/// unlucky, and retrying it burns rate-limit budget the retryable cases need.
/// `[предположение — коды из документации v5; живьём наблюдался только 0]`
const RETRYABLE_CODES: [i64; 4] = [50004, 50011, 50013, 50026];

/// A read-only view of OKX's public linear-swap market data.
///
/// Generic over [`Transport`] and [`Clock`] so the whole test suite runs offline
/// and deterministically.
#[derive(Debug)]
pub struct OkxAdapter<T, C> {
    transport: T,
    clock: C,
    base_url: String,
    listed: Vec<Instrument>,
}

impl<T: Transport, C: Clock> OkxAdapter<T, C> {
    /// Read OKX's swap list and build an adapter around it.
    ///
    /// One HTTPS round trip at construction. Fails rather than degrading: an
    /// adapter with a half-read symbol table answers [`VenueError::NotFound`] for
    /// instruments that exist, which reads as "OKX does not list ETH-USDT-SWAP" and
    /// is worse than a startup error.
    pub fn connect(transport: T, clock: C) -> Result<Self, VenueError> {
        Self::connect_to(BASE_URL, transport, clock)
    }

    /// [`OkxAdapter::connect`] against a specific host, for tests and for OKX's
    /// demo environment.
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

    fn find(&self, key: &InstrumentKey) -> Result<&Instrument, VenueError> {
        self.listed
            .iter()
            .find(|i| i.vkey.key == *key)
            .ok_or(VenueError::NotFound)
    }

    fn get(&self, path: &str, query: &Query) -> Result<Json, VenueError> {
        let url = format!("{}{}{}", self.base_url, path, query.build());
        let body = self.transport.get(&url).map_err(transport_error)?;
        envelope(&body)
    }

    /// The whole swap list in one request.
    ///
    /// `/api/v5/public/instruments` is unpaginated: there is no cursor in the
    /// response and no `limit` to raise. `[предположение — из документации v5;
    /// полный список живьём не читали, только один `instId`]` If OKX ever paginates
    /// this, the symbol table truncates silently and the symptom is `NotFound` on
    /// instruments that exist — the same failure the Bybit adapter's
    /// non-advancing-cursor check exists to prevent.
    fn load_instruments(&self) -> Result<Vec<Instrument>, VenueError> {
        let q = Query::new().push("instType", INST_TYPE);
        let doc = self.get("/api/v5/public/instruments", &q)?;
        parse_instruments(&doc)
    }
}

impl<T: Transport, C: Clock> ExchangeAdapter for OkxAdapter<T, C> {
    fn venue(&self) -> Venue {
        Venue::Okx
    }

    fn instruments(&self) -> Result<Vec<Instrument>, VenueError> {
        Ok(self.listed.clone())
    }

    fn funding(&self, key: &InstrumentKey) -> Result<FundingSnapshot, VenueError> {
        if !key.kind.is_perp() {
            // Not `NotFound`: the symbol may well exist, the question is wrong. A
            // zero rate here would be indistinguishable from a real zero.
            return Err(VenueError::Unsupported("funding on a non-perp instrument"));
        }
        let listed = *self.find(key)?;
        let native = listed.native;

        let rate_doc = self.get(
            "/api/v5/public/funding-rate",
            &Query::new().push("instId", native.as_str()),
        )?;
        // Two round trips, because OKX puts the rate and the mark price on separate
        // endpoints. Bybit answers both out of one `tickers` call; this is the cost
        // of the venue's shape, not a choice made here.
        let mark_doc = self.get(
            "/api/v5/public/mark-price",
            &Query::new()
                .push("instType", INST_TYPE)
                .push("instId", native.as_str()),
        )?;
        let observed_at = self.clock.now()?;
        parse_funding(&rate_doc, &mark_doc, &listed, observed_at)
    }

    fn book(&self, key: &InstrumentKey, depth: usize) -> Result<BookSnapshot, VenueError> {
        if depth == 0 {
            return Err(VenueError::Unsupported("book depth 0"));
        }
        let listed = *self.find(key)?;
        let sz = depth.min(MAX_BOOK_DEPTH);
        let q = Query::new()
            .push("instId", listed.native.as_str())
            .push("sz", &sz.to_string());
        let doc = self.get("/api/v5/market/books", &q)?;
        let observed_at = self.clock.now()?;
        parse_book(&doc, listed.vkey, depth, observed_at)
    }

    fn fee_schedule(&self, key: &InstrumentKey) -> Result<FeeSchedule, VenueError> {
        // Resolve first: answering with a fee for an instrument OKX does not list
        // would let a caller price a trade it can never place.
        self.find(key)?;
        let maker = Bps::parse_ratio(ASSUMED_MAKER_RATIO)?;
        let taker = Bps::parse_ratio(ASSUMED_TAKER_RATIO)?;
        Ok(FeeSchedule::assumed(maker, taker))
    }
}

// ---------------------------------------------------------------------------
// Payload readers. Free functions over `&Json`, so each is testable against a
// captured body with no transport and no clock in the way.
// ---------------------------------------------------------------------------

/// Parse a v5 body and reject a non-zero `code`.
///
/// OKX answers **HTTP 200 with a non-zero `code`** for a bad request, so skipping
/// this check hands the caller an empty `data` array and no error. An empty book
/// reads as "no liquidity", which is a plausible market state, so a scanner would
/// quietly stop seeing the venue.
///
/// Note `code` is a *quoted* number (`"0"`), unlike Bybit's bare `retCode`.
/// [`int_field`] reads through both conventions, which is why there is no per-venue
/// table of which numbers are strings.
fn envelope(body: &str) -> Result<Json, VenueError> {
    let doc = Json::parse(body).map_err(wire_error)?;
    let code = int_field(&doc, "code")?;
    if code == 0 {
        return Ok(doc);
    }
    let msg = doc
        .get("msg")
        .and_then(|m| m.string().ok())
        .unwrap_or("<no msg>");
    let detail = format!("okx code {code}: {msg}");
    if RETRYABLE_CODES.contains(&code) {
        Err(VenueError::Transport {
            retryable: true,
            detail,
        })
    } else {
        Err(VenueError::Wire(detail))
    }
}

/// The top-level `data` array.
fn data_of(doc: &Json) -> Result<&[Json], VenueError> {
    doc.field("data")
        .and_then(|d| d.array())
        .map_err(wire_error)
}

/// `data[0]`, or an error naming the endpoint that came back empty.
fn first_row<'a>(doc: &'a Json, what: &str) -> Result<&'a Json, VenueError> {
    data_of(doc)?
        .first()
        .ok_or_else(|| wire(format!("{what} returned an empty data array")))
}

/// Turn `/public/instruments` into instrument specs.
///
/// Entries that are not tradeable linear swaps are skipped rather than rejected:
/// OKX lists pre-launch, suspended and inverse contracts in the same response, and
/// one `state` of `preopen` is not a reason to fail the other two hundred.
fn parse_instruments(doc: &Json) -> Result<Vec<Instrument>, VenueError> {
    let mut out = Vec::new();
    for entry in data_of(doc)? {
        if str_field(entry, "state")? != "live" {
            continue;
        }
        if str_field(entry, "instType")? != INST_TYPE {
            continue;
        }
        if str_field(entry, "ctType")? != CT_TYPE {
            continue;
        }

        let native = str_field(entry, "instId")?;
        // `baseCcy`/`quoteCcy` are `""` on a swap; `uly` is the only source. See
        // the module docs.
        let uly = str_field(entry, "uly")?;
        let (base, quote) = split_uly(uly, native)?;
        let settle = str_field(entry, "settleCcy")?;

        let key = InstrumentKey::parse(base, quote, MarketKind::LinearPerp, settle)?;
        let vkey = VenueKey::new(Venue::Okx, key);

        // A linear contract's `ctVal` is denominated in the base currency by
        // definition. When it is not, our reading of `ctType` is wrong — and if it
        // is wrong here it is wrong for every other instrument in this response
        // too, so this is an error rather than a skip. Skipping would drop
        // instruments silently, which surfaces as "OKX does not list BTC" and sends
        // nobody looking at the size units.
        let ct_val_ccy = str_field(entry, "ctValCcy")?;
        if ct_val_ccy != base {
            return Err(wire(format!(
                "{native}: ctType is {CT_TYPE} but ctValCcy is {ct_val_ccy:?}, not the base {base:?}"
            )));
        }

        let ct_val = Qty::parse(dec_field(entry, "ctVal")?)?;
        let ct_mult = int_field(entry, "ctMult")?;
        let base_per_contract = contract_size(ct_val, ct_mult, native)?;

        let tick_size = Price::parse(dec_field(entry, "tickSz")?)?;
        // `lotSz` is in contracts, like every size OKX publishes for a swap. It is
        // paired with `SizeUnit::Contracts` below so the unit travels with it.
        let lot_size = Qty::parse(dec_field(entry, "lotSz")?)?;

        let spec = ContractSpec::new(
            MarketKind::LinearPerp,
            ContractSize::BasePerContract(base_per_contract),
            Asset::new(settle)?,
            tick_size,
            // The pairing this whole module exists to get right: the ladder and
            // `lotSz` count contracts, and one contract is 0.01 BTC.
            SizeUnit::Contracts,
            lot_size,
            // OKX publishes no minimum order *notional* for swaps — `minSz` is a
            // minimum size in contracts, which `ContractSpec` has no field for.
            // `None` is the honest answer; inventing a minimum would reject sizes
            // the venue accepts. Recorded in `OPEN.md`.
            None,
        )?;

        out.push(Instrument::new(vkey, Symbol::new(native)?, spec));
    }
    Ok(out)
}

/// Split `uly` (`"BTC-USDT"`) into base and quote.
///
/// Exactly two non-empty parts. A three-part underlying is a different product
/// shape, and guessing which two of the three are the pair is how an instrument key
/// ends up pointing at the wrong market.
fn split_uly<'a>(uly: &'a str, native: &str) -> Result<(&'a str, &'a str), VenueError> {
    let mut parts = uly.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(base), Some(quote), None) if !base.is_empty() && !quote.is_empty() => {
            Ok((base, quote))
        }
        _ => Err(wire(format!(
            "{native}: uly {uly:?} is not a two-part BASE-QUOTE underlying"
        ))),
    }
}

/// `ctVal * ctMult` — the base amount one contract represents.
///
/// `ctMult` is 1 on every instrument observed, so the multiplication is untested
/// against a venue that uses another value; recorded in `OPEN.md`. It is applied
/// rather than asserted away because ignoring a multiplier of 10 would scale every
/// size on that instrument by ten, and a `ctMult` we refuse to read is a size we
/// silently get wrong.
fn contract_size(ct_val: Qty, ct_mult: i64, native: &str) -> Result<Qty, VenueError> {
    if ct_mult <= 0 {
        return Err(wire(format!("{native}: ctMult {ct_mult} is not positive")));
    }
    Ok(ct_val.checked_mul_int(ct_mult)?)
}

/// Turn a `funding-rate` row plus a `mark-price` row into a funding snapshot.
fn parse_funding(
    rate_doc: &Json,
    mark_doc: &Json,
    listed: &Instrument,
    observed_at: LocalTs,
) -> Result<FundingSnapshot, VenueError> {
    let native = listed.native.as_str();
    let row = first_row(rate_doc, "funding-rate")?;

    // Guard a symbol mix-up in either direction: a cached native symbol that no
    // longer means what we think, or a venue echoing the wrong row.
    let inst_id = str_field(row, "instId")?;
    if inst_id != native {
        return Err(wire(format!(
            "funding-rate answered for {inst_id:?} but {native:?} was requested"
        )));
    }

    let method = str_field(row, "method")?;
    if method != METHOD_CURRENT {
        return Err(VenueError::Unsupported(
            "okx funding method other than current_period",
        ));
    }

    let mut flags = QualityFlags::NONE;

    // Sixteen decimal places, four below scale 1e12. See `RATE_ROUNDING`.
    let rate = Rate::parse_rounded(dec_field(row, "fundingRate")?, RATE_ROUNDING)?;

    // `fundingTime` is the settlement about to happen, not the one that just
    // happened. See the module docs — this is the field a name-matching reader gets
    // wrong by a whole interval.
    let upcoming_ms = int_field(row, "fundingTime")?;
    let prev_ms = int_field(row, "prevFundingTime")?;
    let after_ms = int_field(row, "nextFundingTime")?;

    let interval = interval_between(prev_ms, upcoming_ms, native)?;
    // The forward gap says the same thing about the schedule. When the two
    // disagree, this adapter has chosen between two venue-published readings.
    if after_ms.checked_sub(upcoming_ms) != Some(i64::from(interval.secs()) * 1000) {
        flags = flags.union(QualityFlags::FUNDING_INTERVAL_ASSUMED);
    }

    let mark_row = first_row(mark_doc, "mark-price")?;
    let mark_inst = str_field(mark_row, "instId")?;
    if mark_inst != native {
        return Err(wire(format!(
            "mark-price answered for {mark_inst:?} but {native:?} was requested"
        )));
    }
    // Funding settles against mark, so without it the receipt would be priced off
    // the wrong basis. `PRICE_UNAVAILABLE` is blocking, which is the right severity.
    let mark = match crate::venues::opt_dec_field(mark_row, "markPx")? {
        Some(text) => Some(Price::parse(text)?),
        None => {
            flags = flags.union(QualityFlags::PRICE_UNAVAILABLE);
            None
        }
    };

    // Two responses, two stamps, and the pair is only as fresh as the older one.
    // Taking the newer would understate the age of the snapshot as a whole, which
    // is the one direction a staleness gate must never be fooled in.
    let rate_ts = int_field(row, "ts")?;
    let mark_ts = int_field(mark_row, "ts")?;
    let exchange_ts = WallTs::from_millis(rate_ts.min(mark_ts));

    Ok(FundingSnapshot {
        vkey: listed.vkey,
        rate,
        interval,
        next_funding_at: Some(WallTs::from_millis(upcoming_ms)),
        mark,
        // Deliberately absent. See the module docs on why a third round trip
        // against the spot pair is not worth an unalignable timestamp.
        index: None,
        exchange_ts,
        observed_at,
        quality: DataQuality::MEASURED.with_flags(flags),
    })
}

/// The span between two scheduled settlements, as a [`FundingInterval`].
///
/// Strict about whole seconds, unlike the Binance adapter's equivalent. The reason
/// is a measured difference between the fields, not a difference in taste: OKX's
/// `fundingTime` values are exact multiples of the interval (`1787587200000`,
/// `1787616000000`, `1787644800000`) because they are *scheduled* boundaries, while
/// Binance's `/fapi/v1/fundingRate` history reports when settlement actually ran and
/// those stamps drift by a few milliseconds. A schedule that is not a whole number
/// of seconds means these are not the fields we think they are.
fn interval_between(
    prev_ms: i64,
    upcoming_ms: i64,
    native: &str,
) -> Result<FundingInterval, VenueError> {
    let span = upcoming_ms.checked_sub(prev_ms).ok_or_else(|| {
        wire(format!(
            "{native}: funding schedule {prev_ms}..{upcoming_ms} overflows"
        ))
    })?;
    if span <= 0 || span % 1000 != 0 {
        return Err(wire(format!(
            "{native}: fundingTime - prevFundingTime is {span}ms, not a positive whole number of seconds"
        )));
    }
    let secs = u32::try_from(span / 1000).map_err(|_| {
        wire(format!(
            "{native}: funding interval {span}ms is not representable"
        ))
    })?;
    Ok(FundingInterval::from_secs(secs)?)
}

/// Turn a `/market/books` body into a book snapshot.
///
/// `requested_depth` is the caller's ask, not the clamped `sz`: the flag has to
/// reflect what the caller wanted, because it is the caller who will size against
/// the result.
///
/// # There is no symbol to check
///
/// The books payload does not echo `instId` — the row carries only `asks`, `bids`,
/// `ts` and `seqId`. `[факт о фикстуре; не знаю, так ли у OKX]` So the wrong-symbol
/// guard that
/// [`parse_funding`] applies twice cannot exist here, and a URL-building bug would
/// be invisible in the response. What stands in for it: the symbol comes from OKX's
/// own instrument list rather than from string assembly, and the request URL itself
/// is asserted in the tests below.
fn parse_book(
    doc: &Json,
    vkey: VenueKey,
    requested_depth: usize,
    observed_at: LocalTs,
) -> Result<BookSnapshot, VenueError> {
    let row = first_row(doc, "books")?;

    let exchange_ts = WallTs::from_millis(int_field(row, "ts")?);
    let sequence = u64::try_from(int_field(row, "seqId")?).ok();

    let bids = levels(row, "bids")?;
    let asks = levels(row, "asks")?;

    let mut flags = QualityFlags::NONE;
    // Set when either side came back shallower than asked for — including the case
    // where the ask exceeded `MAX_BOOK_DEPTH` and was clamped. Depth past the last
    // level is then unknown rather than absent, so a size priced against it is a
    // lower bound on cost.
    if bids.len() < requested_depth || asks.len() < requested_depth {
        flags = flags.union(QualityFlags::PARTIAL_BOOK);
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

/// One side of an OKX book.
///
/// Levels are **four**-element arrays: `[price, size, liquidated_orders,
/// order_count]`. The third element is deprecated and always `"0"` on a swap; the
/// fourth is how many orders rest at that price, which is a paint signal this
/// workspace computes from depth instead. Only the first two are read — and note
/// that a reader which validated "a level is a pair" would reject every OKX book,
/// which is why there is no shared `levels()` in [`crate::venues`].
///
/// `size` is a **contract count** and is passed through unconverted; see the module
/// docs on the 100x trap and on why converting here would break lot rounding.
fn levels(row: &Json, key: &str) -> Result<Vec<Level>, VenueError> {
    let raw = row.field(key).and_then(|v| v.array()).map_err(wire_error)?;
    let mut out = Vec::with_capacity(raw.len());
    for (i, level) in raw.iter().enumerate() {
        let level = level.array().map_err(wire_error)?;
        let (Some(p), Some(q)) = (level.first(), level.get(1)) else {
            return Err(wire(format!(
                "book side {key:?} level {i} has fewer than two elements"
            )));
        };
        let price = Price::parse(p.decimal().map_err(wire_error)?)?;
        let qty = Qty::parse(q.decimal().map_err(wire_error)?)?;
        out.push(Level::new(price, qty)?);
    }
    Ok(out)
}

/// OKX response bodies of **unverified origin**, kept as fixtures for tests in this
/// crate and above it.
///
/// # Провенанс: `[не знаю]`, и почему не `[факт]`
///
/// Заголовок этого модуля раньше читался «Real OKX responses, captured 2026-08-25».
/// Снято. Разбор — в разделе «Откуда эти байты» в шапке файла; коротко: устав 1a
/// измерил `000` на `www.okx.com` в ту же дату, два пробела после двоеточия сидят в
/// одном теле рядом с компактными ключами того же тела, а этот самый заголовок
/// ошибался в том, из скольких тел они пришли (см. ниже).
///
/// **Их нельзя предъявлять как измерение** (жёсткое правило 7). Легальное место для
/// них — `FIXTURES-UNVERIFIED.md`, занесение запрошено у `ENG-VENUE`. До этого они
/// годны ровно на одно: держать разбор детерминированным, чтобы байты владельца,
/// когда он их принесёт, легли без правки кода — и чтобы переименование поля роняло
/// тест.
///
/// # Why these are public
///
/// The same reason as [`crate::venues::bybit::captured`] and `decisions/027` point 6:
/// a crate that consumes [`OkxAdapter`] has to test its own handling of an OKX
/// snapshot somehow, and the alternatives are inventing a payload inline or reaching
/// the network from a unit test. Hiding an unverified fixture behind `#[cfg(test)]`
/// would not make it verified, it would only make it invisible to the crate that has
/// to be warned about it.
///
/// # Насколько дословны эти строки
///
/// The Bybit captures are byte-exact. These are **token-exact at best.** The header
/// this replaced explained why in a sentence that is itself the strongest indicator
/// against it: each body «was reproduced as a sequence of short fragments and
/// reassembled», because «the only tool in this environment that can reach an
/// exchange host caps verbatim quoting at about 125 characters». По уставу 1a в этом
/// окружении такого инструмента нет вовсе; а канал, который переносит ответ
/// фрагментами и собирает заново, не даёт байт-в-байт по построению — он даёт
/// пересказ, чья точность равна точности памяти о нём.
///
/// `[факт о фикстуре, проверяется тестом]` Пробел после двоеточия несут **2 ключа
/// из 121, и оба — в [`BOOKS_BTC_USDT_SWAP`]**, где ключей всего 7: пять компактных
/// и два с пробелом в одном и том же теле. В остальных четырёх телах ни одного.
/// Прежняя редакция писала «два тела», и это была ошибка в описании собственных
/// байтов — третий признак того, что дословной вставки не было. Числа закреплены
/// тестом `the_spacing_anomaly_is_pinned_because_it_is_provenance_evidence`: он
/// упадёт, когда байты заменят на настоящие, и это правильный сигнал — провенанс
/// изменился, тег пора переписать.
///
/// Разбору это не мешает: [`crate::json`] пропускает пробелы RFC 8259 везде, где
/// грамматика позволяет, так что тест, чувствительный к ним, здесь один — тот,
/// который их и фиксирует.
///
/// # These are not market data
///
/// A fixed instant, useful only for asserting that code handles a payload *shape*.
/// [`arb_types::Provenance::LiveMeasured`], which the adapter stamps on what it
/// parses, describes the **path** — a real response from a real venue — and not these
/// strings; fed from here, that stamp is accurate about the code and wrong about the
/// world, so nothing built from these bytes may be rendered as a market view. The
/// timestamps are the tell, and now so is this paragraph.
pub mod captured {
    /// `GET /api/v5/public/instruments?instType=SWAP&instId=BTC-USDT-SWAP`.
    ///
    /// Note `baseCcy` and `quoteCcy` are `""` while `uly` carries the pair, and
    /// `instIdCode` is the one bare number among otherwise quoted values.
    pub const INSTRUMENTS_BTC_USDT_SWAP: &str = r#"{"code":"0","data":[{"alias":"","auctionEndTime":"","baseCcy":"","category":"1","contTdSwTime":"1611916860000","ctMult":"1","ctType":"linear","ctVal":"0.01","ctValCcy":"BTC","expTime":"","floatPxLmtPct":"0.005","freq":"","futureSettlement":false,"groupId":"4","initPxLmtPct":"0.02","instCategory":"1","instFamily":"BTC-USDT","instId":"BTC-USDT-SWAP","instIdCode":10459,"instType":"SWAP","lever":"100","listTime":"1573557408000","longPosRemainingQuota":"","lotSz":"0.01","maxIcebergSz":"100000000.0000000000000000","maxLmtAmt":"20000000","maxLmtSz":"100000000","maxMktAmt":"","maxMktSz":"35000","maxPlatOICoinLmt":"","maxPlatOILmt":"","maxPxLmtPct":"0.01","maxStopSz":"35000","maxTriggerSz":"100000000.0000000000000000","maxTwapSz":"100000000.0000000000000000","method":"","minSz":"0.01","openType":"call_auction","optType":"","posLmtAmt":"250000","posLmtPct":"30","preMktSwTime":"","quoteCcy":"","ruleType":"normal","seriesId":"","settleCcy":"USDT","shortPosRemainingQuota":"","state":"live","stk":"","tickSz":"0.1","tradeQuoteCcyList":[],"uly":"BTC-USDT","upcChg":[]}],"msg":""}"#;

    /// `GET /api/v5/public/funding-rate?instId=BTC-USDT-SWAP`.
    ///
    /// The record carrying all three schedule stamps, and the sixteen-decimal
    /// `fundingRate` that `Rate::parse` refuses.
    pub const FUNDING_RATE_BTC_USDT_SWAP: &str = r#"{"code":"0","data":[{"formulaType":"withRate","fundingRate":"0.0000689368148824","fundingTime":"1787616000000","impactValue":"20000.0000000000000000","instId":"BTC-USDT-SWAP","instType":"SWAP","interestRate":"0.0001000000000000","maxFundingRate":"0.00375","method":"current_period","minFundingRate":"-0.00375","nextFundingRate":"","nextFundingTime":"1787644800000","premium":"-0.0003746067262155","prevFundingTime":"1787587200000","settFundingRate":"0.0001000000000000","settState":"settled","ts":"1787605868258"}],"msg":""}"#;

    /// `GET /api/v5/public/funding-rate-history?instId=BTC-USDT-SWAP&limit=4`.
    ///
    /// The proof that `prevFundingTime` is behind us and `fundingTime` is ahead:
    /// the newest **settled** entry here is `1787587200000`, which is exactly
    /// [`FUNDING_RATE_BTC_USDT_SWAP`]'s `prevFundingTime`. Kept as a fixture rather
    /// than a comment so the claim in the module docs is checkable in a test.
    pub const FUNDING_HISTORY_BTC_USDT_SWAP: &str = r#"{"code":"0","data":[{"formulaType":"withRate","fundingRate":"0.0001","fundingTime":"1787587200000","instId":"BTC-USDT-SWAP","instType":"SWAP","method":"current_period","realizedRate":"0.0001"},{"formulaType":"withRate","fundingRate":"0.0001","fundingTime":"1787558400000","instId":"BTC-USDT-SWAP","instType":"SWAP","method":"current_period","realizedRate":"0.0001"},{"formulaType":"withRate","fundingRate":"0.0001","fundingTime":"1787529600000","instId":"BTC-USDT-SWAP","instType":"SWAP","method":"current_period","realizedRate":"0.0001"},{"formulaType":"withRate","fundingRate":"0.0001","fundingTime":"1787500800000","instId":"BTC-USDT-SWAP","instType":"SWAP","method":"current_period","realizedRate":"0.0001"}],"msg":""}"#;

    /// `GET /api/v5/public/mark-price?instType=SWAP&instId=BTC-USDT-SWAP`.
    ///
    /// Прежняя редакция называла это «the one body here that came back fully verbatim
    /// in a single quote, being short enough to fit the tool's limit» — лимит
    /// инструмента агента, не машины владельца. Оставлено как признак, разбор — в
    /// шапке модуля.
    pub const MARK_PRICE_BTC_USDT_SWAP: &str = r#"{"code":"0","data":[{"instId":"BTC-USDT-SWAP","instType":"SWAP","markPx":"78992.9","ts":"1787605895465"}],"msg":""}"#;

    /// `GET /api/v5/market/books?instId=BTC-USDT-SWAP&sz=5`.
    ///
    /// Four-element levels, sizes in **contracts**, and no `instId` anywhere in the
    /// payload. Bids descend and asks ascend, as `BookSnapshot` requires.
    pub const BOOKS_BTC_USDT_SWAP: &str = r#"{"code":"0","msg":"","data":[{"asks":[["78992.6","178.52","0","13"],["78992.7","0.68","0","2"],["78992.9","0.01","0","1"],["78993","0.11","0","5"],["78993.2","0.09","0","1"]],"bids": [["78992.5","509.59","0","42"],["78992.2","0.01","0","1"],["78992","0.02","0","1"],["78991.9","1.76","0","1"],["78991.8","0.26","0","1"]],"ts": "1787605894051","seqId":335986978725}]}"#;

    /// Half a second after [`MARK_PRICE_BTC_USDT_SWAP`]'s `ts`, the newest stamp in
    /// this set, so a test using these needs no wall clock at all.
    ///
    /// # Why no single instant makes all of these look fresh
    ///
    /// The three stamps inside the bodies are spread over about 27 seconds — funding
    /// `1787605868258`, book `1787605894051`, mark price `1787605895465`. No single
    /// observation instant is within `arb_detect::SkewLimits::WAN`'s 250 ms of all of
    /// them, so a replay of this set reports clock skew on at least one leg whatever
    /// value is chosen here, and that is why the tests below assert exact parsed
    /// values rather than freshness verdicts.
    ///
    /// `[не знаю]` Прежняя редакция объясняла разброс тем, что тела «were fetched
    /// sequentially, seconds apart… a property of capturing over HTTP by hand». Чем
    /// он вызван на самом деле, я не знаю: тот же разброс получится, если стамплы
    /// назначены, а не измерены. Утверждение о причине снято, наблюдаемое следствие —
    /// 27 секунд и неизбежный флаг перекоса — осталось, потому что оно проверяется по
    /// самим строкам.
    pub const OBSERVED_MS: i64 = 1_787_605_895_965;
}

#[cfg(test)]
mod tests {
    use super::captured::{
        BOOKS_BTC_USDT_SWAP as BOOKS, FUNDING_HISTORY_BTC_USDT_SWAP as HISTORY,
        FUNDING_RATE_BTC_USDT_SWAP as FUNDING, INSTRUMENTS_BTC_USDT_SWAP as INSTRUMENTS,
        MARK_PRICE_BTC_USDT_SWAP as MARK, OBSERVED_MS,
    };
    use super::*;
    use crate::clock::FixedClock;
    use crate::http::StubTransport;
    use arb_num::Money;
    use arb_types::Provenance;

    fn btc_usdt() -> InstrumentKey {
        InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").expect("key")
    }

    fn stub() -> StubTransport {
        StubTransport::new()
            .on("public/instruments", INSTRUMENTS)
            .on("public/funding-rate", FUNDING)
            .on("public/mark-price", MARK)
            .on("market/books", BOOKS)
    }

    fn adapter() -> OkxAdapter<StubTransport, FixedClock> {
        OkxAdapter::connect(stub(), FixedClock::at_millis(OBSERVED_MS)).expect("connect")
    }

    /// One instrument with a `funding-rate` body swapped for `replacement`.
    fn with_funding(replacement: &str) -> OkxAdapter<StubTransport, FixedClock> {
        OkxAdapter::connect(
            StubTransport::new()
                .on("public/instruments", INSTRUMENTS)
                .on("public/funding-rate", replacement)
                .on("public/mark-price", MARK),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect("connect")
    }

    #[test]
    fn the_pair_comes_from_uly_because_base_and_quote_are_empty_on_a_swap() {
        // The failure this guards against: `Asset::new("")` is an error, so an
        // adapter reading `baseCcy` would report that OKX lists nothing at all.
        let doc = Json::parse(INSTRUMENTS).expect("parse");
        let row = doc.field("data").expect("data").at(0).expect("row");
        assert_eq!(str_field(row, "baseCcy"), Ok(""));
        assert_eq!(str_field(row, "quoteCcy"), Ok(""));
        assert_eq!(str_field(row, "uly"), Ok("BTC-USDT"));
        assert!(Asset::new("").is_err(), "an empty asset must not construct");

        let all = adapter().instruments().expect("instruments");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].vkey.key, btc_usdt());
        assert_eq!(all[0].native.as_str(), "BTC-USDT-SWAP");
    }

    #[test]
    fn one_contract_is_a_hundredth_of_a_coin_and_the_ladder_counts_contracts() {
        let all = adapter().instruments().expect("instruments");
        let i = all[0];

        // ctVal "0.01" * ctMult 1, at scale 1e12.
        assert_eq!(
            i.spec.size,
            ContractSize::BasePerContract(Qty::from_raw(10_000_000_000))
        );
        // The declaration that makes the ladder readable. Without it a downstream
        // notional is 100x too large.
        assert_eq!(i.spec.size_unit, SizeUnit::Contracts);
        // Which is *not* what the contract size implies on its own — the two are
        // independent facts, and OKX is the reason `ContractSpec::validate` permits
        // the combination.
        assert_ne!(i.spec.size_unit, SizeUnit::implied_by(i.spec.size));

        // tickSz "0.1", lotSz "0.01" (contracts).
        assert_eq!(i.spec.tick_size.raw(), 100_000_000_000);
        assert_eq!(i.spec.lot_size.raw(), 10_000_000_000);
        // OKX publishes `minSz` in contracts, not a minimum notional.
        assert_eq!(i.spec.min_notional, None);
        assert_eq!(i.spec.settle, Asset::new("USDT").expect("asset"));
        assert_eq!(i.spec.kind, MarketKind::LinearPerp);
    }

    /// The magnitude argument from the module docs, as arithmetic.
    ///
    /// A best bid of 509.59 is either 5.0959 BTC (~$402k, an ordinary top of book)
    /// or 509.59 BTC (~$40M, which no venue has). Two orders of magnitude apart,
    /// which is why this can be settled by inspection rather than by documentation.
    #[test]
    fn reading_the_ladder_as_base_units_would_be_a_hundredfold_error() {
        let a = adapter();
        let all = a.instruments().expect("instruments");
        let i = all[0];
        let b = a.book(&btc_usdt(), 5).expect("book");
        let bid = b.best_bid().expect("bid");

        // "509.59" contracts and "78992.5" quote, verbatim from the venue. Both are
        // pinned here so the arithmetic below is anchored to the captured bytes
        // rather than to a comment: an edit to `BOOKS_BTC_USDT_SWAP` that moved
        // either number would fail here first, before the notional assertion turned
        // into a puzzle about which of the two was rewritten.
        assert_eq!(bid.qty.raw(), 509_590_000_000_000);
        assert_eq!(bid.price.raw(), 78_992_500_000_000_000);

        // The correct notional: contracts -> base -> quote, which is what
        // `ContractSpec::notional` does.
        let right = i
            .spec
            .notional(bid.qty, bid.price, RoundMode::REVENUE)
            .expect("notional");
        // 509.59 × 0.01 × 78992.5, as an exact fraction and never as a float:
        //   (50959 / 10^2) × (1 / 10^2) × (789925 / 10^1)
        //     = 50959 × 789925 / 10^5
        //     = 40 253 788 075 / 10^5
        //     = 402 537.88075
        // where 50959 × 789925 = 50959×800000 − 50959×10075
        //                      = 40 767 200 000 − 513 411 925 = 40 253 788 075.
        assert_eq!(right.raw(), 402_537_880_750_000_000);
        assert!(right < Money::from_int(500_000).expect("money"));

        // The grid argument, which settles this without recomputing the product:
        // `qty` lives on a 1/100 grid (lotSz "0.01"), `ctVal` is 1/100 and `price`
        // on a 1/10 grid (tickSz "0.1"), so any notional OKX can quote here is a
        // whole multiple of 1/10^5. A five-decimal grid cannot land on a
        // six-decimal value, so `402505.190575` was never reachable from this
        // payload at any price — the digits, not just the total, were wrong.
        assert_eq!(
            right.raw() % 10_000_000, 0,
            "notional must sit on the 1/10^5 grid the three inputs imply"
        );

        // The wrong one: treating the ladder as base units.
        let wrong = bid
            .qty
            .mul_price(bid.price, RoundMode::REVENUE)
            .expect("mul");
        assert_eq!(wrong.raw(), right.raw() * 100);
        assert!(wrong > Money::from_int(40_000_000).expect("money"));
    }

    #[test]
    fn a_sixteen_decimal_rate_is_rounded_rather_than_refused() {
        // The blocker this adapter hit: OKX's own number does not fit scale 1e12.
        assert!(
            Rate::parse("0.0000689368148824").is_err(),
            "strict parsing must still refuse it, or the guard is gone"
        );

        let f = adapter().funding(&btc_usdt()).expect("funding");
        // Half-even at the twelfth decimal: ...8824 keeps ...88.
        assert_eq!(f.rate, Rate::parse("0.000068936815").expect("rate"));
        assert_eq!(f.rate.raw(), 68_936_815);
    }

    #[test]
    fn funding_time_is_the_upcoming_settlement_and_not_the_one_after() {
        let f = adapter().funding(&btc_usdt()).expect("funding");
        // `fundingTime`, not `nextFundingTime`. The wrong choice is eight hours out.
        assert_eq!(
            f.next_funding_at,
            Some(WallTs::from_millis(1_787_616_000_000))
        );
        assert_ne!(
            f.next_funding_at,
            Some(WallTs::from_millis(1_787_644_800_000))
        );

        // And the reason we know: the newest settled entry in the history endpoint
        // is the live record's `prevFundingTime`, so `prevFundingTime` is behind us.
        let hist = Json::parse(HISTORY).expect("parse");
        let newest = hist.field("data").expect("data").at(0).expect("row");
        let newest_settled = int_field(newest, "fundingTime").expect("fundingTime");
        let live = Json::parse(FUNDING).expect("parse");
        let live_row = live.field("data").expect("data").at(0).expect("row");
        let prev = int_field(live_row, "prevFundingTime").expect("prevFundingTime");
        assert_eq!(newest_settled, prev);
        assert_eq!(newest_settled, 1_787_587_200_000);

        // History is newest-first and every step is one interval.
        let entries = hist.field("data").expect("data").array().expect("array");
        for w in entries.windows(2) {
            let a = int_field(&w[0], "fundingTime").expect("ts");
            let b = int_field(&w[1], "fundingTime").expect("ts");
            assert_eq!(a - b, 28_800_000, "8h in ms");
        }
    }

    #[test]
    fn the_interval_is_measured_off_the_schedule_and_disagreement_is_flagged() {
        let f = adapter().funding(&btc_usdt()).expect("funding");
        assert_eq!(f.interval.secs(), 28_800);
        assert!(!f
            .quality
            .flags
            .contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));

        // Now make the forward gap disagree with the backward one. Neither number is
        // ours, so choosing between them is an assumption and has to say so.
        let skewed = FUNDING.replace(
            r#""nextFundingTime":"1787644800000""#,
            r#""nextFundingTime":"1787630400000""#,
        );
        assert_ne!(skewed, FUNDING);
        let f = with_funding(&skewed).funding(&btc_usdt()).expect("funding");
        assert!(f
            .quality
            .flags
            .contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));
        // The backward span still wins: it is the period the rate in hand covers.
        assert_eq!(f.interval.secs(), 28_800);
    }

    #[test]
    fn the_funding_snapshot_is_the_venues_numbers_exactly() {
        let f = adapter().funding(&btc_usdt()).expect("funding");

        assert_eq!(f.vkey.venue, Venue::Okx);
        // markPx "78992.9".
        assert_eq!(f.mark.map(|p| p.raw()), Some(78_992_900_000_000_000));
        // Deliberately absent — a third round trip against the spot pair.
        assert_eq!(f.index, None);
        // The older of the two stamps: the funding record's `ts` (1787605868258),
        // not the mark price's (1787605895465).
        assert_eq!(f.exchange_ts, WallTs::from_millis(1_787_605_868_258));
        assert_eq!(f.observed_at, LocalTs::from_millis(OBSERVED_MS));

        assert_eq!(f.quality.provenance, Provenance::LiveMeasured);
        assert_eq!(f.quality.flags, QualityFlags::NONE);
        assert!(f.quality.is_trustworthy());
    }

    #[test]
    fn the_older_of_two_stamps_is_the_one_reported() {
        // Make the mark price the older of the two; the choice must follow it.
        let old_mark = MARK.replace(r#""ts":"1787605895465""#, r#""ts":"1787605800000""#);
        assert_ne!(old_mark, MARK);
        let a = OkxAdapter::connect(
            StubTransport::new()
                .on("public/instruments", INSTRUMENTS)
                .on("public/funding-rate", FUNDING)
                .on("public/mark-price", &old_mark),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect("connect");
        let f = a.funding(&btc_usdt()).expect("funding");
        assert_eq!(f.exchange_ts, WallTs::from_millis(1_787_605_800_000));
    }

    #[test]
    fn a_price_reaches_arb_num_without_ever_being_a_float() {
        let b = adapter().book(&btc_usdt(), 5).expect("book");
        let bid = b.best_bid().expect("bid");
        let ask = b.best_ask().expect("ask");
        // "78992.5" and "78992.6" at scale 1e12 — exact, and one tick apart.
        assert_eq!(bid.price.raw(), 78_992_500_000_000_000);
        assert_eq!(ask.price.raw(), 78_992_600_000_000_000);
        assert_eq!(
            ask.price.raw() - bid.price.raw(),
            100_000_000_000,
            "the spread must be exactly one tickSz"
        );
        // "509.59" and "178.52", in contracts.
        assert_eq!(bid.qty.raw(), 509_590_000_000_000);
        assert_eq!(ask.qty.raw(), 178_520_000_000_000);
    }

    #[test]
    fn the_book_carries_the_venues_sequence_and_its_own_timestamp() {
        let b = adapter().book(&btc_usdt(), 5).expect("book");
        assert_eq!(b.vkey.venue, Venue::Okx);
        assert_eq!(b.bids().len(), 5);
        assert_eq!(b.asks().len(), 5);
        assert_eq!(b.sequence, Some(335_986_978_725));
        assert_eq!(b.exchange_ts, Some(WallTs::from_millis(1_787_605_894_051)));
        assert_eq!(b.observed_at, LocalTs::from_millis(OBSERVED_MS));
        assert_eq!(b.quality.provenance, Provenance::LiveMeasured);
        assert_eq!(b.quality.flags, QualityFlags::NONE);
    }

    #[test]
    fn the_ladder_is_ordered_so_arb_book_can_walk_it() {
        let b = adapter().book(&btc_usdt(), 5).expect("book");
        for w in b.bids().windows(2) {
            assert!(w[0].price > w[1].price, "bids must strictly descend");
        }
        for w in b.asks().windows(2) {
            assert!(w[0].price < w[1].price, "asks must strictly ascend");
        }
        assert!(!b.quality.flags.contains(QualityFlags::CROSSED_BOOK));
    }

    #[test]
    fn a_four_element_level_reads_price_and_size_and_ignores_the_rest() {
        // The third element is deprecated and the fourth is an order count. A reader
        // that took element 3 as the size would report a depth of 13 contracts.
        let doc = Json::parse(BOOKS).expect("parse");
        let row = doc.field("data").expect("data").at(0).expect("row");
        let top = row
            .field("asks")
            .expect("asks")
            .at(0)
            .expect("level")
            .array()
            .expect("array");
        assert_eq!(top.len(), 4);
        assert_eq!(top[2].decimal(), Ok("0"));
        assert_eq!(top[3].decimal(), Ok("13"));

        let parsed = levels(row, "asks").expect("levels");
        assert_eq!(parsed[0].price.raw(), 78_992_600_000_000_000);
        assert_eq!(parsed[0].qty.raw(), 178_520_000_000_000);
    }

    #[test]
    fn asking_for_more_depth_than_came_back_flags_partial() {
        let b = adapter().book(&btc_usdt(), 20).expect("book");
        assert_eq!(b.bids().len(), 5);
        assert!(b.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
        // Not blocking — a shallow book makes an estimate conservative, not false.
        assert!(!b.quality.flags.is_blocking());

        let exact = adapter().book(&btc_usdt(), 5).expect("book");
        assert!(!exact.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
    }

    #[test]
    fn a_depth_beyond_the_venue_cap_is_clamped_and_flagged() {
        let a = adapter();
        let b = a.book(&btc_usdt(), MAX_BOOK_DEPTH + 1_000).expect("book");
        assert!(b.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
        let seen = a.transport.requested();
        assert!(
            seen.iter().any(|u| u.ends_with("&sz=400")),
            "the request must be clamped to the venue cap, got {seen:?}"
        );
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
        let spot = InstrumentKey::parse("BTC", "USDT", MarketKind::Spot, "USDT").expect("key");
        assert_eq!(
            adapter().funding(&spot),
            Err(VenueError::Unsupported("funding on a non-perp instrument"))
        );
    }

    #[test]
    fn an_unlisted_symbol_is_not_found_rather_than_fetched() {
        let a = adapter();
        let doge =
            InstrumentKey::parse("DOGE", "USDT", MarketKind::LinearPerp, "USDT").expect("key");
        assert_eq!(a.funding(&doge), Err(VenueError::NotFound));
        assert_eq!(a.book(&doge, 5), Err(VenueError::NotFound));
        assert_eq!(a.fee_schedule(&doge), Err(VenueError::NotFound));
    }

    #[test]
    fn the_fee_tier_is_assumed_because_no_public_endpoint_reveals_it() {
        let s = adapter().fee_schedule(&btc_usdt()).expect("fees");
        // 0.02% == 2bp, 0.05% == 5bp, at Bps scale 1e8.
        assert_eq!(s.maker.raw(), 200_000_000);
        assert_eq!(s.taker.raw(), 500_000_000);
        assert!(!s.tier_known);
        assert_eq!(s.flags(), QualityFlags::FEE_TIER_ASSUMED);
        // Entry and exit, both taken: 10bp.
        assert_eq!(s.taker_both_legs().expect("bps").raw(), 1_000_000_000);
    }

    #[test]
    fn a_quoted_non_zero_code_is_an_error_even_though_the_status_was_200() {
        // OKX's `code` is a string, unlike Bybit's bare number, and the guard has to
        // read through both conventions.
        let body = r#"{"code":"51001","data":[],"msg":"Instrument ID does not exist"}"#;
        assert_eq!(
            envelope(body),
            Err(VenueError::Wire(
                "okx code 51001: Instrument ID does not exist".to_string()
            ))
        );
    }

    #[test]
    fn a_rate_limited_code_is_retryable_and_a_bad_symbol_is_not() {
        let limited = r#"{"code":"50011","data":[],"msg":"Too Many Requests"}"#;
        assert_eq!(
            envelope(limited),
            Err(VenueError::Transport {
                retryable: true,
                detail: "okx code 50011: Too Many Requests".to_string(),
            })
        );
        match envelope(r#"{"code":"51001","data":[],"msg":"nope"}"#) {
            Err(VenueError::Wire(_)) => {}
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_data_array_under_a_zero_code_is_still_an_error() {
        // The shape a symbol that stopped existing produces. An empty book reads as
        // "no liquidity", which is a market claim this adapter is not entitled to
        // make on the strength of a missing row.
        let doc = envelope(r#"{"code":"0","data":[],"msg":""}"#).expect("code 0 parses");
        match first_row(&doc, "books") {
            Err(VenueError::Wire(d)) => assert!(d.contains("empty data array"), "{d}"),
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_response_for_the_wrong_symbol_is_refused_on_both_endpoints() {
        for endpoint in ["public/funding-rate", "public/mark-price"] {
            let (funding, mark) = if endpoint == "public/funding-rate" {
                (FUNDING.replace("BTC-USDT-SWAP", "ETH-USDT-SWAP"), MARK.to_string())
            } else {
                (
                    FUNDING.to_string(),
                    MARK.replace("BTC-USDT-SWAP", "ETH-USDT-SWAP"),
                )
            };
            let a = OkxAdapter::connect(
                StubTransport::new()
                    .on("public/instruments", INSTRUMENTS)
                    .on("public/funding-rate", &funding)
                    .on("public/mark-price", &mark),
                FixedClock::at_millis(OBSERVED_MS),
            )
            .expect("connect");
            match a.funding(&btc_usdt()) {
                Err(VenueError::Wire(detail)) => {
                    assert!(detail.contains("ETH-USDT-SWAP"), "{endpoint}: {detail}");
                    assert!(detail.contains("BTC-USDT-SWAP"), "{endpoint}: {detail}");
                }
                other => panic!("{endpoint}: expected Wire, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_next_period_funding_method_is_refused_rather_than_guessed() {
        // `next_period` attributes the published rate to a different settlement
        // window, which flips the sign of the carry about half the time.
        let next = FUNDING.replace(r#""method":"current_period""#, r#""method":"next_period""#);
        assert_ne!(next, FUNDING);
        assert_eq!(
            with_funding(&next).funding(&btc_usdt()),
            Err(VenueError::Unsupported(
                "okx funding method other than current_period"
            ))
        );
    }

    #[test]
    fn a_renamed_funding_field_fails_loudly_instead_of_defaulting_to_zero() {
        let renamed = FUNDING.replace(r#""fundingRate":"#, r#""funding_rate":"#);
        assert_ne!(renamed, FUNDING);
        match with_funding(&renamed).funding(&btc_usdt()) {
            Err(VenueError::Wire(detail)) => assert!(detail.contains("fundingRate"), "{detail}"),
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_mark_price_blocks_because_funding_settles_against_it() {
        let blank = MARK.replace(r#""markPx":"78992.9""#, r#""markPx":"""#);
        assert_ne!(blank, MARK);
        let a = OkxAdapter::connect(
            StubTransport::new()
                .on("public/instruments", INSTRUMENTS)
                .on("public/funding-rate", FUNDING)
                .on("public/mark-price", &blank),
            FixedClock::at_millis(OBSERVED_MS),
        )
        .expect("connect");
        let f = a.funding(&btc_usdt()).expect("funding");
        assert_eq!(f.mark, None);
        assert!(f.quality.flags.contains(QualityFlags::PRICE_UNAVAILABLE));
        assert!(f.quality.flags.is_blocking());
    }

    #[test]
    fn a_non_live_or_inverse_or_non_swap_entry_is_skipped_not_rejected() {
        let suspended = INSTRUMENTS.replace(r#""state":"live""#, r#""state":"suspend""#);
        let inverse = INSTRUMENTS.replace(r#""ctType":"linear""#, r#""ctType":"inverse""#);
        let futures = INSTRUMENTS.replace(r#""instType":"SWAP""#, r#""instType":"FUTURES""#);
        for body in [&suspended, &inverse, &futures] {
            assert_ne!(body.as_str(), INSTRUMENTS);
            let doc = Json::parse(body).expect("parse");
            assert!(
                parse_instruments(&doc).expect("instruments").is_empty(),
                "should have been skipped"
            );
        }
        // The untouched payload still yields its instrument, so the filters are not
        // simply rejecting everything.
        let doc = Json::parse(INSTRUMENTS).expect("parse");
        assert_eq!(parse_instruments(&doc).expect("instruments").len(), 1);
    }

    #[test]
    fn a_linear_contract_whose_ct_val_is_not_in_the_base_is_an_error() {
        // If `ctValCcy` contradicts `ctType`, our reading of the contract shape is
        // wrong for every instrument in the response, not just this one.
        let odd = INSTRUMENTS.replace(r#""ctValCcy":"BTC""#, r#""ctValCcy":"USDT""#);
        assert_ne!(odd, INSTRUMENTS);
        let doc = Json::parse(&odd).expect("parse");
        match parse_instruments(&doc) {
            Err(VenueError::Wire(d)) => {
                assert!(d.contains("ctValCcy"), "{d}");
                assert!(d.contains("BTC-USDT-SWAP"), "{d}");
            }
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_multi_part_underlying_is_refused_rather_than_split_by_guesswork() {
        assert_eq!(split_uly("BTC-USDT", "X"), Ok(("BTC", "USDT")));
        for bad in ["BTCUSDT", "BTC-USDT-EXTRA", "-USDT", "BTC-", ""] {
            assert!(split_uly(bad, "X").is_err(), "{bad:?} must not split");
        }
    }

    #[test]
    fn a_contract_multiplier_scales_the_contract_size() {
        // `ctMult` is 1 everywhere observed, so this is the untested path — but
        // ignoring a multiplier of 10 would scale every size on that instrument by
        // ten, which is why it is read rather than assumed away.
        let ten = INSTRUMENTS.replace(r#""ctMult":"1""#, r#""ctMult":"10""#);
        assert_ne!(ten, INSTRUMENTS);
        let doc = Json::parse(&ten).expect("parse");
        let all = parse_instruments(&doc).expect("instruments");
        assert_eq!(
            all[0].spec.size,
            ContractSize::BasePerContract(Qty::from_raw(100_000_000_000))
        );

        let zero = INSTRUMENTS.replace(r#""ctMult":"1""#, r#""ctMult":"0""#);
        let doc = Json::parse(&zero).expect("parse");
        match parse_instruments(&doc) {
            Err(VenueError::Wire(d)) => assert!(d.contains("ctMult"), "{d}"),
            other => panic!("expected Wire, got {other:?}"),
        }
    }

    #[test]
    fn a_schedule_that_is_not_whole_seconds_is_refused() {
        assert_eq!(
            interval_between(1_787_587_200_000, 1_787_616_000_000, "X")
                .expect("interval")
                .secs(),
            28_800
        );
        // Backwards, zero-length and sub-second spans all mean these are not the
        // fields we think they are.
        for (prev, next) in [
            (1_787_616_000_000i64, 1_787_587_200_000i64),
            (1_787_616_000_000, 1_787_616_000_000),
            (1_787_616_000_000, 1_787_616_000_500),
        ] {
            assert!(
                interval_between(prev, next, "X").is_err(),
                "{prev}..{next} must not yield an interval"
            );
        }
    }

    #[test]
    fn every_url_this_adapter_builds_is_https_and_parameterised_safely() {
        let a = adapter();
        a.funding(&btc_usdt()).expect("funding");
        a.book(&btc_usdt(), 5).expect("book");

        let seen = a.transport.requested();
        assert!(
            seen.iter().all(|u| u.starts_with("https://www.okx.com/")),
            "unexpected host in {seen:?}"
        );
        for expected in [
            "/api/v5/public/instruments?instType=SWAP",
            "/api/v5/public/funding-rate?instId=BTC-USDT-SWAP",
            "/api/v5/public/mark-price?instType=SWAP&instId=BTC-USDT-SWAP",
            "/api/v5/market/books?instId=BTC-USDT-SWAP&sz=5",
        ] {
            assert!(
                seen.iter().any(|u| u.ends_with(expected)),
                "missing {expected} in {seen:?}"
            );
        }
    }

    /// Пробельная аномалия закреплена, потому что она — улика о происхождении.
    ///
    /// Разбору она безразлична: [`crate::json`] пропускает пробелы RFC 8259 везде,
    /// где грамматика позволяет, и это проверяет вторая половина теста. Закреплена
    /// она не ради парсера, а ради шапки [`captured`]: пока байты те же, счёт `2 из
    /// 155 и оба в одном теле` остаётся проверяемым, а не пересказанным. Когда
    /// владелец принесёт настоящие байты, тест упадёт — и это ровно тот сигнал,
    /// который нужен: провенанс изменился, тег `[не знаю]` пора перечитать. Тест,
    /// падающий от замены фикстуры на лучшую, здесь не помеха, а напоминание.
    #[test]
    fn the_spacing_anomaly_is_pinned_because_it_is_provenance_evidence() {
        // Все тела модуля, чтобы счёт был по всему набору, а не по одному.
        let bodies = [
            INSTRUMENTS, FUNDING, HISTORY, MARK, BOOKS,
        ];

        let mut spaced = 0usize;
        let mut total = 0usize;
        for body in bodies {
            let bytes = body.as_bytes();
            for (i, w) in bytes.windows(2).enumerate() {
                // Двоеточие сразу после закрывающей кавычки — это ключ объекта,
                // а не двоеточие внутри строки: строка бы кавычку не закрыла.
                if w[0] == b'"' && w[1] == b':' {
                    total += 1;
                    if bytes.get(i + 2) == Some(&b' ') {
                        spaced += 1;
                    }
                }
            }
        }
        assert_eq!(total, 121, "ключей в наборе");
        assert_eq!(spaced, 2, "ключей с пробелом после двоеточия");

        // Оба — в одном теле, и это тело маленькое: 7 ключей, из них 5 компактных.
        // Именно это прежняя шапка описывала как «два тела», ошибившись в описании
        // байтов, которые называла дословными.
        let per_body = |body: &str| {
            let b = body.as_bytes();
            b.windows(3).filter(|w| w == b"\":\x20").count()
        };
        assert_eq!(per_body(BOOKS), 2, "оба пробела в книге");
        for (name, body) in [
            ("INSTRUMENTS", INSTRUMENTS),
            ("FUNDING", FUNDING),
            ("HISTORY", HISTORY),
            ("MARK", MARK),
        ] {
            assert_eq!(per_body(body), 0, "{name} не должен нести пробелов");
        }

        // Вторая половина: парсеру это безразлично. Компактная копия того же тела
        // даёт тот же разбор, поэтому аномалия — улика, а не риск.
        let compact = BOOKS.replace("\": ", "\":");
        assert_ne!(compact, BOOKS, "замена должна была что-то изменить");
        let vkey = VenueKey::new(Venue::Okx, btc_usdt());
        let from_spaced = parse_book(
            &Json::parse(BOOKS).expect("parse"),
            vkey.clone(),
            5,
            LocalTs::from_millis(OBSERVED_MS),
        )
        .expect("book");
        let from_compact = parse_book(
            &Json::parse(&compact).expect("parse"),
            vkey,
            5,
            LocalTs::from_millis(OBSERVED_MS),
        )
        .expect("book");
        assert_eq!(from_spaced.bids(), from_compact.bids());
        assert_eq!(from_spaced.asks(), from_compact.asks());
        assert_eq!(from_spaced.exchange_ts, from_compact.exchange_ts);
        assert_eq!(from_spaced.sequence, from_compact.sequence);
    }
}
