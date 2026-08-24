//! New-listing capture: be there in the first minutes of a new market.
//!
//! # The trade
//!
//! A venue lists an asset that already trades elsewhere. For a window after
//! trading opens, the new venue's book is thin and its price is set by whoever
//! shows up, while the established venue has a real price. We take the
//! dislocated side on the new venue and offset on the established one.
//!
//! # The constraint is reaction latency, not analysis
//!
//! The window is minutes, often seconds. That single fact determines the shape
//! of everything here. There is no time to fetch a fee schedule, resolve a
//! symbol, discover a contract size, warm a connection or ask a human. Anything
//! not already in memory when trading opens is not available during the trade,
//! so the work is front-loaded: staged orders, pre-resolved instruments,
//! pre-authenticated sessions, and a decision rule simple enough to evaluate on
//! the first tick.
//!
//! This is also why the strategy does not scale and is not sold. The
//! opportunity is a fixed quantity of dislocated size that exists once. It goes
//! to whoever is fastest, and a subscriber who is faster than us takes it
//! instead of us — the signal has negative value to its owner.
//!
//! A listing with no reference market elsewhere is not in scope. With no price
//! anywhere, there is nothing to arbitrage against; buying it is a directional
//! bet, and directional bets are a different business.
//!
//! # Status
//!
//! Not implemented. See [`BLOCKED_ON`].

use arb_num::{Money, Qty};
use arb_types::{DataQuality, LocalTs, Millis, Symbol, VenueKey, WallTs};

use crate::error::PrivateError;

/// Stable machine name, used in errors, metrics and [`crate::BLOCKERS`].
pub const STRATEGY: &str = "new_listing_capture";

/// What has to exist before [`plan`] can return a plan.
///
/// Two gaps, and the first one is not code. We do not have a feed of listing
/// announcements: no calendar, no announcement stream, no push. Discovering a
/// listing by noticing a new symbol in an instrument dump is a polling loop,
/// and a polling loop that runs every thirty seconds is useless against a
/// thirty-second window. The second gap is that we have never measured our own
/// order round-trip on any venue, so we cannot say whether we can act inside
/// the window even if we knew about it.
pub const BLOCKED_ON: &str = "a listing announcement feed with trading-start timestamps \
     (arb-venue has no calendar or announcement subscription), and a measured order round-trip \
     latency per venue to size the reaction budget against the window";

/// Whether the newly listed market has a price to arbitrage against.
///
/// This distinction is the whole go/no-go: it decides whether there is a
/// reference price at all, and therefore whether this is arbitrage or a bet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingKind {
    /// First venue anywhere to list the asset. Out of scope: no reference
    /// price exists, so no offsetting leg exists either.
    FirstListingAnywhere,
    /// Already trades elsewhere. In scope: the established venue supplies the
    /// price the new book has not found yet.
    AlreadyTradedElsewhere,
}

impl ListingKind {
    /// True if a reference market exists that we could hedge against.
    pub fn has_reference_market(&self) -> bool {
        matches!(self, ListingKind::AlreadyTradedElsewhere)
    }
}

/// A listing we have been told about.
///
/// Timestamps are split deliberately: `announced_at` and `trading_starts_at`
/// are what the exchange said (`WallTs`), `observed_at` is when we saw it
/// (`LocalTs`). The gap between the last two is our disadvantage, and merging
/// the clocks would hide it.
pub struct ListingEvent {
    /// The new market.
    pub vkey: VenueKey,
    /// The venue's own symbol for it. Needed verbatim: at listing time there is
    /// no time to resolve a symbol, and new listings are exactly where venue
    /// naming conventions break.
    pub native: Symbol,
    /// Whether a reference market exists elsewhere.
    pub kind: ListingKind,
    /// Established market to price against, if any.
    pub reference: Option<VenueKey>,
    /// When the venue announced the listing.
    pub announced_at: WallTs,
    /// When trading opens, per the announcement. Announcements move; a stale
    /// value here means we arm at the wrong moment.
    pub trading_starts_at: WallTs,
    /// When we learned about it.
    pub observed_at: LocalTs,
    /// Provenance and flags. An announcement parsed out of a blog post is not
    /// measured data and must not be dressed up as such.
    pub quality: DataQuality,
}

impl ListingEvent {
    /// Lead time between the announcement and the open, on the exchange's own
    /// clock. `None` if the announcement claims trading started before it was
    /// announced, which means the calendar is wrong and we should not act on
    /// it.
    pub fn lead_time(&self) -> Option<Millis> {
        let d = self.trading_starts_at.since(self.announced_at);
        if d.is_negative() {
            None
        } else {
            Some(d)
        }
    }
}

/// Risk limits and wiring for a listing capture. Supplied by us, not by the
/// venue.
pub struct ListingParams {
    /// Where the offsetting leg goes. Required: without a hedge this is a
    /// directional position in an asset with a two-minute price history.
    pub hedge: Option<VenueKey>,
    /// Hard cap on size in contracts of the new venue.
    pub max_size: Qty,
    /// Hard cap on notional. Both caps exist because a new listing's contract
    /// size is frequently unusual, and a size cap alone can mean far more
    /// exposure than intended.
    pub max_notional: Money,
    /// How long we are allowed to take between the first tick and the order
    /// being on the wire.
    ///
    /// No default is provided on purpose. Any number here is a claim about our
    /// own latency, and we have not measured ours on any venue. A plausible
    /// constant would be indistinguishable from a measured one three weeks from
    /// now, and that is precisely the mistake this codebase refuses to make.
    pub reaction_budget: Millis,
}

/// A staged listing-capture instruction.
///
/// "Staged" is the operative word: by the time the window opens there is no
/// time to build this. It is constructed from the announcement, armed, and then
/// either fires on the first tick or is discarded.
#[derive(Debug, Clone, Copy)]
pub struct ListingPlan {
    /// The new market we act on.
    pub listing_vkey: VenueKey,
    /// Venue and instrument for the offsetting leg.
    pub hedge_vkey: VenueKey,
    /// Size cap for this plan, after applying both caps in
    /// [`ListingParams`].
    pub size_cap: Qty,
    /// When to have orders staged and connections warm — before
    /// `trading_starts_at`, by at least the reaction budget.
    pub arm_at: WallTs,
    /// Budget carried through from the params, so the executor does not have to
    /// re-derive it under time pressure.
    pub reaction_budget: Millis,
    /// Quality of the announcement this plan came from.
    pub quality: DataQuality,
}

/// Plan a listing capture, or say why not.
///
/// Runs the checks that do not need the missing feeds, then returns
/// [`PrivateError::NotImplemented`] with [`BLOCKED_ON`]. It never returns `Ok`
/// today.
pub fn plan(event: &ListingEvent, params: &ListingParams) -> Result<ListingPlan, PrivateError> {
    if !event.kind.has_reference_market() {
        return Err(PrivateError::MissingData {
            what: "a reference market for the listed asset; a first-ever listing has no price to \
                   arbitrage against and is out of scope",
        });
    }

    // `kind` says a reference market exists; `reference` says which one. Both
    // are needed, and a claim without the pointer is not usable.
    let Some(_reference) = event.reference else {
        return Err(PrivateError::MissingData {
            what: "the venue and instrument of the reference market",
        });
    };

    let Some(_hedge) = params.hedge else {
        return Err(PrivateError::MissingData {
            what: "a hedge venue; unhedged listing capture is a directional bet, not arbitrage",
        });
    };

    if event.lead_time().is_none() {
        return Err(PrivateError::MissingData {
            what: "a coherent listing calendar; trading-start is earlier than the announcement",
        });
    }

    if !params.max_size.is_positive() || !params.max_notional.is_positive() {
        return Err(PrivateError::MissingData {
            what: "positive size and notional caps",
        });
    }

    // A non-positive reaction budget means nobody has filled this in. Treated
    // as missing rather than defaulted: guessing our own latency is the error
    // this strategy is most likely to die of.
    if !params.reaction_budget.millis().is_positive() {
        return Err(PrivateError::MissingData {
            what: "a measured reaction-latency budget",
        });
    }

    Err(PrivateError::not_implemented(STRATEGY, BLOCKED_ON))
}
