//! DEX-CEX: one leg on an AMM, one leg on a centralised venue.
//!
//! # The trade
//!
//! An on-chain pool prices the pair differently from a CEX book. We swap on the
//! pool and take the offsetting side on the CEX. Mechanically it looks like any
//! other two-legged spread, and that resemblance is the trap: two cost terms
//! that do not exist anywhere else in this engine dominate the result.
//!
//! # Gas cost
//!
//! Every swap pays gas, denominated in the chain's native asset, at a price set
//! by an auction we do not control. It is a fixed cost per attempt, not a rate
//! on notional, so it inverts the usual sizing logic: below some size the trade
//! cannot pay for itself no matter how wide the spread. It is also charged on
//! attempts that revert. `arb-math` models fees as rates on notional
//! (`Money::mul_rate`) and has no concept of a per-attempt fixed cost in a
//! third asset, so the gas leg cannot currently be priced into an edge at all.
//! That is not a rounding gap; it is a missing term.
//!
//! # Block time
//!
//! The swap does not execute when we send it. It executes when a block includes
//! it, which is a discrete event some seconds away, and the CEX leg is
//! continuous. The hedge is therefore exposed for at least one block time and we
//! do not get to choose the length of that exposure. Nothing in `arb-math`
//! models a leg that settles on a clock tick.
//!
//! # MEV: the price you saw is not the price you get
//!
//! A pending swap is public. Anything profitable enough to be worth doing is
//! profitable enough for a searcher to front-run, sandwich or simply reorder,
//! and the party choosing the order of transactions in the block is not us.
//! Simulating the pool at the current reserves gives the price we *would* have
//! received in an empty block, which is not a block that exists. The realised
//! price is systematically worse, and the size of that haircut is unknown to us
//! because we have never measured it.
//!
//! Those two absences — a fixed-cost/block-time model, and any measured
//! adverse-selection number — are the reason this strategy cannot be turned on.
//! Not "would be nice to add": every plan we could produce today would be
//! optimistic by an unknown amount, and an optimistic edge on a twelve basis
//! point strategy is a loss with extra steps.
//!
//! # Status
//!
//! Not implemented. See [`BLOCKED_ON`].

use arb_num::{Bps, Money, Price, Qty};
use arb_types::{Asset, BookSnapshot, DataQuality, LocalTs, Millis, Side, Symbol, VenueKey};

use crate::error::PrivateError;

/// Stable machine name, used in errors, metrics and [`crate::BLOCKERS`].
pub const STRATEGY: &str = "dex_cex";

/// What has to exist before [`plan`] can return a plan.
pub const BLOCKED_ON: &str = "a cost model that admits a fixed per-attempt gas cost in a third \
     asset and a settlement delay of one block (arb-math models fees only as rates on notional, \
     applied instantly), plus a measured MEV/adverse-selection haircut from our own executed \
     swaps — no such measurement exists, so any number used today would be invented";

/// Which leg is the buy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DexCexDirection {
    /// Buy on the pool, sell on the CEX.
    BuyDexSellCex,
    /// Buy on the CEX, sell on the pool.
    BuyCexSellDex,
}

impl DexCexDirection {
    /// The side we take on the CEX book.
    ///
    /// Buying on the DEX means selling on the CEX, and selling hits bids —
    /// so callers must fetch depth with `taking_levels(Side::Sell)`. Getting
    /// this backwards reads the wrong side of the book and produces an edge
    /// inflated by the whole CEX spread.
    pub fn cex_side(&self) -> Side {
        match self {
            DexCexDirection::BuyDexSellCex => Side::Sell,
            DexCexDirection::BuyCexSellDex => Side::Buy,
        }
    }

    /// The side of the pool swap, i.e. the opposite of the CEX leg.
    pub fn dex_side(&self) -> Side {
        self.cex_side().opposite()
    }
}

/// A simulated pool quote for one specific size.
///
/// There is no "the price" on an AMM: the price is a function of size, because
/// the swap moves along the curve it is quoted from. So this type is a quote for
/// `size` and nothing else. Reusing it for a different size is wrong, and
/// silently wrong in the direction that flatters the trade.
pub struct DexQuote {
    /// Pool or router identifier, as the chain reports it.
    ///
    /// Not a `Venue`: `Venue` enumerates the eight centralised exchanges, and
    /// extending it is not this crate's call (we do not edit `arb-types`). A
    /// `Symbol` holds the pool identifier inline, which is enough to route and
    /// enough to log. If pools become first-class, `arb-types` needs a `Chain`
    /// and a pool key — noted, not assumed.
    pub pool: Symbol,
    /// Base asset of the swap.
    pub base: Asset,
    /// Quote asset of the swap.
    pub quote: Asset,
    /// Effective price for exactly `size`, quote per base, including the pool
    /// fee and the curve impact. Not a spot or mid price.
    pub effective_price: Price,
    /// The size this quote is for, in base units.
    pub size: Qty,
    /// Gas cost of the swap, denominated in `gas_asset`. Fixed per attempt.
    pub gas_cost: Money,
    /// Asset gas is paid in — the chain's native asset, which is usually
    /// neither the base nor the quote of the pair being traded.
    pub gas_asset: Asset,
    /// Median block time of the chain: the floor on how long the DEX leg takes
    /// to settle, and therefore the minimum unhedged exposure.
    pub block_time: Millis,
    /// When we simulated the swap. Pool reserves move every block, so a
    /// simulation has a shelf life measured in single-digit seconds.
    pub observed_at: LocalTs,
    /// Provenance and flags. A quote from a simulation against cached reserves
    /// is not a measured fill and must carry that.
    pub quality: DataQuality,
}

impl DexQuote {
    /// True if this quote was simulated for exactly `size`.
    ///
    /// The check is exact equality, not a tolerance. An AMM quote for a
    /// different size is a different price, and "close enough" here means
    /// booking curve impact we did not pay for.
    pub fn covers_size(&self, size: Qty) -> bool {
        self.size == size
    }
}

/// Inputs to a DEX-CEX plan.
pub struct DexCexParams<'a> {
    /// The pool quote, simulated for [`DexCexParams::size`].
    pub dex: &'a DexQuote,
    /// The CEX book for the offsetting leg.
    pub cex_book: &'a BookSnapshot,
    /// Which leg is the buy.
    pub direction: DexCexDirection,
    /// Size in base units. Must match the size the pool quote was built for.
    pub size: Qty,
    /// Taker fee on the CEX. The CEX leg is always a taker: the DEX leg
    /// executes in a block whether or not our resting order filled, so a
    /// passive CEX leg would leave a naked position.
    pub cex_taker_fee: Bps,
    /// Price of one unit of `dex.gas_asset` in the CEX quote asset. Required
    /// unless gas is paid in the quote asset, because the gas cost otherwise
    /// cannot be added to a quote-denominated edge.
    pub gas_in_quote: Option<Price>,
    /// Measured haircut for MEV and adverse selection, applied to the DEX leg.
    ///
    /// `Option` and not a defaulted constant: there is no measured value, and a
    /// plausible default would be a guess embedded in a cost model where nobody
    /// would ever find it. `None` is an honest refusal to price the trade.
    pub mev_haircut: Option<Bps>,
    /// Net edge, after fees, gas and haircut, below which we do not trade.
    pub min_edge: Bps,
}

/// A DEX-CEX instruction: one pool swap and one CEX order.
#[derive(Debug, Clone, Copy)]
pub struct DexCexPlan {
    /// Pool the swap goes to.
    pub pool: Symbol,
    /// Venue and instrument for the CEX leg.
    pub cex_vkey: VenueKey,
    /// Which leg is the buy.
    pub direction: DexCexDirection,
    /// Size in base units.
    pub size: Qty,
    /// Edge before gas and haircut, in bps. Reported separately from
    /// `net_edge` because the difference between the two is the entire reason
    /// this strategy is hard, and collapsing them hides it.
    pub gross_edge: Bps,
    /// Gas cost converted into the CEX quote asset.
    pub gas_cost_in_quote: Money,
    /// Edge after CEX fee, pool fee, gas and MEV haircut.
    pub net_edge: Bps,
    /// Minimum time the position is unhedged: one block, at least.
    pub exposure_window: Millis,
    /// Combined quality of the pool simulation and the CEX book, worst
    /// provenance retained.
    pub quality: DataQuality,
}

/// Plan a DEX-CEX pair, or say why not.
///
/// Runs the checks that do not depend on the missing cost model, then returns
/// [`PrivateError::NotImplemented`] with [`BLOCKED_ON`]. It never returns `Ok`
/// today, and it must not: see the module docs for what would be silently
/// omitted from any edge it produced.
pub fn plan(params: &DexCexParams<'_>) -> Result<DexCexPlan, PrivateError> {
    if !params.size.is_positive() {
        return Err(PrivateError::MissingData {
            what: "a positive trade size",
        });
    }

    if !params.dex.covers_size(params.size) {
        return Err(PrivateError::MissingData {
            what: "a pool simulation for the exact size being traded; AMM price is a function of \
                   size and a quote for another size does not apply",
        });
    }

    if !params.dex.effective_price.is_positive() {
        return Err(PrivateError::MissingData {
            what: "a positive effective price from the pool simulation",
        });
    }

    if params.cex_book.touch(params.direction.cex_side()).is_none() {
        return Err(PrivateError::MissingData {
            what: "top of book on the CEX for the side we would take",
        });
    }

    // Gas is charged in the chain's native asset. Unless that happens to be the
    // pair's quote asset, the cost cannot enter a quote-denominated edge without
    // a conversion price, and omitting it would understate the cost by its whole
    // value.
    if params.dex.gas_asset.as_str() != params.dex.quote.as_str() && params.gas_in_quote.is_none() {
        return Err(PrivateError::MissingData {
            what: "a price for the gas asset in the quote asset; gas cannot otherwise be \
                   converted into the edge",
        });
    }

    // The haircut has no default and no fallback. Refusing here is the point.
    if params.mev_haircut.is_none() {
        return Err(PrivateError::MissingData {
            what: "a measured MEV/adverse-selection haircut; the simulated pool price is not the \
                   price we get and the difference has never been measured",
        });
    }

    // Block time is the floor on unhedged exposure. Zero or negative means
    // nobody supplied it, and a zero-latency settlement assumption is the most
    // flattering possible error.
    if !params.dex.block_time.millis().is_positive() {
        return Err(PrivateError::MissingData {
            what: "a positive chain block time; the DEX leg does not settle instantly",
        });
    }

    Err(PrivateError::not_implemented(STRATEGY, BLOCKED_ON))
}
