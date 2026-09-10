#![forbid(unsafe_code)]
//! HTX (formerly Huobi) linear swap public market data. **Not implemented yet.**
//!
//! This file is a placeholder so that `venues/mod.rs` and the crate's re-exports
//! can name all eight venues at once, and so the engineer who writes this adapter
//! changes exactly one file. Follow [`crate::venues::bybit`], which is the
//! finished reference.
//!
//! # What is already measured
//!
//! `[факт, проверено 2026-08-25]` `api.hbdm.com` answers `GET /api/v1/timestamp`
//! with `{"status":"ok","ts":1787608382302}` from this environment.
//!
//! The envelope is keyed on a **string verdict** rather than a code:
//! `status: "ok"` versus `status: "error"` with `err_code`/`err_msg` beside it.
//! That is a fifth distinct success spelling across these eight venues. Check
//! `status == "ok"` with [`crate::venues::str_field`] — using
//! [`crate::venues::dec_field`] here would accept `"ok"` as text and compare it
//! against nothing useful, which is the exact failure `str_field`'s documentation
//! describes.
//!
//! # Endpoint pointers, which are not citations
//!
//! `[предположение — указатели из FIXTURES-UNVERIFIED.md, восстановленные по
//! памяти об API]`
//!
//! | Purpose | Recalled path |
//! |---|---|
//! | instrument specs | `/linear-swap-api/v1/swap_contract_info?contract_code=BTC-USDT` |
//! | funding rate | `/linear-swap-api/v1/swap_funding_rate?contract_code=BTC-USDT` |
//! | order book | `/linear-swap-api/v1/swap_ex_market_depth?contract_code=BTC-USDT&type=step0` |
//!
//! `FIXTURES-UNVERIFIED.md` flags this whole path family as recalled, including
//! the `linear-swap-api` prefix itself and the separation between linear-swap,
//! coin-swap and dated futures. Verify the prefix before anything else: a wrong
//! prefix fails every request identically and looks like an outage rather than a
//! typo. Note also `BTC-USDT` with a **hyphen**, where MEXC and Gate use an
//! underscore and Bybit uses no separator at all.
//!
//! # The three hazards specific to this venue
//!
//! **`type=step0` is what an order book means here.** HTX's depth endpoint is
//! recalled as requiring an aggregation level, where `step0` is the unaggregated
//! book and every other step merges price levels. An aggregated ladder is not a
//! book: it understates the number of levels and overstates the size at each, so a
//! size validated against it can be unfillable. If the parameter is omitted and
//! the venue defaults to an aggregated step, the resulting snapshot is wrong in a
//! way no assertion about ordering would catch.
//!
//! **The funding interval must be derived, and the two timestamps look alike.**
//! HTX is recalled as publishing `funding_time` and `next_funding_time` rather
//! than an interval, so the interval is their difference. Two failure modes
//! follow: the pair may be the *current and next* settlement, or the *previous and
//! current*, and subtracting the wrong pairing still yields a plausible 8 hours.
//! Whichever it is, the interval is then *computed by us* from two venue numbers
//! and not published as a measurement — which is the situation
//! [`arb_types::QualityFlags::FUNDING_INTERVAL_ASSUMED`] exists for. Setting it is
//! not pessimism; a derived interval genuinely is an inference.
//!
//! **`contract_size` and the settlement asset decide the size unit.** HTX linear
//! swaps are recalled as contract-denominated with a per-contract size in the base
//! asset. See `decisions/019` for what [`arb_types::SizeUnit`] must record and
//! `FIXTURES-UNVERIFIED.md` for the magnitude of getting it wrong. Request only
//! the linear-USDT family and skip the coin-margined swaps entirely: they settle
//! in the base asset and take quote-denominated contracts, which is a different
//! notional formula, and admitting them on the strength of a shape that merely
//! parses is how a size ends up wrong by the price of Bitcoin.

/// HTX's derivatives API host. `[факт, проверено 2026-08-25 — отвечает на
/// `/api/v1/timestamp`]`
///
/// `api.hbdm.com` is the derivatives host and is not the spot host
/// (`api.huobi.pro`). The old Huobi name survives in the hostname; see
/// [`arb_types::Venue::parse`], which accepts `huobi` as an alias for the same
/// reason.
pub const BASE_URL: &str = "https://api.hbdm.com";

/// Placeholder. Generic over [`crate::http::Transport`] and
/// [`crate::clock::Clock`] like every adapter here, so the finished version needs
/// no edit outside this file.
#[derive(Debug)]
#[allow(dead_code)]
pub struct HtxAdapter<T, C> {
    transport: T,
    clock: C,
}
