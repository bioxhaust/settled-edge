#![forbid(unsafe_code)]
//! Gate.io v4 USDT-settled futures public market data. **Not implemented yet.**
//!
//! This file is a placeholder so that `venues/mod.rs` and the crate's re-exports
//! can name all eight venues at once, and so the engineer who writes this adapter
//! changes exactly one file. Follow [`crate::venues::bybit`], which is the
//! finished reference.
//!
//! # What is already measured
//!
//! `[факт, проверено 2026-08-25]` `api.gateio.ws` answers
//! `GET /api/v4/spot/time` with `{"server_time":1787608356076}` from this
//! environment.
//!
//! That response is the venue's most important structural fact: **there is no
//! envelope.** No `code`, no `retCode`, no `data` wrapper — the payload is the
//! answer. Gate is recalled as signalling failure with a real HTTP status and a
//! `{"label":…,"message":…}` body, which means [`crate::venues::transport_error`]
//! already handles it and this adapter needs **no** `envelope()` function at all.
//!
//! Do not add one for symmetry with the other seven. An envelope check that looks
//! for a success code no venue sends would either always pass, which is
//! decoration, or always fail. Instead, assert the *absence* in a test: a 400 body
//! must arrive as [`arb_venue::VenueError::Transport`] and not be mistaken for an
//! empty result.
//!
//! Note also `server_time`: Gate spells its fields `snake_case` while the other
//! seven use `camelCase`. Nothing in this workspace depends on the convention,
//! but a field name copied across from another adapter will silently be absent —
//! and absence is exactly what [`crate::venues::opt_dec_field`] turns into
//! `None`, so a copied name becomes a missing value rather than an error.
//!
//! # Endpoint pointers, which are not citations
//!
//! `[предположение — указатели из FIXTURES-UNVERIFIED.md, восстановленные по
//! памяти об API]`
//!
//! | Purpose | Recalled path |
//! |---|---|
//! | instrument specs, funding, interval | `/api/v4/futures/usdt/contracts/BTC_USDT` |
//! | all contracts | `/api/v4/futures/usdt/contracts` |
//! | order book | `/api/v4/futures/usdt/order_book?contract=BTC_USDT&with_id=true` |
//!
//! The settlement asset is a **path segment**: `/futures/usdt/` and
//! `/futures/btc/` are different markets, so the linear/inverse distinction is
//! made by the URL rather than by a query parameter. Request only `usdt`.
//!
//! # The two hazards specific to this venue
//!
//! **`quanto_multiplier` is the contract size, and the ladder is in contracts.**
//! Gate's futures sizes are recalled as contract counts with a per-contract
//! multiplier, so a book level's size is *not* a base quantity until it has been
//! multiplied. `decisions/019` is the rule this must satisfy: a ladder is
//! denominated in base for linear and spot and in contracts for inverse, and
//! [`arb_types::SizeUnit`] is what records which. A multiplier read as 1 when it
//! is not is a size error of exactly that factor, in the direction that makes a
//! position look affordable.
//!
//! **`funding_interval` is recalled as seconds, and it sits in the same response
//! as the rate.** That is the convenient case — one round trip, no reconciliation
//! — which is precisely why it is worth checking rather than assuming: a value of
//! `28800` is 8 hours in seconds and 8 hours is also what `480` means in Bybit's
//! minutes. Both numbers are plausible, and only one unit is right per venue.
//! `[assumption]` If Gate publishes only one interval source, there is nothing to
//! cross-check it against, so state that in the module docs rather than implying a
//! second opinion exists.
//!
//! **`with_id=true` is what makes the book comparable to itself.** Without it the
//! recalled response carries no `id`, and [`arb_types::BookSnapshot`]'s `sequence`
//! would be `None` — which is legal but gives `arb-detect` nothing to check
//! continuity with. Ask for the id.

/// Gate.io's production API host. `[факт, проверено 2026-08-25 — отвечает на
/// `/api/v4/spot/time`]`
pub const BASE_URL: &str = "https://api.gateio.ws";

/// Placeholder. Generic over [`crate::http::Transport`] and
/// [`crate::clock::Clock`] like every adapter here, so the finished version needs
/// no edit outside this file.
#[derive(Debug)]
#[allow(dead_code)]
pub struct GateAdapter<T, C> {
    transport: T,
    clock: C,
}
