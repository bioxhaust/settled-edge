//! The only [`ExchangeAdapter`] this round ships: replay from hand-written
//! fixtures.
//!
//! # What this module is for
//!
//! It proves the seam. Every consumer upstream — sizing, ranking, the report —
//! talks to [`ExchangeAdapter`] and cannot tell whether the number behind it
//! came from a WebSocket or from a file on disk. When the network opens, a live
//! adapter is a new `impl ExchangeAdapter` beside this one and nothing else in
//! the workspace changes.
//!
//! # Provenance, mechanically
//!
//! Every [`arb_types::DataQuality`] produced here is built by
//! [`crate::fixture::synthesized`], the crate's single quality constructor, so
//! there is no code path in `arb-venue` that yields
//! [`arb_types::Provenance::LiveMeasured`]. The fixtures were invented from
//! memory of exchange API shapes and have **never** been compared against a
//! live response: they exercise arithmetic and are worthless as evidence about
//! any exchange. A test asserts the provenance of every method's output rather
//! than trusting this paragraph.
//!
//! # I/O
//!
//! [`FixtureAdapter::load_dir`] is the only function in the workspace that
//! touches the filesystem. Everything else here takes a `&str`, which is why
//! all but one test in this module runs without a disk.

use std::fs;
use std::path::{Path, PathBuf};

use arb_types::{
    BookSnapshot, BorrowSnapshot, DataQuality, FundingSnapshot, Instrument, InstrumentKey, LocalTs,
    RateHistory, Venue,
};

use crate::adapter::{ExchangeAdapter, FeeSchedule};
use crate::error::{ParseReason, VenueError};
use crate::fixture::{parse_fixture, synthesized, FixtureRecord, UNVERIFIED_HEADER};

/// The file extension `load_dir` accepts. Anything else in the directory is
/// ignored silently — a README beside the fixtures is not an error — but a
/// `.fix` file that fails to parse always is.
const FIXTURE_EXT: &str = "fix";

/// A replay adapter over parsed fixture records for exactly ONE venue.
///
/// One adapter, one venue, matching [`ExchangeAdapter::venue`]. A multi-venue
/// adapter would let `BTCUSDT` — which exists on five of our eight exchanges —
/// resolve to whichever record happened to be first in the `Vec`, and route an
/// order to the wrong exchange.
#[derive(Debug, Clone)]
pub struct FixtureAdapter {
    venue: Venue,
    records: Vec<FixtureRecord>,
}

impl FixtureAdapter {
    /// Parse fixture text into an adapter for `venue`. Performs no I/O.
    ///
    /// The text may hold several instruments: each one begins with its own
    /// [`UNVERIFIED_HEADER`] line, so concatenating fixture files is a legal
    /// input and needs no new syntax. Line numbers in errors are absolute
    /// within `text`, not relative to the document that failed, because the
    /// caller is looking at one buffer.
    ///
    /// A record whose body declares a different `venue` is
    /// [`VenueError::Malformed`]: the mismatch has no single line to blame, and
    /// serving a Bybit book from an adapter that claims to be Binance is how a
    /// leg gets sent to an exchange where the position does not exist.
    pub fn from_str(venue: Venue, text: &str) -> Result<Self, VenueError> {
        let documents = split_documents(text)?;
        let mut records = Vec::with_capacity(documents.len());
        for doc in &documents {
            records.push(parse_document(doc)?);
        }
        Self::checked(venue, records)
    }

    /// Build from already-parsed records, trusting the caller.
    ///
    /// The signature cannot fail, so this constructor validates nothing; it
    /// exists for tests and for a future loader that has already checked its
    /// input. Records belonging to another venue are *not* deleted here (a
    /// constructor that silently drops data is worse than one that keeps it),
    /// but they are unreachable: every lookup matches on venue as well as
    /// instrument key. Use [`FixtureAdapter::from_str`] or
    /// [`FixtureAdapter::load_dir`] when you want the mismatch reported.
    pub fn from_records(venue: Venue, records: Vec<FixtureRecord>) -> Self {
        Self { venue, records }
    }

    /// Which venue every record here is supposed to describe.
    pub fn venue(&self) -> Venue {
        self.venue
    }

    /// The parsed records, in the order they were supplied.
    ///
    /// Exposed because a replay harness wants to iterate instruments without
    /// going through [`ExchangeAdapter::instruments`], which allocates.
    pub fn records(&self) -> &[FixtureRecord] {
        &self.records
    }

    /// The [`DataQuality`] that must travel with [`ExchangeAdapter::fee_schedule`].
    ///
    /// [`FeeSchedule`] carries `tier_known` but no provenance — it is a plain
    /// value type shared with a future live adapter. That leaves a hole: a
    /// caller could pair fixture fees with measured-looking quality. This method
    /// closes it, so fee-derived costs can be combined with book quality via
    /// [`DataQuality::combine`] and keep the worst provenance. Filed as a change
    /// request rather than by putting a `DataQuality` inside `FeeSchedule`,
    /// which is another engineer's type.
    pub fn fee_quality(&self, key: &InstrumentKey) -> Result<DataQuality, VenueError> {
        Ok(synthesized().with_flags(self.find(key)?.fees.flags()))
    }

    /// Validate a batch of records for one venue.
    ///
    /// Two checks, both of which are silent wrong answers if omitted:
    /// a record for a different venue, and two records describing the same
    /// instrument. The second matters because every lookup returns the *first*
    /// match: a duplicate would make one fixture authoritative and the other
    /// dead, with no diagnostic and no way to tell which one the numbers came
    /// from.
    fn checked(venue: Venue, records: Vec<FixtureRecord>) -> Result<Self, VenueError> {
        for (i, r) in records.iter().enumerate() {
            if r.venue() != venue {
                return Err(VenueError::Malformed);
            }
            if records[..i]
                .iter()
                .any(|prev| same_key(&prev.key(), &r.key()))
            {
                return Err(VenueError::Malformed);
            }
        }
        Ok(Self { venue, records })
    }

    /// The record for `key`, or [`VenueError::NotFound`].
    ///
    /// Matches venue as well as key: an [`InstrumentKey`] does not carry a
    /// venue, so without this an adapter holding a foreign record would answer
    /// for an exchange it does not speak for.
    fn find(&self, key: &InstrumentKey) -> Result<&FixtureRecord, VenueError> {
        self.records
            .iter()
            .find(|r| r.venue() == self.venue && same_key(&r.key(), key))
            .ok_or(VenueError::NotFound)
    }
}

/// Compare instrument identity by field.
///
/// Compares assets as `&str` rather than relying on `PartialEq` for
/// [`arb_types::Asset`], which the frozen digest does not promise. All four
/// fields participate: `BTC/USDT` linear settled in USDT and `BTC/USD` inverse
/// settled in BTC are different instruments that would otherwise collide on
/// base alone.
fn same_key(a: &InstrumentKey, b: &InstrumentKey) -> bool {
    a.kind == b.kind
        && a.base.as_str() == b.base.as_str()
        && a.quote.as_str() == b.quote.as_str()
        && a.settle.as_str() == b.settle.as_str()
}

/// One fixture document inside a larger buffer.
struct Document {
    /// 1-based line in the enclosing buffer where this document's header sits.
    first_line: usize,
    /// The document's own text, header line included, so `parse_fixture` sees
    /// the provenance check it insists on.
    text: String,
}

/// Split a buffer into fixture documents at each `!unverified` header line.
///
/// The header is the record separator precisely *because* it is mandatory and
/// byte-exact: there is no second delimiter to keep in sync, and text that
/// starts with anything else is rejected at line 1 instead of being scanned for
/// a header further down. A file that lost its provenance line must not become
/// a file with one record fewer.
fn split_documents(text: &str) -> Result<Vec<Document>, VenueError> {
    let mut out: Vec<Document> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line == UNVERIFIED_HEADER {
            out.push(Document {
                first_line: idx + 1,
                text: String::from(line),
            });
            continue;
        }
        match out.last_mut() {
            Some(doc) => {
                doc.text.push('\n');
                doc.text.push_str(line);
            }
            // Content before any header: the provenance line is missing, which
            // is the one error in this crate that is never negotiable.
            None => return Err(VenueError::parse(1, ParseReason::MissingUnverifiedHeader)),
        }
    }
    if out.is_empty() {
        return Err(VenueError::parse(1, ParseReason::MissingUnverifiedHeader));
    }
    Ok(out)
}

/// Parse one document, translating its line numbers back to the buffer's.
fn parse_document(doc: &Document) -> Result<FixtureRecord, VenueError> {
    parse_fixture(&doc.text).map_err(|e| match e {
        // Inside the document the header is line 1, so the offset is
        // `first_line - 1`. Reporting a relative line would send an engineer to
        // the wrong line of the buffer they are actually editing.
        VenueError::Parse { line, reason } => VenueError::parse(line + doc.first_line - 1, reason),
        other => other,
    })
}

// ---------------------------------------------------------------------------
// The only I/O in the workspace
// ---------------------------------------------------------------------------

// Kept in its own `impl` block so the pure surface above can be reviewed
// without reading past a filesystem call, and so `grep -n 'std::fs' ` over the
// workspace lands on exactly one function.
impl FixtureAdapter {
    /// Load every `.fix` file in `dir` and group the records by venue.
    ///
    /// This is the only function in the workspace that performs I/O. Four
    /// consequences follow from that, all of them deliberate:
    ///
    /// 1. **Paths are sorted before reading.** `read_dir` yields entries in
    ///    whatever order the filesystem feels like — on APFS that is not even
    ///    stable across a rename. A replay whose input order depends on the
    ///    directory's internal layout is not a replay: the same fixtures
    ///    produce a different tie-break in the ranking on a different machine,
    ///    and a "regression" is then indistinguishable from a re-sort. Sorting
    ///    by path makes the run reproducible from the file names alone.
    /// 2. **Only `.fix` is read, but a broken `.fix` is fatal.** A README or a
    ///    stray `.md` beside the fixtures is ignored silently. A `.fix` file
    ///    that fails to parse aborts the load, because the alternative — skip
    ///    it and carry on — means the engine ranks a subset of the venues and
    ///    the table looks complete.
    /// 3. **One adapter per venue, in [`Venue::ALL`] order.** Venue order comes
    ///    from the enum, not from the file names, so adding
    ///    `aaa-binance.fix` cannot reorder the output.
    /// 4. **An empty directory is an error.** A load that quietly returns no
    ///    adapters prints an empty opportunity table, which reads exactly like
    ///    "the market has no arbitrage today" rather than "you pointed me at
    ///    the wrong directory".
    pub fn load_dir(dir: &Path) -> Result<Vec<FixtureAdapter>, VenueError> {
        let mut paths: Vec<PathBuf> = Vec::new();
        let entries =
            fs::read_dir(dir).map_err(|e| VenueError::Io(format!("{}: {e}", dir.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| VenueError::Io(format!("{}: {e}", dir.display())))?;
            let path = entry.path();
            // A directory called `something.fix` has an extension too.
            if path.is_dir() {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) == Some(FIXTURE_EXT) {
                paths.push(path);
            }
        }
        // Determinism: see point 1 above.
        paths.sort();

        if paths.is_empty() {
            return Err(VenueError::Io(format!(
                "no *.{FIXTURE_EXT} fixtures in {}",
                dir.display()
            )));
        }

        let mut records: Vec<FixtureRecord> = Vec::new();
        for path in &paths {
            let text = fs::read_to_string(path)
                .map_err(|e| VenueError::Io(format!("{}: {e}", path.display())))?;
            // CHANGE REQUEST (error.rs, not mine to edit): `VenueError::Parse`
            // carries a line but no file, so with seventeen fixtures loaded the
            // message reads `line 14: unknown key "takerr"` and does not say
            // which of the seventeen. The structured error is preserved as-is
            // rather than flattened into `Io(String)`, because a caller that can
            // still match on `ParseReason` is worth more than a prettier
            // string; the real fix is a `source: Option<String>` field on
            // `Parse`, filed rather than invented here.
            for doc in &split_documents(&text)? {
                records.push(parse_document(doc)?);
            }
        }

        // Partition in `Venue::ALL` order. `remaining` is moved out and rebuilt
        // each iteration; because every record's venue is one of the eight, it
        // ends empty, and nothing can be dropped without being served.
        let mut remaining = records;
        let mut adapters: Vec<FixtureAdapter> = Vec::new();
        for venue in Venue::ALL {
            let (mine, rest): (Vec<FixtureRecord>, Vec<FixtureRecord>) =
                remaining.into_iter().partition(|r| r.venue() == venue);
            remaining = rest;
            if !mine.is_empty() {
                adapters.push(FixtureAdapter::checked(venue, mine)?);
            }
        }
        debug_assert!(
            remaining.is_empty(),
            "every parsed record must belong to one of the eight venues"
        );

        Ok(adapters)
    }
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

impl ExchangeAdapter for FixtureAdapter {
    fn venue(&self) -> Venue {
        self.venue
    }

    fn instruments(&self) -> Result<Vec<Instrument>, VenueError> {
        // Only records that really belong to this venue are served; see
        // `from_records`. The spec was built by `ContractSpec::new` at parse
        // time, so a linear market declared quote-per-contract never reaches
        // here — the ~68,000x notional error on BTC is already impossible.
        Ok(self
            .records
            .iter()
            .filter(|r| r.venue() == self.venue)
            .map(|r| r.instrument.clone())
            .collect())
    }

    fn funding(&self, key: &InstrumentKey) -> Result<FundingSnapshot, VenueError> {
        // Delegated rather than assembled here. The flags a fixture forces —
        // FUNDING_INTERVAL_ASSUMED, FUNDING_RATE_HISTORICAL, PRICE_UNAVAILABLE,
        // FEE_TIER_ASSUMED — are derived by `FixtureRecord::funding_snapshot`
        // from the `interval_source` / `rate_source` fields. Rebuilding the
        // snapshot in this file would be a second implementation of that rule,
        // free to drift out of agreement with the first, and the failure mode of
        // the disagreement is an assumed 8h interval printed as if the venue had
        // stated it. Spot returns `Unsupported` from there.
        self.find(key)?.funding_snapshot(OBSERVED_AT)
    }

    fn book(&self, key: &InstrumentKey, depth: usize) -> Result<BookSnapshot, VenueError> {
        // Same delegation, same reason. `book_snapshot` refuses depth 0, refuses
        // a record with no `[book]` section rather than answering with an empty
        // book, sets PARTIAL_BOOK only when a level was really dropped, and
        // leaves BOOK_EMPTY and CROSSED_BOOK to `BookSnapshot::new`.
        //
        // Deliberately NOT set anywhere on this path: THIN_DEPTH, which is a
        // judgement about whether the visible depth can absorb OUR size — this
        // adapter is not told the size, so any value it guessed would be wrong
        // in one of the two directions, and it is arb-detect's call. Also not
        // set: STALE, because `OBSERVED_AT` below makes every fixture snapshot
        // maximally old and the staleness check therefore fails safe by itself.
        self.find(key)?.book_snapshot(depth, OBSERVED_AT)
    }

    fn fee_schedule(&self, key: &InstrumentKey) -> Result<FeeSchedule, VenueError> {
        // `tier = unknown` became `FeeSchedule::assumed` at parse time, so
        // `tier_known` is false and `flags()` yields FEE_TIER_ASSUMED. The
        // matching `DataQuality` is `FixtureAdapter::fee_quality`, because
        // `FeeSchedule` has nowhere to carry provenance.
        Ok(self.find(key)?.fees)
    }

    fn borrow(&self, key: &InstrumentKey) -> Result<Option<BorrowSnapshot>, VenueError> {
        // Overridden rather than inherited even though every shipped fixture
        // answers `None`, because the default would keep answering `None` after a
        // recorded borrow rate arrived in a file — the numbers would be in the
        // fixture, the format would accept them, and the adapter would still
        // report "unknown". A silently ignored input is the harder bug of the two.
        //
        // The `None` this returns today is not a placeholder: no venue has quoted
        // us a borrow rate, so the reverse-carry rows stay blocked. See the module
        // docs of `crate::fixture` for why inventing one is refused.
        self.find(key)?.borrow_snapshot(OBSERVED_AT)
    }

    fn rate_history(&self, key: &InstrumentKey) -> Result<Option<RateHistory>, VenueError> {
        // Same reasoning. `rate_history` takes no observation time: a settled rate
        // is stamped when money moved, and `OBSERVED_AT` has no business in it.
        Ok(self.find(key)?.rate_history())
    }
}

/// The observation time every snapshot from a fixture carries.
///
/// An adapter must never be the clock (see [`ExchangeAdapter`]), and the trait
/// gives us no `LocalTs` to use, so a fixture's observation time is the epoch
/// sentinel rather than "now". That is the honest answer and the safe one: a
/// fixture was never observed at all, and any staleness check comparing this
/// against a real clock sees a maximally old snapshot and refuses to trade it.
/// Calling `LocalTs`-of-now here would manufacture freshness for a number that
/// was typed by hand — the invented book would look like it arrived this
/// millisecond.
const OBSERVED_AT: LocalTs = LocalTs::EPOCH;

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{ContractSize, MarketKind, Provenance, QualityFlags, Side};
    use std::collections::BTreeSet;

    /// The real fixture directory, resolved from the crate manifest rather than
    /// the process's working directory: `cargo test` does not promise which
    /// directory it runs a test binary from, and a relative path that works from
    /// the workspace root silently reads nothing from a crate directory.
    fn fixtures_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
    }

    const LINEAR: &str = "\
!unverified invented-from-memory-not-checked-against-live-api
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

[funding]
rate = 0.0001
interval_secs = 28800
exchange_ts = 1735660800000
interval_source = venue
rate_source = predicted

[fees]
maker = 0.0002
taker = 0.00055
tier = unknown

[book]
bid = 68120.0 x 1.5
ask = 68123.5 x 1.2
";

    /// A second instrument at the same venue, so `from_str` has something to
    /// split.
    const LINEAR_ETH: &str = "\
!unverified invented-from-memory-not-checked-against-live-api
venue = binance

[instrument]
base = ETH
quote = USDT
kind = linear_perp
settle = USDT
native = ETHUSDT
contract_size = base:1
tick_size = 0.01
size_unit = base
lot_size = 0.001

[funding]
rate = 0.00012
interval_secs = 28800
exchange_ts = 1735660800000
interval_source = venue
rate_source = predicted

[fees]
maker = 0.0002
taker = 0.00055
tier = unknown
";

    fn adapter(text: &str) -> FixtureAdapter {
        match FixtureAdapter::from_str(Venue::Binance, text) {
            Ok(a) => a,
            Err(e) => panic!("expected an adapter, got {e:?}"),
        }
    }

    fn key(a: &FixtureAdapter, i: usize) -> InstrumentKey {
        a.records()[i].key()
    }

    // -----------------------------------------------------------------------
    // the acid test: the sixteen files that actually exist
    // -----------------------------------------------------------------------

    #[test]
    fn every_fixture_shipped_with_this_repository_parses() {
        // The one test that would have caught a parser and a fixture set that
        // agree with each other's documentation and not with each other. It reads
        // the real directory rather than an inline string precisely because the
        // inline strings in these tests were written by whoever wrote the parser,
        // and cannot disagree with it.
        let adapters = match FixtureAdapter::load_dir(&fixtures_dir()) {
            Ok(a) => a,
            Err(e) => panic!("the shipped fixtures must parse: {e:?}"),
        };
        assert!(!adapters.is_empty());

        let total: usize = adapters.iter().map(|a| a.records().len()).sum();
        assert_eq!(
            total, 16,
            "the repository ships sixteen .fix files; if this number changed, \
             update it deliberately rather than loosening the assertion"
        );

        // All eight venues represented. A missing venue means either a lost file
        // or a venue name the parser silently failed to match, and both make the
        // opportunity table look complete while covering seven exchanges.
        let venues: BTreeSet<&'static str> = adapters.iter().map(|a| a.venue().as_str()).collect();
        assert_eq!(venues.len(), 8, "found venues: {venues:?}");
    }

    #[test]
    fn every_shipped_record_answers_the_whole_trait() {
        // Parsing is not the same as being usable. This walks each record through
        // the four trait methods a scanner actually calls, so a record that
        // parses but cannot produce a book or a funding snapshot is caught here
        // rather than as an empty row in the report.
        let adapters = match FixtureAdapter::load_dir(&fixtures_dir()) {
            Ok(a) => a,
            Err(e) => panic!("{e:?}"),
        };
        let mut perps = 0usize;
        let mut books = 0usize;
        for a in &adapters {
            let instruments = match a.instruments() {
                Ok(v) => v,
                Err(e) => panic!("{:?}: {e:?}", a.venue()),
            };
            assert_eq!(instruments.len(), a.records().len());

            for inst in &instruments {
                let k = inst.vkey.key;
                assert_eq!(inst.vkey.venue, a.venue());

                // The contract size must agree with the market kind, or the
                // notional is out by roughly the price of the base asset.
                match (inst.spec.kind, inst.spec.size) {
                    (MarketKind::InversePerp, ContractSize::QuotePerContract(_)) => {}
                    (
                        MarketKind::Spot | MarketKind::LinearPerp,
                        ContractSize::BasePerContract(_),
                    ) => {}
                    other => panic!("{:?} has an impossible spec: {other:?}", inst.native),
                }

                match a.fee_schedule(&k) {
                    Ok(f) => assert!(
                        !f.tier_known,
                        "no fixture can know our fee tier, but {:?} claims to",
                        inst.native
                    ),
                    Err(e) => panic!("{:?}: {e:?}", inst.native),
                }

                if inst.spec.kind.is_perp() {
                    perps += 1;
                    match a.funding(&k) {
                        Ok(f) => {
                            assert_eq!(f.vkey.venue, a.venue());
                            assert_eq!(f.quality.provenance, Provenance::SynthesizedUnverified);
                            assert!(f.quality.flags.contains(QualityFlags::FEE_TIER_ASSUMED));
                        }
                        Err(e) => panic!("perp {:?} must fund: {e:?}", inst.native),
                    }
                } else {
                    assert!(
                        a.funding(&k).is_err(),
                        "spot {:?} must not fund",
                        inst.native
                    );
                }

                // A book is optional in the format, so an absent one is not a
                // failure — but a present one must be well-formed on both sides.
                if let Ok(b) = a.book(&k, 50) {
                    books += 1;
                    assert_eq!(b.quality.provenance, Provenance::SynthesizedUnverified);
                    let bid = b.touch(Side::Sell);
                    let ask = b.touch(Side::Buy);
                    if let (Some(bid), Some(ask)) = (bid, ask) {
                        assert!(
                            bid < ask,
                            "{:?} ships a crossed book: bid {bid} >= ask {ask}",
                            inst.native
                        );
                    }
                }
            }
        }
        // Census taken from the files on disk: 14 of the 16 carry [funding] and
        // 15 carry [book]. Asserted so that a fixture losing a section shows up
        // here instead of as a quietly shorter table.
        assert_eq!(perps, 14, "fourteen of the sixteen fixtures are perps");
        assert_eq!(books, 15, "fifteen of the sixteen carry a [book]");
    }

    #[test]
    fn loading_the_same_directory_twice_gives_the_same_thing() {
        // Determinism is a requirement, not a nicety: `read_dir` order is not
        // stable on APFS, and a replay whose input order depends on the
        // filesystem's internal layout makes a re-sort indistinguishable from a
        // regression.
        let a = match FixtureAdapter::load_dir(&fixtures_dir()) {
            Ok(v) => v,
            Err(e) => panic!("{e:?}"),
        };
        let b = match FixtureAdapter::load_dir(&fixtures_dir()) {
            Ok(v) => v,
            Err(e) => panic!("{e:?}"),
        };
        let tags_a: Vec<&str> = a.iter().map(|x| x.venue().as_str()).collect();
        let tags_b: Vec<&str> = b.iter().map(|x| x.venue().as_str()).collect();
        assert_eq!(tags_a, tags_b);
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.records(), y.records());
        }
    }

    #[test]
    fn venues_come_out_in_enum_order_not_in_filename_order() {
        // Venue order is taken from `Venue::ALL`, so adding `aaa-binance.fix`
        // cannot reorder the report.
        let loaded = match FixtureAdapter::load_dir(&fixtures_dir()) {
            Ok(v) => v,
            Err(e) => panic!("{e:?}"),
        };
        let got: Vec<Venue> = loaded.iter().map(|a| a.venue()).collect();
        let expected: Vec<Venue> = Venue::ALL
            .into_iter()
            .filter(|v| got.contains(v))
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn a_directory_with_no_fixtures_is_an_error_not_an_empty_table() {
        // An empty result prints an empty opportunity table, which reads exactly
        // like "the market has no arbitrage today" rather than "you pointed me at
        // the wrong directory".
        let empty = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert!(matches!(
            FixtureAdapter::load_dir(&empty),
            Err(VenueError::Io(_))
        ));
    }

    #[test]
    fn a_missing_directory_is_an_io_error() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-directory");
        assert!(matches!(
            FixtureAdapter::load_dir(&missing),
            Err(VenueError::Io(_))
        ));
    }

    // -----------------------------------------------------------------------
    // concatenated documents
    // -----------------------------------------------------------------------

    #[test]
    fn several_instruments_may_share_one_buffer() {
        let text = format!("{LINEAR}{LINEAR_ETH}");
        let a = adapter(&text);
        assert_eq!(a.records().len(), 2);
        assert_eq!(key(&a, 0).base.as_str(), "BTC");
        assert_eq!(key(&a, 1).base.as_str(), "ETH");
    }

    #[test]
    fn a_line_number_in_the_second_document_is_absolute_within_the_buffer() {
        // A relative line number would send an engineer to the wrong line of the
        // file they are actually editing. LINEAR is 29 lines including its
        // trailing newline, so the second document's header is line 30 and its
        // `taker` is line 53.
        let broken = LINEAR_ETH.replacen("taker = 0.00055", "takr = 0.00055", 1);
        let text = format!("{LINEAR}{broken}");
        match FixtureAdapter::from_str(Venue::Binance, &text) {
            Err(VenueError::Parse { line, reason }) => {
                assert_eq!(reason, ParseReason::MissingKey("taker"));
                // The [fees] header of the SECOND document. Counting the buffer:
                // LINEAR occupies lines 1..29, so document two starts at 30 and
                // its [fees] header — the 22nd line of LINEAR_ETH — is 29+22=51.
                assert_eq!(line, 51, "line must be absolute within the buffer");
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn text_before_the_first_header_is_a_missing_header() {
        // A file that lost its provenance line must not become a file with one
        // record fewer.
        let text = format!("# a stray comment\n{LINEAR}");
        assert!(matches!(
            FixtureAdapter::from_str(Venue::Binance, &text),
            Err(VenueError::Parse {
                line: 1,
                reason: ParseReason::MissingUnverifiedHeader
            })
        ));
    }

    #[test]
    fn an_empty_buffer_is_a_missing_header() {
        assert!(matches!(
            FixtureAdapter::from_str(Venue::Binance, ""),
            Err(VenueError::Parse {
                reason: ParseReason::MissingUnverifiedHeader,
                ..
            })
        ));
    }

    // -----------------------------------------------------------------------
    // one adapter, one venue
    // -----------------------------------------------------------------------

    #[test]
    fn a_record_for_another_venue_is_refused() {
        // Serving a Bybit book from an adapter that claims to be Binance is how a
        // leg gets sent to an exchange where the position does not exist.
        assert!(matches!(
            FixtureAdapter::from_str(Venue::Bybit, LINEAR),
            Err(VenueError::Malformed)
        ));
    }

    #[test]
    fn two_records_for_the_same_instrument_are_refused() {
        // Every lookup returns the FIRST match, so a duplicate would make one
        // fixture authoritative and the other dead, with no diagnostic and no way
        // to tell which one the numbers came from.
        let text = format!("{LINEAR}{LINEAR}");
        assert!(matches!(
            FixtureAdapter::from_str(Venue::Binance, &text),
            Err(VenueError::Malformed)
        ));
    }

    #[test]
    fn identity_uses_all_four_key_fields() {
        // BTC/USDT linear settled in USDT and BTC/USD inverse settled in BTC are
        // different instruments that would collide on base alone.
        let a = adapter(LINEAR);
        let btc = key(&a, 0);
        let inverse = InstrumentKey::new(
            btc.base,
            btc.quote,
            MarketKind::InversePerp,
            btc.settle,
        );
        assert!(a.funding(&btc).is_ok());
        assert!(
            matches!(a.funding(&inverse), Err(VenueError::NotFound)),
            "kind must participate in identity"
        );
    }

    #[test]
    fn an_unknown_key_is_not_found_rather_than_the_first_record() {
        let a = adapter(&format!("{LINEAR}{LINEAR_ETH}"));
        let sol = InstrumentKey::new(
            match arb_types::Asset::new("SOL") {
                Ok(x) => x,
                Err(e) => panic!("{e:?}"),
            },
            key(&a, 0).quote,
            MarketKind::LinearPerp,
            key(&a, 0).settle,
        );
        assert!(matches!(a.funding(&sol), Err(VenueError::NotFound)));
        assert!(matches!(a.book(&sol, 5), Err(VenueError::NotFound)));
        assert!(matches!(a.fee_schedule(&sol), Err(VenueError::NotFound)));
    }

    // -----------------------------------------------------------------------
    // the seam
    // -----------------------------------------------------------------------

    #[test]
    fn a_fixture_observation_time_is_the_epoch_so_staleness_fails_safe() {
        // An adapter must never be the clock. A fixture was never observed at
        // all, so claiming "now" would manufacture freshness for a number that
        // was typed by hand: the invented book would look like it arrived this
        // millisecond and sail through every staleness check.
        let a = adapter(LINEAR);
        let k = key(&a, 0);
        match a.book(&k, 5) {
            Ok(b) => assert_eq!(b.observed_at, LocalTs::EPOCH),
            Err(e) => panic!("{e:?}"),
        }
        match a.funding(&k) {
            Ok(f) => assert_eq!(f.observed_at, LocalTs::EPOCH),
            Err(e) => panic!("{e:?}"),
        }
    }

    #[test]
    fn no_path_through_this_adapter_can_claim_a_live_measurement() {
        // Hard rule 7, mechanically, at the seam rather than in the parser.
        let a = adapter(LINEAR);
        let k = key(&a, 0);
        let quality = [
            a.book(&k, 5).map(|b| b.quality),
            a.funding(&k).map(|f| f.quality),
            a.fee_quality(&k),
        ];
        for q in quality {
            match q {
                Ok(q) => {
                    assert_eq!(q.provenance, Provenance::SynthesizedUnverified);
                    assert!(!q.is_trustworthy());
                }
                Err(e) => panic!("{e:?}"),
            }
        }
    }

    #[test]
    fn fee_quality_carries_the_assumed_tier_that_fee_schedule_cannot() {
        // `FeeSchedule` is a plain value type shared with a future live adapter
        // and has nowhere to put provenance. Without this method a caller could
        // pair fixture fees with measured-looking quality.
        let a = adapter(LINEAR);
        let k = key(&a, 0);
        match a.fee_quality(&k) {
            Ok(q) => assert!(q.flags.contains(QualityFlags::FEE_TIER_ASSUMED)),
            Err(e) => panic!("{e:?}"),
        }
    }

    #[test]
    fn depth_zero_is_refused_at_the_seam_too() {
        let a = adapter(LINEAR);
        assert!(matches!(
            a.book(&key(&a, 0), 0),
            Err(VenueError::Unsupported(_))
        ));
    }

    #[test]
    fn the_trait_venue_and_the_struct_venue_are_the_same_venue() {
        let a = adapter(LINEAR);
        assert_eq!(ExchangeAdapter::venue(&a), a.venue());
        assert_eq!(a.venue(), Venue::Binance);
    }

    #[test]
    fn from_records_keeps_what_it_is_given() {
        // Documented not to validate: a constructor that silently drops data is
        // worse than one that keeps it. Foreign records are unreachable through
        // lookups rather than deleted.
        let parsed = adapter(LINEAR);
        let a = FixtureAdapter::from_records(Venue::Bybit, parsed.records().to_vec());
        assert_eq!(a.records().len(), 1);
        assert!(
            matches!(a.funding(&key(&a, 0)), Err(VenueError::NotFound)),
            "a Binance record must be unreachable from a Bybit adapter"
        );
    }
}
