# FROZEN API — arb-num + arb-types (Gate 0)

Generated from source. LEAD owns these two crates; nobody else edits them.
Need a change? Message the lead. Do not work around a missing method.

## arb-num
```rust
// ---- decimal.rs ----
pub fn pow10_i128(n: u32) -> Option<i128>;
pub fn parse_scaled(s: &str, scale_digits: u32) -> Result<i128, NumError>;
pub fn format_scaled(v: i128, scale_digits: u32) -> String;
pub fn format_scaled_places(v: i128, scale_digits: u32, places: u32) -> String;
// ---- error.rs ----
pub enum NumError;
impl fmt::Display for NumError;
impl std::error::Error for NumError {};
// ---- lib.rs ----
// ---- muldiv.rs ----
pub fn mul_div(a: i128, b: i128, d: i128, mode: RoundMode) -> Result<i128, NumError>;
// ---- round.rs ----
pub enum RoundMode;
impl RoundMode;
    pub const COST: RoundMode = RoundMode::AwayFromZero;
    pub const REVENUE: RoundMode = RoundMode::Trunc;
// ---- units.rs ----
pub const SCALE_DIGITS: u32 = 12;
pub const SCALE: i128 = 1_000_000_000_000;
pub const BPS_SCALE_DIGITS: u32 = 8;
pub const BPS_SCALE: i64 = 100_000_000;
impl Price;
    pub fn mul_qty(self, qty: Qty, mode: RoundMode) -> Result<Money, NumError>;
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Price, NumError>;
    pub fn div_price(self, other: Price, mode: RoundMode) -> Result<Rate, NumError>;
    pub const fn reinterpret_as_money(self) -> Money;
impl Qty;
    pub fn mul_price(self, price: Price, mode: RoundMode) -> Result<Money, NumError>;
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn mul_qty(self, other: Qty, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn div_qty(self, other: Qty, mode: RoundMode) -> Result<Rate, NumError>;
    pub const fn reinterpret_as_money(self) -> Money;
impl Money;
    pub fn mul_rate(self, rate: Rate, mode: RoundMode) -> Result<Money, NumError>;
    pub fn div_price(self, price: Price, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn div_qty(self, qty: Qty, mode: RoundMode) -> Result<Price, NumError>;
    pub fn div_money(self, other: Money, mode: RoundMode) -> Result<Rate, NumError>;
    pub const fn reinterpret_as_qty(self) -> Qty;
impl Rate;
    pub fn mul_rate(self, other: Rate, mode: RoundMode) -> Result<Rate, NumError>;
    pub fn div_rate(self, other: Rate, mode: RoundMode) -> Result<Rate, NumError>;
    pub fn recip(self, mode: RoundMode) -> Result<Rate, NumError>;
    pub fn to_bps(self) -> Result<Bps, NumError>;
    pub const fn from_bps(bps: Bps) -> Rate;
pub struct Bps(i64);
impl Bps;
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(BPS_SCALE);
    pub const ONE_PERCENT: Self = Self(100 * BPS_SCALE);
    pub const fn from_raw(raw: i64) -> Self;
    pub const fn raw(self) -> i64;
    pub fn from_whole(bps: i64) -> Result<Self, NumError>;
    pub fn parse_bps(s: &str) -> Result<Self, NumError>;
    pub fn parse_ratio(s: &str) -> Result<Self, NumError>;
    pub const fn to_rate(self) -> Rate;
    pub fn checked_add(self, rhs: Self) -> Result<Self, NumError>;
    pub fn checked_sub(self, rhs: Self) -> Result<Self, NumError>;
    pub fn checked_neg(self) -> Result<Self, NumError>;
    pub fn checked_mul_int(self, n: i64) -> Result<Self, NumError>;
    pub const fn is_zero(self) -> bool;
    pub const fn is_negative(self) -> bool;
    pub fn to_places(self, places: u32) -> String;
impl fmt::Display for Bps;
impl fmt::Debug for Bps;
```

## arb-types
```rust
// ---- book.rs ----
pub enum Side;
impl Side;
    pub const fn opposite(self) -> Self;
    pub const fn tag(self) -> &'static str;
impl fmt::Display for Side;
pub struct Level;
impl Level;
    pub fn new(price: Price, qty: Qty) -> Result<Self, TypeError>;
impl fmt::Display for Level;
pub struct BookSnapshot;
impl BookSnapshot;
    pub fn new(;
    pub fn bids(&self) -> &[Level];
    pub fn asks(&self) -> &[Level];
    pub fn taking_levels(&self, side: Side) -> &[Level];
    pub fn touch(&self, side: Side) -> Option<Price>;
    pub fn best_bid(&self) -> Option<Level>;
    pub fn best_ask(&self) -> Option<Level>;
    pub fn mid(&self) -> Option<Price>;
    pub fn is_empty(&self) -> bool;
    pub fn total_depth(&self, side: Side) -> Result<Qty, arb_num::NumError>;
// ---- funding.rs ----
pub const SECONDS_PER_YEAR: i64 = 365 * 24 * 60 * 60;
pub enum PositionSide;
impl PositionSide;
    pub const fn funding_receipt_sign(self) -> i64;
    pub const fn opposite(self) -> Self;
    pub const fn tag(self) -> &'static str;
impl fmt::Display for PositionSide;
pub struct FundingInterval(u32);
impl FundingInterval;
    pub const EIGHT_HOURS: Self = Self(8 * 3600);
    pub const FOUR_HOURS: Self = Self(4 * 3600);
    pub const TWO_HOURS: Self = Self(2 * 3600);
    pub const ONE_HOUR: Self = Self(3600);
    pub const fn from_secs(secs: u32) -> Result<Self, TypeError>;
    pub const fn secs(self) -> u32;
    pub const fn as_millis(self) -> Millis;
    pub const fn periods_per_year_exact(self) -> Option<i64>;
impl fmt::Display for FundingInterval;
pub struct FundingSnapshot;
impl FundingSnapshot;
    pub fn time_to_settlement(&self, now: WallTs) -> Option<Millis>;
    pub fn receipt_rate(&self, side: PositionSide) -> Result<Rate, arb_num::NumError>;
// ---- instrument.rs ----
pub enum MarketKind;
impl MarketKind;
    pub const fn tag(self) -> &'static str;
    pub const fn is_perp(self) -> bool;
impl fmt::Display for MarketKind;
pub enum ContractSize;
pub struct ContractSpec;
impl ContractSpec;
    pub fn notional(;
    pub fn base_qty(&self, contracts: Qty, price: Price, mode: RoundMode) -> Result<Qty, NumError>;
    pub fn spot(settle: Asset, tick_size: Price, lot_size: Qty) -> Self;
pub struct InstrumentKey;
impl InstrumentKey;
    pub const fn new(base: Asset, quote: Asset, kind: MarketKind, settle: Asset) -> Self;
    pub fn parse(;
    pub fn same_pair(&self, other: &InstrumentKey) -> bool;
impl fmt::Display for InstrumentKey;
pub struct VenueKey;
impl VenueKey;
    pub const fn new(venue: Venue, key: InstrumentKey) -> Self;
impl fmt::Display for VenueKey;
pub struct Instrument;
impl Instrument;
    pub const fn new(vkey: VenueKey, native: Symbol, spec: ContractSpec) -> Self;
    pub const fn venue(&self) -> Venue;
    pub const fn key(&self) -> InstrumentKey;
impl fmt::Display for Instrument;
// ---- lib.rs ----
pub enum TypeError;
impl fmt::Display for TypeError;
impl std::error::Error for TypeError {};
// ---- quality.rs ----
pub enum Provenance;
impl Provenance;
    pub fn worst(self, other: Self) -> Self;
    pub const fn tag(self) -> &'static str;
impl fmt::Display for Provenance;
pub struct QualityFlags(u32);
impl QualityFlags;
    pub const NONE: Self = Self(0);
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
    pub const BLOCKING: Self = Self(;
    pub const NAMED: [(Self, &'static str); 13] = [;
    pub const fn bits(self) -> u32;
    pub const fn is_empty(self) -> bool;
    pub const fn contains(self, other: Self) -> bool;
    pub const fn intersects(self, other: Self) -> bool;
    pub const fn union(self, other: Self) -> Self;
    pub fn insert(&mut self, other: Self);
    pub fn remove(&mut self, other: Self);
    pub const fn is_blocking(self) -> bool;
impl fmt::Display for QualityFlags;
impl fmt::Debug for QualityFlags;
pub struct DataQuality;
impl DataQuality;
    pub const MEASURED: Self = Self;
    pub const SYNTHESIZED: Self = Self;
    pub const fn new(flags: QualityFlags, provenance: Provenance) -> Self;
    pub fn combine(self, other: Self) -> Self;
    pub fn with_flags(mut self, flags: QualityFlags) -> Self;
    pub fn is_trustworthy(self) -> bool;
    pub fn is_rankable(self) -> bool;
    pub fn is_tradeable(self) -> bool;
impl Default for DataQuality;
impl fmt::Display for DataQuality;
// ---- strings.rs ----
// ---- time.rs ----
pub struct Millis(i64);
impl Millis;
    pub const ZERO: Self = Self(0);
    pub const fn from_millis(ms: i64) -> Self;
    pub const fn from_secs(secs: i64) -> Self;
    pub const fn millis(self) -> i64;
    pub const fn abs(self) -> Self;
    pub const fn is_negative(self) -> bool;
impl fmt::Display for Millis;
pub struct WallTs(i64);
impl WallTs;
    pub const EPOCH: Self = Self(0);
    pub const fn from_millis(ms: i64) -> Self;
    pub const fn millis(self) -> i64;
    pub const fn since(self, earlier: WallTs) -> Millis;
    pub const fn add_millis(self, d: Millis) -> Self;
    pub const fn apparent_skew_from(self, local: LocalTs) -> Millis;
pub struct LocalTs(i64);
impl LocalTs;
    pub const EPOCH: Self = Self(0);
    pub const fn from_millis(ms: i64) -> Self;
    pub const fn millis(self) -> i64;
    pub const fn since(self, earlier: LocalTs) -> Millis;
    pub const fn add_millis(self, d: Millis) -> Self;
    pub const fn assume_no_skew(self) -> WallTs;
// ---- venue.rs ----
pub enum Venue;
impl Venue;
    pub const ALL: [Venue; 8] = [;
    pub const fn as_str(self) -> &'static str;
    pub fn parse(s: &str) -> Result<Venue, TypeError>;
impl fmt::Display for Venue;
```


## Macro-generated methods on Price, Qty, Money, Rate

These four types are produced by one macro, so the extraction above misses their
shared surface. Every one of them has, identically:

```rust
const ZERO: Self;  const ONE: Self;  const MIN: Self;  const MAX: Self;
const EPSILON: Self;                    // 1e-12, one unit in the last place
const fn from_raw(raw: i128) -> Self;   // caller asserts the 1e12 scaling
const fn raw(self) -> i128;
fn from_int(n: i64) -> Result<Self, NumError>;
fn parse(s: &str) -> Result<Self, NumError>;   // refuses to truncate: PrecisionLoss
fn checked_add(self, rhs: Self) -> Result<Self, NumError>;
fn checked_sub(self, rhs: Self) -> Result<Self, NumError>;
fn checked_neg(self) -> Result<Self, NumError>;
fn checked_abs(self) -> Result<Self, NumError>;
fn checked_mul_int(self, n: i64) -> Result<Self, NumError>;   // exact, no rounding
fn checked_div_int(self, n: i64, mode: RoundMode) -> Result<Self, NumError>;
const fn signum(self) -> i32;
const fn is_zero(self) -> bool;
const fn is_positive(self) -> bool;
const fn is_negative(self) -> bool;
fn to_places(self, places: u32) -> String;    // fixed-width table output
```

Plus `Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Display, Debug`.
`Debug` prints the decimal value, not the raw integer.

## There are deliberately no operator impls

No `Add`, `Sub`, `Mul`, `Div` on any money type. Every lossy operation demands an
explicit `RoundMode` — see `decisions/005`. Use `RoundMode::COST` for anything we
pay and `RoundMode::REVENUE` for anything we receive, so the reported edge is a
lower bound.

## Non-negotiable conventions

| Question | Answer | Authority |
|---|---|---|
| Scale of money types | 1e12 over i128 | `decisions/001` |
| 1 bp as a raw integer | `100_000_000`, same in `Bps` and `Rate` | `decisions/002` |
| Seconds per year | `31_536_000` (365d exactly) | `decisions/004` |
| Annualisation | `mul_div(rate, SECONDS_PER_YEAR, interval_secs)` — never a rounded period count | `decisions/004` |
| Funding sign | rate > 0 ⇒ longs pay shorts; use `PositionSide::funding_receipt_sign()` | `decisions/006` |
| Which levels a buy consumes | asks — use `book.taking_levels(Side::Buy)` | `arb-types::book` |
| Inverse contract notional | `contracts * quote_per_contract`, price does NOT appear | `decisions/003` |
| Staleness measured against | `LocalTs`, our clock, never the venue's | `decisions/007` |
| Quality of fixture-derived data | `Provenance::SynthesizedUnverified`, propagated by `DataQuality::combine` | rule 7 |
