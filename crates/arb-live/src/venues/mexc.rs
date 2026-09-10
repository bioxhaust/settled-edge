#![forbid(unsafe_code)]
//! MEXC contract (USDT-M futures) public market data. **Not implemented yet.**
//!
//! This file is a placeholder so that `venues/mod.rs` and the crate's re-exports
//! can name all eight venues at once, and so the engineer who writes this adapter
//! changes exactly one file. Follow [`crate::venues::bybit`], which is the
//! finished reference.
//!
//! # What is already measured
//!
//! `[факт, проверено 2026-08-25]` `contract.mexc.com` answers
//! `GET /api/v1/contract/ping` with `{"success":true,"code":0,"data":1787608354642}`
//! from this environment.
//!
//! The envelope is a third distinct shape: a **boolean** `success` beside a
//! **bare** `code`. Checking `code == 0` and checking `success == true` are not
//! the same test, and this adapter should say in one place which one it trusts and
//! why. `data` here is a bare integer rather than an object, so nothing may assume
//! `data` is a map before looking.
//!
//! # Endpoint pointers, which are not citations
//!
//! `[предположение — указатели из FIXTURES-UNVERIFIED.md, восстановленные по
//! памяти об API]`
//!
//! | Purpose | Recalled path |
//! |---|---|
//! | instrument specs | `/api/v1/contract/detail?symbol=BTC_USDT` |
//! | funding rate + interval | `/api/v1/contract/funding_rate/BTC_USDT` |
//! | order book | `/api/v1/contract/depth/BTC_USDT` |
//! | settled history | `/api/v1/contract/funding_rate/history` |
//!
//! Note the symbol form: `BTC_USDT`, with an underscore, and embedded in the
//! **path** rather than passed as a query parameter on some of these. A path
//! segment is not percent-encoded by [`crate::http::Query`], so it must be built
//! from a symbol the venue itself published — never from a caller-supplied string.
//!
//! # The two hazards specific to this venue
//!
//! **Sizes are in contracts, and `contractSize` is what makes a number a
//! quantity.** MEXC futures order sizes are recalled as contract counts with a
//! per-symbol `contractSize` multiplier, which is a different
//! [`arb_types::ContractSize`] and a different [`arb_types::SizeUnit`] from
//! Bybit's one-coin-per-contract linear perps. `FIXTURES-UNVERIFIED.md` ranks a
//! wrong contract multiplier among the largest errors in this workspace: it is not
//! a rounding concern, it is a position that cannot be margined. Read the field,
//! do not infer it from a price that merely looks plausible.
//!
//! **`collectCycle` is the funding interval, and its unit is the whole game.**
//! It is recalled as **hours**. `arb-math` annualises with
//! `rate * SECONDS_PER_YEAR / interval`, so reading hours as minutes divides the
//! true APR by sixty and reading it as seconds multiplies it by thousands. Prove
//! the unit against a second source — the gap between the settled-history
//! timestamps is one — before writing a conversion, and set
//! [`arb_types::QualityFlags::FUNDING_INTERVAL_ASSUMED`] if the two sources are
//! reconciled by choosing rather than by agreeing.

/// MEXC's futures API host. `[факт, проверено 2026-08-25 — отвечает на
/// `/api/v1/contract/ping`]`
pub const BASE_URL: &str = "https://contract.mexc.com";

/// Placeholder. Generic over [`crate::http::Transport`] and
/// [`crate::clock::Clock`] like every adapter here, so the finished version needs
/// no edit outside this file.
#[derive(Debug)]
#[allow(dead_code)]
pub struct MexcAdapter<T, C> {
    transport: T,
    clock: C,
}
