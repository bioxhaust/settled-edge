# FROZEN API DIGEST — read ONLY this, do not cat the foundation

Complete signatures of `arb-num` + `arb-types`. This file is sufficient. Reading
the foundation source is **forbidden this round** — three agents burned their
entire context doing it and wrote zero lines. If a signature you need is missing
here, report a change request; do not go looking.

`Price`, `Qty`, `Money`, `Rate` are `i128` at scale **1e12**. `Bps` is `i64` at
scale **1e8**, chosen so `Bps` and `Rate` share an identical raw integer.

## arb-num

```rust
pub enum NumError { Overflow, DivByZero, PrecisionLoss, InvalidFormat, ExponentOutOfRange }
pub enum RoundMode { Trunc, Floor, Ceil, AwayFromZero, HalfEven }
impl RoundMode {
    pub const COST:    RoundMode = RoundMode::AwayFromZero; // magnitude UP   — fees, slippage, margin
    pub const REVENUE: RoundMode = RoundMode::Trunc;        // magnitude DOWN — fills, funding received
}
pub fn mul_div(a: i128, b: i128, d: i128, mode: RoundMode) -> Result<i128, NumError>;
pub fn pow10_i128(n: u32) -> Option<i128>;
pub fn parse_scaled(s: &str, scale_digits: u32) -> Result<i128, NumError>;
pub fn format_scaled(v: i128, scale_digits: u32) -> String;
pub fn format_scaled_places(v: i128, scale_digits: u32, places: u32) -> String;
pub const SCALE_DIGITS: u32 = 12;      pub const SCALE: i128 = 1_000_000_000_000;
pub const BPS_SCALE_DIGITS: u32 = 8;   pub const BPS_SCALE: i64 = 100_000_000;

// Price, Qty, Money, Rate each have ALL of these.
// Derived: Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Display, Debug.
// There are NO operator impls (no Mul/Div/Add) — that is deliberate, decision 003.
impl Price /* and Qty, Money, Rate */ {
    pub const ZERO: Self; pub const ONE: Self; pub const MIN: Self; pub const MAX: Self;
    pub const EPSILON: Self;                       // 1e-12
    pub const fn from_raw(raw: i128) -> Self;      pub const fn raw(self) -> i128;
    pub fn from_int(n: i64)  -> Result<Self, NumError>;
    pub fn parse(s: &str)    -> Result<Self, NumError>;   // rejects >12 dp as PrecisionLoss
    pub fn checked_add(self, rhs: Self) -> Result<Self, NumError>;
    pub fn checked_sub(self, rhs: Self) -> Result<Self, NumError>;
    pub fn checked_neg(self) -> Result<Self, NumError>;
    pub fn checked_abs(self) -> Result<Self, NumError>;
    pub fn checked_mul_int(self, n: i64) -> Result<Self, NumError>;
    pub fn checked_div_int(self, n: i64, mode: RoundMode) -> Result<Self, NumError>;
    pub const fn signum(self) -> i32;
    pub const fn is_zero(self) -> bool;
    pub const fn is_positive(self) -> bool;
    pub const fn is_negative(self) -> bool;
    pub fn to_places(self, places: u32) -> String;
}

// Cross-dimension ops. EVERY one takes a RoundMode and returns Result<_, NumError>.
Price::mul_qty(Qty,   RoundMode) -> Money   // notional
Price::mul_rate(Rate, RoundMode) -> Price
Price::div_price(Price, RoundMode) -> Rate  // basis ratio
Qty::mul_price(Price, RoundMode) -> Money
Qty::mul_rate(Rate,  RoundMode) -> Qty
Qty::mul_qty(Qty,    RoundMode) -> Qty      // contracts * contract_size
Qty::div_qty(Qty,    RoundMode) -> Rate     // filled / requested
Money::mul_rate(Rate, RoundMode) -> Money   // every fee
Money::div_price(Price, RoundMode) -> Qty   // notional -> size
Money::div_qty(Qty,   RoundMode) -> Price   // *** THIS IS VWAP ***
Money::div_money(Money, RoundMode) -> Rate
Rate::mul_rate(Rate, RoundMode) -> Rate
Rate::div_rate(Rate, RoundMode) -> Rate
Rate::recip(RoundMode) -> Rate
Rate::to_bps() -> Result<Bps, NumError>     // checked narrowing
Rate::from_bps(Bps) -> Rate                 // const, exact, free

// Dimension-crossing escape hatches (const, no rounding) — for inverse contracts:
Price::reinterpret_as_money() -> Money
Qty::reinterpret_as_money()   -> Money
Money::reinterpret_as_qty()   -> Qty

pub struct Bps(i64);
impl Bps {
    pub const ZERO: Self; pub const ONE: Self;      // ONE == 1 basis point
    pub const ONE_PERCENT: Self;                    // == 100 bps
    pub const fn from_raw(raw: i64) -> Self;  pub const fn raw(self) -> i64;
    pub fn from_whole(bps: i64) -> Result<Self, NumError>;   // from_whole(12) == 12 bps
    pub fn parse_bps(s: &str)   -> Result<Self, NumError>;   // "12.5"   == 12.5 bps
    pub fn parse_ratio(s: &str) -> Result<Self, NumError>;   // "0.0001" == 1 bp
    pub const fn to_rate(self) -> Rate;                      // exact, free
    pub fn checked_add/checked_sub/checked_neg(..) -> Result<Self, NumError>;
    pub fn checked_mul_int(self, n: i64) -> Result<Self, NumError>;
    pub const fn is_zero(self) -> bool;  pub const fn is_negative(self) -> bool;
    pub fn to_places(self, places: u32) -> String;
}
```

## arb-types

```rust
pub enum TypeError { Empty, TooLong, InvalidChar, UnsortedLevels, NonPositiveLevel,
                     ZeroFundingInterval, NonPositivePrice, ContractSizeMismatch,
                     SettleMismatch }

pub enum Venue { Binance, Bybit, Okx, Bitget, Mexc, Gate, Kucoin, Htx }
impl Venue { pub const ALL: [Venue; 8];
             pub const fn as_str(self) -> &'static str;
             pub fn parse(s: &str) -> Result<Venue, TypeError>; }

// Inline Copy ASCII strings. Asset CAPACITY=16 (alnum), Symbol CAPACITY=32 (alnum + -_./:)
pub struct Asset;  pub struct Symbol;
impl Asset /* and Symbol */ {
    pub const CAPACITY: usize;
    pub fn new(s: &str) -> Result<Self, TypeError>;   // upper-cases input
    pub fn as_str(&self) -> &str;  pub fn len(&self) -> usize;  pub fn is_empty(&self) -> bool;
}

// ---- TIME: two clocks, deliberately NOT interconvertible (decision 008) ----
pub struct Millis(i64);
impl Millis { pub const ZERO: Self;
    pub const fn from_millis(ms: i64) -> Self;  pub const fn from_secs(secs: i64) -> Self;
    pub const fn millis(self) -> i64;  pub const fn abs(self) -> Self;
    pub const fn is_negative(self) -> bool; }

pub struct WallTs(i64);   // what the EXCHANGE said. Milliseconds.
impl WallTs { pub const EPOCH: Self;
    pub const fn from_millis(ms: i64) -> Self;  pub const fn millis(self) -> i64;
    pub const fn since(self, earlier: WallTs) -> Millis;
    pub const fn add_millis(self, d: Millis) -> Self;
    pub const fn apparent_skew_from(self, local: LocalTs) -> Millis; }

pub struct LocalTs(i64);  // when WE observed it. Milliseconds.
impl LocalTs { pub const EPOCH: Self;
    pub const fn from_millis(ms: i64) -> Self;  pub const fn millis(self) -> i64;
    pub const fn since(self, earlier: LocalTs) -> Millis;
    pub const fn add_millis(self, d: Millis) -> Self;
    pub const fn assume_no_skew(self) -> WallTs; }   // named to be ugly on purpose

// ---- DATA QUALITY: flows through every calculation (rule 7) ----
pub enum Provenance { LiveMeasured, RecordedFixture, SynthesizedUnverified }
impl Provenance { pub fn worst(self, other: Self) -> Self;
                  pub const fn tag(self) -> &'static str; }

pub struct QualityFlags(u32);
impl QualityFlags {
    pub const NONE: Self;
    pub const STALE: Self;                    // 1<<0
    pub const SEQ_GAP: Self;                  // 1<<1
    pub const CLOCK_SKEW: Self;               // 1<<2
    pub const CROSSED_BOOK: Self;             // 1<<3
    pub const THIN_DEPTH: Self;               // 1<<4
    pub const PAINTED_DEPTH: Self;            // 1<<5
    pub const PARTIAL_BOOK: Self;             // 1<<6
    pub const BOOK_EMPTY: Self;               // 1<<7
    pub const SIZE_EXCEEDS_DEPTH: Self;       // 1<<8
    pub const FUNDING_INTERVAL_ASSUMED: Self; // 1<<9
    pub const FEE_TIER_ASSUMED: Self;         // 1<<10
    pub const PRICE_UNAVAILABLE: Self;        // 1<<11
    pub const FUNDING_RATE_HISTORICAL: Self;  // 1<<12
    pub const BLOCKING: Self;                 // the subset that blocks trading
    pub const NAMED: [(Self, &'static str); 13];
    pub const fn bits(self) -> u32;
    pub const fn is_empty(self) -> bool;
    pub const fn contains(self, other: Self) -> bool;
    pub const fn intersects(self, other: Self) -> bool;
    pub const fn union(self, other: Self) -> Self;
    pub fn insert(&mut self, other: Self);
    pub fn remove(&mut self, other: Self);
    pub const fn is_blocking(self) -> bool;
}

pub struct DataQuality { pub flags: QualityFlags, pub provenance: Provenance }
impl DataQuality {
    pub const MEASURED: Self;      pub const SYNTHESIZED: Self;
    pub const fn new(flags: QualityFlags, provenance: Provenance) -> Self;
    pub fn combine(self, other: Self) -> Self;   // unions flags, keeps WORST provenance
    pub fn with_flags(self, flags: QualityFlags) -> Self;
    pub fn is_trustworthy(self) -> bool;         // measured AND no flags
    pub fn is_rankable(self) -> bool;            // may enter the ranking
    pub fn is_tradeable(self) -> bool;           // may be executed
}

// ---- INSTRUMENTS ----
pub enum MarketKind { Spot, LinearPerp, InversePerp }
impl MarketKind { pub const fn tag(self) -> &'static str; pub const fn is_perp(self) -> bool; }

pub enum ContractSize {
    BasePerContract(Qty),     // linear + spot: contract = N BASE units
    QuotePerContract(Money),  // inverse:       contract = N QUOTE units
}

pub struct ContractSpec {          // ALL FIELDS PUBLIC
    pub kind: MarketKind,
    pub size: ContractSize,
    pub settle: Asset,
    pub tick_size: Price,
    pub lot_size: Qty,
    pub min_notional: Option<Money>,
}
impl ContractSpec {
    // `new` is the SANCTIONED constructor: it enforces the cross-field
    // invariants (linear/spot must be BasePerContract, inverse must be
    // QuotePerContract, sizes positive, tick positive).
    pub fn new(kind: MarketKind, size: ContractSize, settle: Asset,
               tick_size: Price, lot_size: Qty,
               min_notional: Option<Money>) -> Result<Self, TypeError>;
    pub fn spot(settle: Asset, tick_size: Price, lot_size: Qty) -> Self;
    pub fn validate(&self) -> Result<(), TypeError>;   // for literal-built specs
    // THE ONLY sanctioned contract->exposure conversions (decision 004):
    pub fn notional(&self, contracts: Qty, price: Price, mode: RoundMode) -> Result<Money, NumError>;
    pub fn base_qty(&self, contracts: Qty, price: Price, mode: RoundMode) -> Result<Qty, NumError>;
}

pub struct InstrumentKey { pub base: Asset, pub quote: Asset,
                           pub kind: MarketKind, pub settle: Asset }
impl InstrumentKey {
    pub const fn new(base: Asset, quote: Asset, kind: MarketKind, settle: Asset) -> Self;
    pub fn parse(base: &str, quote: &str, kind: MarketKind, settle: &str) -> Result<Self, TypeError>;
    pub fn same_pair(&self, other: &InstrumentKey) -> bool;
}
pub struct VenueKey { pub venue: Venue, pub key: InstrumentKey }
impl VenueKey { pub const fn new(venue: Venue, key: InstrumentKey) -> Self; }

pub struct Instrument { pub vkey: VenueKey, pub native: Symbol, pub spec: ContractSpec }
impl Instrument {
    pub const fn new(vkey: VenueKey, native: Symbol, spec: ContractSpec) -> Self;
    pub const fn venue(&self) -> Venue;   pub const fn key(&self) -> InstrumentKey;
}

// ---- FUNDING ----
pub const SECONDS_PER_YEAR: i64 = 31_536_000;   // exactly 365 days, decision 005

pub enum PositionSide { Long, Short }
impl PositionSide {
    pub const fn funding_receipt_sign(self) -> i64;  // Long => -1, Short => +1
    pub const fn opposite(self) -> Self;
    pub const fn tag(self) -> &'static str;
}

pub struct FundingInterval(u32);
impl FundingInterval {
    pub const EIGHT_HOURS: Self; pub const FOUR_HOURS: Self;
    pub const TWO_HOURS: Self;   pub const ONE_HOUR: Self;
    pub const fn from_secs(secs: u32) -> Result<Self, TypeError>;
    pub const fn secs(self) -> u32;
    pub const fn as_millis(self) -> Millis;
    pub const fn periods_per_year_exact(self) -> Option<i64>;  // None if it does not divide evenly
}

// No constructor: build it with a struct literal. ALL FIELDS PUBLIC.
// NOTE the three Options and the field names — `mark`, not `mark_price`.
pub struct FundingSnapshot {
    pub vkey: VenueKey,
    pub rate: Rate,                        // the PREDICTED rate for next_funding_at (decision 007)
    pub interval: FundingInterval,
    pub next_funding_at: Option<WallTs>,   // None when the venue did not tell us
    pub mark: Option<Price>,               // None => caller sets PRICE_UNAVAILABLE
    pub index: Option<Price>,
    pub exchange_ts: WallTs,
    pub observed_at: LocalTs,
    pub quality: DataQuality,
}
impl FundingSnapshot {
    pub fn time_to_settlement(&self, now: WallTs) -> Option<Millis>;   // signed; None if no settlement time
    pub fn receipt_rate(&self, side: PositionSide) -> Result<Rate, NumError>;  // signed for holder
}

// ---- BOOKS ----
pub enum Side { Buy, Sell }
impl Side { pub const fn opposite(self) -> Self; pub const fn tag(self) -> &'static str; }

pub struct Level { pub price: Price, pub qty: Qty }
impl Level { pub fn new(price: Price, qty: Qty) -> Result<Self, TypeError>; }

// `bids`/`asks` are PRIVATE (the sort invariant is enforced in `new`).
// vkey / sequence / exchange_ts / observed_at / quality are PUBLIC fields.
pub struct BookSnapshot {
    pub vkey: VenueKey,
    pub sequence: Option<u64>,
    pub exchange_ts: Option<WallTs>,
    pub observed_at: LocalTs,
    pub quality: DataQuality,
}
impl BookSnapshot {
    // Rejects unsorted or duplicate-priced levels: bids must be strictly
    // descending, asks strictly ascending, else TypeError::UnsortedLevels.
    pub fn new(vkey: VenueKey, bids: Vec<Level>, asks: Vec<Level>,
               sequence: Option<u64>, exchange_ts: Option<WallTs>,
               observed_at: LocalTs, quality: DataQuality) -> Result<Self, TypeError>;
    pub fn bids(&self) -> &[Level];   // descending price
    pub fn asks(&self) -> &[Level];   // ascending price
    pub fn taking_levels(&self, side: Side) -> &[Level];  // Buy => ASKS. USE THIS.
    pub fn touch(&self, side: Side) -> Option<Price>;
    pub fn best_bid(&self) -> Option<Level>;
    pub fn best_ask(&self) -> Option<Level>;
    pub fn mid(&self) -> Option<Price>;
    pub fn is_empty(&self) -> bool;
    pub fn total_depth(&self, side: Side) -> Result<Qty, NumError>;
}
```

## The laws, in one screen

- Costs use `RoundMode::COST`, proceeds use `RoundMode::REVENUE`. Unsure? Pick
  whichever makes the opportunity look **worse**.
- Positive funding rate ⇒ **longs pay shorts**. Only sanctioned path:
  `PositionSide::funding_receipt_sign`. Never hand-write a minus sign next to a
  funding rate. Cash-and-carry (long spot, short perp) **earns** when positive.
- Annualise as `rate * SECONDS_PER_YEAR / interval.secs()` via exact `mul_div`.
  Never multiply by a rounded period count.
- Funding intervals are **per instrument**, never per venue. 8h assumed for a 1h
  instrument overstates APR **8x**, and ranking puts that row first.
- Buying lifts **asks**; selling hits **bids**. Use `taking_levels(side)`.
- Linear vs inverse notional differ. Only `ContractSpec::notional` /
  `ContractSpec::base_qty` may convert contracts to exposure.
- `DataQuality::combine` keeps the **worst** provenance. Fixture-derived numbers
  stay `SynthesizedUnverified` all the way to the screen.
