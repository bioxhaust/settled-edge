//! Economics. Pure functions, no state, no I/O.
//!
//! This crate exists to make one sentence true: **the number on the screen
//! equals the money that arrives.** Every scanner on the market computes a
//! top-of-book spread and calls it an opportunity. That number is wrong in four
//! specific ways, and this crate is the four fixes:
//!
//! 1. **Fees are real and tiered.** [`fees`] resolves a tier and charges the
//!    notional, rounding against us. An unknown tier resolves to the *worst*
//!    tier, so a reported edge is a lower bound rather than a hope.
//! 2. **Your size does not fill at the touch.** [`execution`] walks the ladder
//!    and returns the VWAP you would actually pay, the size that is actually
//!    there, and the largest size that fits inside a slippage budget.
//! 3. **Funding is a rate over an interval, not a number.** [`funding`]
//!    annualises exactly and — more usefully — reports how many settlements it
//!    takes to pay off the round trip. That figure is the one no competitor
//!    shows, because for most "opportunities" it is larger than anyone would
//!    hold.
//! 4. **Leverage has a cliff.** [`liquidation`] gives the price at which the
//!    position ends, for linear *and* inverse contracts, which use different
//!    formulas.
//!
//! # What this crate deliberately does not do
//!
//! - **It does not annualise perp basis.** See [`basis`]. Annualising a perp
//!   basis is a category error: there is no expiry to converge to. Presenting
//!   it as an APR would be the single most flattering lie available to us.
//! - **It does not model borrow cost.** Any strategy with a short *spot* leg
//!   pays to borrow, at rates that dwarf a twelve basis point edge. Until that
//!   is modelled, results for such configurations are not net-of-costs and must
//!   not be presented as actionable. Tracked as `OPEN.md` A1.
//! - **It does not model transfer cost between venues,** so every figure here
//!   is a steady-state figure that ignores the cost of reaching steady state.
//!   Tracked as `OPEN.md` A2.
//! - **It assumes isolated margin, one-way mode** in [`liquidation`]. Cross
//!   margin gives a materially different answer. Tracked as `OPEN.md` A3.
//!
//! Those four omissions are stated here rather than buried, because each one is
//! individually capable of exceeding the edge we claim.
//!
//! # Conventions
//!
//! Every lossy operation names its [`arb_num::RoundMode`]. Where this crate
//! chooses the mode itself, it chooses the one that makes the opportunity look
//! worse, and says so in the function's docs. Where the honest direction
//! depends on a sign the caller does not know in advance — a funding payment
//! that may be a receipt or a cost — this crate inspects the sign and picks,
//! rather than accepting a mode it cannot validate.

#![forbid(unsafe_code)]

pub mod basis;
pub mod error;
pub mod execution;
pub mod fees;
pub mod funding;
pub mod liquidation;
pub mod sizing;

pub use error::MathError;

pub use basis::{basis_bps, basis_ratio, convergence_capture};
pub use execution::{
    executable_price, max_size_within_slippage, slippage_bps, Execution, SlippageBudget,
};
pub use fees::{fee_on_notional, FeeRates, FeeRole, FeeTable, RoundTrip, VolumeTier};
pub use funding::{
    annualise, funding_payment, net_apr_over_horizon, periods_to_breakeven, CarryLeg,
};
pub use liquidation::{distance_to_liquidation_bps, liquidation_price, LiquidationInputs};
pub use sizing::{contracts_for_notional, margin_required, round_down_to_lot};
