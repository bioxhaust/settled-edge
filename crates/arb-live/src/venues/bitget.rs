#![forbid(unsafe_code)]
//! Bitget v2 mix (USDT-M futures) public market data. **Not implemented yet.**
//!
//! This file is a placeholder so that `venues/mod.rs` and the crate's re-exports
//! can name all eight venues at once, and so the engineer who writes this adapter
//! changes exactly one file. Follow [`crate::venues::bybit`], which is the
//! finished reference.
//!
//! # What is already measured
//!
//! `[факт, проверено 2026-08-25]` `api.bitget.com` answers
//! `GET /api/v2/public/time` with
//! `{"code":"00000","msg":"success","requestTime":1787608354022,"data":{"serverTime":"1787608354022"}}`
//! from this environment.
//!
//! Two things are already visible in that one response and both are load-bearing:
//! success is the string **`"00000"`**, not `"0"` and not a bare `0`; and
//! `serverTime` is **quoted** while `requestTime` beside it is bare. So the
//! envelope check cannot be copied from Bybit or OKX by changing one literal, and
//! every numeric field needs [`crate::venues::int_field`] rather than an
//! assumption about quoting.
//!
//! As everywhere, a non-success code arrives with HTTP 200. An unchecked envelope
//! hands the caller an empty `data`, which reads as "no liquidity" — a plausible
//! market state, and therefore the most dangerous possible failure.
//!
//! # Endpoint pointers, which are not citations
//!
//! `[предположение — указатели из FIXTURES-UNVERIFIED.md, восстановленные по
//! памяти об API]`
//!
//! | Purpose | Recalled path |
//! |---|---|
//! | instrument specs | `/api/v2/mix/market/contracts?productType=USDT-FUTURES` |
//! | funding rate | `/api/v2/mix/market/current-fund-rate` |
//! | funding interval / next time | `/api/v2/mix/market/funding-time` |
//! | mark, index, ticker | `/api/v2/mix/market/ticker` |
//! | order book | `/api/v2/mix/market/orderbook` |
//!
//! # The two hazards specific to this venue
//!
//! **`productType` is not optional and it is not cosmetic.** `USDT-FUTURES`,
//! `COIN-FUTURES` and `USDC-FUTURES` are different product families with
//! different settlement assets and different contract semantics. Requesting the
//! wrong one returns a well-formed answer about instruments this adapter must not
//! claim to price. Filter to USDT-M linear perpetuals and skip the rest, the way
//! Bybit's adapter skips every `contractType` that is not `LinearPerpetual`.
//!
//! **The funding interval lives in a different endpoint from the funding rate.**
//! That is two round trips whose answers must be about the same symbol, and a
//! mismatch is not detectable from the interval alone — so echo-check the symbol
//! in both responses the way `parse_funding` does in the Bybit adapter. If the
//! interval endpoint answers in hours while the rate endpoint implies otherwise,
//! choosing between them is an assumption and
//! [`arb_types::QualityFlags::FUNDING_INTERVAL_ASSUMED`] must say so.

/// Bitget's production API host. `[факт, проверено 2026-08-25 — отвечает на
/// `/api/v2/public/time`]`
pub const BASE_URL: &str = "https://api.bitget.com";

/// Placeholder. Generic over [`crate::http::Transport`] and
/// [`crate::clock::Clock`] like every adapter here, so the finished version needs
/// no edit outside this file.
#[derive(Debug)]
#[allow(dead_code)]
pub struct BitgetAdapter<T, C> {
    transport: T,
    clock: C,
}
