#![forbid(unsafe_code)]
//! KuCoin Futures public market data. **Not implemented yet.**
//!
//! This file is a placeholder so that `venues/mod.rs` and the crate's re-exports
//! can name all eight venues at once, and so the engineer who writes this adapter
//! changes exactly one file. Follow [`crate::venues::bybit`], which is the
//! finished reference.
//!
//! # What is already measured
//!
//! `[факт, проверено 2026-08-25]` `api-futures.kucoin.com` answers
//! `GET /api/v1/timestamp` with `{"code":"200000","data":1787608381492}` from this
//! environment.
//!
//! Success is the string **`"200000"`** — a fourth distinct spelling of "OK"
//! across these eight venues, after Bybit's bare `0`, OKX's `"0"` and Bitget's
//! `"00000"`. It resembles an HTTP status and is not one. As everywhere, a
//! non-success code arrives with HTTP 200, so an unchecked envelope produces an
//! empty result that reads as "no liquidity".
//!
//! # This adapter settles an open decision
//!
//! `decisions/021` fixed `native = "XBTUSDTM"` for KuCoin's BTC perpetual, on the
//! ground that KuCoin historically spells Bitcoin `XBT`, and recorded plainly that
//! the string was **recalled, not verified** `[предположение]`. It named the check:
//! *"подлежит проверке первым же запросом к `/api/v1/contracts/active`"*.
//!
//! This adapter makes that request. Whatever `/api/v1/contracts/active` answers is
//! authoritative, and the answer must be written back into `decisions/021` and
//! `FIXTURES-UNVERIFIED.md` — including, and especially, if it contradicts
//! `XBTUSDTM`. The general rule 021 states applies: where the brief and the venue
//! disagree about the venue's own naming, the venue is right. Resolve it in one
//! direction or the other; do not leave a fixture and an adapter disagreeing about
//! what the symbol is.
//!
//! # Endpoint pointers, which are not citations
//!
//! `[предположение — указатели из FIXTURES-UNVERIFIED.md, восстановленные по
//! памяти об API]`
//!
//! | Purpose | Recalled path |
//! |---|---|
//! | instrument specs, funding, interval | `/api/v1/contracts/active` |
//! | single contract | `/api/v1/contracts/XBTUSDTM` |
//! | order book, full | `/api/v1/level2/snapshot?symbol=…` |
//! | order book, top 20 | `/api/v1/level2/depth20?symbol=…` |
//!
//! # The three hazards specific to this venue
//!
//! **`multiplier` is the contract size and it is not 1.** KuCoin futures are
//! recalled as contract-denominated with a per-symbol multiplier, so a book level
//! is a contract count, not a base quantity. See `decisions/019` for what
//! [`arb_types::SizeUnit`] must then record, and `FIXTURES-UNVERIFIED.md` for why
//! a wrong multiplier is among the largest errors available here.
//!
//! **The minimum order is a contract count, not a quote notional.** `OPEN.md` E6
//! records this: `kucoin-btcusdtm-perp.fix` deliberately carries
//! `min_notional: None` because KuCoin gates on contracts instead — and
//! [`arb_types::ContractSpec`] has no field for a contract minimum, so passing
//! `None` records *the absence of a notional floor, not the absence of a floor*.
//! Whatever the venue publishes, do not launder a contract minimum into
//! `min_notional`: those are different quantities and the comparison in
//! `arb-scan` is against a notional.
//!
//! **`fundingRateGranularity` is recalled as milliseconds.** Not minutes, not
//! hours, not seconds — a fourth unit for the field whose misreading is worth an
//! 8x or 60x error in the annualised APR. `arb-math` divides
//! `SECONDS_PER_YEAR` by this quantity in seconds, so the conversion is the one
//! line in this adapter most worth a test that states the wrong answers it is
//! ruling out, the way Bybit's
//! `four_hundred_and_eighty_is_minutes_and_a_naive_reading_would_be_sixty_times_wrong`
//! does.

/// KuCoin's futures API host. `[факт, проверено 2026-08-25 — отвечает на
/// `/api/v1/timestamp`]`
///
/// Distinct from the spot host: KuCoin splits futures onto `api-futures` and spot
/// onto `api`, so a path copied between them reaches the wrong market rather than
/// failing.
pub const BASE_URL: &str = "https://api-futures.kucoin.com";

/// Placeholder. Generic over [`crate::http::Transport`] and
/// [`crate::clock::Clock`] like every adapter here, so the finished version needs
/// no edit outside this file.
#[derive(Debug)]
#[allow(dead_code)]
pub struct KucoinAdapter<T, C> {
    transport: T,
    clock: C,
}
