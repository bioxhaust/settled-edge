#![forbid(unsafe_code)]

//! `arb-live` — the first code in this workspace that can produce a measured
//! number.
//!
//! # What this crate is for
//!
//! Every other crate is pure. `arb-venue` holds the [`arb_venue::ExchangeAdapter`]
//! seam and exactly one implementation, `FixtureAdapter`, which reads
//! hand-written files and stamps every value
//! [`arb_types::Provenance::SynthesizedUnverified`]. That crate *cannot* produce
//! `LiveMeasured`; a single constructor funnel and a test enforce it.
//!
//! This crate is where that changes. It fetches public market data over HTTPS
//! from real exchanges and produces snapshots whose provenance is
//! [`arb_types::Provenance::LiveMeasured`], because the numbers in them were
//! measured — they came off the venue's own endpoint at a known instant.
//!
//! The guard in `arb-venue` stays exactly as it was. Nothing there gained the
//! ability to claim a measurement; a new crate gained it, which is visible in a
//! dependency graph and in a manifest review.
//!
//! # Structure
//!
//! - [`http`] — one HTTPS GET, via the system `curl`. No dependency, no
//!   hand-rolled TLS. Also the [`http::Transport`] seam and the
//!   [`http::StubTransport`] that keeps every adapter test offline.
//! - [`json`] — a reader that keeps numbers as **text**, never as `f64`, so
//!   `arb-num`'s exactness survives the very first step.
//! - [`clock`] — the [`clock::Clock`] seam. `ExchangeAdapter` forbids an adapter
//!   from *being* the clock, so a live adapter is *given* one, and every test
//!   gets a stopped one.
//! - [`venues`] — one module per exchange, each implementing
//!   [`arb_venue::ExchangeAdapter`] against a `Transport` and a `Clock`.
//!
//! # Only public endpoints
//!
//! Nothing here authenticates. Books, funding rates and instrument specs are all
//! public, so this crate holds no API key, no secret, and no request signing, and
//! there is no code path by which a credential could reach it. Private endpoints
//! are a separate concern with a separate threat model and are deliberately not
//! begun here.
//!
//! The visible consequence is [`arb_venue::FeeSchedule::assumed`]: an account's
//! fee tier is only readable over an authenticated endpoint, so every adapter
//! here reports the venue's worst published tier and sets
//! [`arb_types::QualityFlags::FEE_TIER_ASSUMED`].
//!
//! # What a measured number still does not guarantee
//!
//! `LiveMeasured` says the number came from the venue, not that a trade at it
//! would fill. Staleness, clock skew, painted depth and crossed books are all
//! still possible and are still `arb-detect`'s job; this crate makes no claim
//! beyond provenance and the observation instant.

pub mod clock;
pub mod http;
pub mod json;
pub mod venues;

pub use clock::{Clock, FixedClock, SystemClock};
pub use http::{
    percent_encode, CurlTransport, HttpError, Query, StubTransport, Transport,
    DEFAULT_CONNECT_TIMEOUT_SECS, DEFAULT_MAX_BYTES, DEFAULT_TIMEOUT_SECS,
};
pub use json::{Json, JsonError, MAX_DEPTH};
pub use venues::bybit::BybitAdapter;
