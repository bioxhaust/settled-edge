#![forbid(unsafe_code)]
//! Binance USD-M futures public market data. **Not implemented yet.**
//!
//! This file is a placeholder so that `venues/mod.rs` and the crate's re-exports
//! can name all eight venues at once, and so the engineer who writes this adapter
//! changes exactly one file. Follow [`crate::venues::bybit`], which is the
//! finished reference.
//!
//! # What is already measured
//!
//! `[факт, проверено 2026-08-25]` `fapi.binance.com` answers
//! `GET /fapi/v1/time` with `{"serverTime":1787608352752}` from this environment.
//!
//! `[факт, проверено 2026-08-25]` The **spot** host `api.binance.com` does not:
//! it answers 403 through the only fetch path available here. `decisions/027`
//! already records `exchangeInfo` as content-blocked. So a spot leg on Binance
//! is not measurable from here today, and this adapter is USD-M only until it is.
//!
//! # Endpoint pointers, which are not citations
//!
//! `[предположение — указатели из FIXTURES-UNVERIFIED.md, восстановленные по
//! памяти об API]` Expect a wrong path or a wrong field spelling; a 404 is
//! information, not a blocker. Measure, then write down what answered.
//!
//! | Purpose | Recalled path |
//! |---|---|
//! | instrument filters | `/fapi/v1/exchangeInfo` |
//! | mark, index, funding | `/fapi/v1/premiumIndex` |
//! | funding interval | `/fapi/v1/fundingInfo` |
//! | order book | `/fapi/v1/depth` |
//!
//! # The two hazards specific to this venue
//!
//! **`lastFundingRate` is not the last funding rate.** It is recalled as the
//! *estimate for the next* settlement. If that is wrong, every Binance rate here
//! is one interval stale and
//! [`arb_types::QualityFlags::FUNDING_RATE_HISTORICAL`] is the flag that says so.
//! Resolve it by measurement, not by reading the field name.
//!
//! **`fundingInfo` is recalled as listing only symbols that deviate from the
//! 8-hour default**, so a symbol's *absence* is the assertion of 8 hours rather
//! than a missing datum. An adapter that treats absence as an error refuses most
//! of the venue; one that treats it as 8 hours without setting
//! [`arb_types::QualityFlags::FUNDING_INTERVAL_ASSUMED`] is claiming a
//! measurement it does not have. `arb-math` annualises with
//! `rate * SECONDS_PER_YEAR / interval`, so this field is an 8x error on the APR,
//! not a rounding concern.

/// Binance USD-M futures host. `[факт, проверено 2026-08-25 — отвечает на
/// `/fapi/v1/time`]`
pub const BASE_URL: &str = "https://fapi.binance.com";

/// Placeholder. Generic over [`crate::http::Transport`] and
/// [`crate::clock::Clock`] like every adapter here, so the finished version needs
/// no edit outside this file.
#[derive(Debug)]
#[allow(dead_code)]
pub struct BinanceAdapter<T, C> {
    transport: T,
    clock: C,
}
