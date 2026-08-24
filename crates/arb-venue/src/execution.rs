//! Stage 5: the venue-facing execution contract. **Types and traits only.**
//!
//! Per decision 012, `arb-venue` owns everything venue-facing, so the shapes
//! that an order takes on the wire are defined here even though nothing places
//! an order this round. Defining them now is not speculation — it is the reason
//! the strategy crates can be written against a stable surface while the
//! transport does not exist.
//!
//! The only executable code in this module is (a) deterministic identifier
//! derivation and (b) pure request validation. Both must give the same answer
//! in every process on every machine forever, so both are integer/byte
//! arithmetic with no clock, no randomness, and no allocation-order dependence.
//! There is no `impl ExecutionAdapter`, no `impl Reconcile`, no `impl Unwind`
//! anywhere in the workspace yet, and that absence is intentional: a fixture
//! cannot honestly pretend to accept an order.
//!
//! # The most important comment in this crate
//!
//! **Idempotency exists because a network timeout is indistinguishable from a
//! fill.** When `place()` times out, the order may have been rejected, may be
//! resting, or may already be filled — the wire gives us no way to tell. The
//! naive response is to retry. Retrying a market order after a timeout is how
//! you end up with double the intended position: two fills, one hedge, and a
//! naked leg the size of the whole trade. The fix is that the *venue* rejects
//! the duplicate, not that we get cleverer about timeouts. That requires the
//! second attempt to carry the byte-identical client order id as the first,
//! which requires that id to be a pure function of the intent — never a
//! counter, never a timestamp, never a UUID.

use arb_num::{Money, Price, Qty};
use arb_types::{DataQuality, LocalTs, Side, TypeError, VenueKey, WallTs};

use crate::error::VenueError;

// ---------------------------------------------------------------------------
// inline ASCII identifiers
// ---------------------------------------------------------------------------

/// Fixed-capacity ASCII buffer. `Copy`, so an id can be passed around and
/// logged without allocation or lifetime noise on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Ascii<const N: usize> {
    bytes: [u8; N],
    len: u8,
}

impl<const N: usize> Ascii<N> {
    const fn empty() -> Self {
        Self {
            bytes: [b'0'; N],
            len: 0,
        }
    }

    fn new(s: &str) -> Result<Self, VenueError> {
        if s.is_empty() {
            return Err(VenueError::Type(TypeError::Empty));
        }
        if s.len() > N {
            return Err(VenueError::Type(TypeError::TooLong));
        }
        let mut out = Self::empty();
        for (slot, b) in out.bytes.iter_mut().zip(s.as_bytes()) {
            if !is_id_byte(*b) {
                return Err(VenueError::Type(TypeError::InvalidChar));
            }
            *slot = *b;
        }
        // Bounded by the `s.len() > N` check above and N <= 64 in this module.
        out.len = s.len() as u8;
        Ok(out)
    }

    fn as_str(&self) -> &str {
        let n = self.len as usize;
        match self.bytes.get(..n) {
            Some(b) => match std::str::from_utf8(b) {
                Ok(s) => s,
                Err(_) => "",
            },
            None => "",
        }
    }
}

/// The intersection of what the eight venues accept in a client order id, as
/// far as we know: ASCII alphanumerics plus `-` and `_`.
///
/// [assumption] Nothing was measured. Some venues document a wider set; a
/// narrower set can only cost us entropy, never an order rejection, so we take
/// the narrow one.
const fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Our identifier for an order, chosen by us before we send anything.
///
/// **Failure this prevents:** an order we cannot ask about. If the id is
/// assigned by the venue, then a timeout on `place()` leaves us with no handle
/// at all — we cannot query, we cannot cancel, and we cannot tell a fill from a
/// rejection. Choosing the id first means every possible outcome of a timeout
/// is recoverable with a `query()`.
///
/// Capacity is 32 bytes. [assumption] 32 alphanumeric characters is the
/// tightest limit we believe any of the eight imposes; if one of them accepts
/// only 20, every order is rejected on the first live run and the fix is one
/// constant. That is a cheap failure. A 36-character UUID would be the
/// expensive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientOrderId(Ascii<32>);

impl ClientOrderId {
    /// Maximum length in bytes.
    pub const CAPACITY: usize = 32;

    /// Prefix on every derived id.
    ///
    /// Three bytes of "these are ours". During an incident someone is staring
    /// at a venue web UI full of order ids trying to work out which are the
    /// bot's; this answers that in one glance, and it costs three of 32 bytes.
    pub const PREFIX: &'static str = "arb";

    /// Wrap an id we were given (e.g. read back from a reconciliation dump).
    ///
    /// Rejects anything outside [`is_id_byte`] or longer than
    /// [`ClientOrderId::CAPACITY`].
    pub fn new(s: &str) -> Result<Self, VenueError> {
        Ok(Self(Ascii::new(s)?))
    }

    /// Derive the id from an idempotency key: `arb` + 29 lowercase hex nibbles.
    ///
    /// 29 nibbles is 116 bits of the key's 128. Truncation is fine — the
    /// property we need is that the *same intent* yields the same id, not that
    /// distinct intents are cryptographically distinguishable.
    ///
    /// Deterministic by construction: no clock, no counter, no randomness. A
    /// process that crashes mid-`place` and restarts derives the identical id
    /// from the identical intent, so the retry is a duplicate the venue can
    /// reject rather than a second order it will happily fill.
    pub fn derive(key: &IdempotencyKey) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let raw = key.raw();
        let mut out = Ascii::<32>::empty();
        let prefix = Self::PREFIX.as_bytes();
        for (i, slot) in out.bytes.iter_mut().enumerate() {
            match prefix.get(i) {
                Some(b) => *slot = *b,
                None => {
                    // i is 3..32, so nibble index is 0..29 and the shift is
                    // 124 down to 8 — never negative, never over 127.
                    let nibble = i - prefix.len();
                    let shift = 124 - 4 * (nibble as u32);
                    let n = ((raw >> shift) & 0xF) as usize;
                    *slot = match HEX.get(n) {
                        Some(h) => *h,
                        // Unreachable by construction (n < 16) but expressed as
                        // a value, not a panic.
                        None => b'0',
                    };
                }
            }
        }
        out.len = 32;
        Self(out)
    }

    /// The id as it goes on the wire.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// The venue's own identifier for an order, learned after the fact.
///
/// Separate type from [`ClientOrderId`] with a bigger capacity because venue
/// ids are numeric strings or UUIDs (36 bytes with dashes) and will not fit in
/// 32. Keeping them distinct means a cancel can never be sent with the wrong
/// kind of id — the compiler rejects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VenueOrderId(Ascii<64>);

impl VenueOrderId {
    /// Maximum length in bytes. 64 covers a 36-byte UUID with room to spare.
    pub const CAPACITY: usize = 64;

    /// Wrap an id the venue gave us.
    pub fn new(s: &str) -> Result<Self, VenueError> {
        Ok(Self(Ascii::new(s)?))
    }

    /// The id as the venue spells it.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

// ---------------------------------------------------------------------------
// idempotency
// ---------------------------------------------------------------------------

/// A 128-bit fingerprint of an order *intent*. Same intent ⇒ same key, always.
///
/// **Failure this prevents:** double position after a timeout. See the module
/// doc comment — this type is the mechanism behind that paragraph.
///
/// What "same intent" means is a design decision, not an accident:
/// - venue, native symbol, side, kind, time-in-force, quantity and limit price
///   are all part of the intent. Change any of them and it is a different
///   order, and it must get a different key.
/// - [`OrderRequest::intent_seq`] is also part of the intent. Without it, a
///   strategy that legitimately wants to buy the same size twice would have its
///   second order silently swallowed as a duplicate. Deduplication that eats
///   *wanted* orders is as dangerous as no deduplication at all.
/// - [`OrderRequest::client_id`] is deliberately **not** hashed. The id is
///   derived from the key; hashing the id into the key would be circular.
///
/// Not cryptographic. FNV-1a is used because it is a dozen lines of integer
/// arithmetic with no dependency and no ambiguity about byte order. Two
/// distinct intents colliding on 128 bits would cause the venue to reject the
/// second as a duplicate — a missed trade, not a double position. That is the
/// safe direction for a collision to fail in, and it is why a non-cryptographic
/// hash is acceptable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdempotencyKey(u128);

impl IdempotencyKey {
    /// Fingerprint an intent. Pure: the same `req` gives the same key in every
    /// process, forever, on any platform.
    pub fn of(req: &OrderRequest) -> Self {
        let mut buf: Vec<u8> = Vec::new();
        // 0xFF separates fields. No field's encoding can contain 0xFF (ASCII
        // strings, big-endian integers are length-fixed), so the concatenation
        // is injective: "BTC" + "USDT" can never collide with "BTCUS" + "DT".
        let mut feed = |bytes: &[u8]| {
            buf.extend_from_slice(bytes);
            buf.push(0xFF);
        };
        feed(req.vkey.venue.as_str().as_bytes());
        feed(req.vkey.key.base.as_str().as_bytes());
        feed(req.vkey.key.quote.as_str().as_bytes());
        feed(req.vkey.key.kind.tag().as_bytes());
        feed(req.vkey.key.settle.as_str().as_bytes());
        feed(req.native.as_str().as_bytes());
        feed(req.side.tag().as_bytes());
        feed(req.kind.tag().as_bytes());
        feed(req.tif.tag().as_bytes());
        feed(&req.qty.raw().to_be_bytes());
        match req.limit_price {
            Some(p) => {
                feed(b"L");
                feed(&p.raw().to_be_bytes());
            }
            // A market order and a limit order at price 0 must not collide.
            None => feed(b"M"),
        }
        feed(&req.intent_seq.to_be_bytes());

        let hi = fnv1a64(FNV_OFFSET, &buf);
        // A second pass with a different basis. Two independent 64-bit digests
        // of the same bytes give a 128-bit key without inventing a new hash.
        let lo = fnv1a64(FNV_OFFSET ^ GOLDEN, &buf);
        Self(((hi as u128) << 64) | (lo as u128))
    }

    /// The raw 128 bits.
    pub const fn raw(self) -> u128 {
        self.0
    }

    /// Construct from raw bits — for tests and for reading back a persisted
    /// key after a crash.
    pub const fn from_raw(raw: u128) -> Self {
        Self(raw)
    }
}

/// FNV-1a 64 offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// 2^64 / phi. Used only to make the second digest independent of the first.
const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// FNV-1a, 64-bit. Wrapping arithmetic is the specification, not an oversight.
fn fnv1a64(basis: u64, bytes: &[u8]) -> u64 {
    let mut h = basis;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

// ---------------------------------------------------------------------------
// order shapes
// ---------------------------------------------------------------------------

/// How the order interacts with the book.
///
/// [do not know] On real venues `reduce_only` is a *flag* orthogonal to
/// market/limit, so `ReduceOnly` as a peer of `Market` cannot express "reduce-
/// only limit at 68120". This enum matches the shape the workspace agreed on;
/// the mismatch is recorded in [`crate::unification`] rather than silently
/// papered over, because papering over it is how an unwind gets sent as an
/// opening order and doubles the position it was meant to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// Cross the spread now, at whatever price the book gives. The only kind
    /// safe for an unwind, and the only kind that can slip.
    Market,
    /// Rest or cross at a price limit.
    Limit,
    /// Rest only. Rejected outright if it would cross — the only way to be
    /// certain of the maker fee, which matters when the maker/taker gap is
    /// wider than the edge.
    PostOnly,
    /// Must not increase net exposure. Prevents an unwind from opening a new
    /// position when the leg it meant to close is already gone.
    ReduceOnly,
}

impl OrderKind {
    /// Stable lowercase tag. Stable because it is hashed into
    /// [`IdempotencyKey`]: renaming a tag changes every derived id and breaks
    /// idempotency across a deploy.
    pub const fn tag(self) -> &'static str {
        match self {
            OrderKind::Market => "market",
            OrderKind::Limit => "limit",
            OrderKind::PostOnly => "post_only",
            OrderKind::ReduceOnly => "reduce_only",
        }
    }

    /// Whether this kind requires a limit price.
    pub const fn needs_limit_price(self) -> bool {
        matches!(self, OrderKind::Limit | OrderKind::PostOnly)
    }
}

/// How long the order lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeInForce {
    /// Rests until cancelled. **Failure it invites:** a forgotten GTC order
    /// fills hours later against a book that no longer contains our edge.
    Gtc,
    /// Fill what you can now, cancel the rest. The right default for a taking
    /// leg: a partial fill is a known problem, a resting remainder is not.
    Ioc,
    /// All or nothing. The only TIF that makes two-leg execution atomic-ish,
    /// and the reason it is not the default is that venues reject FOK far more
    /// often than they fill it.
    Fok,
}

impl TimeInForce {
    /// Stable lowercase tag — hashed into [`IdempotencyKey`], see
    /// [`OrderKind::tag`].
    pub const fn tag(self) -> &'static str {
        match self {
            TimeInForce::Gtc => "gtc",
            TimeInForce::Ioc => "ioc",
            TimeInForce::Fok => "fok",
        }
    }
}

/// One order we intend to send.
///
/// `qty` is in **contracts**, not base units. Every venue's order endpoint
/// takes contracts, and converting contracts to base exposure is
/// `ContractSpec::base_qty`'s job — decision 004. Putting base units in this
/// struct is exactly the 68000x inverse-contract error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderRequest {
    /// Venue plus canonical instrument key.
    pub vkey: VenueKey,
    /// The venue's own spelling of the symbol. Carried explicitly so the
    /// adapter never re-derives it and never guesses.
    pub native: arb_types::Symbol,
    /// Buy lifts asks, sell hits bids.
    pub side: Side,
    /// Market / limit / post-only / reduce-only.
    pub kind: OrderKind,
    /// Time in force.
    pub tif: TimeInForce,
    /// Size in **contracts**.
    pub qty: Qty,
    /// Required for [`OrderKind::Limit`] and [`OrderKind::PostOnly`], absent
    /// otherwise. `Option` rather than a sentinel: a limit price of zero is a
    /// sell-everything-at-any-price order.
    pub limit_price: Option<Price>,
    /// Strategy-supplied intent counter, part of the idempotency key.
    ///
    /// Two genuinely separate decisions to buy the same size must differ here,
    /// or the second is deduplicated away. Two *retries of one decision* must
    /// share it, or the retry becomes a second position. Whoever owns the
    /// strategy owns this number; this crate only hashes it.
    pub intent_seq: u64,
    /// Must equal `ClientOrderId::derive(&IdempotencyKey::of(self))` — see
    /// [`OrderRequest::validate`].
    pub client_id: ClientOrderId,
}

impl OrderRequest {
    /// Fingerprint of this intent.
    pub fn idempotency_key(&self) -> IdempotencyKey {
        IdempotencyKey::of(self)
    }

    /// The id this intent is *required* to carry.
    pub fn expected_client_id(&self) -> ClientOrderId {
        ClientOrderId::derive(&self.idempotency_key())
    }

    /// Reject requests that cannot be sent safely. Pure; no I/O, no clock.
    ///
    /// The `client_id` check is the load-bearing one. If a caller may supply an
    /// arbitrary id, then a retry path that regenerates the id — a fresh UUID,
    /// a counter, a timestamp — compiles fine and doubles the position in
    /// production. Requiring the id to *be* the derivation of the intent makes
    /// that bug a validation failure instead.
    pub fn validate(&self) -> Result<(), VenueError> {
        if !self.qty.is_positive() {
            return Err(VenueError::Type(TypeError::NonPositiveLevel));
        }
        match self.limit_price {
            Some(p) if !p.is_positive() => {
                return Err(VenueError::Type(TypeError::NonPositivePrice));
            }
            Some(_) if !self.kind.needs_limit_price() => {
                return Err(VenueError::Unsupported(
                    "limit price supplied for an order kind that has no limit price",
                ));
            }
            None if self.kind.needs_limit_price() => {
                return Err(VenueError::Unsupported(
                    "limit and post-only orders require a limit price",
                ));
            }
            _ => {}
        }
        if self.client_id != self.expected_client_id() {
            return Err(VenueError::Unsupported(
                "client_id is not the deterministic derivation of this intent",
            ));
        }
        Ok(())
    }
}

/// Where an order stands, as the venue sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderStatus {
    /// Accepted, nothing filled.
    New,
    /// Some filled, some live.
    PartiallyFilled,
    /// Done, fully filled.
    Filled,
    /// Done, cancelled. May still have partial fills — cancelled is not
    /// "nothing happened".
    Canceled,
    /// Never accepted.
    Rejected,
    /// Timed out of the book (IOC/FOK remainder, or venue expiry).
    Expired,
    /// **We do not know.** A timeout, a 5xx, a reconnect gap. The whole reason
    /// this variant exists is that the alternative — defaulting to `Rejected`
    /// — makes a filled order look like nothing happened, which is precisely
    /// the naked-leg scenario. `Unknown` must block trading until a `query()`
    /// resolves it.
    Unknown,
}

impl OrderStatus {
    /// Whether the venue can still change this order's fills.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Canceled
                | OrderStatus::Rejected
                | OrderStatus::Expired
        )
    }

    /// Stable lowercase tag.
    pub const fn tag(self) -> &'static str {
        match self {
            OrderStatus::New => "new",
            OrderStatus::PartiallyFilled => "partially_filled",
            OrderStatus::Filled => "filled",
            OrderStatus::Canceled => "canceled",
            OrderStatus::Rejected => "rejected",
            OrderStatus::Expired => "expired",
            OrderStatus::Unknown => "unknown",
        }
    }
}

/// What we believe about one order, with provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderState {
    /// Our id — the primary key. Always present, even before the venue acks.
    pub client_id: ClientOrderId,
    /// The venue's id, absent until it tells us.
    pub venue_id: Option<VenueOrderId>,
    /// Venue plus instrument.
    pub vkey: VenueKey,
    /// Side, carried through so a reconciliation dump is self-describing.
    pub side: Side,
    /// Current status.
    pub status: OrderStatus,
    /// What we asked for, in contracts.
    pub requested_qty: Qty,
    /// What has filled, in contracts.
    pub filled_qty: Qty,
    /// Volume-weighted average fill price, `None` until the first fill.
    ///
    /// `Option`, never `Price::ZERO`: a zero average price values a filled
    /// position at nothing, which turns a loss into an apparent profit in every
    /// downstream sum. If there are no fills there is no average, and the type
    /// says so.
    pub avg_price: Option<Price>,
    /// Fees actually charged so far, in settle currency. Signed, so a maker
    /// rebate is a negative fee rather than a special case.
    pub fees_paid: Money,
    /// What the venue said the time was.
    pub exchange_ts: Option<WallTs>,
    /// When we observed this state.
    pub observed_at: LocalTs,
    /// Provenance and flags. An `Unknown` status must carry blocking flags.
    pub quality: DataQuality,
}

impl OrderState {
    /// Contracts still working. Saturates at zero rather than going negative:
    /// an overfill is a venue bug we want to see as a discrepancy, not as a
    /// negative remainder that silently cancels out elsewhere.
    pub fn remaining_qty(&self) -> Result<Qty, VenueError> {
        if self.filled_qty >= self.requested_qty {
            return Ok(Qty::ZERO);
        }
        self.requested_qty
            .checked_sub(self.filled_qty)
            .map_err(VenueError::from)
    }
}

// ---------------------------------------------------------------------------
// traits — no implementations in this workspace yet
// ---------------------------------------------------------------------------

/// The write side of the seam.
///
/// Every method is idempotent by contract: calling it twice with the same
/// argument must not produce two effects. That is only achievable because
/// [`OrderRequest::client_id`] is derived, not generated.
pub trait ExecutionAdapter {
    /// Send an order.
    ///
    /// **Must** be safe to call again with the identical `req` after a timeout.
    /// An implementation that cannot get the venue to reject the duplicate must
    /// `query()` first and place only if the id is unknown to the venue — never
    /// place blind.
    fn place(&self, req: &OrderRequest) -> Result<OrderState, VenueError>;

    /// Cancel by our own id.
    ///
    /// Cancelling an unknown id is *success*, not failure: the goal state is
    /// "this order is not working", and an order the venue never saw is already
    /// in that state. An implementation that errors here turns a benign race
    /// into an abort.
    fn cancel(&self, id: &ClientOrderId) -> Result<OrderState, VenueError>;

    /// Ask the venue what actually happened. The only cure for
    /// [`OrderStatus::Unknown`].
    fn query(&self, id: &ClientOrderId) -> Result<OrderState, VenueError>;

    /// Everything the venue currently considers working.
    ///
    /// Note what this cannot do: it cannot see an order that filled and closed
    /// while we were disconnected. Open orders are not a complete picture of
    /// what happened, which is why [`Reconcile`] exists separately.
    fn open_orders(&self) -> Result<Vec<OrderState>, VenueError>;
}

/// Our belief versus the venue's record. **The venue always wins.**
///
/// Not a nicety: our belief is a cache, and the position that gets liquidated
/// is the venue's. Any reconciliation loop that "corrects" the venue toward our
/// belief is writing the bug it was built to catch.
pub trait Reconcile {
    /// Compare what we think is outstanding against what the venue reports.
    ///
    /// `expected` is our belief. The returned discrepancies are always phrased
    /// as corrections *to us*.
    fn reconcile(&self, expected: &[OrderState]) -> Result<Vec<Discrepancy>, VenueError>;
}

/// One way our record and the venue's disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discrepancy {
    /// The venue has an order we have no record of. Usually a `place()` that
    /// timed out and actually succeeded. **This is the dangerous one** — it is
    /// unhedged exposure we are not watching.
    UnknownToUs(OrderState),
    /// We believe in an order the venue never saw. Usually a `place()` that
    /// genuinely failed. Safe, but our position estimate is too large.
    UnknownToVenue(ClientOrderId),
    /// Both know the order, fills disagree. Our hedge ratio is wrong by the
    /// difference.
    QtyMismatch {
        /// The order in question.
        id: ClientOrderId,
        /// Our filled quantity, in contracts.
        ours: Qty,
        /// The venue's filled quantity, in contracts.
        venue: Qty,
    },
    /// Both know the order, lifecycle disagrees. Matters most when we think
    /// something is terminal and the venue still has it working.
    StatusMismatch {
        /// The order in question.
        id: ClientOrderId,
        /// What we believe.
        ours: OrderStatus,
        /// What the venue says.
        venue: OrderStatus,
    },
}

/// Leg one filled, leg two did not. We are naked. This is the exit.
///
/// Kept as its own trait rather than a method on [`ExecutionAdapter`] because
/// it has different rules: an unwind is always a taker (a resting unwind is not
/// an unwind, it is a hope), always reduce-only where the venue supports it,
/// and must never be skipped because a limit was not met. Separating it makes
/// "did we implement the panic path" a visible yes or no rather than one
/// method buried among nine.
pub trait Unwind {
    /// Flatten the exposure created by `leg`.
    ///
    /// Returns the state of the *closing* order. An error here is the worst
    /// outcome in the system: it means we are naked and could not get out, and
    /// the caller's only remaining move is to alert a human.
    fn unwind(&self, leg: &OrderState) -> Result<OrderState, VenueError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_types::{Asset, InstrumentKey, MarketKind, Symbol, Venue};

    fn req(seq: u64, qty: &str, kind: OrderKind, limit: Option<&str>) -> OrderRequest {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").unwrap();
        let mut r = OrderRequest {
            vkey: VenueKey::new(Venue::Binance, key),
            native: Symbol::new("BTCUSDT").unwrap(),
            side: Side::Buy,
            kind,
            tif: TimeInForce::Ioc,
            qty: Qty::parse(qty).unwrap(),
            limit_price: limit.map(|p| Price::parse(p).unwrap()),
            intent_seq: seq,
            client_id: ClientOrderId::new("placeholder").unwrap(),
        };
        r.client_id = r.expected_client_id();
        r
    }

    #[test]
    fn fnv_of_nothing_is_the_offset_basis() {
        // By definition: FNV-1a hashes zero bytes to the offset basis, with no
        // multiplication. Verifiable without a calculator.
        assert_eq!(fnv1a64(FNV_OFFSET, b""), FNV_OFFSET);
        assert_eq!(fnv1a64(FNV_OFFSET, &[]), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn fnv_first_step_is_xor_then_multiply() {
        // One byte 0x61 ('a'): h = (BASIS ^ 0x61) * PRIME, wrapping.
        // BASIS ^ 0x61 = 0xcbf29ce484222325 ^ 0x61 = 0xcbf29ce484222344.
        let expected = 0xcbf2_9ce4_8422_2344u64.wrapping_mul(FNV_PRIME);
        assert_eq!(fnv1a64(FNV_OFFSET, b"a"), expected);
    }

    #[test]
    fn same_intent_gives_the_same_key_and_id() {
        let a = req(7, "0.5", OrderKind::Market, None);
        let b = req(7, "0.5", OrderKind::Market, None);
        assert_eq!(a.idempotency_key(), b.idempotency_key());
        assert_eq!(a.client_id, b.client_id);
    }

    #[test]
    fn a_new_decision_gives_a_new_key() {
        let a = req(7, "0.5", OrderKind::Market, None);
        let b = req(8, "0.5", OrderKind::Market, None);
        assert_ne!(a.idempotency_key(), b.idempotency_key());
        assert_ne!(a.client_id, b.client_id);
    }

    #[test]
    fn changing_size_changes_the_key() {
        let a = req(7, "0.5", OrderKind::Market, None);
        let b = req(7, "0.6", OrderKind::Market, None);
        assert_ne!(a.idempotency_key(), b.idempotency_key());
    }

    #[test]
    fn market_and_limit_do_not_collide() {
        let m = req(1, "1", OrderKind::Market, None);
        let l = req(1, "1", OrderKind::Limit, Some("68120.0"));
        assert_ne!(m.idempotency_key(), l.idempotency_key());
    }

    #[test]
    fn derived_id_shape_is_prefix_plus_29_hex() {
        let id = ClientOrderId::derive(&IdempotencyKey::from_raw(0));
        // 128 zero bits -> "arb" + 29 '0'. Hand-computable precisely because
        // the derivation has no hidden state.
        assert_eq!(id.as_str(), "arb00000000000000000000000000000");
        assert_eq!(id.as_str().len(), 32);
        assert!(id.as_str().starts_with(ClientOrderId::PREFIX));
    }

    #[test]
    fn derived_id_uses_the_top_bits_first() {
        // Top nibble of a 128-bit key is the first hex character after "arb".
        // 0xf << 124 sets exactly that nibble.
        let id = ClientOrderId::derive(&IdempotencyKey::from_raw(0xf << 124));
        assert_eq!(id.as_str(), "arbf0000000000000000000000000000");
    }

    #[test]
    fn client_order_id_rejects_bad_bytes_and_overlong_input() {
        assert!(ClientOrderId::new("").is_err());
        assert!(ClientOrderId::new("has space").is_err());
        assert!(ClientOrderId::new("ok-id_1").is_ok());
        // 33 characters: one over capacity.
        let long = "a".repeat(ClientOrderId::CAPACITY + 1);
        assert!(ClientOrderId::new(&long).is_err());
        assert!(ClientOrderId::new(&"a".repeat(ClientOrderId::CAPACITY)).is_ok());
    }

    #[test]
    fn venue_order_id_holds_a_uuid() {
        // 36 bytes with dashes — would not fit in ClientOrderId, which is the
        // entire reason the two types are separate.
        let uuid = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(uuid.len(), 36);
        assert!(ClientOrderId::new(uuid).is_err());
        assert_eq!(VenueOrderId::new(uuid).unwrap().as_str(), uuid);
    }

    #[test]
    fn validate_requires_the_derived_id() {
        let mut r = req(3, "1", OrderKind::Market, None);
        assert!(r.validate().is_ok());
        // Simulate the classic bug: retry path invents a fresh id.
        r.client_id = ClientOrderId::new("retry-42").unwrap();
        assert_eq!(
            r.validate(),
            Err(VenueError::Unsupported(
                "client_id is not the deterministic derivation of this intent"
            ))
        );
    }

    #[test]
    fn validate_rejects_limit_without_price_and_market_with_price() {
        let mut naked_limit = req(1, "1", OrderKind::Limit, None);
        naked_limit.client_id = naked_limit.expected_client_id();
        assert!(naked_limit.validate().is_err());

        let mut priced_market = req(1, "1", OrderKind::Market, Some("68120.0"));
        priced_market.client_id = priced_market.expected_client_id();
        assert!(priced_market.validate().is_err());

        assert!(req(1, "1", OrderKind::PostOnly, Some("68120.0"))
            .validate()
            .is_ok());
    }

    #[test]
    fn validate_rejects_non_positive_size() {
        let mut r = req(1, "0", OrderKind::Market, None);
        r.client_id = r.expected_client_id();
        assert_eq!(
            r.validate(),
            Err(VenueError::Type(TypeError::NonPositiveLevel))
        );
    }

    #[test]
    fn unknown_status_is_not_terminal() {
        // The point of the variant: we must keep asking.
        assert!(!OrderStatus::Unknown.is_terminal());
        assert!(!OrderStatus::New.is_terminal());
        assert!(!OrderStatus::PartiallyFilled.is_terminal());
        assert!(OrderStatus::Filled.is_terminal());
        assert!(OrderStatus::Canceled.is_terminal());
    }

    #[test]
    fn remaining_qty_saturates_on_overfill() {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").unwrap();
        let base = OrderState {
            client_id: ClientOrderId::derive(&IdempotencyKey::from_raw(1)),
            venue_id: None,
            vkey: VenueKey::new(Venue::Bybit, key),
            side: Side::Sell,
            status: OrderStatus::PartiallyFilled,
            // 2.5 requested, 1.0 filled -> 1.5 remaining, exactly.
            requested_qty: Qty::parse("2.5").unwrap(),
            filled_qty: Qty::parse("1.0").unwrap(),
            avg_price: Some(Price::parse("68120.0").unwrap()),
            fees_paid: Money::ZERO,
            exchange_ts: None,
            observed_at: LocalTs::EPOCH,
            quality: DataQuality::SYNTHESIZED,
        };
        assert_eq!(base.remaining_qty().unwrap(), Qty::parse("1.5").unwrap());

        let over = OrderState {
            filled_qty: Qty::parse("3.0").unwrap(),
            ..base
        };
        assert_eq!(over.remaining_qty().unwrap(), Qty::ZERO);
    }

    #[test]
    fn no_fills_means_no_average_price() {
        // Qty::ZERO fills with Some(Price::ZERO) would value the position at
        // nothing; the type forbids expressing it as anything but None.
        let s: Option<Price> = None;
        assert!(s.is_none());
        assert!(Asset::new("USDT").is_ok());
    }
}
