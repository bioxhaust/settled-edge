#![forbid(unsafe_code)]

//! Private strategies: the ones we run and do not sell.
//!
//! # The boundary is physical, not a flag
//!
//! `arb-scan` holds the strategies we sell. This crate holds the ones we do not:
//! quote-and-cover, new-listing capture, DEX-CEX. The split is a crate boundary
//! rather than a `cfg` feature or a config field because a flag has exactly one
//! failure mode and it is fatal: somebody flips it, or forgets to flip it back,
//! and a low-capacity strategy reaches the customers who would consume the
//! capacity. These strategies do not scale with capital. Every unit of size a
//! subscriber puts through them is a unit we cannot put through them. Selling
//! them does not dilute the edge, it deletes it.
//!
//! A crate boundary fails loudly instead. If `arb-private` is not in the
//! dependency graph, its code is not in the binary — there is no runtime state
//! that can be wrong.
//!
//! # Two rules that are not negotiable
//!
//! 1. `arb-private` must never appear in the dependency list of anything we
//!    ship. Not the customer binary, not the hosted API, not the SDK, not an
//!    example, not a `dev-dependency` of a shipped crate. If a review sees
//!    `arb-private` in a manifest that leaves the building, that review fails.
//! 2. `arb-scan` must never depend on `arb-private`. The dependency arrow may
//!    only point this way: `arb-private -> arb-math/arb-types/arb-num`, and
//!    `arb-private -> arb-scan` is acceptable if we need scan's shared
//!    machinery. The reverse turns rule 1 into a transitive leak that no
//!    manifest review will catch.
//!
//! # State of the crate
//!
//! Types and seams. No strategy in here computes a plan yet, and none of them
//! pretends to: every `plan` function returns
//! [`PrivateError::NotImplemented`] carrying the specific thing it is waiting
//! on. There is no `todo!()` anywhere in this crate — a stub that panics is a
//! landmine that ships, and it ships in the one code path nobody exercised.
//! The blocked-on strings are the actual work list; see [`BLOCKERS`].

pub mod dex_cex;
pub mod error;
pub mod listings;
pub mod quote_cover;

pub use error::PrivateError;

/// Every strategy in this crate, paired with what it is blocked on.
///
/// This is the work list, in one place, so that "what is left" is a fact in the
/// source rather than a claim in a status meeting. Each entry is the same pair
/// of strings the corresponding `plan` function returns in
/// [`PrivateError::NotImplemented`], so this table cannot drift from behaviour
/// without a test failing.
pub const BLOCKERS: [(&str, &str); 3] = [
    (quote_cover::STRATEGY, quote_cover::BLOCKED_ON),
    (listings::STRATEGY, listings::BLOCKED_ON),
    (dex_cex::STRATEGY, dex_cex::BLOCKED_ON),
];
