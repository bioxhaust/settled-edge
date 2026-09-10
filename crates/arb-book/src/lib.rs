#![forbid(unsafe_code)]
//! Order book state: ladders, delta application, sequence tracking, resync.
//!
//! Pure state machine. No I/O, no clocks, no thresholds. Feed it snapshots and
//! deltas, ask it for a [`arb_types::BookSnapshot`].
//!
//! # Three things decide whether this crate is correct
//!
//! **1. Depth updates are ABSOLUTE size per level, and zero means delete.**
//! `apply(price, qty)` sets the level to `qty`; it never adds to it. Every
//! venue we plan to connect to (Binance `depthUpdate`, Bybit `orderbook.N`,
//! OKX `books`, Bitget, Mexc, Gate, Kucoin, Htx) is *believed* to publish
//! absolute per-level sizes, but that belief came from memory of API docs and
//! **was never checked against a live socket** — mark it `[assumption]`. A venue
//! that actually sends true increments would corrupt us silently: sizes would
//! drift wrong by a few percent, top-of-book would still look plausible, and
//! the twelve basis points we trade on would be noise. The only defence is
//! measurement: replay a recorded stream against a REST snapshot and compare.
//! [`semantics`] is that comparison, plus the cheaper evidence available from
//! the stream alone; it counts and never judges, and
//! `arb_detect::assess_depth_semantics` turns the counts into a verdict. The
//! assumption is now *measurable*, which is not the same as measured: until
//! someone runs a recorded feed through it, the flag on every book built from a
//! fixture stays [`arb_types::Provenance::SynthesizedUnverified`].

//!
//! **2. A sequence gap means the ladder is UNKNOWN, not merely late.**
//! There is no "catch up" from a gap. A missed delta may have deleted the level
//! we are about to price against, and nothing in a later delta will tell us so.
//! On a gap this crate stops mutating, buffers, and waits for a fresh snapshot.
//! It never interpolates and never guesses.
//!
//! **3. This crate reports FACTS about sequences and owns NO thresholds.**
//! [`SeqTracker`] counts accepts, duplicates, gaps, resets and out-of-order
//! messages. It does not decide how many gaps per minute make a venue
//! untradeable — `arb-detect` owns that. The only constants here are the ones
//! needed to *classify* a single observation (see [`sequence::RESET_BACKWARD_JUMP`])
//! and to bound memory (see [`ladder::MAX_LEVELS`], [`state::MAX_PENDING`]).
//! Each is justified where it is defined.

pub mod error;
pub mod ladder;
pub mod semantics;
pub mod sequence;
pub mod state;

pub use error::BookError;
pub use ladder::{Ladder, MAX_INPUT_LEVELS, MAX_LEVELS};
pub use semantics::{AuditScope, DeltaVocabulary, SnapshotAudit};
pub use sequence::{
    SeqCursor, SeqFact, SeqInput, SeqScheme, SeqTracker, RESET_BACKWARD_JUMP, RESET_LOW_ID,
};
pub use state::{BookState, Delta, PendingDelta, MAX_PENDING};
