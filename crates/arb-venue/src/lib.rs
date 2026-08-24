#![forbid(unsafe_code)]

//! `arb-venue` — the seam between our types and eight exchanges.
//!
//! Everything upstream of this crate (numbers, instruments, books, funding,
//! sizing, ranking) is pure and testable. Everything venue-shaped —
//! symbol spellings, sign conventions, fee tiers, idempotency, reconciliation,
//! unwinds — lives behind the traits in [`adapter`] and [`execution`].
//!
//! The point of the seam: when the network opens, `tokio` and a WebSocket
//! client appear behind these same traits and **nothing else in the workspace
//! changes by one line**. A live adapter is a new `impl ExchangeAdapter`, not a
//! new set of types. If a future live adapter needs a signature that is not
//! here, that is a defect in this crate, not a reason to leak `reqwest` types
//! into the strategy.
//!
//! # This round has no network
//!
//! The only implementation shipped is [`fixture_adapter::FixtureAdapter`],
//! which reads a hand-written line-oriented format. Every number it produces is
//! [`arb_types::Provenance::SynthesizedUnverified`] and there is no code path in
//! this crate that can produce `LiveMeasured`. That is enforced by a single
//! constructor funnel ([`fixture::synthesized`]) and asserted by test.
//!
//! # I/O
//!
//! Workspace rule: no I/O outside this crate. Inside this crate, I/O is
//! confined to exactly one function, [`fixture_adapter::FixtureAdapter::load_dir`].
//! Every other function here is pure — give it a `&str` and it needs no
//! filesystem, which is why the whole test suite runs without touching disk
//! except for one integration test that deliberately exercises `load_dir`.
//! (The workspace rule says "I/O only inside trait implementations in
//! arb-venue"; `load_dir` is an inherent constructor, not a trait method. The
//! deviation is deliberate and is the only one: a trait method cannot be a
//! constructor, and hiding the directory walk behind a trait would buy nothing
//! but indirection.)
//!
//! # What is *not* here
//!
//! No sizing, no ranking, no annualisation, no VWAP walking. This crate hands
//! out [`arb_types::BookSnapshot`] / [`arb_types::FundingSnapshot`] /
//! [`FeeSchedule`] and stops. Anything that decides whether a trade is good
//! belongs upstream.

pub mod adapter;
pub mod error;
pub mod execution;
pub mod fixture;
pub mod fixture_adapter;
pub mod unification;

pub use adapter::{ExchangeAdapter, FeeSchedule};
pub use error::{ParseReason, VenueError};
pub use execution::{
    ClientOrderId, Discrepancy, ExecutionAdapter, IdempotencyKey, OrderKind, OrderRequest,
    OrderState, OrderStatus, Reconcile, TimeInForce, Unwind, VenueOrderId,
};
pub use fixture::{
    parse_fixture, FixtureRecord, FundingFixture, IntervalSource, RateSource, UNVERIFIED_HEADER,
};
pub use fixture_adapter::FixtureAdapter;
pub use unification::{Confidence, Topic, UnificationGap};
