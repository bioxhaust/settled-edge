#![forbid(unsafe_code)]
//! `arb-num` — fixed-point money arithmetic. The foundation of the workspace.
//!
//! # Why this crate exists
//!
//! Our only product claim is that the number on screen equals the money that
//! lands in the account. Our edge is ~12 basis points. IEEE-754 `f64` has 53
//! bits of mantissa, so `0.1 + 0.2 != 0.3`, and repeated multiply/divide on
//! prices and rates accumulates error exactly in the fourth decimal place of a
//! percentage — which is where 12 bps lives. Therefore:
//!
//! **There is no `f64` anywhere in this workspace for a price, size, rate, fee,
//! or money amount.** Not as an intermediate, not "just for display".
//!
//! # Representation
//!
//! Five newtypes, all integer-backed:
//!
//! | Type    | Backing | Scale | Meaning                                    |
//! |---------|---------|-------|--------------------------------------------|
//! | [`Price`] | `i128` | 1e12 | quote units per 1 base unit                |
//! | [`Qty`]   | `i128` | 1e12 | an amount of something (base units, contracts) |
//! | [`Money`] | `i128` | 1e12 | an amount of quote/settle currency         |
//! | [`Rate`]  | `i128` | 1e12 | a dimensionless ratio (0.0001 == 1 bp)     |
//! | [`Bps`]   | `i64`  | 1e8  | basis points, fractional (1 bp == 1e8 raw) |
//!
//! ## Why scale 1e12
//!
//! [decision 001] `i128` at 1e12 holds prices from ~1e-12 to ~1.7e26 with
//! 12 decimal places. BTC at 1e5 and SHIB at 8.12e-6 both keep full exchange
//! precision. Funding rates quoted to 8 decimals keep 4 spare digits, which
//! matters because APR extrapolation multiplies a funding rate by up to 8760.
//!
//! Known limit: assets quoted below ~1e-9 (PEPE-class) retain only 3-4
//! significant figures. Venues quote those as `1000PEPE`; see `OPEN.md`.
//!
//! ## Why [`Bps`] and [`Rate`] share a raw value
//!
//! [decision 002] `Bps` is scale 1e8 and `Rate` is scale 1e12. One basis point
//! is `1e-4` as a ratio. So `1 bp` is `1 * 1e8 = 100_000_000` raw Bps, and
//! `0.0001 * 1e12 = 100_000_000` raw Rate. **The raw integers are identical.**
//! Conversion is a widening cast with zero arithmetic, so the single most
//! common unit-confusion bug in this domain — a stray factor of 10_000 between
//! percent, bps, and ratio — cannot be introduced by a rounding mistake here.
//! It can still be introduced by calling the wrong constructor, which is why
//! [`Bps::from_whole`] and [`Bps::from_ratio_str`] are named so differently.
//!
//! # Invariants this crate guarantees
//!
//! 1. No operator overloading. `+`, `-`, `*`, `/` are **not** implemented for
//!    these types. [decision 003] Every operation that can overflow, divide by
//!    zero, or lose precision returns [`Result`], and a trait like
//!    `std::ops::Add` cannot. `Neg` would panic on `i128::MIN`, which rule 4
//!    forbids. Call sites are more verbose and every failure is visible.
//! 2. No panics, no `unwrap`, no `expect`, no `unreachable!` in this crate.
//! 3. Every lossy operation takes an explicit [`RoundMode`]. There is no
//!    default rounding, because "who eats the sub-tick remainder" is an
//!    economic decision, not a numerics detail. Costs round
//!    [`RoundMode::AwayFromZero`], revenues round [`RoundMode::Trunc`].
//! 4. `parse` refuses to silently truncate. A string carrying more precision
//!    than scale 1e12 can hold returns [`NumError::PrecisionLoss`] rather than
//!    a slightly wrong number. A rejected quote is cheap; a wrong one is not.
//! 5. Round-trip: for any `v`, `parse(&format(v)) == v`.

mod decimal;
mod error;
mod muldiv;
mod round;
mod units;

pub use decimal::{
    format_scaled, format_scaled_places, parse_scaled, parse_scaled_rounded, pow10_i128,
};
pub use error::NumError;
pub use muldiv::mul_div;
pub use round::RoundMode;
pub use units::{Bps, Money, Price, Qty, Rate, BPS_SCALE, BPS_SCALE_DIGITS, SCALE, SCALE_DIGITS};
