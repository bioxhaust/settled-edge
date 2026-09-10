# FROZEN API — arb-num + arb-types (Gate 0)

`LEAD` owns these two crates; nobody else edits them. Need a change? Message the
lead in `team/inbox/LEAD.md`. **Do not work around a missing method** (charter §4).

`[fact, verified 2026-08-25]` This listing was extracted from the source on
2026-08-25 by a signature pass that joins multi-line declarations and cuts each
file at its first `#[cfg(test)]`, then hand-checked against the files named. It is
**not** produced by a committed generator — no such script exists in the
workspace, which is why the previous edition drifted two rounds behind the code.
Treat the date as part of the claim: this document is a snapshot, and
`decisions/028` records what a stale snapshot costs.

Superseded edition: `decisions/030`. Rewritten under charter rule 8 — every claim
here carries a tag, which the previous edition did not.

## How to read this

`[fact, verified 2026-08-25]` Signatures are complete: where a declaration spans
several lines in the source it is joined here, not truncated. The previous edition
rendered five multi-line items as `pub fn new(;`, `pub fn notional(;`,
`pub fn parse(;`, `BLOCKING = Self(;` and `NAMED = [;` — present-looking and
empty, which is worse than absent. One of them cost a compile failure that stopped
the whole workspace (`decisions/030`).

**Public struct fields are listed.** They are part of the contract: these types
are built as struct literals throughout the workspace, so a field name is as
load-bearing as a method name. Fields marked `(private)` cannot be set from
outside the crate — for those types the constructor is the only way in, and that
is deliberate: it is what forces validation.

---

## arb-num

Re-exported at the crate root: `Bps, Money, Price, Qty, Rate, RoundMode,
NumError, mul_div, format_scaled, format_scaled_places, parse_scaled,
parse_scaled_rounded, pow10_i128, BPS_SCALE, BPS_SCALE_DIGITS, SCALE,
SCALE_DIGITS`.

### Scale constants — `units.rs`

```rust
pub const SCALE_DIGITS: u32 = 12;
pub const SCALE: i128 = 1_000_000_000_000;
pub const BPS_SCALE_DIGITS: u32 = 8;
pub const BPS_SCALE: i64 = 100_000_000;
```

### `decimal.rs`

```rust
pub fn pow10_i128(n: u32) -> Option<i128>;
pub fn parse_scaled(s: &str, scale_digits: u32) -> Result<i128, NumError>;
pub fn parse_scaled_rounded(
    s: &str,
    scale_digits: u32,
    mode: RoundMode,
) -> Result<i128, NumError>;
pub fn format_scaled(v: i128, scale_digits: u32) -> String;
pub fn format_scaled_places(v: i128, scale_digits: u32, places: u32) -> String;
```

`[fact, verified 2026-08-25]` `parse_scaled` **refuses** to drop digits:
`"0.0000689368148824"` at scale 12 is `NumError::PrecisionLoss`, not a truncated
value. `parse_scaled_rounded` is the sanctioned way to accept it, and it exists
because OKX publishes `fundingRate` to sixteen decimal places — four below our
scale. Without it an adapter reports no funding at all for that venue.
**Do not pre-truncate the string by hand outside these two functions**: that moves
a rounding decision out of `RoundMode` and into an adapter, which is the thing
`decisions/003` exists to prevent.

### `round.rs`

```rust
pub enum RoundMode { Trunc, Floor, Ceil, AwayFromZero, HalfEven }

impl RoundMode {
    pub const COST: RoundMode = RoundMode::AwayFromZero;
    pub const REVENUE: RoundMode = RoundMode::Trunc;
}
```

### `muldiv.rs`

```rust
pub fn mul_div(a: i128, b: i128, d: i128, mode: RoundMode) -> Result<i128, NumError>;
```

### `error.rs`

```rust
pub enum NumError {
    Overflow, DivByZero, PrecisionLoss, InvalidFormat,
    ExponentOutOfRange, NegativeNotAllowed,
}
```

### Dimensioned cross-operations — `units.rs`

Every one is a `mul_div`, so every one can drop a sub-unit remainder and every one
demands a `RoundMode`.

```rust
impl Price {
    pub fn mul_qty(self, qty: Qty, mode: RoundMode) -> Result<Money, NumError>;
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Price, NumError>;
    pub fn div_price(self, other: Price, mode: RoundMode) -> Result<Rate, NumError>;
    pub const fn reinterpret_as_money(self) -> Money;
}

impl Qty {
    pub fn mul_price(self, price: Price, mode: RoundMode) -> Result<Money, NumError>;
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn mul_qty(self, other: Qty, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn div_size(self, size: Qty, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn div_qty(self, other: Qty, mode: RoundMode) -> Result<Rate, NumError>;
    pub const fn reinterpret_as_money(self) -> Money;
}

impl Money {
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Money, NumError>;
    pub fn div_price(self, price: Price, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn div_qty(self, qty: Qty, mode: RoundMode) -> Result<Price, NumError>;
    pub fn div_money(self, other: Money, mode: RoundMode) -> Result<Rate, NumError>;
    pub const fn reinterpret_as_qty(self) -> Qty;
}

impl Rate {
    pub fn mul_rate(self, other: Rate, mode: RoundMode) -> Result<Rate, NumError>;
    pub fn div_rate(self, other: Rate, mode: RoundMode) -> Result<Rate, NumError>;
    pub fn recip(self, mode: RoundMode) -> Result<Rate, NumError>;
    pub fn to_bps(self) -> Result<Bps, NumError>;
    pub const fn from_bps(bps: Bps) -> Rate;
}
```

`[fact, verified 2026-08-25]` There is **no `Money::div_rate`**. `Rate::div_rate`
divides a rate by a rate; it will not divide money by a rate. `OPEN.md` C3 records
why the missing one was not added: dividing `notional` by `leverage` in two steps
rounds twice, both away from zero, which makes `margin_required` pessimistic by
about 2e-12 relative — safe in direction and far below the edge. That is an
`OPEN.md` C3 case, not a `decisions/014` case: the formula stays expressible.

`[fact, verified 2026-08-25]` `reinterpret_as_money` / `reinterpret_as_qty` are
**not** conversions. They reinterpret the same raw integer under a different
dimension and exist only where the dimension was already known to be wrong by
construction. They are `const` and infallible precisely because they compute
nothing; if you reach for one to make types line up, the formula is wrong.

### `Bps` — `units.rs`

```rust
pub struct Bps(i64);

impl Bps {
    pub const ZERO: Self;
    pub const ONE: Self;             // one basis point
    pub const ONE_PERCENT: Self;
    pub const fn from_raw(raw: i64) -> Self;
    pub const fn raw(self) -> i64;
    pub fn from_whole(bps: i64) -> Result<Self, NumError>;
    pub fn parse_bps(s: &str) -> Result<Self, NumError>;    // "5.5" => 5.5 bp
    pub fn parse_ratio(s: &str) -> Result<Self, NumError>;  // "0.0001" => 1 bp
    pub const fn to_rate(self) -> Rate;
    pub fn checked_add(self, rhs: Self) -> Result<Self, NumError>;
    pub fn checked_sub(self, rhs: Self) -> Result<Self, NumError>;
    pub fn checked_neg(self) -> Result<Self, NumError>;
    pub fn checked_mul_int(self, n: i64) -> Result<Self, NumError>;
    pub const fn is_zero(self) -> bool;
    pub const fn is_negative(self) -> bool;
    pub const fn is_positive(self) -> bool;
    pub fn to_places(self, places: u32) -> String;
}
```

`parse_bps` and `parse_ratio` read the **same number two ways**. `"0.0001"`
through `parse_bps` is a ten-thousandth of a basis point; through `parse_ratio` it
is one basis point. Venues publish both conventions, so pick by what the payload
means, never by which one parses.

---

## Macro-generated surface on `Price`, `Qty`, `Money`, `Rate`

`[fact, verified 2026-08-25]` These four are instantiated from one macro
(`fixed_i128!`, `units.rs:179-185`), so each has all of the following,
identically:

```rust
pub const ZERO: Self;
pub const ONE: Self;                 // == SCALE
pub const MIN: Self;                 // i128::MIN — a sentinel, not a price
pub const MAX: Self;
pub const EPSILON: Self;             // raw 1: one unit in the last place, 1e-12
pub const fn from_raw(raw: i128) -> Self;   // caller asserts the 1e12 scaling
pub const fn raw(self) -> i128;
pub fn from_int(n: i64) -> Result<Self, NumError>;
pub fn parse(s: &str) -> Result<Self, NumError>;                    // PrecisionLoss, never truncation
pub fn parse_rounded(s: &str, mode: RoundMode) -> Result<Self, NumError>;
pub fn checked_add(self, rhs: Self) -> Result<Self, NumError>;
pub fn checked_sub(self, rhs: Self) -> Result<Self, NumError>;
pub fn checked_neg(self) -> Result<Self, NumError>;
pub fn checked_abs(self) -> Result<Self, NumError>;
pub fn checked_mul_int(self, n: i64) -> Result<Self, NumError>;      // exact, no rounding
pub fn checked_div_int(self, n: i64, mode: RoundMode) -> Result<Self, NumError>;
pub const fn signum(self) -> i32;
pub const fn is_zero(self) -> bool;
pub const fn is_positive(self) -> bool;
pub const fn is_negative(self) -> bool;
pub fn to_places(self, places: u32) -> String;                      // fixed-width table output
```

Plus `Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Display, Debug`.
`Debug` prints the decimal value, not the raw integer.

`parse_rounded` is the per-type twin of `parse_scaled_rounded` and exists for the
same reason. `parse` is the default: a venue string that does not fit the scale is
an error, not a rounding opportunity.

## There are deliberately no operator impls

No `Add`, `Sub`, `Mul`, `Div` on any money type. Every lossy operation demands an
explicit `RoundMode` — `decisions/003`. Use `RoundMode::COST` for anything we pay
and `RoundMode::REVENUE` for anything we receive, so the reported edge is a lower
bound.

---

## arb-types

Re-exported at the crate root: `BookSnapshot, Level, Side, BorrowInterval,
BorrowSnapshot, FundingInterval, FundingSnapshot, PositionSide,
SECONDS_PER_YEAR, RateHistory, SettledRate, ContractSize, ContractSpec,
Instrument, InstrumentKey, MarketKind, SizeUnit, VenueKey, DataQuality,
Provenance, QualityFlags, Asset, Symbol, LocalTs, Millis, WallTs, Venue,
TypeError`.

### `book.rs`

```rust
pub enum Side { Buy, Sell }

impl Side {
    pub const fn opposite(self) -> Self;
    pub const fn tag(self) -> &'static str;
}

pub struct Level {
    pub price: Price,
    pub qty: Qty,        // NOT `size`
}

impl Level {
    pub fn new(price: Price, qty: Qty) -> Result<Self, TypeError>;
}

pub struct BookSnapshot {
    pub vkey: VenueKey,
    bids: Vec<Level>,                 // (private) strictly descending; index 0 is best bid
    asks: Vec<Level>,                 // (private) strictly ascending;  index 0 is best ask
    pub sequence: Option<u64>,
    pub exchange_ts: Option<WallTs>,
    pub observed_at: LocalTs,         // our clock, not the venue's
    pub quality: DataQuality,
}

impl BookSnapshot {
    pub fn new(
        vkey: VenueKey,
        bids: Vec<Level>,
        asks: Vec<Level>,
        sequence: Option<u64>,
        exchange_ts: Option<WallTs>,
        observed_at: LocalTs,
        quality: DataQuality,
    ) -> Result<Self, TypeError>;

    pub fn bids(&self) -> &[Level];
    pub fn asks(&self) -> &[Level];
    pub fn taking_levels(&self, side: Side) -> &[Level];
    pub fn touch(&self, side: Side) -> Option<Price>;
    pub fn best_bid(&self) -> Option<Level>;
    pub fn best_ask(&self) -> Option<Level>;
    pub fn mid(&self) -> Option<Price>;
    pub fn is_empty(&self) -> bool;
    pub fn total_depth(&self, side: Side) -> Result<Qty, arb_num::NumError>;
}
```

`[fact, verified 2026-08-25]` `bids` and `asks` are **private**, so a struct
literal for `BookSnapshot` will not compile outside `arb-types`. `new` is the only
way in, it takes **seven** arguments, and it validates ordering and positivity
before it returns — that is the point of closing the fields. `sequence` and
`exchange_ts` are `Option` because many payloads omit them; `None` is a legitimate
answer (`decisions/016`), and `new` raises the flags that follow from it.

### `borrow.rs`

```rust
pub struct BorrowInterval(u32);

impl BorrowInterval {
    pub const HOURLY: Self;                       // 3600
    pub const DAILY: Self;                        // 86400
    pub const fn from_secs(secs: u32) -> Result<Self, TypeError>;
    pub const fn secs(self) -> u32;
    pub const fn as_millis(self) -> Millis;
    pub const fn periods_per_year_exact(self) -> Option<i64>;
}

pub struct BorrowSnapshot {
    pub vkey: VenueKey,
    pub asset: Asset,                 // the BASE asset for a short spot leg
    pub rate: Rate,                   // per one `interval`; non-negative by construction
    pub interval: BorrowInterval,
    pub exchange_ts: Option<WallTs>,
    pub observed_at: LocalTs,
    pub quality: DataQuality,
}

impl BorrowSnapshot {
    pub fn new(
        vkey: VenueKey,
        asset: Asset,
        rate: Rate,
        interval: BorrowInterval,
        exchange_ts: Option<WallTs>,
        observed_at: LocalTs,
        quality: DataQuality,
    ) -> Result<Self, TypeError>;

    pub const fn is_free(&self) -> bool;
}
```

`[fact, verified 2026-08-25]` `BorrowInterval` is a **separate type** from
`FundingInterval`, and there is deliberately no `BorrowInterval::EIGHT_HOURS`:
nobody quotes margin interest per eight hours, and offering the constant would
invite reading a daily rate as an eight-hourly one. The hourly/daily confusion is
exactly 24×, in the direction that makes an opportunity appear — `decisions/025`.
A negative rate is `TypeError::NegativeBorrowRate` at construction: negative
interest would enter `net_apr` as income and rank the row **first**.

### `funding.rs`

```rust
pub const SECONDS_PER_YEAR: i64 = 365 * 24 * 60 * 60;   // 31_536_000

pub enum PositionSide { Long, Short }

impl PositionSide {
    pub const fn funding_receipt_sign(self) -> i64;
    pub const fn opposite(self) -> Self;
    pub const fn tag(self) -> &'static str;
}

pub struct FundingInterval(u32);

impl FundingInterval {
    pub const EIGHT_HOURS: Self;
    pub const FOUR_HOURS: Self;
    pub const TWO_HOURS: Self;
    pub const ONE_HOUR: Self;
    pub const fn from_secs(secs: u32) -> Result<Self, TypeError>;
    pub const fn secs(self) -> u32;
    pub const fn as_millis(self) -> Millis;
    pub const fn periods_per_year_exact(self) -> Option<i64>;
}

pub struct FundingSnapshot {
    pub vkey: VenueKey,
    pub rate: Rate,                       // applied at the NEXT settlement
    pub interval: FundingInterval,
    pub next_funding_at: Option<WallTs>,
    pub mark: Option<Price>,              // funding is charged against this
    pub index: Option<Price>,
    pub exchange_ts: WallTs,              // NOT Option, unlike BookSnapshot
    pub observed_at: LocalTs,
    pub quality: DataQuality,
}

impl FundingSnapshot {
    pub fn time_to_settlement(&self, now: WallTs) -> Option<Millis>;
    pub fn receipt_rate(&self, side: PositionSide) -> Result<Rate, arb_num::NumError>;
}
```

### `history.rs`

```rust
pub struct SettledRate {
    pub settled_at: WallTs,
    pub rate: Rate,          // signed; the sign law of funding_receipt_sign applies
}

pub struct RateHistory {
    pub vkey: VenueKey,
    entries: Vec<SettledRate>,   // (private) oldest first, strictly increasing in time
    pub quality: DataQuality,    // one quality for the series, not per entry
}

impl RateHistory {
    pub fn new(
        vkey: VenueKey,
        entries: Vec<SettledRate>,
        quality: DataQuality,
    ) -> Result<Self, TypeError>;

    pub fn empty(vkey: VenueKey, quality: DataQuality) -> Self;
    pub fn entries(&self) -> &[SettledRate];
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn latest(&self) -> Option<SettledRate>;
    pub fn oldest(&self) -> Option<SettledRate>;
    pub fn span(&self) -> Option<Millis>;
    pub fn current_sign_run(&self) -> usize;
}
```

`[fact, verified 2026-08-25]` Entries carry **settled** rates, never forecasts,
and are strictly increasing in `settled_at` — a duplicate stamp is
`TypeError::UnsortedRateHistory`. Both properties are load-bearing: a
double-counted period makes a run of agreement look one period longer than it was,
and mixing a forecast into the series overstates persistence. `RateHistory`
computes **no probability** by design — the verdict is `arb-detect`'s, the
arithmetic `arb-math`'s. An empty history obliges the caller to raise
`QualityFlags::RATE_HISTORY_MISSING`.

### `instrument.rs`

```rust
pub enum MarketKind { Spot, LinearPerp, InversePerp }

impl MarketKind {
    pub const fn tag(self) -> &'static str;
    pub const fn is_perp(self) -> bool;
}

pub enum ContractSize {
    BasePerContract(Qty),      // linear:  ctVal x ctMult
    QuotePerContract(Money),   // inverse: price does NOT enter the notional
}

pub enum SizeUnit { Base, Contracts }

impl SizeUnit {
    pub const fn implied_by(size: ContractSize) -> Self;
    pub const fn as_str(self) -> &'static str;
}

pub struct ContractSpec {
    pub kind: MarketKind,
    pub size: ContractSize,
    pub settle: Asset,
    pub tick_size: Price,
    pub size_unit: SizeUnit,        // declared by the venue, never inferred
    pub lot_size: Qty,              // in `size_unit`; NOT the contract multiplier
    pub min_notional: Option<Money>,
}

impl ContractSpec {
    pub fn notional(
        &self,
        contracts: Qty,
        price: Price,
        mode: RoundMode,
    ) -> Result<Money, NumError>;

    pub fn base_qty(&self, contracts: Qty, price: Price, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn spot(settle: Asset, tick_size: Price, lot_size: Qty) -> Self;
    pub fn new(
        kind: MarketKind,
        size: ContractSize,
        settle: Asset,
        tick_size: Price,
        size_unit: SizeUnit,
        lot_size: Qty,
        min_notional: Option<Money>,
    ) -> Result<Self, TypeError>;

    pub fn validate(&self) -> Result<(), TypeError>;
    pub fn check_settle(&self, key: &InstrumentKey) -> Result<(), TypeError>;
}

pub struct InstrumentKey {
    pub base: Asset,
    pub quote: Asset,
    pub kind: MarketKind,     // part of the key: spot and perp are different instruments
    pub settle: Asset,        // part of the key: BTC/USD settled in USDT vs in BTC differ
}

impl InstrumentKey {
    pub const fn new(base: Asset, quote: Asset, kind: MarketKind, settle: Asset) -> Self;
    pub fn parse(
        base: &str,
        quote: &str,
        kind: MarketKind,
        settle: &str,
    ) -> Result<Self, TypeError>;
    pub fn same_pair(&self, other: &InstrumentKey) -> bool;
}

pub struct VenueKey {
    pub venue: Venue,
    pub key: InstrumentKey,
}

impl VenueKey {
    pub const fn new(venue: Venue, key: InstrumentKey) -> Self;
}

pub struct Instrument {
    pub vkey: VenueKey,
    pub native: Symbol,       // the symbol string THIS venue expects
    pub spec: ContractSpec,
}

impl Instrument {
    pub const fn new(vkey: VenueKey, native: Symbol, spec: ContractSpec) -> Self;
    pub const fn venue(&self) -> Venue;
    pub const fn key(&self) -> InstrumentKey;
}
```

`[fact, verified 2026-08-25]` **`lot_size` is not the contract multiplier.** The
contracts-to-base factor is `ContractSize::BasePerContract`, built from the
venue's `ctVal × ctMult`; `lot_size` is an independent minimum size increment.
On `BTC-USDT-SWAP` both happen to be `0.01`, so substituting one for the other
gives the right answer there and a wrong one elsewhere. This exact substitution
appeared in a round-2 derivation and is adjudicated in `decisions/028`.

`[fact, verified 2026-08-25]` `validate` deliberately does **not** require
`size_unit` to equal `SizeUnit::implied_by(size)`. Disagreement is a real signal
about the payload, not an occasion to paper over it — so a self-contradictory spec
is accepted and nothing flags it. `OPEN.md` D5 keeps that open.

### `quality.rs`

```rust
pub enum Provenance { LiveMeasured, RecordedFixture, SynthesizedUnverified }

impl Provenance {
    pub fn worst(self, other: Self) -> Self;
    pub const fn tag(self) -> &'static str;
}

pub struct QualityFlags(u32);

impl QualityFlags {
    pub const NONE: Self = Self(0);                        // the empty set; there is no `EMPTY`
    pub const STALE: Self = Self(1 << 0);
    pub const SEQ_GAP: Self = Self(1 << 1);
    pub const CLOCK_SKEW: Self = Self(1 << 2);
    pub const CROSSED_BOOK: Self = Self(1 << 3);
    pub const THIN_DEPTH: Self = Self(1 << 4);
    pub const PAINTED_DEPTH: Self = Self(1 << 5);
    pub const PARTIAL_BOOK: Self = Self(1 << 6);
    pub const BOOK_EMPTY: Self = Self(1 << 7);
    pub const SIZE_EXCEEDS_DEPTH: Self = Self(1 << 8);
    pub const FUNDING_INTERVAL_ASSUMED: Self = Self(1 << 9);
    pub const FEE_TIER_ASSUMED: Self = Self(1 << 10);
    pub const PRICE_UNAVAILABLE: Self = Self(1 << 11);
    pub const FUNDING_RATE_HISTORICAL: Self = Self(1 << 12);
    pub const BORROW_COST_UNKNOWN: Self = Self(1 << 13);
    pub const RATE_HISTORY_MISSING: Self = Self(1 << 14);

    pub const BLOCKING: Self = Self(
        Self::STALE.0
            | Self::SEQ_GAP.0
            | Self::CROSSED_BOOK.0
            | Self::PAINTED_DEPTH.0
            | Self::BOOK_EMPTY.0
            | Self::SIZE_EXCEEDS_DEPTH.0
            | Self::PRICE_UNAVAILABLE.0
            | Self::BORROW_COST_UNKNOWN.0,
    );

    pub const NAMED: [(Self, &'static str); 15] = [
        (Self::STALE, "STALE"),
        (Self::SEQ_GAP, "SEQ_GAP"),
        (Self::CLOCK_SKEW, "CLOCK_SKEW"),
        (Self::CROSSED_BOOK, "CROSSED"),
        (Self::THIN_DEPTH, "THIN"),
        (Self::PAINTED_DEPTH, "PAINTED"),
        (Self::PARTIAL_BOOK, "PARTIAL"),
        (Self::BOOK_EMPTY, "EMPTY"),
        (Self::SIZE_EXCEEDS_DEPTH, "OVER_DEPTH"),
        (Self::FUNDING_INTERVAL_ASSUMED, "IVL_ASSUMED"),
        (Self::FEE_TIER_ASSUMED, "FEE_ASSUMED"),      // label differs from the constant
        (Self::PRICE_UNAVAILABLE, "NO_PRICE"),
        (Self::FUNDING_RATE_HISTORICAL, "RATE_HIST"),
        (Self::BORROW_COST_UNKNOWN, "BORROW_UNKNOWN"),
        (Self::RATE_HISTORY_MISSING, "NO_HISTORY"),
    ];

    pub const fn bits(self) -> u32;
    pub const fn is_empty(self) -> bool;
    pub const fn contains(self, other: Self) -> bool;
    pub const fn intersects(self, other: Self) -> bool;
    pub const fn union(self, other: Self) -> Self;
    pub const fn intersection(self, other: Self) -> Self;
    pub fn insert(&mut self, other: Self);
    pub fn remove(&mut self, other: Self);
    pub const fn is_blocking(self) -> bool;
}

pub struct DataQuality {
    pub flags: QualityFlags,
    pub provenance: Provenance,
}

impl DataQuality {
    pub const MEASURED: Self;        // NONE + LiveMeasured
    pub const SYNTHESIZED: Self;     // NONE + SynthesizedUnverified
    pub const fn new(flags: QualityFlags, provenance: Provenance) -> Self;  // flags FIRST
    pub fn combine(self, other: Self) -> Self;
    pub fn with_flags(mut self, flags: QualityFlags) -> Self;
    pub fn is_trustworthy(self) -> bool;
    pub fn is_rankable(self) -> bool;
    pub fn is_tradeable(self) -> bool;
}
```

`[fact, verified 2026-08-25]` Three details that have each been guessed wrong:
the empty set is **`NONE`**, not `EMPTY`; `DataQuality::new` takes **flags first,
provenance second**; and the printed label of `FEE_TIER_ASSUMED` is the string
`"FEE_ASSUMED"`, so grepping a report for the constant's name finds nothing.

`[fact, verified 2026-08-25]` `BLOCKING` contains `BORROW_COST_UNKNOWN` and does
**not** contain `FEE_TIER_ASSUMED`. That asymmetry is the whole of
`decisions/026`: an unmeasured *cost* makes the number wrong, an assumed *tier*
makes it pessimistic, and only the first may not be ranked. `combine` unions flags
and takes the **worse** provenance — that is the mechanism of rule 7.

### `strings.rs`

```rust
pub struct Asset;     // CAPACITY 16 — alphanumeric, upper-case: BTC, USDT, 1000PEPE
pub struct Symbol;    // CAPACITY 32 — venue-native, permits -_./: : BTC-USDT-SWAP

// identical surface on both:
pub const CAPACITY: usize;
pub fn new(s: &str) -> Result<Self, TypeError>;
pub fn as_str(&self) -> &str;
pub fn len(&self) -> usize;
pub fn is_empty(&self) -> bool;
```

Inline, fixed-capacity, no heap. `Symbol` is never a join key across venues —
that is `InstrumentKey`.

### `time.rs`

```rust
pub struct Millis(i64);

impl Millis {
    pub const ZERO: Self;
    pub const fn from_millis(ms: i64) -> Self;
    pub const fn from_secs(secs: i64) -> Self;
    pub const fn millis(self) -> i64;
    pub const fn abs(self) -> Self;
    pub const fn is_negative(self) -> bool;
}

pub struct WallTs(i64);            // what the exchange said

impl WallTs {
    pub const EPOCH: Self;
    pub const fn from_millis(ms: i64) -> Self;
    pub const fn millis(self) -> i64;
    pub const fn since(self, earlier: WallTs) -> Millis;
    pub const fn add_millis(self, d: Millis) -> Self;
    pub const fn apparent_skew_from(self, local: LocalTs) -> Millis;
}

pub struct LocalTs(i64);           // when we observed it

impl LocalTs {
    pub const EPOCH: Self;
    pub const fn from_millis(ms: i64) -> Self;
    pub const fn millis(self) -> i64;
    pub const fn since(self, earlier: LocalTs) -> Millis;
    pub const fn add_millis(self, d: Millis) -> Self;
    pub const fn assume_no_skew(self) -> WallTs;
}
```

`[fact, verified 2026-08-25]` There is no conversion between `WallTs` and
`LocalTs` except `assume_no_skew`, whose name is the warning — `decisions/008`.
Staleness is always measured on `LocalTs`. Subtracting one clock from the other is
how stale data looks fresh.

### `venue.rs`

```rust
pub enum Venue { Binance, Bybit, Okx, Bitget, Mexc, Gate, Kucoin, Htx }

impl Venue {
    pub const ALL: [Venue; 8] = [
        Venue::Binance, Venue::Bybit, Venue::Okx, Venue::Bitget,
        Venue::Mexc, Venue::Gate, Venue::Kucoin, Venue::Htx,
    ];
    pub const fn as_str(self) -> &'static str;
    pub fn parse(s: &str) -> Result<Venue, TypeError>;
}
```

### `lib.rs`

```rust
pub enum TypeError {
    Empty, TooLong, InvalidChar,
    UnsortedLevels, NonPositiveLevel,
    ZeroFundingInterval, NonPositivePrice,
    ContractSizeMismatch, SettleMismatch,
    ZeroBorrowInterval, NegativeBorrowRate, UnsortedRateHistory,
}
```

---

## Non-negotiable conventions

`[fact, verified 2026-08-25]` Every citation below was checked against the body of
the decision named. The previous edition mis-cited five of these rows; the errors
are listed in `decisions/030`.

| Question | Answer | Authority |
|---|---|---|
| Scale of money types | 1e12 over i128 | `decisions/001` |
| 1 bp as a raw integer | `100_000_000`, same in `Bps` and `Rate` | `decisions/002` |
| Why every lossy op takes a `RoundMode` | no default rounding exists; costs round against us | `decisions/003` |
| Inverse contract notional | `contracts * quote_per_contract`, price does NOT appear | `decisions/004` |
| Seconds per year | `31_536_000` (365 d exactly) | `decisions/005` |
| Annualisation | `mul_div(rate, SECONDS_PER_YEAR, interval_secs)` — never a rounded period count | `decisions/005` |
| Funding sign | rate > 0 ⇒ longs pay shorts; use `PositionSide::funding_receipt_sign()` | `decisions/006` |
| Predicted vs historical funding rate | separate concerns; `FUNDING_RATE_HISTORICAL` marks the substitution | `decisions/007` |
| Staleness measured against | `LocalTs`, our clock, never the venue's | `decisions/008` |
| Which levels a buy consumes | asks — use `book.taking_levels(Side::Buy)` | `arb-types::book` |
| The three quality predicates | `is_trustworthy` / `is_rankable` / `is_tradeable`, and why they differ | `decisions/009` |
| Adding a method after the freeze | additive only, `LEAD` only, and only if absence makes a correct formula inexpressible | `decisions/014` |
| Ladder units | declared, never inferred; `size_unit` is required | `decisions/019`, `decisions/024` |
| Quality of fixture-derived data | `Provenance::SynthesizedUnverified`, propagated by `DataQuality::combine` | rule 7 |
| Borrow rate sign and period | non-negative at construction; period travels in `BorrowInterval` | `decisions/025` |
| Assumed fee tier | a flag, not a verdict — `FEE_TIER_ASSUMED` is not `BLOCKING` | `decisions/026` |

## When you think a method is missing

`[fact, verified 2026-08-25]` Classify before asking, because the two cases have
opposite answers (`decisions/014` vs `OPEN.md` C3):

- Absence makes a correct formula **inexpressible**, or expressible only by
  re-implementing the arithmetic outside `arb-num` → that is a `decisions/014`
  case. Message `LEAD`; the method gets added additively.
- Absence only makes a correct formula **slightly pessimistic**, in the safe
  direction → the freeze holds, the gap is recorded in `OPEN.md`, and nothing is
  added this round. `Money::div_rate` is the worked example.

Either way: do not write a local helper that repeats the arithmetic, and do not
bend the formula to what is expressible. Both compile, and both are defects
(charter §4).
