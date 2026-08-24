//! Every refusal this crate can make, expressed as a type.
//!
//! Two enums, and the split between them is load-bearing.
//!
//! [`ScanError`] is returned from a `Result`: something was impossible and there
//! is no row to show. [`crate::report::ExclusionReason`] is a *field on a row we
//! did produce*: the row is visible, its flags are visible, and it is kept out
//! of the ranking. Collapsing the two would force the caller to choose between
//! hiding a bad row (the table then looks complete when it is not) and ranking
//! it (a stale number then looks tradeable). Both of those are the failures this
//! crate exists to prevent, so the distinction lives in the types.
//!
//! Nothing here is a `String`. A caller deciding whether to retry a venue, widen
//! a size, or fix a symbol map has to distinguish "the book was empty" from "the
//! two legs are not the same economic exposure", and a message forces it to
//! match on prose.

use core::fmt;

use arb_detect::DetectError;
use arb_math::MathError;
use arb_num::NumError;
use arb_types::{Side, TypeError, VenueKey};
use arb_venue::VenueError;

/// Why two instruments may not be treated as the two legs of one position.
///
/// Each variant is a hedge that would not be a hedge. They are separate
/// variants, not one `NotPairable`, because the remedies differ: a base mismatch
/// is a bug in whoever built the pair list, a linear-against-inverse attempt is
/// a bug in the model, and a same-venue pair is a misunderstanding of what an
/// arbitrage is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairRefusal {
    /// The same instrument on the same venue. Buying and selling one instrument
    /// is not a position, it is a pair of fees.
    SameInstrument,
    /// Different base assets. There is no exposure being hedged.
    BaseMismatch,
    /// Different quote assets — `BTC/USD` against `BTC/USDT` is the commonest
    /// real instance. The two prices differ by the USDT basis, which is itself
    /// an unhedged position we would be taking without saying so.
    QuoteMismatch,
    /// One leg is a linear perp and the other an inverse perp.
    ///
    /// Their notional formulas are different functions:
    /// [`arb_types::ContractSize::BasePerContract`] scales with price,
    /// [`arb_types::ContractSize::QuotePerContract`] does not. An inverse
    /// contract's P&L is non-linear in price, so equal notionals at entry drift
    /// apart the moment the price moves. Pairing them silently produces a
    /// position that looks flat on a screen and is not flat in the account.
    LinearAgainstInverse,
    /// Two perps on the same venue. Funding is charged by the venue against your
    /// net position, so a long and a short of equal size on one venue collect
    /// nothing at all — there is no differential to earn. This is one position
    /// with extra fees, not an arbitrage.
    SameVenuePerpPerp,
    /// Two spot legs on the same venue: buying and selling the same asset at the
    /// same place, which nets to holding nothing and paying twice.
    SameVenueSpotSpot,
}

impl PairRefusal {
    /// One-line reason, for a report column.
    pub const fn tag(self) -> &'static str {
        match self {
            PairRefusal::SameInstrument => "same instrument",
            PairRefusal::BaseMismatch => "base assets differ",
            PairRefusal::QuoteMismatch => "quote assets differ",
            PairRefusal::LinearAgainstInverse => "linear against inverse",
            PairRefusal::SameVenuePerpPerp => "both perp legs on one venue",
            PairRefusal::SameVenueSpotSpot => "both spot legs on one venue",
        }
    }
}

impl fmt::Display for PairRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PairRefusal::SameInstrument => {
                f.write_str("the two legs are the same instrument on the same venue")
            }
            PairRefusal::BaseMismatch => f.write_str("base assets differ: no exposure is hedged"),
            PairRefusal::QuoteMismatch => f.write_str(
                "quote assets differ: the residual is an unhedged position in the quote basis",
            ),
            PairRefusal::LinearAgainstInverse => f.write_str(
                "linear against inverse: the notional formulas differ and the hedge is not flat",
            ),
            PairRefusal::SameVenuePerpPerp => {
                f.write_str("both perp legs on one venue: there is no funding differential")
            }
            PairRefusal::SameVenueSpotSpot => {
                f.write_str("both spot legs on one venue: the position nets to nothing")
            }
        }
    }
}

impl std::error::Error for PairRefusal {}

/// Everything that can stop `arb-scan` from producing an answer.
///
/// `Clone` but not `Copy`: [`VenueError::Io`] owns a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    /// The two legs may not be paired. See [`PairRefusal`].
    Pair(PairRefusal),
    /// An economics calculation refused.
    Math(MathError),
    /// Fixed-point arithmetic refused.
    Num(NumError),
    /// A type invariant was violated.
    Type(TypeError),
    /// A venue or fixture could not answer.
    Venue(VenueError),
    /// A data-quality detector could not reach a verdict.
    Detect(DetectError),
    /// A perp leg arrived with no funding snapshot. A missing rate is not a rate
    /// of zero: zero is a real and informative value, and substituting it would
    /// turn "we do not know what this venue pays" into "this venue pays
    /// nothing", which ranks the row instead of hiding it.
    MissingFunding(VenueKey),
    /// A spot leg arrived carrying a funding snapshot. Spot does not fund;
    /// something upstream has attached the wrong instrument's data, and using it
    /// would credit a cash-and-carry with funding it never receives.
    UnexpectedFunding(VenueKey),
    /// The funding snapshot belongs to an instrument other than the leg's.
    /// Silently accepting it would price one venue's carry with another's rate.
    FundingKeyMismatch(VenueKey),
    /// The book belongs to an instrument other than the leg's.
    BookKeyMismatch(VenueKey),
    /// The side of the book we have to take has no levels, so there is no price
    /// at which our size could trade at all.
    NoLiquidity(VenueKey, Side),
    /// A configuration field cannot produce a meaningful answer. Carries a
    /// static description of which one, because the fix is to the config file
    /// and the operator needs to be told the field name.
    BadConfig(&'static str),
    /// Neither leg funds, so there is no funding carry to evaluate. Returned for
    /// a spot-spot pair, which is a legitimate [`crate::pair::LegConfig`] and a
    /// seam for a later stage, not a stage-1 opportunity.
    NoFundingLeg,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Pair(r) => write!(f, "cannot pair: {r}"),
            ScanError::Math(e) => write!(f, "economics: {e}"),
            ScanError::Num(e) => write!(f, "arithmetic: {e}"),
            ScanError::Type(e) => write!(f, "type: {e}"),
            ScanError::Venue(e) => write!(f, "venue: {e}"),
            ScanError::Detect(e) => write!(f, "data quality: {e}"),
            ScanError::MissingFunding(v) => {
                write!(f, "{} {} is a perp with no funding snapshot", v.venue.as_str(), v.key)
            }
            ScanError::UnexpectedFunding(v) => {
                write!(f, "{} {} is spot but carries funding", v.venue.as_str(), v.key)
            }
            ScanError::FundingKeyMismatch(v) => {
                write!(f, "{} {} funding snapshot is for another instrument", v.venue.as_str(), v.key)
            }
            ScanError::BookKeyMismatch(v) => {
                write!(f, "{} {} book is for another instrument", v.venue.as_str(), v.key)
            }
            ScanError::NoLiquidity(v, side) => write!(
                f,
                "{} {} has no {} levels to take",
                v.venue.as_str(),
                v.key,
                side.tag()
            ),
            ScanError::BadConfig(what) => write!(f, "configuration is unusable: {what}"),
            ScanError::NoFundingLeg => {
                f.write_str("neither leg funds, so there is no funding carry to evaluate")
            }
        }
    }
}

impl std::error::Error for ScanError {}

impl From<PairRefusal> for ScanError {
    fn from(e: PairRefusal) -> Self {
        ScanError::Pair(e)
    }
}

impl From<MathError> for ScanError {
    fn from(e: MathError) -> Self {
        ScanError::Math(e)
    }
}

impl From<NumError> for ScanError {
    fn from(e: NumError) -> Self {
        ScanError::Num(e)
    }
}

impl From<TypeError> for ScanError {
    fn from(e: TypeError) -> Self {
        ScanError::Type(e)
    }
}

impl From<VenueError> for ScanError {
    fn from(e: VenueError) -> Self {
        ScanError::Venue(e)
    }
}

impl From<DetectError> for ScanError {
    fn from(e: DetectError) -> Self {
        ScanError::Detect(e)
    }
}
