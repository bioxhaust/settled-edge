//! The fixture format, and the only parser for it.
//!
//! A line-oriented text format, chosen over JSON for one reason: we have no
//! external crates, so JSON would mean hand-writing a JSON parser, and a
//! hand-written JSON parser is a much larger surface of subtle bugs than a
//! hand-written INI-ish reader. The format is deliberately boring.
//!
//! ```text
//! !unverified invented-from-memory-not-checked-against-live-api
//! # comments are free-form
//! venue = binance
//!
//! [instrument]
//! base = BTC
//! quote = USDT
//! kind = linear_perp
//! settle = USDT
//! native = BTCUSDT
//! contract_size = base:1
//! tick_size = 0.1
//! lot_size = 0.001
//! min_notional = 5
//!
//! [funding]
//! rate = 0.0001
//! interval_secs = 28800
//! next_funding_at = 1735689600000
//! mark = 68121.8
//! index = 68120.9
//! exchange_ts = 1735660800000
//! interval_source = venue
//! rate_source = predicted
//!
//! [fees]
//! maker = 0.0002
//! taker = 0.00055
//! tier = unknown
//!
//! [book]
//! sequence = 12345
//! exchange_ts = 1735660800100
//! bid = 68120.0 x 1.5
//! ask = 68123.5 x 1.2
//! ```
//!
//! # Everything here is strict on purpose
//!
//! An unknown key is an error, not a warning. A duplicate key is an error. A
//! missing key is an error. There are no defaults for anything that affects a
//! number.
//!
//! That is not pedantry, it is the central defence of this crate. A tolerant
//! parser turns `takr = 0.00055` into a taker fee of zero, and a taker fee of
//! zero manufactures 5.5 basis points of edge out of a typo — against a real
//! edge of twelve. The engine would then trade on it. Every silent default in a
//! config reader is a number the operator was never shown.
//!
//! # The header line is not decoration
//!
//! Byte one of every fixture must be [`UNVERIFIED_HEADER`]. A file without it
//! is rejected outright, before any other check. The reason is hard rule 7:
//! data not obtained by measurement is always marked untrustworthy, and the
//! worst available error in this round would be to pass an invented Bybit
//! response off as a fact — because the core gets built on it and we find out
//! with real money. Making the mark load-bearing means it cannot be forgotten:
//! a fixture that drops it does not silently become trusted, it stops parsing.

use arb_num::{Bps, Money, Price, Qty, Rate};
use arb_types::{
    Asset, BookSnapshot, ContractSize, ContractSpec, DataQuality, FundingInterval,
    FundingSnapshot, Instrument, InstrumentKey, Level, LocalTs, MarketKind, QualityFlags, SizeUnit,
    Symbol, Venue, VenueKey, WallTs,
};

use crate::adapter::FeeSchedule;
use crate::error::{ParseReason, VenueError};

/// The exact first line every fixture must carry.
///
/// Compared byte-for-byte after trimming trailing whitespace only. Not a prefix
/// match: `!unverified but actually I checked this one` must fail, because a
/// per-file caveat is exactly the kind of edit that erodes a provenance marker
/// until it means nothing.
pub const UNVERIFIED_HEADER: &str = "!unverified invented-from-memory-not-checked-against-live-api";

/// The one and only quality any fixture-derived datum may start from.
///
/// This function is the funnel. Nothing in this crate constructs a
/// [`DataQuality`] any other way, and there is a test asserting that no code
/// path here can produce [`arb_types::Provenance::LiveMeasured`]. When a live
/// adapter arrives it will have its own funnel, and the two will be visibly
/// different functions rather than one function with a boolean.
#[inline]
pub fn synthesized() -> DataQuality {
    DataQuality::SYNTHESIZED
}

/// Whether the funding interval in a fixture was recalled from the venue's own
/// documentation or copied from a neighbour.
///
/// This distinction is worth a whole field because the interval is a
/// *multiplier* on the headline number. Annualising an 8h rate uses a factor of
/// 1095; if the venue actually settles hourly the true factor is 8760, and the
/// APR we print is wrong by 8x. Not 8%, 8x.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntervalSource {
    /// Recalled from the venue's own documentation. Still unverified — see
    /// `FIXTURES-UNVERIFIED.md` — but at least attributable.
    Venue,
    /// Copied from another venue because nobody remembered this one. Forces
    /// [`QualityFlags::FUNDING_INTERVAL_ASSUMED`].
    Assumed,
}

/// Whether a funding rate is the venue's forecast for the next settlement or
/// the last one that actually settled.
///
/// A historical rate is a fact about the past presented in a column headed
/// "expected return". Every funding scanner in existence blurs these two, and
/// the blur is most of why their numbers do not arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateSource {
    /// The venue's predicted rate for the coming settlement.
    Predicted,
    /// The rate that settled last time. Forces
    /// [`QualityFlags::FUNDING_RATE_HISTORICAL`].
    Historical,
}

/// The `[funding]` section, before it becomes a [`FundingSnapshot`].
///
/// A separate type from `FundingSnapshot` because it carries the two provenance
/// fields, which are facts about *our* fixture rather than about the venue and
/// have no place in a snapshot the strategy sees. They are consumed into
/// [`QualityFlags`] by [`FixtureRecord::funding_snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FundingFixture {
    /// Rate per settlement period, as a ratio. Positive means longs pay shorts.
    pub rate: Rate,
    /// How often this instrument settles.
    pub interval: FundingInterval,
    /// Next settlement on the venue's clock.
    pub next_funding_at: Option<WallTs>,
    /// Mark price, which funding is charged against.
    pub mark: Option<Price>,
    /// Index price, the spot reference.
    pub index: Option<Price>,
    /// The venue's timestamp on this payload.
    pub exchange_ts: WallTs,
    /// Where the interval came from.
    pub interval_source: IntervalSource,
    /// Whether the rate is a forecast or a settled fact.
    pub rate_source: RateSource,
}

impl FundingFixture {
    /// The flags this section forces onto anything derived from it.
    fn flags(&self) -> QualityFlags {
        let mut flags = QualityFlags::NONE;
        if self.interval_source == IntervalSource::Assumed {
            flags.insert(QualityFlags::FUNDING_INTERVAL_ASSUMED);
        }
        if self.rate_source == RateSource::Historical {
            flags.insert(QualityFlags::FUNDING_RATE_HISTORICAL);
        }
        if self.mark.is_none() || self.index.is_none() {
            // Without a mark there is nothing to charge funding against, and
            // without an index there is no basis. Either absence makes every
            // downstream number partial.
            flags.insert(QualityFlags::PRICE_UNAVAILABLE);
        }
        flags
    }
}

/// One parsed fixture file: one instrument at one venue.
///
/// Holds the raw levels rather than a [`BookSnapshot`] because a snapshot needs
/// an `observed_at` on *our* clock, and a file has no opinion about when we read
/// it. That separation is deliberate: it is what keeps [`parse_fixture`] a pure
/// function of a `&str` and keeps the clock out of the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureRecord {
    /// Venue, canonical key, native symbol, contract spec.
    pub instrument: Instrument,
    /// `None` for spot. Spot does not fund, and a zero rate would be
    /// indistinguishable from a real zero.
    pub funding: Option<FundingFixture>,
    /// Maker/taker fees. Always [`FeeSchedule::assumed`] — see
    /// [`parse_fixture`].
    pub fees: FeeSchedule,
    /// Bid levels, strictly descending. Empty when the file has no `[book]`.
    pub bids: Vec<Level>,
    /// Ask levels, strictly ascending. Empty when the file has no `[book]`.
    pub asks: Vec<Level>,
    /// Venue sequence number on the book payload.
    pub book_sequence: Option<u64>,
    /// Venue timestamp on the book payload.
    pub book_exchange_ts: Option<WallTs>,
    /// Whether the file carried a `[book]` section at all. Distinct from "the
    /// book was empty": a missing section means we never asked, an empty one
    /// would mean the venue had no liquidity.
    pub has_book: bool,
}

impl FixtureRecord {
    /// Convenience: which venue this record speaks for.
    #[inline]
    pub const fn venue(&self) -> Venue {
        self.instrument.vkey.venue
    }

    /// Convenience: the canonical cross-venue key.
    #[inline]
    pub const fn key(&self) -> InstrumentKey {
        self.instrument.vkey.key
    }

    /// Build the funding snapshot, attaching the flags this fixture forces.
    ///
    /// `observed_at` is ours to supply because the file cannot know it. Returns
    /// [`VenueError::Unsupported`] for spot.
    pub fn funding_snapshot(&self, observed_at: LocalTs) -> Result<FundingSnapshot, VenueError> {
        let f = self
            .funding
            .as_ref()
            .ok_or(VenueError::Unsupported("spot instruments do not fund"))?;
        Ok(FundingSnapshot {
            vkey: self.instrument.vkey,
            rate: f.rate,
            interval: f.interval,
            next_funding_at: f.next_funding_at,
            mark: f.mark,
            index: f.index,
            exchange_ts: f.exchange_ts,
            observed_at,
            quality: synthesized()
                .with_flags(f.flags())
                .with_flags(self.fees.flags()),
        })
    }

    /// Build a book snapshot of at most `depth` levels per side.
    ///
    /// Sets [`QualityFlags::PARTIAL_BOOK`] whenever the file held more levels
    /// than were returned, because a truncated book makes a size look fillable
    /// when it is not. `depth` of zero is refused rather than answered with an
    /// empty book, which would be indistinguishable from a venue with no
    /// liquidity.
    pub fn book_snapshot(
        &self,
        depth: usize,
        observed_at: LocalTs,
    ) -> Result<BookSnapshot, VenueError> {
        if depth == 0 {
            return Err(VenueError::Unsupported("depth must be at least 1"));
        }
        if !self.has_book {
            return Err(VenueError::Unsupported("fixture carries no [book] section"));
        }
        let truncated = self.bids.len() > depth || self.asks.len() > depth;
        let bids: Vec<Level> = self.bids.iter().take(depth).copied().collect();
        let asks: Vec<Level> = self.asks.iter().take(depth).copied().collect();

        let mut flags = self.fees.flags();
        if truncated {
            flags.insert(QualityFlags::PARTIAL_BOOK);
        }

        // `BookSnapshot::new` adds its own structural flags (BOOK_EMPTY,
        // CROSSED_BOOK) and validates ordering. We do not pre-empt it.
        BookSnapshot::new(
            self.instrument.vkey,
            bids,
            asks,
            self.book_sequence,
            self.book_exchange_ts,
            observed_at,
            synthesized().with_flags(flags),
        )
        .map_err(VenueError::from)
    }
}

// ---------------------------------------------------------------------------
// parsing
// ---------------------------------------------------------------------------

/// One `key = value` pair, with the line it came from.
///
/// The line number is carried on the *value*, not looked up later, because by
/// the time a `Rate::parse` fails we are three functions away from the loop that
/// read the file and reconstructing the position is guesswork.
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    value: String,
    line: usize,
}

/// One `[section]` and its entries, in file order.
#[derive(Debug)]
struct Section {
    name: &'static str,
    header_line: usize,
    entries: Vec<Entry>,
}

/// The sections this parser knows. Anything else is an error, not a comment.
const KNOWN_SECTIONS: [&str; 4] = ["instrument", "funding", "fees", "book"];

impl Section {
    /// Remove and return a key, if present.
    fn take(&mut self, key: &str) -> Option<Entry> {
        self.entries
            .iter()
            .position(|e| e.key == key)
            .map(|i| self.entries.remove(i))
    }

    /// Remove and return a required key, or report it missing against the
    /// section header.
    fn require(&mut self, key: &'static str) -> Result<Entry, VenueError> {
        self.take(key)
            .ok_or_else(|| VenueError::parse(self.header_line, ParseReason::MissingKey(key)))
    }

    /// Remove and return every occurrence of a repeatable key, in file order.
    fn drain_repeated(&mut self, key: &str) -> Vec<Entry> {
        let mut out = Vec::new();
        while let Some(e) = self.take(key) {
            out.push(e);
        }
        out
    }

    /// Every key not consumed by now is a key this parser does not know.
    ///
    /// This is why the extraction functions below use `require`/`take` rather
    /// than reading from a map: consuming a key is what proves it was
    /// understood, so an unknown key cannot slip through by being ignored.
    fn finish(self) -> Result<(), VenueError> {
        match self.entries.into_iter().next() {
            None => Ok(()),
            Some(e) => Err(VenueError::parse(e.line, ParseReason::UnknownKey(e.key))),
        }
    }
}

fn as_price(e: &Entry) -> Result<Price, VenueError> {
    Price::parse(&e.value).map_err(|n| VenueError::parse(e.line, ParseReason::Number(n)))
}

fn as_qty(e: &Entry) -> Result<Qty, VenueError> {
    Qty::parse(&e.value).map_err(|n| VenueError::parse(e.line, ParseReason::Number(n)))
}

fn as_money(e: &Entry) -> Result<Money, VenueError> {
    Money::parse(&e.value).map_err(|n| VenueError::parse(e.line, ParseReason::Number(n)))
}

fn as_rate(e: &Entry) -> Result<Rate, VenueError> {
    Rate::parse(&e.value).map_err(|n| VenueError::parse(e.line, ParseReason::Number(n)))
}

/// A fee, read as a **ratio** and stored as [`Bps`].
///
/// [`Bps::parse_ratio`] and not [`Bps::parse_bps`]: every venue quotes fees as a
/// ratio (`0.00055`), and the two constructors differ by a factor of 10,000.
/// Reading `0.00055` as basis points would give a taker fee of 0.000055 bps and
/// hand us 5.5 bps of edge that does not exist.
fn as_fee_ratio(e: &Entry) -> Result<Bps, VenueError> {
    Bps::parse_ratio(&e.value).map_err(|n| VenueError::parse(e.line, ParseReason::Number(n)))
}

/// An integer field. Goes through `str::parse::<i64>`, never through a float:
/// `1735689600000` is beyond f64's exact integer range at f32 and close enough
/// to trouble at f64 that we do not go near it.
fn as_i64(field: &'static str, e: &Entry) -> Result<i64, VenueError> {
    e.value.parse::<i64>().map_err(|_| {
        VenueError::parse(
            e.line,
            ParseReason::BadInteger {
                field,
                value: e.value.clone(),
            },
        )
    })
}

fn as_u64(field: &'static str, e: &Entry) -> Result<u64, VenueError> {
    e.value.parse::<u64>().map_err(|_| {
        VenueError::parse(
            e.line,
            ParseReason::BadInteger {
                field,
                value: e.value.clone(),
            },
        )
    })
}

fn as_asset(e: &Entry) -> Result<Asset, VenueError> {
    Asset::new(&e.value).map_err(|t| VenueError::parse(e.line, ParseReason::Type(t)))
}

fn bad_enum(field: &'static str, e: &Entry) -> VenueError {
    VenueError::parse(
        e.line,
        ParseReason::BadEnum {
            field,
            value: e.value.clone(),
        },
    )
}

fn as_market_kind(e: &Entry) -> Result<MarketKind, VenueError> {
    match e.value.as_str() {
        "spot" => Ok(MarketKind::Spot),
        "linear_perp" => Ok(MarketKind::LinearPerp),
        "inverse_perp" => Ok(MarketKind::InversePerp),
        _ => Err(bad_enum("kind", e)),
    }
}

/// `base` or `contracts`.
///
/// Mandatory, with no default and no inference from `contract_size`. Inferring it
/// is the defect class `decisions/019` records: on Binance one contract is one
/// coin, so a wrong inference is numerically invisible there and wrong by 10,000x
/// on Gate. A fixture that does not state the unit its ladder is denominated in
/// is not describing a venue, and is refused. See `decisions/023` item 4.
fn as_size_unit(e: &Entry) -> Result<SizeUnit, VenueError> {
    match e.value.as_str() {
        "base" => Ok(SizeUnit::Base),
        "contracts" => Ok(SizeUnit::Contracts),
        _ => Err(bad_enum("size_unit", e)),
    }
}

/// `base:QTY` or `quote:MONEY`.
///
/// The prefix is mandatory and there is no default. This one field is the
/// difference between "one contract is 0.01 BTC" and "one contract is 100 USD",
/// which for BTC is a factor of roughly 68,000, and
/// [`ContractSpec::validate`] cross-checks it against the market kind so that a
/// `quote:` size on a linear market is rejected rather than believed.
fn as_contract_size(e: &Entry) -> Result<ContractSize, VenueError> {
    let (tag, rest) = e
        .value
        .split_once(':')
        .ok_or_else(|| VenueError::parse(e.line, ParseReason::BadContractSize(e.value.clone())))?;
    let inner = Entry {
        key: e.key.clone(),
        value: rest.trim().to_string(),
        line: e.line,
    };
    match tag.trim() {
        "base" => Ok(ContractSize::BasePerContract(as_qty(&inner)?)),
        "quote" => Ok(ContractSize::QuotePerContract(as_money(&inner)?)),
        _ => Err(VenueError::parse(
            e.line,
            ParseReason::BadContractSize(e.value.clone()),
        )),
    }
}

/// `PRICE x QTY`.
fn as_level(e: &Entry) -> Result<Level, VenueError> {
    let (p, q) = e
        .value
        .split_once('x')
        .ok_or_else(|| VenueError::parse(e.line, ParseReason::BadLevel(e.value.clone())))?;
    let pe = Entry {
        key: e.key.clone(),
        value: p.trim().to_string(),
        line: e.line,
    };
    let qe = Entry {
        key: e.key.clone(),
        value: q.trim().to_string(),
        line: e.line,
    };
    Level::new(as_price(&pe)?, as_qty(&qe)?)
        .map_err(|t| VenueError::parse(e.line, ParseReason::Type(t)))
}

/// Parse one fixture file.
///
/// Pure: give it a `&str` and it needs no filesystem. Every error carries the
/// 1-based line number of the offending line, except [`ParseReason::MissingKey`]
/// and [`ParseReason::MissingSection`], which have no line of their own and point
/// at the section header and the end of file respectively.
pub fn parse_fixture(text: &str) -> Result<FixtureRecord, VenueError> {
    let mut numbered = text.lines().enumerate();

    // Header first, before anything else is looked at.
    match numbered.next() {
        Some((_, first)) if first.trim_end() == UNVERIFIED_HEADER => {}
        _ => return Err(VenueError::parse(1, ParseReason::MissingUnverifiedHeader)),
    }

    let mut sections: Vec<Section> = Vec::new();
    let mut venue_entry: Option<Entry> = None;
    let mut current: Option<usize> = None;
    let mut last_line = 1usize;

    for (idx, raw) in numbered {
        let line = idx + 1;
        last_line = line;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('[') {
            let name = match rest.strip_suffix(']') {
                Some(n) => n.trim(),
                None => {
                    return Err(VenueError::parse(
                        line,
                        ParseReason::UnknownSection(trimmed.to_string()),
                    ))
                }
            };
            let known = KNOWN_SECTIONS.iter().find(|k| **k == name).ok_or_else(|| {
                VenueError::parse(line, ParseReason::UnknownSection(name.to_string()))
            })?;
            if sections.iter().any(|s| s.name == *known) {
                return Err(VenueError::parse(
                    line,
                    ParseReason::DuplicateSection(name.to_string()),
                ));
            }
            sections.push(Section {
                name: known,
                header_line: line,
                entries: Vec::new(),
            });
            current = Some(sections.len() - 1);
            continue;
        }

        let (key, value) = match trimmed.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => return Err(VenueError::parse(line, ParseReason::ExpectedKeyValue)),
        };
        if key.is_empty() {
            return Err(VenueError::parse(line, ParseReason::ExpectedKeyValue));
        }
        if value.is_empty() {
            return Err(VenueError::parse(line, ParseReason::EmptyValue));
        }
        let entry = Entry {
            key: key.to_string(),
            value: value.to_string(),
            line,
        };

        match current {
            None => {
                // Only `venue` lives outside a section: it identifies the file
                // as a whole rather than any one aspect of the instrument.
                if key != "venue" {
                    return Err(VenueError::parse(
                        line,
                        ParseReason::KeyOutsideSection(key.to_string()),
                    ));
                }
                if venue_entry.is_some() {
                    return Err(VenueError::parse(
                        line,
                        ParseReason::DuplicateKey(key.to_string()),
                    ));
                }
                venue_entry = Some(entry);
            }
            Some(i) => {
                // `bid` and `ask` are the only repeatable keys. Everything else
                // duplicated is two people editing the same fixture and neither
                // noticing the other, which last-write-wins would hide.
                let repeatable = key == "bid" || key == "ask";
                if !repeatable && sections[i].entries.iter().any(|e| e.key == key) {
                    return Err(VenueError::parse(
                        line,
                        ParseReason::DuplicateKey(key.to_string()),
                    ));
                }
                sections[i].entries.push(entry);
            }
        }
    }

    build(sections, venue_entry, last_line)
}

/// Remove a named section from the collected list.
fn take_section(sections: &mut Vec<Section>, name: &str) -> Option<Section> {
    sections
        .iter()
        .position(|s| s.name == name)
        .map(|i| sections.remove(i))
}

/// Turn collected sections into a validated record.
///
/// Split out from [`parse_fixture`] so the line-reading loop stays about lines
/// and this stays about meaning.
fn build(
    mut sections: Vec<Section>,
    venue_entry: Option<Entry>,
    last_line: usize,
) -> Result<FixtureRecord, VenueError> {
    // `venue` is a file-level key, so a missing one is reported against line 1
    // rather than the end of file: it belongs at the top and that is where the
    // reader will look.
    let venue_entry =
        venue_entry.ok_or_else(|| VenueError::parse(1, ParseReason::MissingKey("venue")))?;
    let venue = Venue::parse(&venue_entry.value).map_err(|_| bad_enum("venue", &venue_entry))?;

    let mut instrument = take_section(&mut sections, "instrument").ok_or_else(|| {
        VenueError::parse(last_line, ParseReason::MissingSection("instrument"))
    })?;
    let mut fees = take_section(&mut sections, "fees")
        .ok_or_else(|| VenueError::parse(last_line, ParseReason::MissingSection("fees")))?;
    let funding = take_section(&mut sections, "funding");
    let book = take_section(&mut sections, "book");

    // ---- [instrument] -----------------------------------------------------

    let base = as_asset(&instrument.require("base")?)?;
    let quote = as_asset(&instrument.require("quote")?)?;
    let kind_entry = instrument.require("kind")?;
    let kind = as_market_kind(&kind_entry)?;
    let settle = as_asset(&instrument.require("settle")?)?;
    let native_entry = instrument.require("native")?;
    let native = Symbol::new(&native_entry.value)
        .map_err(|t| VenueError::parse(native_entry.line, ParseReason::Type(t)))?;
    let size_entry = instrument.require("contract_size")?;
    let size = as_contract_size(&size_entry)?;
    let tick_size = as_price(&instrument.require("tick_size")?)?;
    // Required, never inferred: see `as_size_unit`. A file that omits it fails
    // with `MissingKey`, which is the point — the old parser inferred the unit
    // from `contract_size` and was right only by luck on Binance.
    let size_unit = as_size_unit(&instrument.require("size_unit")?)?;
    let lot_size = as_qty(&instrument.require("lot_size")?)?;
    let min_notional = match instrument.take("min_notional") {
        Some(e) => Some(as_money(&e)?),
        None => None,
    };

    // Reported against the `contract_size` line: the cross-field check that
    // fails here is almost always a `base:`/`quote:` prefix disagreeing with
    // `kind`, and that is the 68,000x error.
    let spec = ContractSpec::new(kind, size, settle, tick_size, size_unit, lot_size, min_notional)
        .map_err(|t| VenueError::parse(size_entry.line, ParseReason::Type(t)))?;
    instrument.finish()?;

    let key = InstrumentKey::new(base, quote, kind, settle);
    let vkey = VenueKey::new(venue, key);
    let instrument = Instrument::new(vkey, native, spec);

    // ---- [fees] -----------------------------------------------------------

    let maker = as_fee_ratio(&fees.require("maker")?)?;
    let taker = as_fee_ratio(&fees.require("taker")?)?;
    let tier_entry = fees.require("tier")?;
    // `unknown` is the only accepted value, and that is a deliberate
    // restriction rather than an unfinished enum. A fixture cannot know which
    // fee tier *our* account is on — nobody measured it — so allowing
    // `tier = vip3` would let an invention be recorded as a measurement, which
    // is exactly what hard rule 7 forbids. When a live adapter can read the
    // tier off an authenticated endpoint, it will construct
    // `FeeSchedule::known` directly and will not go through this parser.
    if tier_entry.value != "unknown" {
        return Err(bad_enum("tier", &tier_entry));
    }
    let fee_schedule = FeeSchedule::assumed(maker, taker);
    fees.finish()?;

    // ---- [funding] --------------------------------------------------------

    let funding = match funding {
        Some(mut section) => {
            if kind == MarketKind::Spot {
                // Spot does not fund. A `[funding]` block on a spot instrument
                // is a copy-paste that would double-count carry: the spot leg
                // of a cash-and-carry would be credited the perp's rate.
                return Err(VenueError::parse(
                    section.header_line,
                    ParseReason::FundingOnSpot,
                ));
            }
            let rate = as_rate(&section.require("rate")?)?;
            let secs_entry = section.require("interval_secs")?;
            let secs_u64 = as_u64("interval_secs", &secs_entry)?;
            let secs = u32::try_from(secs_u64).map_err(|_| {
                VenueError::parse(
                    secs_entry.line,
                    ParseReason::BadInteger {
                        field: "interval_secs",
                        value: secs_entry.value.clone(),
                    },
                )
            })?;
            let interval = FundingInterval::from_secs(secs)
                .map_err(|t| VenueError::parse(secs_entry.line, ParseReason::Type(t)))?;
            let next_funding_at = match section.take("next_funding_at") {
                Some(e) => Some(WallTs::from_millis(as_i64("next_funding_at", &e)?)),
                None => None,
            };
            let mark = match section.take("mark") {
                Some(e) => Some(as_price(&e)?),
                None => None,
            };
            let index = match section.take("index") {
                Some(e) => Some(as_price(&e)?),
                None => None,
            };
            let ts_entry = section.require("exchange_ts")?;
            let exchange_ts = WallTs::from_millis(as_i64("exchange_ts", &ts_entry)?);
            let is_entry = section.require("interval_source")?;
            let interval_source = match is_entry.value.as_str() {
                "venue" => IntervalSource::Venue,
                "assumed" => IntervalSource::Assumed,
                _ => return Err(bad_enum("interval_source", &is_entry)),
            };
            let rs_entry = section.require("rate_source")?;
            let rate_source = match rs_entry.value.as_str() {
                "predicted" => RateSource::Predicted,
                "historical" => RateSource::Historical,
                _ => return Err(bad_enum("rate_source", &rs_entry)),
            };
            section.finish()?;
            Some(FundingFixture {
                rate,
                interval,
                next_funding_at,
                mark,
                index,
                exchange_ts,
                interval_source,
                rate_source,
            })
        }
        None => {
            if kind.is_perp() {
                // A perp with no funding section is not a perp with zero
                // funding. Leaving it `None` would make `funding_snapshot`
                // report "spot does not fund" about a perpetual, which sends
                // whoever reads the error looking in the wrong place.
                return Err(VenueError::parse(
                    last_line,
                    ParseReason::MissingSection("funding"),
                ));
            }
            None
        }
    };

    // ---- [book] -----------------------------------------------------------

    let (bids, asks, book_sequence, book_exchange_ts, has_book) = match book {
        Some(mut section) => {
            let sequence = match section.take("sequence") {
                Some(e) => Some(as_u64("sequence", &e)?),
                None => None,
            };
            let exchange_ts = match section.take("exchange_ts") {
                Some(e) => Some(WallTs::from_millis(as_i64("exchange_ts", &e)?)),
                None => None,
            };
            let mut bids = Vec::new();
            for e in section.drain_repeated("bid") {
                bids.push(as_level(&e)?);
            }
            let mut asks = Vec::new();
            for e in section.drain_repeated("ask") {
                asks.push(as_level(&e)?);
            }
            section.finish()?;
            (bids, asks, sequence, exchange_ts, true)
        }
        None => (Vec::new(), Vec::new(), None, None, false),
    };

    // Any section left over was known by name but is not one we asked for,
    // which can only happen if `KNOWN_SECTIONS` and this function disagree.
    if let Some(s) = sections.into_iter().next() {
        return Err(VenueError::parse(
            s.header_line,
            ParseReason::UnknownSection(s.name.to_string()),
        ));
    }

    Ok(FixtureRecord {
        instrument,
        funding,
        fees: fee_schedule,
        bids,
        asks,
        book_sequence,
        book_exchange_ts,
        has_book,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::Provenance;

    /// A complete, valid linear-perp fixture. Tests mutate this by string
    /// replacement so that every negative test differs from a *passing* input by
    /// exactly one edit — otherwise a test can pass for the wrong reason, because
    /// a second unnoticed defect in the same input produces the expected error.
    const LINEAR: &str = "\
!unverified invented-from-memory-not-checked-against-live-api
# recalled from memory of the Binance USDT-M API, never checked
venue = binance

[instrument]
base = BTC
quote = USDT
kind = linear_perp
settle = USDT
native = BTCUSDT
contract_size = base:1
tick_size = 0.1
size_unit = base
lot_size = 0.001
min_notional = 5

[funding]
rate = 0.0001
interval_secs = 28800
next_funding_at = 1735689600000
mark = 68121.8
index = 68120.9
exchange_ts = 1735660800000
interval_source = venue
rate_source = predicted

[fees]
maker = 0.0002
taker = 0.00055
tier = unknown

[book]
sequence = 12345
exchange_ts = 1735660800100
bid = 68120.0 x 1.5
bid = 68119.9 x 3.0
ask = 68123.5 x 1.2
ask = 68124.0 x 2.5
";

    /// An inverse perp: `contract_size` is quote-per-contract and there is no
    /// `min_notional`.
    const INVERSE: &str = "\
!unverified invented-from-memory-not-checked-against-live-api
venue = okx

[instrument]
base = BTC
quote = USD
kind = inverse_perp
settle = BTC
native = BTC-USD-SWAP
contract_size = quote:100
tick_size = 0.1
size_unit = contracts
lot_size = 1

[funding]
rate = -0.00005
interval_secs = 28800
exchange_ts = 1735660800000
interval_source = venue
rate_source = historical

[fees]
maker = 0.0002
taker = 0.0005
tier = unknown
";

    /// Spot: no `[funding]` section at all.
    const SPOT: &str = "\
!unverified invented-from-memory-not-checked-against-live-api
venue = kucoin

[instrument]
base = ETH
quote = USDT
kind = spot
settle = USDT
native = ETH-USDT
contract_size = base:1
tick_size = 0.01
size_unit = base
lot_size = 0.0001
min_notional = 1

[fees]
maker = 0.001
taker = 0.001
tier = unknown

[book]
bid = 3820.11 x 4.0
ask = 3820.34 x 2.0
";

    fn ok(text: &str) -> FixtureRecord {
        match parse_fixture(text) {
            Ok(r) => r,
            Err(e) => panic!("expected a parse, got {e:?}"),
        }
    }

    /// The `(line, reason)` of a parse failure. Panics if the input parsed: a
    /// negative test that silently succeeds is worse than no test.
    fn fail(text: &str) -> (usize, ParseReason) {
        match parse_fixture(text) {
            Err(VenueError::Parse { line, reason }) => (line, reason),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    fn price(s: &str) -> Price {
        match Price::parse(s) {
            Ok(p) => p,
            Err(e) => panic!("bad test literal {s:?}: {e:?}"),
        }
    }

    fn qty(s: &str) -> Qty {
        match Qty::parse(s) {
            Ok(q) => q,
            Err(e) => panic!("bad test literal {s:?}: {e:?}"),
        }
    }

    fn money(s: &str) -> Money {
        match Money::parse(s) {
            Ok(m) => m,
            Err(e) => panic!("bad test literal {s:?}: {e:?}"),
        }
    }

    fn rate(s: &str) -> Rate {
        match Rate::parse(s) {
            Ok(r) => r,
            Err(e) => panic!("bad test literal {s:?}: {e:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // the header
    // -----------------------------------------------------------------------

    #[test]
    fn a_fixture_without_the_unverified_header_does_not_parse() {
        let text = LINEAR.replacen(UNVERIFIED_HEADER, "# a normal comment", 1);
        assert_eq!(
            fail(&text),
            (1, ParseReason::MissingUnverifiedHeader),
            "dropping the provenance line must stop the parse, not silently \
             promote invented data to trusted"
        );
    }

    #[test]
    fn the_header_is_matched_whole_not_as_a_prefix() {
        // The edit this rejects is the realistic one: someone checks a single
        // file against a live API and annotates the line instead of removing it.
        // Accepting a prefix would let the marker erode one caveat at a time
        // until it means nothing.
        let text = LINEAR.replacen(
            UNVERIFIED_HEADER,
            &format!("{UNVERIFIED_HEADER} but I checked this one"),
            1,
        );
        assert_eq!(fail(&text), (1, ParseReason::MissingUnverifiedHeader));
    }

    #[test]
    fn an_empty_file_is_a_missing_header_not_an_empty_record() {
        assert_eq!(fail(""), (1, ParseReason::MissingUnverifiedHeader));
    }

    #[test]
    fn trailing_whitespace_on_the_header_is_tolerated() {
        // Only trailing whitespace, because editors add it and it changes no
        // meaning. Leading whitespace is not tolerated: the header must be byte
        // one of the file.
        let text = LINEAR.replacen(
            UNVERIFIED_HEADER,
            &format!("{UNVERIFIED_HEADER}   "),
            1,
        );
        assert!(parse_fixture(&text).is_ok());

        let indented = LINEAR.replacen(
            UNVERIFIED_HEADER,
            &format!("  {UNVERIFIED_HEADER}"),
            1,
        );
        assert_eq!(fail(&indented), (1, ParseReason::MissingUnverifiedHeader));
    }

    // -----------------------------------------------------------------------
    // the happy path, field by field
    // -----------------------------------------------------------------------

    #[test]
    fn a_linear_perp_parses_every_field() {
        let r = ok(LINEAR);

        assert_eq!(r.venue(), Venue::Binance);
        assert_eq!(r.key().base.as_str(), "BTC");
        assert_eq!(r.key().quote.as_str(), "USDT");
        assert_eq!(r.key().settle.as_str(), "USDT");
        assert_eq!(r.key().kind, MarketKind::LinearPerp);
        assert_eq!(r.instrument.native.as_str(), "BTCUSDT");

        assert_eq!(r.instrument.spec.kind, MarketKind::LinearPerp);
        assert_eq!(
            r.instrument.spec.size,
            ContractSize::BasePerContract(Qty::ONE)
        );
        assert_eq!(r.instrument.spec.tick_size, price("0.1"));
        assert_eq!(r.instrument.spec.lot_size, qty("0.001"));
        assert_eq!(r.instrument.spec.min_notional, Some(money("5")));

        let f = match &r.funding {
            Some(f) => f,
            None => panic!("a linear perp must carry funding"),
        };
        assert_eq!(f.rate, rate("0.0001"));
        assert_eq!(f.interval, FundingInterval::EIGHT_HOURS);
        assert_eq!(f.next_funding_at, Some(WallTs::from_millis(1_735_689_600_000)));
        assert_eq!(f.mark, Some(price("68121.8")));
        assert_eq!(f.index, Some(price("68120.9")));
        assert_eq!(f.exchange_ts, WallTs::from_millis(1_735_660_800_000));
        assert_eq!(f.interval_source, IntervalSource::Venue);
        assert_eq!(f.rate_source, RateSource::Predicted);

        assert!(!r.fees.tier_known, "a fixture cannot know our fee tier");
        assert!(r.has_book);
        assert_eq!(r.book_sequence, Some(12345));
        assert_eq!(
            r.book_exchange_ts,
            Some(WallTs::from_millis(1_735_660_800_100))
        );
    }

    #[test]
    fn fees_are_read_as_ratios_not_as_basis_points() {
        // `taker = 0.00055` is the ratio every venue publishes. As a ratio it is
        // 5.5 basis points. Bps is i64 at scale 1e8, so 1 bps has raw 1e8 and
        // 5.5 bps has raw 5.5e8 = 550_000_000.
        //
        // Read through `Bps::parse_bps` instead, 0.00055 would become 0.00055
        // bps — raw 55_000 — and we would have handed ourselves 5.5 bps of edge
        // that does not exist, against a real edge of twelve. The factor is
        // exactly 10,000, and this assertion is the whole reason `as_fee_ratio`
        // is a named function rather than an inline call.
        let r = ok(LINEAR);
        assert_eq!(r.fees.taker.raw(), 550_000_000);
        assert_eq!(r.fees.maker.raw(), 200_000_000); // 0.0002 => 2 bps
    }

    #[test]
    fn book_levels_keep_file_order_on_both_sides() {
        let r = ok(LINEAR);
        assert_eq!(r.bids.len(), 2);
        assert_eq!(r.asks.len(), 2);
        assert_eq!(r.bids[0].price, price("68120.0"));
        assert_eq!(r.bids[0].qty, qty("1.5"));
        assert_eq!(r.bids[1].price, price("68119.9"));
        assert_eq!(r.asks[0].price, price("68123.5"));
        assert_eq!(r.asks[1].price, price("68124.0"));
    }

    #[test]
    fn comments_and_blank_lines_carry_no_meaning() {
        let noisy = LINEAR.replacen(
            "[funding]",
            "\n# a comment\n\n   # an indented comment\n\n[funding]",
            1,
        );
        assert_eq!(ok(&noisy), ok(LINEAR));
    }

    #[test]
    fn an_inverse_perp_parses_a_quote_per_contract_size() {
        let r = ok(INVERSE);
        assert_eq!(r.venue(), Venue::Okx);
        assert_eq!(r.key().kind, MarketKind::InversePerp);
        assert_eq!(r.key().settle.as_str(), "BTC");
        assert_eq!(
            r.instrument.spec.size,
            ContractSize::QuotePerContract(money("100")),
            "an inverse contract is worth a fixed amount of QUOTE; reading this \
             as base-per-contract is the ~68,000x error"
        );
        assert_eq!(r.instrument.spec.min_notional, None);
        assert!(!r.has_book, "this fixture has no [book] section");
        assert!(r.bids.is_empty() && r.asks.is_empty());
    }

    #[test]
    fn a_negative_funding_rate_survives_with_its_sign() {
        // -0.00005 per 8h. The sign is the whole trade: negative means SHORTS
        // pay LONGS, so this venue is the long leg. An absolute value here would
        // reverse the position the scanner recommends.
        let r = ok(INVERSE);
        let f = match &r.funding {
            Some(f) => f,
            None => panic!("inverse perp must carry funding"),
        };
        assert!(f.rate.is_negative());
        assert_eq!(f.rate, rate("-0.00005"));
    }

    #[test]
    fn a_spot_fixture_parses_with_no_funding() {
        let r = ok(SPOT);
        assert_eq!(r.key().kind, MarketKind::Spot);
        assert!(
            r.funding.is_none(),
            "spot does not fund, and None is not the same claim as a rate of zero"
        );
        assert!(r.has_book);
    }

    // -----------------------------------------------------------------------
    // the 68,000x defence
    // -----------------------------------------------------------------------

    #[test]
    fn a_quote_per_contract_size_on_a_linear_market_is_refused_at_parse_time() {
        // The single most expensive edit available in a fixture file. On a linear
        // market the notional is `contracts * base_per * price`; on an inverse
        // one it is `contracts * quote_per`, independent of price. Believing a
        // `quote:` size on a linear BTC market misprices the position by roughly
        // the price itself — about 68,000x — and the resulting number looks
        // entirely plausible in a table.
        //
        // Reported against the `contract_size` line, which is line 11 of LINEAR:
        // 1 header, 2 comment, 3 venue, 4 blank, 5 [instrument], 6 base,
        // 7 quote, 8 kind, 9 settle, 10 native, 11 contract_size.
        let text = LINEAR.replacen("contract_size = base:1", "contract_size = quote:100", 1);
        let (line, reason) = fail(&text);
        assert_eq!(line, 11);
        assert!(
            matches!(reason, ParseReason::Type(_)),
            "expected ContractSpec::new to refuse the cross-field combination, got {reason:?}"
        );
    }

    #[test]
    fn a_base_per_contract_size_on_an_inverse_market_is_refused_too() {
        // The mirror error, and the cheaper one to make: `base:1` is what every
        // linear fixture says, so copying a linear file and only editing `kind`
        // lands here. It would multiply by price a second time.
        let text = INVERSE.replacen("contract_size = quote:100", "contract_size = base:1", 1);
        let (_, reason) = fail(&text);
        assert!(matches!(reason, ParseReason::Type(_)), "got {reason:?}");
    }

    #[test]
    fn a_contract_size_without_a_prefix_is_refused_rather_than_guessed() {
        // There is deliberately no default. `contract_size = 1` is ambiguous
        // between "one BTC" and "one dollar", and a parser that guessed would be
        // right on linear markets and catastrophically wrong on inverse ones.
        let text = LINEAR.replacen("contract_size = base:1", "contract_size = 1", 1);
        let (line, reason) = fail(&text);
        assert_eq!(line, 11);
        assert_eq!(reason, ParseReason::BadContractSize(String::from("1")));
    }

    #[test]
    fn an_unknown_contract_size_prefix_is_refused() {
        let text = LINEAR.replacen("contract_size = base:1", "contract_size = coin:1", 1);
        assert_eq!(
            fail(&text).1,
            ParseReason::BadContractSize(String::from("coin:1"))
        );
    }

    // -----------------------------------------------------------------------
    // strictness: every silent default is a number the operator never saw
    // -----------------------------------------------------------------------

    #[test]
    fn a_misspelled_key_is_an_error_and_not_a_default() {
        // This is the central defence of the crate. Tolerated, `takr` would
        // leave `taker` at whatever a default said — zero, most likely — and a
        // taker fee of zero manufactures 5.5 bps of edge out of a typo. The
        // engine would then trade on it.
        let text = LINEAR.replacen("taker = 0.00055", "takr = 0.00055", 1);
        let (line, reason) = fail(&text);
        assert_eq!(reason, ParseReason::MissingKey("taker"));
        // Reported against the [fees] header, since a missing key has no line of
        // its own. Counting LINEAR: 1 header, 2 comment, 3 venue, 4 blank,
        // 5 [instrument], 6..15 its ten keys, 16 blank, 17 [funding],
        // 18..25 its eight keys, 26 blank, 27 [fees].
        assert_eq!(line, 27);
    }

    #[test]
    fn an_extra_key_is_an_error_because_nothing_read_it() {
        // A key the parser does not consume was written by someone who expected
        // it to matter. Ignoring it means their intent silently did not happen.
        let text = LINEAR.replacen("tier = unknown", "tier = unknown\nrebate = 0.0001", 1);
        assert_eq!(
            fail(&text).1,
            ParseReason::UnknownKey(String::from("rebate"))
        );
    }

    #[test]
    fn a_duplicated_key_is_an_error_and_not_last_write_wins() {
        // Two people editing one fixture, neither seeing the other. Last-write
        // wins would hide it and make the file's meaning depend on line order.
        let text = LINEAR.replacen("rate = 0.0001", "rate = 0.0001\nrate = 0.0009", 1);
        assert_eq!(fail(&text).1, ParseReason::DuplicateKey(String::from("rate")));
    }

    #[test]
    fn bid_and_ask_are_the_only_repeatable_keys() {
        // A book is a list; everything else is a single value. This is asserted
        // rather than assumed because the repeatability check is a two-name
        // allowlist and adding a third name by accident would silently permit a
        // duplicated `rate`.
        let with_levels = LINEAR.replacen("ask = 68124.0 x 2.5", "ask = 68124.0 x 2.5\nask = 68125.0 x 1.0", 1);
        assert_eq!(ok(&with_levels).asks.len(), 3);

        let with_seq = LINEAR.replacen("sequence = 12345", "sequence = 12345\nsequence = 12346", 1);
        assert_eq!(
            fail(&with_seq).1,
            ParseReason::DuplicateKey(String::from("sequence"))
        );
    }

    #[test]
    fn a_duplicated_section_is_an_error() {
        let text = format!("{LINEAR}\n[fees]\nmaker = 0\ntaker = 0\ntier = unknown\n");
        assert_eq!(
            fail(&text).1,
            ParseReason::DuplicateSection(String::from("fees"))
        );
    }

    #[test]
    fn an_unknown_section_is_an_error() {
        let text = LINEAR.replacen("[fees]", "[limits]\nmax = 1\n\n[fees]", 1);
        assert_eq!(
            fail(&text).1,
            ParseReason::UnknownSection(String::from("limits"))
        );
    }

    #[test]
    fn an_unterminated_section_header_is_an_error() {
        let text = LINEAR.replacen("[fees]", "[fees", 1);
        assert!(matches!(fail(&text).1, ParseReason::UnknownSection(_)));
    }

    #[test]
    fn venue_is_the_only_key_allowed_outside_a_section() {
        let text = LINEAR.replacen("venue = binance", "venue = binance\nregion = eu", 1);
        assert_eq!(
            fail(&text).1,
            ParseReason::KeyOutsideSection(String::from("region"))
        );
    }

    #[test]
    fn a_missing_venue_is_reported_against_line_one() {
        let text = LINEAR.replacen("venue = binance\n", "", 1);
        assert_eq!(fail(&text), (1, ParseReason::MissingKey("venue")));
    }

    #[test]
    fn an_unknown_venue_is_an_error_not_a_ninth_exchange() {
        let text = LINEAR.replacen("venue = binance", "venue = binanace", 1);
        assert_eq!(
            fail(&text).1,
            ParseReason::BadEnum {
                field: "venue",
                value: String::from("binanace")
            }
        );
    }

    #[test]
    fn a_line_without_an_equals_sign_is_an_error() {
        let text = LINEAR.replacen("tier = unknown", "tier unknown", 1);
        assert_eq!(fail(&text).1, ParseReason::ExpectedKeyValue);
    }

    #[test]
    fn an_empty_value_is_an_error_and_not_a_zero() {
        // `taker =` reading as zero is the same 5.5-bps invention as the typo
        // above, arrived at from the other direction.
        let text = LINEAR.replacen("taker = 0.00055", "taker =", 1);
        assert_eq!(fail(&text).1, ParseReason::EmptyValue);
    }

    #[test]
    fn a_malformed_level_is_an_error() {
        let text = LINEAR.replacen("bid = 68120.0 x 1.5", "bid = 68120.0", 1);
        assert_eq!(
            fail(&text).1,
            ParseReason::BadLevel(String::from("68120.0"))
        );
    }

    #[test]
    fn a_zero_size_level_is_refused_because_it_means_deletion() {
        // Zero size is a *delete* in most venues' delta protocols. Stored as
        // depth it makes VWAP divide by a size that is not there.
        let text = LINEAR.replacen("bid = 68120.0 x 1.5", "bid = 68120.0 x 0", 1);
        assert!(matches!(fail(&text).1, ParseReason::Type(_)));
    }

    #[test]
    fn an_unparseable_number_is_an_error_not_a_zero() {
        let text = LINEAR.replacen("rate = 0.0001", "rate = 0.0001%", 1);
        assert!(matches!(fail(&text).1, ParseReason::Number(_)));
    }

    #[test]
    fn a_timestamp_is_read_as_an_integer_and_never_through_a_float() {
        let text = LINEAR.replacen(
            "exchange_ts = 1735660800000",
            "exchange_ts = 1.7356608e12",
            1,
        );
        assert_eq!(
            fail(&text).1,
            ParseReason::BadInteger {
                field: "exchange_ts",
                value: String::from("1.7356608e12")
            }
        );
    }

    // -----------------------------------------------------------------------
    // funding: presence, interval, and what the fixture is allowed to claim
    // -----------------------------------------------------------------------

    #[test]
    fn a_funding_section_on_a_spot_instrument_is_refused() {
        // The copy-paste that double-counts carry: the spot leg of a
        // cash-and-carry would be credited the perp's funding rate, and the
        // scanner would print roughly twice the real edge.
        let text = SPOT.replacen(
            "[fees]",
            "[funding]\nrate = 0.0001\ninterval_secs = 28800\nexchange_ts = 1\n\
             interval_source = venue\nrate_source = predicted\n\n[fees]",
            1,
        );
        assert_eq!(fail(&text).1, ParseReason::FundingOnSpot);
    }

    /// Drop a whole section, header and body, from a fixture.
    fn without_section(text: &str, section: &str) -> String {
        let mut out = String::new();
        let mut skipping = false;
        for line in text.lines() {
            let t = line.trim();
            if t == section {
                skipping = true;
                continue;
            }
            if skipping && t.starts_with('[') {
                skipping = false;
            }
            if !skipping {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn a_perp_without_a_funding_section_is_refused() {
        // Absent funding on a perp is not funding of zero. Left as `None` the
        // record would later report "spot does not fund" about a perpetual,
        // sending whoever reads the error to the wrong place entirely.
        let stripped = without_section(LINEAR, "[funding]");
        assert_eq!(fail(&stripped).1, ParseReason::MissingSection("funding"));
    }

    #[test]
    fn a_fixture_without_an_instrument_or_fees_section_is_refused() {
        // Both are mandatory. Fees especially: a carry trade's whole question is
        // whether the funding beats the round trip, and a missing fee section
        // defaulting to zero answers it with a yes every time.
        assert_eq!(
            fail(&without_section(LINEAR, "[fees]")).1,
            ParseReason::MissingSection("fees")
        );
        assert_eq!(
            fail(&without_section(LINEAR, "[instrument]")).1,
            ParseReason::MissingSection("instrument")
        );
    }

    #[test]
    fn every_settlement_interval_we_have_seen_maps_to_its_constant() {
        // The interval is a MULTIPLIER on the headline number: annualising an 8h
        // rate uses 1095 periods, an hourly one 8760. Getting it wrong is not an
        // 8% error, it is an 8x error.
        for (secs, expected, periods) in [
            (28800, FundingInterval::EIGHT_HOURS, 1095),
            (14400, FundingInterval::FOUR_HOURS, 2190),
            (7200, FundingInterval::TWO_HOURS, 4380),
            (3600, FundingInterval::ONE_HOUR, 8760),
        ] {
            let text = LINEAR.replacen(
                "interval_secs = 28800",
                &format!("interval_secs = {secs}"),
                1,
            );
            let r = ok(&text);
            let f = match &r.funding {
                Some(f) => f,
                None => panic!("funding must be present"),
            };
            assert_eq!(f.interval, expected, "for {secs}s");
            assert_eq!(
                f.interval.periods_per_year_exact(),
                Some(periods),
                "31_536_000 / {secs} must be exact"
            );
        }
    }

    #[test]
    fn an_unusual_interval_is_accepted_and_marked_inexact_rather_than_refused() {
        // Seven hours. No venue we know of uses it, and the tempting move is to
        // refuse it as a typo — but refusing would make the whole venue invisible
        // in the table, and an invisible venue is a worse failure than an odd
        // number: the operator sees seven venues and concludes the eighth had no
        // opportunity.
        //
        // It is safe to accept because `arb_math::funding::annualise` multiplies
        // by SECONDS_PER_YEAR *first* and divides by the interval second, so it
        // works on the exact rational and rounds once, at the end, in the
        // caller's chosen direction. What is lost is only the integer period
        // count, and `periods_per_year_exact` reports that loss as `None` instead
        // of rounding 1251.43 to 1251 and understating the APR by 0.43 bps.
        let text = LINEAR.replacen("interval_secs = 28800", "interval_secs = 25200", 1);
        let r = ok(&text);
        let f = match &r.funding {
            Some(f) => f,
            None => panic!("funding must be present"),
        };
        assert_eq!(f.interval.secs(), 25_200);
        assert_eq!(
            f.interval.periods_per_year_exact(),
            None,
            "31_536_000 / 25_200 = 1251.428... is not an integer and must not \
             pretend to be one"
        );
    }

    #[test]
    fn a_negative_or_fractional_interval_is_not_even_an_integer() {
        // The field is read with `str::parse::<u64>`, so a negative or decimal
        // value fails as an integer before `FundingInterval` ever sees it. Worth
        // asserting because the failure has to be a refusal and not a silent
        // truncation of `-28800` into something enormous.
        for bad in ["-28800", "2.88e4", "28800.0"] {
            let text = LINEAR.replacen(
                "interval_secs = 28800",
                &format!("interval_secs = {bad}"),
                1,
            );
            assert_eq!(
                fail(&text).1,
                ParseReason::BadInteger {
                    field: "interval_secs",
                    value: String::from(bad)
                },
                "for {bad}"
            );
        }
    }

    #[test]
    fn a_zero_interval_is_refused() {
        let text = LINEAR.replacen("interval_secs = 28800", "interval_secs = 0", 1);
        assert!(matches!(fail(&text).1, ParseReason::Type(_)));
    }

    #[test]
    fn a_fee_tier_other_than_unknown_is_refused() {
        // A fixture cannot know which tier OUR account is on: nobody measured
        // it. Accepting `tier = vip3` would let an invention be recorded as a
        // measurement and drop FEE_TIER_ASSUMED from the output, which is
        // precisely the flag telling the operator the fee is a guess.
        let text = LINEAR.replacen("tier = unknown", "tier = vip3", 1);
        assert_eq!(
            fail(&text).1,
            ParseReason::BadEnum {
                field: "tier",
                value: String::from("vip3")
            }
        );
    }

    #[test]
    fn an_unknown_interval_or_rate_source_is_refused() {
        let a = LINEAR.replacen("interval_source = venue", "interval_source = probably", 1);
        assert_eq!(
            fail(&a).1,
            ParseReason::BadEnum {
                field: "interval_source",
                value: String::from("probably")
            }
        );
        let b = LINEAR.replacen("rate_source = predicted", "rate_source = guessed", 1);
        assert_eq!(
            fail(&b).1,
            ParseReason::BadEnum {
                field: "rate_source",
                value: String::from("guessed")
            }
        );
    }

    // -----------------------------------------------------------------------
    // provenance and flags: what reaches the screen
    // -----------------------------------------------------------------------

    #[test]
    fn nothing_a_fixture_produces_can_claim_to_have_been_measured() {
        // Hard rule 7, mechanically. This is the assertion that makes the
        // paragraph in the module docs load-bearing instead of decorative.
        for text in [LINEAR, INVERSE, SPOT] {
            let r = ok(text);
            if let Ok(f) = r.funding_snapshot(LocalTs::EPOCH) {
                assert_eq!(f.quality.provenance, Provenance::SynthesizedUnverified);
            }
            if let Ok(b) = r.book_snapshot(5, LocalTs::EPOCH) {
                assert_eq!(b.quality.provenance, Provenance::SynthesizedUnverified);
            }
        }
        assert_eq!(
            synthesized().provenance,
            Provenance::SynthesizedUnverified,
            "the single funnel must never widen"
        );
    }

    #[test]
    fn an_assumed_interval_flags_itself_all_the_way_to_the_snapshot() {
        let text = LINEAR.replacen("interval_source = venue", "interval_source = assumed", 1);
        let r = ok(&text);
        let f = match r.funding_snapshot(LocalTs::EPOCH) {
            Ok(f) => f,
            Err(e) => panic!("expected a snapshot, got {e:?}"),
        };
        assert!(f.quality.flags.contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));
        // And the unmodified file must NOT carry it, or the flag says nothing.
        let clean = match ok(LINEAR).funding_snapshot(LocalTs::EPOCH) {
            Ok(f) => f,
            Err(e) => panic!("expected a snapshot, got {e:?}"),
        };
        assert!(!clean
            .quality
            .flags
            .contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));
    }

    #[test]
    fn a_historical_rate_is_flagged_as_the_past_and_not_a_forecast() {
        // A settled rate is a fact about the past presented in a column headed
        // "expected return". Every funding scanner blurs these and the blur is
        // most of why their numbers do not arrive.
        let f = match ok(INVERSE).funding_snapshot(LocalTs::EPOCH) {
            Ok(f) => f,
            Err(e) => panic!("expected a snapshot, got {e:?}"),
        };
        assert!(f.quality.flags.contains(QualityFlags::FUNDING_RATE_HISTORICAL));
    }

    #[test]
    fn a_missing_mark_or_index_flags_the_price_as_unavailable() {
        // Without a mark there is nothing to charge funding against; without an
        // index there is no basis. INVERSE omits both.
        let f = match ok(INVERSE).funding_snapshot(LocalTs::EPOCH) {
            Ok(f) => f,
            Err(e) => panic!("expected a snapshot, got {e:?}"),
        };
        assert!(f.quality.flags.contains(QualityFlags::PRICE_UNAVAILABLE));
        assert_eq!(f.mark, None);
        assert_eq!(f.index, None);
    }

    #[test]
    fn an_assumed_fee_tier_reaches_both_snapshots() {
        // Every fixture is `tier = unknown`, so FEE_TIER_ASSUMED must be on
        // everything derived from one. A cost computed from a guessed tier is a
        // guessed cost, and that has to survive to the screen.
        let r = ok(LINEAR);
        let f = match r.funding_snapshot(LocalTs::EPOCH) {
            Ok(f) => f,
            Err(e) => panic!("{e:?}"),
        };
        let b = match r.book_snapshot(5, LocalTs::EPOCH) {
            Ok(b) => b,
            Err(e) => panic!("{e:?}"),
        };
        assert!(f.quality.flags.contains(QualityFlags::FEE_TIER_ASSUMED));
        assert!(b.quality.flags.contains(QualityFlags::FEE_TIER_ASSUMED));
    }

    #[test]
    fn spot_refuses_to_produce_a_funding_snapshot() {
        assert!(matches!(
            ok(SPOT).funding_snapshot(LocalTs::EPOCH),
            Err(VenueError::Unsupported(_))
        ));
    }

    // -----------------------------------------------------------------------
    // book snapshots
    // -----------------------------------------------------------------------

    #[test]
    fn truncating_the_book_flags_it_as_partial() {
        // A truncated book makes a size look fillable when it is not. The flag
        // is what tells a sizer "the depth you can see is not the depth that
        // exists".
        let r = ok(LINEAR);
        let one = match r.book_snapshot(1, LocalTs::EPOCH) {
            Ok(b) => b,
            Err(e) => panic!("{e:?}"),
        };
        assert_eq!(one.bids().len(), 1);
        assert_eq!(one.asks().len(), 1);
        assert!(one.quality.flags.contains(QualityFlags::PARTIAL_BOOK));

        // Asking for exactly what is there is not partial, and asking for more
        // than exists is not partial either: nothing was hidden from us.
        let two = match r.book_snapshot(2, LocalTs::EPOCH) {
            Ok(b) => b,
            Err(e) => panic!("{e:?}"),
        };
        assert!(!two.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
        let ten = match r.book_snapshot(10, LocalTs::EPOCH) {
            Ok(b) => b,
            Err(e) => panic!("{e:?}"),
        };
        assert!(!ten.quality.flags.contains(QualityFlags::PARTIAL_BOOK));
        assert_eq!(ten.bids().len(), 2);
    }

    #[test]
    fn a_depth_of_zero_is_refused_rather_than_answered_with_an_empty_book() {
        // An empty book reads downstream as "nothing is fillable" rather than
        // "you asked for nothing", and the two lead to opposite decisions.
        assert!(matches!(
            ok(LINEAR).book_snapshot(0, LocalTs::EPOCH),
            Err(VenueError::Unsupported(_))
        ));
    }

    #[test]
    fn a_fixture_with_no_book_section_refuses_to_invent_one() {
        // Distinct from "the venue had no liquidity": a missing section means we
        // never asked. Returning an empty book would conflate the two.
        let r = ok(INVERSE);
        assert!(!r.has_book);
        assert!(matches!(
            r.book_snapshot(5, LocalTs::EPOCH),
            Err(VenueError::Unsupported(_))
        ));
    }

    #[test]
    fn an_out_of_order_book_is_rejected_by_the_snapshot() {
        // The parser preserves file order and does not sort. Sorting would
        // silently repair a fixture whose sides are the wrong way round, and a
        // book whose "best bid" is its worst bid produces a VWAP that is wrong
        // in our favour — the one direction we can never tolerate.
        let text = LINEAR.replacen("bid = 68119.9 x 3.0", "bid = 68121.9 x 3.0", 1);
        let r = ok(&text);
        assert!(
            r.book_snapshot(5, LocalTs::EPOCH).is_err(),
            "ascending bids must not be accepted as a book"
        );
    }

    #[test]
    fn the_observation_time_is_ours_to_supply_and_not_the_files() {
        // The file has no opinion about when we read it. Keeping `observed_at`
        // out of the parser is what lets the harness choose EPOCH for a
        // deterministic replay and a real clock for a staleness check.
        let r = ok(LINEAR);
        let at = LocalTs::from_millis(1_735_660_900_000);
        let b = match r.book_snapshot(5, at) {
            Ok(b) => b,
            Err(e) => panic!("{e:?}"),
        };
        assert_eq!(b.observed_at, at);
        assert_eq!(b.exchange_ts, Some(WallTs::from_millis(1_735_660_800_100)));
    }

    #[test]
    fn parsing_is_a_pure_function_of_the_text() {
        // Same input, same output, twice. Cheap to assert and it forecloses a
        // whole class of nondeterminism in the replay harness.
        assert_eq!(ok(LINEAR), ok(LINEAR));
        assert_ne!(ok(LINEAR), ok(INVERSE));
    }
}



