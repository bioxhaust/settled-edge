#![forbid(unsafe_code)]
//! One row of the opportunity table, and the vocabulary for why a row cannot be
//! ranked.
//!
//! # What a row is
//!
//! A row is a claim about money, and the claim is only true **for one size**.
//! "BTC funding pays 38% annualised between MEXC and OKX" is not a fact about the
//! market; it is a fact about a specific quantity that a specific pair of books
//! can absorb right now. [`Opportunity::size`] is therefore not an afterthought —
//! it is the qualifier without which [`Opportunity::net_apr`] means nothing. Every
//! other scanner in this space prints the rate and omits the size, which is
//! exactly how a 38% headline becomes a 4% fill.
//!
//! # Why costs are itemised and not netted
//!
//! `gross_apr`, `fee_bps`, `slippage_bps` and `net_apr` are all carried
//! separately, even though only `net_apr` is actionable. An operator who cannot
//! see *where* the gross went cannot tell a thin-fee/thin-book row from a
//! fat-fee/deep-book row with the same net, and those two rows behave completely
//! differently when the size moves. The subtraction is shown, not just its result.
//!
//! # Unrankable rows stay on the screen
//!
//! A row whose data is stale, whose depth is painted or whose clocks disagree is
//! **not** dropped. It is carried with [`Opportunity::blocked`] populated and
//! [`Opportunity::is_rankable`] false. Dropping it would leave the operator
//! looking at a shorter table with no way to tell "no opportunity here" from "we
//! refused to price this one", and those two conclusions lead to opposite actions.

use core::fmt;

use arb_detect::{ClockVerdict, Staleness};
use arb_num::{Bps, Money, Price, Qty, Rate};
use arb_types::{DataQuality, PositionSide, VenueKey};

/// Why a row is present but not tradeable on its printed number.
///
/// Ordered roughly by how badly it invalidates the number, but the enum is a set
/// member rather than a severity: a row can be blocked several ways at once and
/// all of them are reported, because fixing only the first would still leave the
/// row unusable and the operator would have to re-run to discover that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unrankable {
    /// One leg's book has not updated recently enough to price a fill against.
    StaleBook(VenueKey, Staleness),
    /// One leg's funding snapshot is too old to annualise from.
    StaleFunding(VenueKey, Staleness),
    /// Exchange and local clocks disagree by more than the limit, so "recent"
    /// cannot be established on that venue at all.
    ClockSkew(VenueKey, ClockVerdict),
    /// The visible depth looks placed rather than real. Sizing against it would
    /// produce a capacity number that evaporates on contact.
    PaintedDepth(VenueKey, u8),
    /// One leg's book crosses itself: best bid at or above best ask. Either the
    /// feed is corrupt or the sides came from different instants.
    CrossedBook(VenueKey),
    /// The size the two books can jointly absorb is below the venue's own
    /// minimum, so this trade cannot legally be placed at any size.
    BelowMinNotional {
        /// The leg that refuses the size.
        vkey: VenueKey,
        /// What the pair can actually absorb.
        available: Money,
        /// What the venue insists on.
        required: Money,
    },
    /// Depth ran out before the requested size, so the printed number describes a
    /// smaller trade than the one that was asked for.
    SizeExceedsDepth {
        /// The binding leg.
        vkey: VenueKey,
        /// What was asked for.
        requested: Qty,
        /// What the book could actually fill.
        available: Qty,
    },
    /// Provenance is not a measurement. Always present in a fixture replay, and
    /// deliberately so: it is the marker that separates "the engine works" from
    /// "these numbers are real".
    Unverified(VenueKey),
}

impl fmt::Display for Unrankable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unrankable::StaleBook(v, s) => write!(f, "{} book {}", v.venue, s),
            Unrankable::StaleFunding(v, s) => write!(f, "{} funding {}", v.venue, s),
            Unrankable::ClockSkew(v, c) => write!(f, "{} {}", v.venue, c),
            Unrankable::PaintedDepth(v, score) => {
                write!(f, "{} depth looks painted (score {score})", v.venue)
            }
            Unrankable::CrossedBook(v) => write!(f, "{} book crossed", v.venue),
            Unrankable::BelowMinNotional {
                vkey,
                available,
                required,
            } => write!(
                f,
                "{} min notional {required} > available {available}",
                vkey.venue
            ),
            Unrankable::SizeExceedsDepth {
                vkey,
                requested,
                available,
            } => write!(
                f,
                "{} depth {available} < requested {requested}",
                vkey.venue
            ),
            Unrankable::Unverified(v) => write!(f, "{} data unverified", v.venue),
        }
    }
}

/// One side of the pair, priced for the size the pair actually supports.
#[derive(Debug, Clone)]
pub struct OppLeg {
    /// Venue and instrument.
    pub vkey: VenueKey,
    /// Which way we sit. Set by the funding comparison, not by the caller.
    pub side: PositionSide,
    /// The venue's published per-interval funding rate, carried unchanged so the
    /// operator can reconcile against the exchange's own screen.
    pub published_rate: Rate,
    /// Signed annualised funding **receipt** for [`OppLeg::side`]. Negative means
    /// this leg pays. This is the number the sign law has already been applied to;
    /// the published rate above has not.
    pub annual_receipt: Rate,
    /// Best price on the side we would take.
    pub touch: Price,
    /// Volume-weighted price actually achievable for [`Opportunity::size`]. This,
    /// not `touch`, is the price the money moves at.
    pub vwap: Price,
    /// `vwap` against `touch`, in bps. The part of the cost that no fee schedule
    /// shows.
    pub slippage: Bps,
    /// The largest size this leg alone could absorb inside the slippage budget.
    pub capacity: Qty,
    /// Margin this leg requires at the configured leverage.
    pub margin: Money,
    /// Liquidation price, when the leg has one and it is computable.
    pub liquidation: Option<Price>,
    /// Distance from entry to liquidation, in bps. `None` for spot, which cannot
    /// be liquidated, and `None` where the inputs do not determine it.
    pub distance_to_liq: Option<Bps>,
    /// Book freshness verdict.
    pub book_age: Staleness,
    /// Funding freshness verdict. `None` for spot, which has no funding.
    pub funding_age: Option<Staleness>,
    /// Clock agreement verdict.
    pub clock: ClockVerdict,
    /// Painted-depth score, 0 = clean.
    pub paint_score: u8,
    /// Combined quality for this leg.
    pub quality: DataQuality,
}

/// One ranked pair.
#[derive(Debug, Clone)]
pub struct Opportunity {
    /// The leg we sell. By construction the one whose annualised receipt is
    /// better to be short of.
    pub short: OppLeg,
    /// The leg we buy.
    pub long: OppLeg,
    /// The size both legs can carry, in the base asset. **Every other number in
    /// this row is a function of it.**
    pub size: Qty,
    /// Notional at the achievable prices, not at the touch.
    pub notional: Money,
    /// Sum of the two signed annualised receipts. Not the difference of the two
    /// published rates — see [`crate::scan`] for why those are different numbers
    /// whenever the intervals differ.
    pub gross_apr: Rate,
    /// Four fee legs: enter both, exit both.
    pub fee_bps: Bps,
    /// Depth cost on both legs, entry and exit.
    pub slippage_bps: Bps,
    /// `fee_bps + slippage_bps`. What the round trip costs once, in bps of
    /// notional.
    pub cost_bps: Bps,
    /// Gross annualised receipt minus the round-trip cost amortised over the
    /// configured holding horizon. **This is the number that must equal the money
    /// that arrives.**
    pub net_apr: Rate,
    /// Margin for both legs together.
    pub margin: Money,
    /// The nearer of the two liquidation distances — the one that actually ends
    /// the trade. `None` when neither leg can be liquidated.
    pub distance_to_liq: Option<Bps>,
    /// How long the position must be held for the funding received to cover the
    /// round trip. `None` when the carry never covers it.
    pub periods_to_breakeven: Option<u64>,
    /// Combined quality across both legs.
    pub quality: DataQuality,
    /// Everything standing between this row and a tradeable number. Empty means
    /// the row is rankable.
    pub blocked: Vec<Unrankable>,
}

impl Opportunity {
    /// Whether the printed number may be acted on.
    ///
    /// A row is rankable only when nothing blocks it **and** its combined quality
    /// says so. Both conditions are needed: `blocked` catches what this crate
    /// measured, `quality.is_rankable()` catches what the layers below flagged.
    pub fn is_rankable(&self) -> bool {
        self.blocked.is_empty() && self.quality.is_rankable()
    }

    /// Sort key: best net first, and never a blocked row above an unblocked one.
    ///
    /// Blocked rows sink regardless of how good their arithmetic looks, because a
    /// blocked row's arithmetic is precisely what is in doubt. Within each group
    /// the order is by `net_apr` descending, then by venue for determinism — two
    /// rows with an identical net must not swap places between runs, or a diff of
    /// two reports becomes unreadable.
    pub fn rank_key(&self) -> (bool, core::cmp::Reverse<Rate>, &'static str, &'static str) {
        (
            !self.is_rankable(),
            core::cmp::Reverse(self.net_apr),
            self.short.vkey.venue.as_str(),
            self.long.vkey.venue.as_str(),
        )
    }
}
