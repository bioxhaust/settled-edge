//! Quote-and-cover: post passive on the wide book, cover aggressive on the tight
//! one.
//!
//! # The trade
//!
//! Two venues quote the same pair with different spreads. We post a resting
//! order inside the wide venue's spread and, the moment it fills, we take the
//! opposite side on the tight venue. The gross edge is the difference between
//! our passive fill price and the price we pay to cover, less one maker fee and
//! one taker fee. On venues with a maker rebate the fee term can be negative,
//! which is a large part of why the trade is viable at all.
//!
//! # Why the capacity is small, and why that is structural
//!
//! The edge is queue position. A resting order earns only if it is in front of
//! the flow that arrives, and the front of the queue is a fixed-size resource
//! at each price level: one order can be first. Capital does not buy it.
//! Doubling the size on the order does not double the fills — it lengthens the
//! part of the order sitting behind the flow, and it raises the adverse-
//! selection cost, because the fills you do get are increasingly the ones that
//! arrive when the price is about to move through you. So the return is roughly
//! bounded by arriving flow times spread, not by our balance sheet.
//!
//! That is exactly why this strategy lives in `arb-private`. A funding-basis
//! signal can be handed to a hundred subscribers and still work for all of
//! them. A queue position cannot be handed to anybody, because the second
//! participant standing in the same queue is in front of us or behind us, and
//! either way one of us is not getting paid.
//!
//! # Status
//!
//! Not implemented. See [`BLOCKED_ON`].

use arb_num::{Bps, Price, Qty};
use arb_types::{BookSnapshot, ContractSpec, DataQuality, Side, VenueKey};

use crate::error::PrivateError;

/// Stable machine name, used in errors, metrics and [`crate::BLOCKERS`].
pub const STRATEGY: &str = "quote_and_cover";

/// What has to exist before [`plan`] can return a plan.
///
/// Queue position is the whole edge, and nothing we have measures it. An L2
/// book tells us how much size is at a price; it does not tell us how much of
/// that size is ahead of ours, and the aggregate depth at the touch changes for
/// reasons (cancels behind us) that do not affect our position at all. Without
/// per-order sequencing there is no way to estimate fill probability, and
/// without fill probability the expected value of a resting quote is not a
/// number — it is a guess with our money behind it.
pub const BLOCKED_ON: &str = "a per-order (L3) book stream on the quoting venue, plus a measured \
     fill-probability model keyed on queue position and arriving flow; arb-venue exposes only \
     aggregated L2 snapshots and arb-math has no queue model";

/// Inputs to a quote-and-cover plan.
///
/// Books are borrowed rather than owned: a `BookSnapshot` carries level vectors
/// and this struct is constructed on every book update, so copying the depth
/// per evaluation would dominate the work.
pub struct QuoteCoverParams<'a> {
    /// The venue we post passively on. This should be the *wider* book — the
    /// spread we are stepping inside of is the raw material.
    pub quote_book: &'a BookSnapshot,
    /// The venue we cover on by taking. This should be the *tighter* book,
    /// because the cover is a market order and pays that venue's spread.
    pub cover_book: &'a BookSnapshot,
    /// Side of the resting order on `quote_book`. The cover leg is the
    /// opposite side, taken — see [`QuoteCoverParams::cover_side`].
    pub quote_side: Side,
    /// Contract spec of the quoting venue. Needed because the resting order is
    /// priced in ticks and sized in lots, and because contracts convert to
    /// exposure only through `ContractSpec`.
    pub quote_spec: ContractSpec,
    /// Contract spec of the covering venue. Two venues quoting the same pair
    /// routinely disagree on contract size; the legs are equal in exposure,
    /// not in contract count.
    pub cover_spec: ContractSpec,
    /// Size to show, in contracts of the quoting venue.
    pub show_size: Qty,
    /// Maker fee on the quoting venue. Negative means rebate; a rebate is not
    /// a rounding detail here, it is frequently most of the edge.
    pub quote_maker_fee: Bps,
    /// Taker fee on the covering venue.
    pub cover_taker_fee: Bps,
    /// Net edge, after both fees, below which we do not post at all.
    pub min_edge: Bps,
}

impl QuoteCoverParams<'_> {
    /// The side we take on the covering venue: the opposite of the side we
    /// rested on. Sell passive on venue A means buy back on venue B.
    ///
    /// Routed through `Side::opposite` rather than written out, so the sign
    /// convention lives in exactly one place.
    pub fn cover_side(&self) -> Side {
        self.quote_side.opposite()
    }
}

/// A quote-and-cover instruction: one resting order plus the cover it implies.
///
/// The cover price is an *expectation*, not a commitment. By the time we are
/// filled the covering book has moved, and the difference between
/// `expected_cover_price` and the realised cover is the adverse selection this
/// strategy lives or dies by.
#[derive(Debug, Clone, Copy)]
pub struct QuoteCoverPlan {
    /// Venue and instrument the resting order goes to.
    pub quote_vkey: VenueKey,
    /// Venue and instrument the cover is taken on.
    pub cover_vkey: VenueKey,
    /// Side of the resting order.
    pub quote_side: Side,
    /// Limit price of the resting order, already snapped to the quoting
    /// venue's tick.
    pub quote_price: Price,
    /// Size of the resting order, in contracts of the quoting venue, already
    /// snapped to that venue's lot size.
    pub quote_size: Qty,
    /// Size of the cover, in contracts of the *covering* venue — the exposure
    /// match, not the contract count.
    pub cover_size: Qty,
    /// Cover price we expect to get if filled now, from the current covering
    /// book. Priced with `RoundMode::COST` when this is implemented: the cover
    /// is a cost and must not be flattered.
    pub expected_cover_price: Price,
    /// Net edge in bps at `expected_cover_price`, after maker and taker fees.
    pub expected_edge: Bps,
    /// Combined quality of both books. Carries the worse provenance of the two
    /// — a plan built partly on fixture data is a fixture-derived plan.
    pub quality: DataQuality,
}

/// Plan a quote-and-cover pair, or say why not.
///
/// Validates what can be validated from the inputs, then returns
/// [`PrivateError::NotImplemented`] with [`BLOCKED_ON`]. It never returns `Ok`
/// today; the checks run first anyway so that a caller wiring this up gets told
/// about a bad input immediately instead of discovering it on the day the
/// strategy is switched on.
///
/// Note what is deliberately *not* checked: `DataQuality`. Gating on
/// tradeability belongs to whatever executes a plan, and this function produces
/// nothing executable. Quality is propagated, not enforced.
pub fn plan(params: &QuoteCoverParams<'_>) -> Result<QuoteCoverPlan, PrivateError> {
    if !params.show_size.is_positive() {
        return Err(PrivateError::MissingData {
            what: "a positive show size for the resting order",
        });
    }

    // Both legs must be the same pair. The two books arrive from independent
    // feeds and a mis-keyed subscription is a silent way to hedge BTC with ETH.
    // `same_pair` compares base/quote only: quoting spot against a perp is a
    // legitimate configuration of this strategy.
    // `vkey` is a public field on `BookSnapshot`, not an accessor, so this is
    // field access. (The digest documents it as a field; there is no `vkey()`
    // method. Only `bids`/`asks` are private, behind accessors that preserve the
    // sort invariant.)
    if !params
        .quote_book
        .vkey
        .key
        .same_pair(&params.cover_book.vkey.key)
    {
        return Err(PrivateError::MissingData {
            what: "a cover book on the same base/quote pair as the quoting book",
        });
    }

    // Both touches of the quoting book are required, not just ours: pricing a
    // resting order means choosing where to sit between the two, and a one-sided
    // book gives no spread to sit inside. `best_bid`/`best_ask` are used rather
    // than `touch(side)` because `touch` follows the taking convention
    // (`Buy => asks`) and a passive buy joins the *bids* — reading it the other
    // way would price our quote across the spread.
    if params.quote_book.best_bid().is_none() || params.quote_book.best_ask().is_none() {
        return Err(PrivateError::MissingData {
            what: "both sides of the quoting book; a resting quote is priced between them",
        });
    }
    // Here `touch` is the right call: the cover is a take, so the taking
    // convention is what we want.
    if params.cover_book.touch(params.cover_side()).is_none() {
        return Err(PrivateError::MissingData {
            what: "top of book on the covering venue for the side we would take",
        });
    }

    Err(PrivateError::not_implemented(STRATEGY, BLOCKED_ON))
}
