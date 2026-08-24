//! What we assumed about eight exchanges, and what it costs if we are wrong.
//!
//! # Why this module exists at all
//!
//! Unifying eight venues behind one set of types means choosing, eight times,
//! how a venue's wire format maps onto ours: what the symbol is called, which
//! direction a funding rate points, how long a funding period lasts, whether a
//! depth level is one order or an aggregate, what unit a timestamp is in. Every
//! one of those choices is a place where the engine can be confidently wrong.
//!
//! **Nothing in this table was measured.** No request was ever sent to any
//! exchange this round; there is no network. Every entry below is recalled from
//! memory of API documentation, which is exactly the kind of source that is
//! wrong in the details and right in the shape. That is why
//! [`Confidence::Measured`] never appears — a test asserts it — and why the
//! interesting field is not `assumed` but
//! [`UnificationGap::consequence_if_wrong`].
//!
//! # How to read `consequence_if_wrong`
//!
//! "Prices would be wrong" is not an entry; it is an apology. Each consequence
//! states a **magnitude and a direction**, because that is what decides whether
//! a gap must be closed before the engine is allowed to trade or can be left
//! open with a flag. Our edge is twelve basis points. A gap that moves a number
//! by half a basis point is noise; one that multiplies an annualised rate by
//! eight puts a fictional row at the top of the ranking, and the ranking is
//! sorted descending, so the worst error is always the first thing we see.
//!
//! This module is data, not behaviour: it performs no I/O, allocates nothing,
//! and is a `&'static [UnificationGap]` so a report can print it verbatim.

use arb_types::Venue;

/// How much we actually know about one assumption.
///
/// Three levels rather than a bool because "we read it in a doc from memory"
/// and "we have no idea" are different risks and want different treatment:
/// the first can be spot-checked against one API call, the second needs
/// somebody to go and find out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Someone compared this against a live response and it matched.
    ///
    /// **Unused this round, deliberately.** It exists so that its absence from
    /// [`gaps`] is visible and assertable, rather than being a fact buried in a
    /// paragraph of prose.
    Measured,
    /// Recalled from documentation or from the shape of other venues. Plausible,
    /// unverified, and specific enough to be checked in one request when the
    /// network opens.
    Assumed,
    /// We do not know. Not a placeholder for a value we could look up — a real
    /// statement that this behaviour was never established and the engine must
    /// not depend on it.
    Unknown,
}

impl Confidence {
    /// Short tag for a report column.
    pub const fn tag(self) -> &'static str {
        match self {
            Confidence::Measured => "measured",
            Confidence::Assumed => "assumed",
            Confidence::Unknown => "unknown",
        }
    }

    /// Whether an opportunity resting on this assumption may be executed.
    ///
    /// Only measured assumptions qualify. This is the same asymmetry as the fee
    /// tier: skipping a real trade costs opportunity, taking a fictional one
    /// costs money, and the two are not comparable.
    pub const fn is_tradeable(self) -> bool {
        matches!(self, Confidence::Measured)
    }
}

/// The subject of an assumption — the eight ways a venue can differ from our
/// model.
///
/// A closed enum rather than a free string so the report can group by topic and
/// so a new venue cannot introduce a ninth kind of gap without someone deciding
/// it is really new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// How the venue spells the instrument. `BTCUSDT`, `BTC-USDT-SWAP`,
    /// `BTCUSDT_UMCBL`, `BTC_USDT`, `BTCUSDTM` are all the same market.
    SymbolNaming,
    /// Which side pays when the rate is positive.
    FundingSign,
    /// How long one funding period is, per instrument.
    FundingInterval,
    /// What one depth level means: one order, an aggregate, or a bucket.
    DepthSemantics,
    /// How the venue numbers its book updates, and therefore how we detect a gap.
    SequenceScheme,
    /// How many quote units one inverse contract is worth.
    InverseContractSize,
    /// Milliseconds, microseconds, nanoseconds or seconds.
    TimestampUnit,
    /// How many requests per second before the venue starts refusing them.
    RateLimit,
}

impl Topic {
    /// Short tag for a report column.
    pub const fn tag(self) -> &'static str {
        match self {
            Topic::SymbolNaming => "symbol-naming",
            Topic::FundingSign => "funding-sign",
            Topic::FundingInterval => "funding-interval",
            Topic::DepthSemantics => "depth-semantics",
            Topic::SequenceScheme => "sequence-scheme",
            Topic::InverseContractSize => "inverse-contract-size",
            Topic::TimestampUnit => "timestamp-unit",
            Topic::RateLimit => "rate-limit",
        }
    }

    /// Every topic, for a report that wants to show empty rows too.
    pub const ALL: [Topic; 8] = [
        Topic::SymbolNaming,
        Topic::FundingSign,
        Topic::FundingInterval,
        Topic::DepthSemantics,
        Topic::SequenceScheme,
        Topic::InverseContractSize,
        Topic::TimestampUnit,
        Topic::RateLimit,
    ];
}

/// One thing we assumed about one venue, and the size of the damage if the
/// assumption is false.
///
/// `&'static str` for both prose fields: this table is a compile-time constant,
/// so printing it allocates nothing and it cannot be edited at runtime by
/// anything that later wants the numbers to look better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnificationGap {
    /// The exchange this is about.
    pub venue: Venue,
    /// What kind of assumption it is.
    pub topic: Topic,
    /// How much we know. Never [`Confidence::Measured`] this round.
    pub confidence: Confidence,
    /// What we assumed, stated concretely enough to be falsified by one request.
    pub assumed: &'static str,
    /// The magnitude and direction of the error if the assumption is wrong.
    pub consequence_if_wrong: &'static str,
}

/// Every assumption we are running on, all eight venues.
///
/// Ordered by [`Venue::ALL`] so a report prints in a stable order, and grouped
/// so one venue's risks read together.
pub fn gaps() -> &'static [UnificationGap] {
    GAPS
}

/// Shorthand so the table below is readable as a table.
const fn gap(
    venue: Venue,
    topic: Topic,
    confidence: Confidence,
    assumed: &'static str,
    consequence_if_wrong: &'static str,
) -> UnificationGap {
    UnificationGap {
        venue,
        topic,
        confidence,
        assumed,
        consequence_if_wrong,
    }
}

use Confidence::{Assumed, Unknown};
use Topic::{
    DepthSemantics, FundingInterval, FundingSign, InverseContractSize, RateLimit, SequenceScheme,
    SymbolNaming, TimestampUnit,
};

const GAPS: &[UnificationGap] = &[
    // -- Binance ------------------------------------------------------------
    gap(
        Venue::Binance,
        SymbolNaming,
        Assumed,
        "USD-M perp is `BTCUSDT`, identical to the spot symbol; the two are told \
         apart only by which API host you ask",
        "A spot symbol sent to the futures endpoint 404s, which is loud and \
         harmless. The dangerous direction is the reverse: our own symbol map \
         keying both markets as `BTCUSDT` would let a spot book be paired with a \
         perp funding rate, producing a basis of exactly zero and a carry trade \
         with no perp leg at all",
    ),
    gap(
        Venue::Binance,
        FundingInterval,
        Assumed,
        "8h on BTCUSDT and ETHUSDT, settling at 00:00/08:00/16:00 UTC",
        "Binance switches some pairs to 4h or 1h when the rate hits the cap, and \
         we would not notice. Annualising a 1h rate as if it were 8h overstates \
         APR by 8x: a genuine 15% carry prints as 120% and sorts to the top of \
         the ranking, so the most-wrong row is the one we act on first",
    ),
    gap(
        Venue::Binance,
        SequenceScheme,
        Unknown,
        "`lastUpdateId` on the REST depth snapshot is comparable with the `u`/`U` \
         range on the diff stream, so a gap is detectable by arithmetic",
        "If the two counters are not the same series we cannot detect a dropped \
         update at all. A book missing one update keeps a level that was cancelled: \
         we compute a fill against depth that is not there, and the loss is the \
         whole size of the phantom level, not a rounding error",
    ),
    gap(
        Venue::Binance,
        RateLimit,
        Unknown,
        "roughly 2400 request-weight per minute per IP, with depth snapshots \
         costing more weight at greater depth",
        "Exceeding it returns 429 and then an IP ban measured in minutes. Mid-arb \
         that is the worst possible failure: one leg is filled, the other leg's \
         venue stops answering, and we hold unhedged directional risk on the full \
         notional until the ban lifts",
    ),
    // -- Bybit --------------------------------------------------------------
    gap(
        Venue::Bybit,
        SymbolNaming,
        Assumed,
        "v5 perp is `BTCUSDT` with `category=linear`; the same string with \
         `category=spot` is the spot market",
        "The symbol alone is ambiguous, so the category parameter carries the \
         market type. Send `category=spot` to a perp query and the funding lookup \
         returns nothing, which we would read as `PRICE_UNAVAILABLE` and skip a \
         real opportunity. Send it the other way on an ORDER and we open a \
         leveraged perp position where we intended to buy spot: same notional, \
         completely different risk, and no hedge",
    ),
    gap(
        Venue::Bybit,
        FundingSign,
        Assumed,
        "a positive `fundingRate` means longs pay shorts, matching Binance and \
         our own convention",
        "An inverted sign flips which leg earns. A cash-and-carry that we score \
         at +12 bps per period is then -12 bps, a 24 bp swing on the whole \
         notional per settlement — twice our entire edge, in the wrong direction, \
         three times a day",
    ),
    gap(
        Venue::Bybit,
        SequenceScheme,
        Unknown,
        "the `u` field on the orderbook topic increases by one per update and \
         `seq` is a separate cross-topic counter",
        "If `u` can skip legitimately (Bybit is recalled as publishing snapshots \
         into the same stream) we would flag SEQ_GAP on healthy data and refuse \
         to trade the venue at all — a total loss of one of eight venues. If it \
         can skip silently instead, we miss real gaps and quote against a stale \
         level",
    ),
    // -- OKX ----------------------------------------------------------------
    gap(
        Venue::Okx,
        SymbolNaming,
        Assumed,
        "hyphenated triples: `BTC-USDT-SWAP` for the linear perp, `BTC-USD-SWAP` \
         for the inverse, `BTC-USDT` for spot",
        "The suffix is the only thing distinguishing linear from inverse, and \
         those two settle in different assets. Confusing them makes the notional \
         wrong by the price of BTC (~68,000x per contract) and leaves the P&L \
         denominated in the wrong currency, so even the sign of the hedge is not \
         reliable",
    ),
    gap(
        Venue::Okx,
        InverseContractSize,
        Assumed,
        "one `BTC-USD-SWAP` contract is 100 USD of notional, independent of price",
        "This is the single most expensive number in the table. Treating 100 USD \
         per contract as 1 BTC per contract oversizes by the BTC price: at 68,000 \
         a 10-contract order intended as 1,000 USD becomes 680,000 USD. Margin \
         either rejects it or, worse, accepts it on leverage and one leg is 680x \
         the other",
    ),
    gap(
        Venue::Okx,
        DepthSemantics,
        Unknown,
        "the `books` channel is price-aggregated across orders, and the third \
         field of each level is an order count we can ignore",
        "If a level is really one order rather than an aggregate, visible depth \
         overstates what a single fill can absorb, and every VWAP we compute is \
         better than reality. The error grows with size: small orders are \
         unaffected, and the size we would actually trade is the one that slips",
    ),
    gap(
        Venue::Okx,
        FundingSign,
        Assumed,
        "positive `fundingRate` means longs pay shorts, as elsewhere",
        "Same 2x-edge sign flip as Bybit. Worse here because OKX inverse settles \
         in BTC: a mis-signed funding payment on an inverse leg also changes our \
         BTC balance, so the hedge drifts and needs re-balancing at a cost we \
         never budgeted",
    ),
    // -- Bitget -------------------------------------------------------------
    gap(
        Venue::Bitget,
        SymbolNaming,
        Unknown,
        "`BTCUSDT_UMCBL`, where the suffix is the v1 product-type marker for a \
         USDT-margined perp; v2 is recalled as dropping the suffix and passing \
         `productType` separately",
        "We do not know which API generation this engine will talk to, so the \
         symbol in our fixtures is a coin flip. Wrong generation means every \
         request 404s and Bitget silently contributes zero rows — the ranking \
         looks complete while one venue of eight is missing, and missing venues \
         are invisible in a table sorted by edge",
    ),
    gap(
        Venue::Bitget,
        FundingInterval,
        Assumed,
        "8h on the USDT-margined majors",
        "If a pair is really on 4h we understate APR by 2x and skip a trade that \
         clears our threshold; if it is on 1h we overstate by 8x and put a \
         fictional leader at the top of the ranking. The two errors are not \
         symmetric and only one of them costs money",
    ),
    gap(
        Venue::Bitget,
        DepthSemantics,
        Unknown,
        "depth responses are aggregated by price and capped near 100 levels per \
         side",
        "If the cap is lower than we think, PARTIAL_BOOK is set on a book that is \
         genuinely complete, or — the expensive direction — not set on one that is \
         truncated, and a size that needs 40 levels is priced against 15. Slippage \
         then arrives entirely as a surprise, after both legs are live",
    ),
    // -- MEXC ---------------------------------------------------------------
    gap(
        Venue::Mexc,
        SymbolNaming,
        Unknown,
        "futures use an underscore (`BTC_USDT`) while spot uses none (`BTCUSDT`) \
         — two different spellings on one exchange",
        "A one-character difference between two markets with different risk. If we \
         have it backwards, a query 404s (harmless) or resolves to the other \
         market (not harmless): pairing MEXC spot depth with MEXC perp funding \
         invents a basis trade whose perp leg does not exist",
    ),
    gap(
        Venue::Mexc,
        DepthSemantics,
        Unknown,
        "quoted depth is real resting size, not a smoothed or bucketed view",
        "MEXC is the venue we trust least on this point and we have measured \
         nothing. If levels are painted or bucketed, visible depth is fiction and \
         the fill is at a price nobody quoted — that is PAINTED_DEPTH, which we \
         cannot detect from a fixture and must measure before sizing anything here",
    ),
    gap(
        Venue::Mexc,
        RateLimit,
        Unknown,
        "no figure at all; not even the shape of the limit is recalled",
        "Unknown means unbudgeted. A poll loop tuned for Binance's weight system \
         may be an order of magnitude too aggressive here, and the failure lands \
         as a ban between our two legs: one side filled, the other unreachable, \
         holding directional risk on the full notional",
    ),
    // -- Gate ---------------------------------------------------------------
    gap(
        Venue::Gate,
        SymbolNaming,
        Assumed,
        "`BTC_USDT` with an underscore for both spot and the USDT-settled perp, \
         the two separated by API path rather than by symbol",
        "Same ambiguity as MEXC and Bybit: the string does not say which market it \
         is. If our map keys on the symbol alone, two of our eight venues collapse \
         spot and perp into one row and the basis computed from it is between a \
         market and itself, i.e. exactly zero — an opportunity that silently \
         disappears rather than one that loudly fails",
    ),
    gap(
        Venue::Gate,
        TimestampUnit,
        Unknown,
        "Gate is recalled as returning SECONDS in several REST fields where every \
         other venue we touch returns milliseconds",
        "A seconds value read as milliseconds lands in January 1970: `1735660800` \
         becomes 1970-01-21. Every staleness check then sees a snapshot 56 years \
         old and blocks the venue entirely. The reverse — ms read as seconds — is \
         the dangerous one: the stamp lands in the year 56,000 and the data looks \
         eternally fresh, so a stale book is never rejected",
    ),
    gap(
        Venue::Gate,
        InverseContractSize,
        Unknown,
        "nothing is recalled about Gate's inverse contracts; whether they exist in \
         our universe was never established",
        "We have no inverse Gate fixture and no reason to trust one if we wrote it. \
         Sizing an inverse leg on an assumed contract size is the ~68,000x error \
         from the OKX row with none of the OKX row's (weak) support. This gap must \
         be closed by measurement before any Gate inverse instrument is enabled",
    ),
    // -- KuCoin -------------------------------------------------------------
    gap(
        Venue::Kucoin,
        SymbolNaming,
        Unknown,
        "our fixtures spell the futures perp `BTCUSDTM` with a trailing M; memory \
         ALSO says KuCoin uses `XBT` rather than `BTC` for bitcoin on futures, \
         i.e. `XBTUSDTM`. We do not know which is right",
        "This is a genuine unresolved contradiction, recorded rather than \
         smoothed over. If it is `XBT`, every KuCoin futures request in this \
         engine 404s and the venue contributes nothing — while our fixtures, which \
         we wrote ourselves, keep parsing perfectly and hide the fact. Spot is a \
         third spelling again (`BTC-USDT`)",
    ),
    gap(
        Venue::Kucoin,
        TimestampUnit,
        Unknown,
        "KuCoin futures is recalled as mixing units — nanoseconds on some \
         websocket fields, milliseconds on REST",
        "Nanoseconds read as milliseconds put the stamp roughly a million times \
         too far in the future, so the snapshot looks permanently fresh and STALE \
         never fires. A clock-skew check would also read ~19,000 years of skew \
         and, if it clamps instead of erroring, silently pass",
    ),
    gap(
        Venue::Kucoin,
        FundingInterval,
        Assumed,
        "8h on XBT/BTC-USDT-margined perps",
        "Unverified like every other interval here. 8h assumed on a 1h instrument \
         overstates the annualised carry 8x, and because the ranking is sorted \
         descending on exactly that number, the row we got most wrong is the row \
         printed first",
    ),
    gap(
        Venue::Kucoin,
        RateLimit,
        Unknown,
        "a weighted per-endpoint pool whose numbers we do not have",
        "Unbudgeted, as with MEXC. The specific hazard on KuCoin is that futures \
         and spot are recalled as separate hosts with separate pools, so a limit \
         tuned by observing one of them tells us nothing about the other — and a \
         cash-and-carry uses both at once",
    ),
    // -- HTX ----------------------------------------------------------------
    gap(
        Venue::Htx,
        SymbolNaming,
        Unknown,
        "`BTC-USDT` for the linear swap and `BTC-USD` for the coin-margined swap, \
         with delivery futures using contract codes like `BTC_CQ` (current \
         quarter) that are not symbols at all",
        "Three naming systems on one exchange, and the quarterly codes MOVE between \
         instruments as quarters roll: `BTC_CQ` means a different contract after \
         the roll date. A stored `BTC_CQ` mapping silently retargets to another \
         expiry, so we would hedge one expiry with a position in another and call \
         it flat",
    ),
    gap(
        Venue::Htx,
        InverseContractSize,
        Assumed,
        "one coin-margined BTC swap contract is 100 USD, matching OKX's inverse \
         convention",
        "Assumed by analogy with OKX, which is the weakest kind of evidence in this \
         table — it is a guess about one venue derived from a guess about another. \
         If HTX uses 10 USD instead, every inverse size is 10x off, and the error \
         is in the direction of oversizing on the leg denominated in BTC",
    ),
    gap(
        Venue::Htx,
        FundingSign,
        Unknown,
        "positive rate means longs pay shorts, as everywhere else",
        "HTX is the venue where we have least reason to believe the convention \
         matches, because we recall the least about its docs. A flipped sign turns \
         a +12 bp carry into -12 bp: a 24 bp error per settlement on full notional, \
         which is our entire edge twice over, and it compounds every 8 hours we \
         hold the position",
    ),
];
