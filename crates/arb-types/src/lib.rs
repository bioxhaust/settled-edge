#![forbid(unsafe_code)]
//! `arb-types` — the shared vocabulary: venues, instruments, snapshots, and the
//! data-quality machinery.
//!
//! This crate holds no strategy math (that is `arb-math`) and performs no I/O
//! (that is `arb-venue`). It exists so that eight people cannot independently
//! invent eight incompatible notions of "the price of a thing at a time".
//!
//! # The three design choices that matter
//!
//! **1. Wall clock and local clock are different types.** [`WallTs`] is a
//! timestamp the *exchange* told us. [`LocalTs`] is when *we* observed it. They
//! are not interchangeable: subtracting one from the other measures clock skew
//! plus latency, and confusing them is how a scanner decides a two-minute-old
//! quote is fresh. There is deliberately no `From` impl between them.
//!
//! **2. Contract size is an enum, not a number.** [`ContractSize`] forces the
//! caller to distinguish "one contract is N base units" (linear) from "one
//! contract is N quote units" (inverse). These produce different notional
//! formulas. A single `contract_size: Qty` field would let an inverse BTCUSD
//! position be sized wrongly by a factor of the price — about 68,000x.
//!
//! **3. Book depth is accessed by intent.** [`BookSnapshot::taking_levels`]
//! takes a [`Side`] and returns the levels you actually consume: buying lifts
//! *asks*, selling hits *bids*. Reading `book.bids` to price a buy is the
//! single most common depth bug, and this API makes it awkward to write.
//!
//! # Data quality is structural, not advisory
//!
//! Rule 7 of this project says data not obtained by measurement is marked
//! untrustworthy. That is enforced here rather than remembered: every snapshot
//! carries a [`DataQuality`], and [`DataQuality::combine`] unions the warning
//! flags and keeps the *worst* [`Provenance`]. Any calculation that touches a
//! synthesized fixture yields a result stamped
//! [`Provenance::SynthesizedUnverified`], all the way to the screen.

mod book;
mod funding;
mod instrument;
mod quality;
mod strings;
mod time;
mod venue;

pub use book::{BookSnapshot, Level, Side};
pub use funding::{FundingInterval, FundingSnapshot, PositionSide, SECONDS_PER_YEAR};
pub use instrument::{
    ContractSize, ContractSpec, Instrument, InstrumentKey, MarketKind, SizeUnit, VenueKey,
};
pub use quality::{DataQuality, Provenance, QualityFlags};
pub use strings::{Asset, Symbol};
pub use time::{LocalTs, Millis, WallTs};
pub use venue::Venue;

use core::fmt;

/// Every way constructing a type in this crate can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeError {
    /// A symbol or asset code was empty.
    Empty,
    /// A symbol or asset code exceeded its inline capacity.
    TooLong,
    /// A character outside the permitted set for this string type.
    InvalidChar,
    /// Book levels were not in the required monotonic order.
    UnsortedLevels,
    /// A level had a non-positive price or quantity.
    NonPositiveLevel,
    /// A funding interval of zero seconds, which would divide by zero in any
    /// annualisation.
    ZeroFundingInterval,
    /// A price that must be positive was zero or negative.
    NonPositivePrice,
    /// A contract's denomination disagreed with its market kind: a linear or
    /// spot market declared quote-per-contract, or an inverse market declared
    /// base-per-contract. See [`ContractSize`].
    ContractSizeMismatch,
    /// An instrument's key and its contract spec disagreed about the settlement
    /// asset. Two sources of truth for settlement is one too many.
    SettleMismatch,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TypeError::Empty => "empty identifier",
            TypeError::TooLong => "identifier exceeds inline capacity",
            TypeError::InvalidChar => "illegal character in identifier",
            TypeError::UnsortedLevels => "book levels out of order",
            TypeError::NonPositiveLevel => "book level with non-positive price or size",
            TypeError::ZeroFundingInterval => "funding interval of zero seconds",
            TypeError::NonPositivePrice => "non-positive price where positive required",
            TypeError::ContractSizeMismatch => {
                "contract denomination disagrees with market kind"
            }
            TypeError::SettleMismatch => "instrument key and spec disagree on settle asset",
        };
        f.write_str(s)
    }
}

impl std::error::Error for TypeError {}
