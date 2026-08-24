//! What we intend to trade, and every threshold the answer depends on.
//!
//! # Why there is no `Default`
//!
//! Two of these fields — [`ScanConfig::target_notional`] and
//! [`ScanConfig::horizon`] — decide on their own whether an opportunity exists.
//! Size decides how far down the ladder we eat, and therefore how much slippage
//! we pay; horizon decides how thinly the round trip is amortised, and a short
//! enough horizon turns every positive row in the table negative. A `Default`
//! impl would let both be wrong without anybody typing a number, and the wrongness
//! would be invisible because the output would still look like a table of
//! opportunities. [`ScanConfig::conservative`] therefore demands both as
//! arguments and fills in only the detector limits, each of which is a constant
//! somebody else already justified in `arb-detect`.
//!
//! # Why `now` is a field and not a clock read
//!
//! Rule 5: no I/O in this crate, and reading a clock is I/O. But the deeper
//! reason is that a replay must be able to choose its own evaluation instant. A
//! fixture book carries `observed_at == LocalTs::EPOCH` deliberately, so against
//! any realistic wall clock every fixture row is maximally stale and gets refused
//! — which is the system telling the truth about invented data. A deterministic
//! test that wants to exercise the *economics* rather than the staleness gate
//! sets `now = LocalTs::EPOCH` and the ages come out zero. Both behaviours are
//! correct and the caller has to say which one it wants.

use arb_detect::{PaintLimits, SkewLimits, StalenessLimits};
use arb_math::{RoundTrip, SlippageBudget};
use arb_num::{Bps, Money, Rate};
use arb_types::{LocalTs, Millis};

use crate::error::ScanError;

/// Every input to a scan that is not market data.
///
/// All fields are public: this is a configuration record, and a private field
/// with a getter would only add a place for a default to hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanConfig {
    /// The notional we actually intend to put on **each leg**, in the quote
    /// asset.
    ///
    /// This is the single most consequential field. It is not a display
    /// preference: the slippage in every row is measured by walking the real
    /// ladder for exactly this size. Set it too small and the table shows an
    /// edge that exists only for a size nobody would trade; set it too large and
    /// legs come back partially filled and the rows are excluded, which is the
    /// honest outcome but not a useful one. It is also the number the venue
    /// minimum is checked against — see
    /// [`crate::report::ExclusionReason::BelowMinNotional`].
    pub target_notional: Money,

    /// Leverage on each perp leg, as a multiple. `Rate::ONE` is fully funded.
    ///
    /// Only affects margin and the distance to liquidation, never the return:
    /// funding is paid on notional, not on margin. Set it too high and
    /// `margin_required` falls below the maintenance requirement, at which point
    /// [`arb_math::MathError::AlreadyBelowMaintenance`] is the correct answer and
    /// the row is excluded rather than shown with a liquidation price on the
    /// wrong side of entry.
    pub leverage: Rate,

    /// Maintenance margin rate, in `[0, 1)`. `0.005` is 0.5%.
    ///
    /// `[assumption]` 0.5% is recalled from the first tier of the major venues'
    /// BTC perp tables and has never been read off a live endpoint. Set it too
    /// low and the reported distance to liquidation is larger than the truth —
    /// the unsafe direction, because the operator is told there is room that does
    /// not exist. Set it too high and rows get excluded that would in fact be
    /// fine. Also, tiered maintenance margin is not modelled at all: this single
    /// number is correct only inside one tier (`OPEN.md` A3).
    pub maintenance_rate: Rate,

    /// Slippage we are willing to pay on one crossing, in basis points.
    ///
    /// Used as the capacity question — "how much size fits inside this budget" —
    /// not as a filter on the cost we report. The cost we report is always the
    /// measured cost of our actual size, whether or not it is inside the budget,
    /// because a budget breach is a fact about capacity and must not be allowed
    /// to silently shrink the size we claim to be trading.
    pub slippage_budget: SlippageBudget,

    /// Levels per side to request from each venue.
    ///
    /// Too few and the ladder walk runs out early: the leg comes back partially
    /// filled, `SIZE_EXCEEDS_DEPTH` is raised, and the row is excluded — a false
    /// negative caused by our own request. Too many is only a bandwidth cost. It
    /// must be at least 1; a depth of 0 is refused by the adapter rather than
    /// answered with an empty book.
    pub book_depth: usize,

    /// Which of the four legs we expect to be maker and which taker.
    ///
    /// [`RoundTrip::ALL_TAKER`] is the realistic assumption for a cross-venue
    /// carry and is the only one that cannot flatter us: a resting order that
    /// does not fill leaves the other leg naked, and being naked for minutes is a
    /// larger loss than the maker rebate is a saving. Configuring maker legs
    /// lowers every cost in the table and should only be done by somebody who
    /// has measured their own fill rate.
    pub round_trip: RoundTrip,

    /// How long we expect to hold the position.
    ///
    /// The round trip is a one-off cost, so it is annualised over this horizon
    /// and subtracted. Halving the horizon doubles the annualised drag. A horizon
    /// of one funding interval makes almost every row negative, and that is not a
    /// bug in the arithmetic — it is what holding for one settlement actually
    /// pays. Never let this be tuned upward to make a row positive; that is
    /// choosing the answer and then choosing the question.
    pub horizon: Millis,

    /// The instant the scan is evaluated at, on our own clock.
    ///
    /// Every age in the report is `now - observed_at`. See the module docs for
    /// why this is an argument.
    pub now: LocalTs,

    /// Age thresholds for order books. [`StalenessLimits::BOOK`] unless a venue
    /// has been measured publishing slower than assumed.
    pub book_staleness: StalenessLimits,

    /// Age thresholds for funding snapshots. [`StalenessLimits::FUNDING`]:
    /// funding is a slow variable and book thresholds would flag data that is
    /// genuinely fine.
    pub funding_staleness: StalenessLimits,

    /// Tolerance for exchange-versus-local timestamp disagreement.
    /// [`SkewLimits::WAN`] for anything reached over the public internet, which
    /// is all eight venues.
    pub skew: SkewLimits,

    /// Painted-depth thresholds. [`PaintLimits::CONSERVATIVE`].
    ///
    /// `tick_size` inside these limits is overridden per instrument from the
    /// contract spec before the detector runs — a single tick for eight venues
    /// and two assets would make the ladder-gap test meaningless. `round_unit`
    /// is *not* overridden, because no accessor exists for the venue's own round
    /// size and guessing one would invent a threshold.
    pub paint: PaintLimits,
}

impl ScanConfig {
    /// A configuration whose thresholds all come from constants somebody else
    /// justified, with the four numbers that state our intent left explicit.
    ///
    /// `maintenance_rate` is fixed at 0.5% here; see the field docs for why that
    /// is an assumption and which direction it is wrong in.
    /// `slippage_budget` is 10 bps: near the size of the whole edge, so a
    /// crossing that costs more than this is not a trade we would place, and
    /// setting it much wider would report capacity for sizes that consume the
    /// return.
    /// `book_depth` is 20 levels: deep enough that a size worth trading does not
    /// run off the end of our own request, shallow enough to stay inside every
    /// venue's cheapest depth endpoint.
    pub fn conservative(
        target_notional: Money,
        leverage: Rate,
        horizon: Millis,
        now: LocalTs,
    ) -> Result<Self, ScanError> {
        let cfg = Self {
            target_notional,
            leverage,
            // 0.5% maintenance == 50 bps. See the field docs.
            maintenance_rate: Rate::from_bps(Bps::from_whole(50)?),
            slippage_budget: SlippageBudget::new(Bps::from_whole(10)?)?,
            book_depth: 20,
            round_trip: RoundTrip::ALL_TAKER,
            horizon,
            now,
            book_staleness: StalenessLimits::BOOK,
            funding_staleness: StalenessLimits::FUNDING,
            skew: SkewLimits::WAN,
            paint: PaintLimits::CONSERVATIVE,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject a configuration that cannot produce a meaningful answer.
    ///
    /// Checked rather than clamped. A clamped size or a clamped horizon still
    /// produces a table, and the table would be about a trade nobody asked for.
    pub fn validate(&self) -> Result<(), ScanError> {
        if !self.target_notional.is_positive() {
            return Err(ScanError::BadConfig(
                "target_notional must be positive; a zero size has no slippage and no minimum",
            ));
        }
        if !self.leverage.is_positive() {
            return Err(ScanError::BadConfig(
                "leverage must be positive; zero leverage is a division by zero, not a flat position",
            ));
        }
        if self.maintenance_rate.is_negative() || self.maintenance_rate >= Rate::ONE {
            return Err(ScanError::BadConfig(
                "maintenance_rate must be in [0, 1); at 1 the liquidation formula divides by zero",
            ));
        }
        if self.book_depth == 0 {
            return Err(ScanError::BadConfig(
                "book_depth must be at least 1; depth 0 is refused rather than answered empty",
            ));
        }
        // One whole second, because `net_apr_over_horizon` works in seconds and
        // truncates: a horizon under 1000 ms becomes zero seconds and is refused
        // downstream with a less specific message.
        if self.horizon.millis() < 1_000 {
            return Err(ScanError::BadConfig(
                "horizon must be at least one second; a sub-second horizon truncates to zero",
            ));
        }
        if !self.book_staleness.is_ordered() || !self.funding_staleness.is_ordered() {
            return Err(ScanError::BadConfig(
                "staleness limits must satisfy warn <= stale <= dead",
            ));
        }
        if !self.skew.is_ordered() {
            return Err(ScanError::BadConfig(
                "skew limits must satisfy tolerable <= suspicious",
            ));
        }
        if self.paint.touch_reference_levels < 2 {
            return Err(ScanError::BadConfig(
                "paint.touch_reference_levels must be at least 2 for a mean to mean anything",
            ));
        }
        Ok(())
    }
}
