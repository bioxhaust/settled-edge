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
use arb_math::RoundTrip;
use arb_num::{Bps, Money, Price, Qty, Rate};
use arb_types::{DataQuality, PositionSide, QualityFlags, VenueKey};

use crate::persistence::PersistenceEvidence;

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
    /// This row is a reverse carry — the short leg is spot, so opening it means
    /// borrowing the base asset — and no borrow rate accompanies that leg. The
    /// borrow can annualise into the tens of percent against an edge of twelve
    /// basis points, so the printed `net_apr` omits a cost term large enough to
    /// invert its sign. Blocked, not warned: the number is wrong, not imprecise.
    /// See [`arb_types::QualityFlags::BORROW_COST_UNKNOWN`] and `OPEN.md` A1.
    BorrowCostUnknown(VenueKey),
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
    /// One leg carries a [`arb_types::QualityFlags::BLOCKING`] flag that no other
    /// variant here has a dedicated check for — a sequence gap, an empty book, a
    /// missing mark price. Those arrive already set on the adapter's snapshot
    /// rather than being derived by `assess_leg`, so without this catch-all a
    /// snapshot that told us it was broken would be ranked anyway.
    ///
    /// The payload is only the *blocking* subset, not the leg's whole flag set:
    /// naming `FEE_ASSUMED` as a reason a row was sunk, when it is explicitly not
    /// one, would send the operator to fix the wrong thing.
    BlockingFlags(VenueKey, QualityFlags),
    /// Provenance is not a measurement. Always present in a fixture replay, and
    /// deliberately so: it is the marker that separates "the engine works" from
    /// "these numbers are real".
    ///
    /// This is about where the *market data* came from — book, ladder, funding
    /// rate. An assumed fee tier does not reach here; see [`crate::LegInput::quality`]
    /// for why that caveat rides on a flag instead.
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
            Unrankable::BorrowCostUnknown(v) => {
                write!(f, "{} short spot borrow rate unknown", v.venue)
            }
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
            Unrankable::BlockingFlags(v, flags) => write!(f, "{} {flags}", v.venue),
            Unrankable::Unverified(v) => write!(f, "{} data unverified", v.venue),
        }
    }
}

/// What one round-trip execution configuration costs, and what the row is worth
/// under it.
///
/// # Why a row carries three of these and not one
///
/// The fee assumption is the largest single lever in this product's economics
/// (`OPEN.md` A6, `decisions/020`). On a 2/5.5 bp schedule the all-taker round
/// trip is 22 bp and the all-maker one is 8, and at a carry of 1 bp per
/// settlement that is the difference between holding 7.3 days to break even and
/// holding 2.7. A row that printed only the all-taker figure was not merely
/// pessimistic — it hid the biggest decision the operator gets to make, and
/// `decisions/023` step 3 exists because of it.
///
/// The three are not a menu of equally reachable plans, and the labels say so:
/// [`RoundTrip::ALL_TAKER`] is what an operator gets by default,
/// [`RoundTrip::HEDGED_ENTRY`] is what they can actually reach, and
/// [`RoundTrip::ALL_MAKER`] is a **bound** — it requires every leg to rest and
/// fill on both venues in both directions. A row whose net is acceptable only in
/// the last one is a row whose thesis is an execution assumption rather than a
/// funding divergence, which is what [`Opportunity::pays_only_as_maker`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundTripCost {
    /// The configuration priced here.
    pub round_trip: RoundTrip,
    /// The leg whose **entry** rests while the other leg's entry crosses.
    ///
    /// `None` when the configuration prices both entries the same way, so there
    /// is no asymmetry to attribute — all-taker rests nothing and all-maker rests
    /// both. `Some` identifies the venue the resting order is placed on, and it
    /// is load-bearing rather than decorative: the two venues' maker rates and
    /// their taker-minus-maker savings differ, so "post one leg" is not one
    /// number until you say which leg. See [`crate::fees::round_trip_bps_worst_order`].
    pub resting_entry: Option<VenueKey>,
    /// Four fee legs under this configuration: enter both, exit both.
    pub fee_bps: Bps,
    /// `fee_bps` plus the row's depth cost, which is the same under every
    /// configuration — walking a ladder costs what it costs regardless of the fee
    /// role we are charged.
    pub cost_bps: Bps,
    /// Gross annualised receipt minus this configuration's cost, amortised over
    /// the configured horizon, with the borrow term of a reverse carry already
    /// applied.
    ///
    /// The borrow is applied here and not once outside, because subtracting it
    /// from only the configured configuration would have left the other two
    /// quietly omitting a cost that can invert the sign (`OPEN.md` A1).
    pub net_apr: Rate,
    /// The same net, recomputed from what the legs' settled records **actually
    /// paid** instead of from the currently published rates.
    ///
    /// `None` when any funding leg has no record, which is the same condition
    /// that raises [`arb_types::QualityFlags::RATE_HISTORY_MISSING`]. Not a
    /// forecast: it is the identical arithmetic over a measured mean, and it is
    /// here so a row priced on a rate spike cannot look like a row priced on a
    /// rate that has been paying for a month.
    pub net_apr_realised: Option<Rate>,
    /// Settlements the carry must survive to cover this configuration's round
    /// trip. `None` when the legs settle on different intervals, when neither
    /// funds, or when the carry never covers the cost.
    ///
    /// **This is the field the lever is visible in.** It is the same row, the
    /// same market and the same size; only the execution assumption moved.
    pub periods_to_breakeven: Option<u64>,
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
    /// What this leg's settled funding record measures.
    ///
    /// `None` for a leg that has no funding at all (spot receives none, and zero
    /// is the honest figure there) and `None` for a funding leg whose record is
    /// absent or empty — the case
    /// [`arb_types::QualityFlags::RATE_HISTORY_MISSING`] announces. The two are
    /// distinguishable by whether the flag is set, and they must not be conflated
    /// with a record full of zeros: see [`crate::persistence::evidence`].
    pub persistence: Option<PersistenceEvidence>,
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
    /// The same sum, taken over what each leg's settled record **actually paid**
    /// rather than over what each venue is publishing now.
    ///
    /// `None` when any funding leg has no record — the row then makes no claim
    /// about the past at all, which is exactly the state
    /// [`arb_types::QualityFlags::RATE_HISTORY_MISSING`] is for. A leg with no
    /// funding contributes exactly zero here, for the same reason it contributes
    /// zero to `gross_apr`: spot receives no funding, and that is a measurement
    /// rather than a gap.
    ///
    /// Read beside `gross_apr` this is the difference between a row backed by
    /// thirty days of one-signed funding and a row backed by nothing (`OPEN.md`
    /// A5). It is **not** a forecast and nothing here ranks on it.
    pub realised_gross_apr: Option<Rate>,
    /// Four fee legs: enter both, exit both. Under
    /// [`crate::config::ScanConfig::round_trip`] — the *configured* configuration,
    /// which is also one of [`Opportunity::costs`] unless the operator configured
    /// a fifth one of their own.
    pub fee_bps: Bps,
    /// Depth cost on both legs, entry and exit. Independent of the fee
    /// configuration, which is why it is not repeated inside
    /// [`RoundTripCost`].
    pub slippage_bps: Bps,
    /// `fee_bps + slippage_bps`. What the round trip costs once, in bps of
    /// notional, under the configured configuration.
    pub cost_bps: Bps,
    /// Gross annualised receipt minus the round-trip cost amortised over the
    /// configured holding horizon. **This is the number that must equal the money
    /// that arrives.**
    pub net_apr: Rate,
    /// The same row priced under each of [`RoundTrip::REPORTED`], most expensive
    /// first.
    ///
    /// Carried in addition to the flat fields above, not instead of them: the flat
    /// fields answer "what does the configuration I asked for cost", and these
    /// answer "what is the execution assumption worth". Both are computed by one
    /// code path, so they cannot drift — asserted by test.
    pub costs: [RoundTripCost; 3],
    /// Margin for both legs together.
    pub margin: Money,
    /// The nearer of the two liquidation distances — the one that actually ends
    /// the trade. `None` when neither leg can be liquidated.
    pub distance_to_liq: Option<Bps>,
    /// How long the position must be held for the funding received to cover the
    /// round trip, under the configured configuration. `None` when the carry never
    /// covers it. [`Opportunity::costs`] carries the same count for each of the
    /// three reported configurations, and the spread between them is the lever.
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

    /// This row priced under one named configuration, if it is among the three
    /// reported.
    ///
    /// `None` rather than a fallback for an unreported configuration: silently
    /// answering with the nearest one would put a number under a label it does not
    /// belong to, and that is the one failure mode a three-column table introduces
    /// that a one-column table could not have.
    pub fn cost_of(&self, round_trip: RoundTrip) -> Option<RoundTripCost> {
        self.costs.iter().copied().find(|c| c.round_trip == round_trip)
    }

    /// The dearest of the three reported configurations.
    ///
    /// Selected by comparing costs rather than by taking a known index, so it
    /// stays correct if a venue's schedule makes the usual ordering false — a
    /// maker rate above a taker rate is refused upstream, but a *pair* of
    /// schedules can still reorder the middle configuration against the ends.
    pub fn dearest(&self) -> RoundTripCost {
        let mut worst = self.costs[0];
        for c in &self.costs[1..] {
            if c.cost_bps.raw() > worst.cost_bps.raw() {
                worst = *c;
            }
        }
        worst
    }

    /// The cheapest of the three reported configurations. Almost always the
    /// all-maker bound, which is a bound and not a plan.
    pub fn cheapest(&self) -> RoundTripCost {
        let mut best = self.costs[0];
        for c in &self.costs[1..] {
            if c.cost_bps.raw() < best.cost_bps.raw() {
                best = *c;
            }
        }
        best
    }

    /// True when this row only pays under a cheaper fee configuration than the
    /// dearest reported one.
    ///
    /// The row's thesis is then an **execution assumption**, not a funding
    /// divergence: it needs orders to rest and fill, on both venues, and nothing
    /// in this workspace measures fill probability. Reported rather than blocked —
    /// the arithmetic is right and the trade may well be right; what would be
    /// wrong is presenting it as the same kind of row as one that pays even when
    /// every leg crosses the spread.
    pub fn pays_only_as_maker(&self) -> bool {
        !self.dearest().net_apr.is_positive() && self.cheapest().net_apr.is_positive()
    }

    /// True when the legs' settled records would have paid the **opposite sign**
    /// to what the currently published rates promise.
    ///
    /// A threshold-free comparison, deliberately: any rule of the form "flag a
    /// row whose realised return is more than X% below its published one" needs an
    /// X that nothing here has measured, whereas a sign disagreement needs none.
    /// `false` when no record exists — absence of evidence is reported by
    /// [`arb_types::QualityFlags::RATE_HISTORY_MISSING`], and answering `true`
    /// here would make a missing record look like a contradicted one.
    pub fn record_contradicts_sign(&self) -> bool {
        match self.realised_gross_apr {
            None => false,
            Some(realised) => {
                self.gross_apr.is_positive() && !realised.is_positive()
                    || self.gross_apr.is_negative() && !realised.is_negative()
            }
        }
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
