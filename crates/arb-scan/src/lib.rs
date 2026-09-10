#![forbid(unsafe_code)]
//! Cross-venue funding and basis scanning. **Public — this crate is sold.**
//!
//! # What this crate promises
//!
//! One number, and that the number is honest: the net annualised return of
//! holding a funding-carry position across two venues, **for a size those two
//! books can actually absorb**, after all four fee legs and the depth cost of
//! getting in and out.
//!
//! Everything else here exists to keep that promise. The pairing rules refuse
//! trades that are not hedges. The sizing walks real ladders instead of the touch.
//! The quality marks travel with the row so a number computed from an invented
//! fixture cannot be mistaken for one measured on a live feed.
//!
//! # What it does not promise
//!
//! - **That the fixtures are real.** In this round every adapter is fixture-backed
//!   and every row therefore carries [`arb_types::Provenance::SynthesizedUnverified`]
//!   and appears as unrankable. That is the design working, not a defect.
//! - **That the exit costs what the entry cost.** Exit slippage is assumed to
//!   mirror entry slippage, because the book at exit time is unknowable now. It is
//!   the largest known unknown in `net_apr` and it is recorded in `OPEN.md`.
//! - **That funding will still be there.** The rate is a snapshot, often a
//!   *predicted* one. `periods_to_breakeven` exists so the operator can see how
//!   long the rate has to persist before the round trip is paid off.
//!
//! # Public, deliberately
//!
//! This crate is the product. The strategies that do not survive being sold —
//! quote-and-cover, listings, DEX↔CEX — live in `arb-private` and are physically
//! absent from this dependency graph, not switched off by a flag.
//!
//! # Order of operations
//!
//! [`LegInput::gather`] pulls one instrument off an [`arb_venue::ExchangeAdapter`].
//! [`scan`] enumerates unordered pairs, refuses the ones that are not trades, and
//! prices the rest into [`Opportunity`] rows sorted best-first with unrankable rows
//! sunk to the bottom.

pub mod config;
pub mod error;
pub mod fees;
pub mod leg;
pub mod opportunity;
pub mod persistence;
pub mod scan;

pub use config::ScanConfig;
pub use error::{PairRefusal, ScanError};
pub use fees::{
    bridge, bridge_with_quality, round_trip_bps, round_trip_bps_worst_order, tier_flags,
    BridgedFees,
};
pub use leg::LegInput;
pub use opportunity::{OppLeg, Opportunity, RoundTripCost, Unrankable};
pub use persistence::{evidence, PersistenceEvidence};
pub use scan::{pair_check, price_pair, scan};
