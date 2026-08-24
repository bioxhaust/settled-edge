#![forbid(unsafe_code)]
//! Correctness adversary. Contains no product code.
//!
//! Every module here answers one question: **where does the number on the
//! screen diverge from the money that arrives in the account?**
//!
//! Each test states its hand derivation in a comment *before* the assertion.
//! The expected value is derived from the definition, never from what the code
//! returns — a test fitted to the output is a screenshot, not a test.
//!
//! A failing test in this crate is the deliverable, not a bug in this crate.
//! Do not weaken an assertion to make it pass; the owner of the crate under
//! test fixes the code.
//!
//! Modules, and the failure mode each hunts:
//!
//! - [`book`] — absolute-vs-incremental deltas, sequence holes, side words.
//! - [`units`] — scale confusion (1e12 vs 1e8), a ratio used as basis points.
//! - [`funding_sign`] — longs pay shorts, interval annualisation, 8x errors.
//! - [`rounding`] — every lossy operation must round against us.
//! - [`depth`] — a buy walks asks; VWAP is not the touch.
//! - [`staleness`] — a stale quote that still gets ranked.
//! - [`notional`] — linear vs inverse, the 68,000x confusion.

#[cfg(test)]
mod support;

#[cfg(test)]
mod book;
#[cfg(test)]
mod depth;
#[cfg(test)]
mod funding_sign;
#[cfg(test)]
mod notional;
#[cfg(test)]
mod rounding;
#[cfg(test)]
mod staleness;
#[cfg(test)]
mod units;
