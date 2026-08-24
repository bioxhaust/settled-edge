//! One leg of a candidate position, and the invariants it must satisfy before
//! any arithmetic touches it.
//!
//! Separate from [`crate::pair`] because the questions are different: this module
//! asks "is this leg internally consistent" (does the funding snapshot belong to
//! this instrument, does a spot leg wrongly carry a funding rate), while
//! `pair` asks "are these two legs the same economic exposure". Merging them
//! would put a per-leg key check in the same function as a cross-leg asset check,
//! and the two fail for unrelated reasons.
//!
//! No I/O here. [`LegInput::gather`] calls `arb-venue` trait methods, which is
//! where all I/O in the workspace lives; this crate never opens a file or a
//! socket and never reads a clock.

use arb_types::{
    BookSnapshot, BorrowSnapshot, DataQuality, FundingSnapshot, Instrument, InstrumentKey,
    MarketKind, Provenance, RateHistory, Venue, VenueKey,
};
use arb_venue::{ExchangeAdapter, FeeSchedule, VenueError};

use crate::error::ScanError;

/// Everything one venue can tell us about one instrument, checked for internal
/// consistency.
///
/// Not `Copy`: [`BookSnapshot`] owns its level vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegInput {
    /// The instrument, with its native symbol and contract spec.
    pub instrument: Instrument,
    /// Funding, present exactly when the instrument is a perp.
    ///
    /// `Option` here is not a hedge against a missing value — it is the fact that
    /// spot does not fund. A `Rate::ZERO` in its place would be
    /// indistinguishable from a venue that really pays nothing this interval, and
    /// a spot leg credited with zero funding ranks identically to one whose rate
    /// we simply failed to fetch.
    pub funding: Option<FundingSnapshot>,
    /// Top-of-book ladder, as deep as was requested.
    pub book: BookSnapshot,
    /// Maker/taker fees and whether the tier was measured.
    pub fees: FeeSchedule,
    /// Where the fee numbers came from. [`FeeSchedule`] has nowhere to carry
    /// provenance, so it travels beside it — see [`crate::fees`].
    pub fee_quality: DataQuality,
    /// Cost of borrowing the base asset in order to sell it short, present only
    /// when this leg is a *short spot* leg.
    ///
    /// `None` on a leg that is not short spot is correct and costs nothing. `None`
    /// on a leg that *is* short spot is the open hole `OPEN.md` A1 describes: the
    /// scanner raises [`QualityFlags::BORROW_COST_UNKNOWN`], which blocks the row,
    /// rather than pricing a reverse carry at a borrow cost of zero. Margin borrow
    /// annualises into the tens of percent against an edge of twelve basis points,
    /// so the omission is not a rounding error — it can invert the sign.
    ///
    /// Which side is short is decided by [`crate::scan::price_pair`], not here, so
    /// the field is populated on any spot leg that *might* end up short.
    pub borrow: Option<BorrowSnapshot>,
    /// Settled funding rates for this instrument, used to judge whether a
    /// divergence has any history of persisting.
    ///
    /// Empty or absent raises [`QualityFlags::RATE_HISTORY_MISSING`], which
    /// deliberately warns rather than blocks: an unmeasured *probability of
    /// persistence* makes the APR conditional, while an unmeasured *cost* makes it
    /// wrong. Only the second may not be ranked.
    pub history: Option<RateHistory>,
}

impl LegInput {
    /// Assemble a leg, refusing an internally inconsistent one.
    ///
    /// Three checks, each of which is a confidently wrong number if omitted:
    ///
    /// - a perp with no funding snapshot: there is nothing to earn and nothing to
    ///   pay, so the row would rank as a pure fee loss or, worse, as free money
    ///   if a caller substituted zero;
    /// - a spot leg carrying funding: a cash-and-carry would be credited with the
    ///   perp leg's funding twice;
    /// - a funding snapshot or book belonging to a *different* instrument: the
    ///   commonest real form of this is a symbol-map bug (`BTCUSDT` exists on all
    ///   eight venues), and it prices one venue's carry with another's rate while
    ///   every number on the row stays plausible.
    pub fn new(
        instrument: Instrument,
        funding: Option<FundingSnapshot>,
        book: BookSnapshot,
        fees: FeeSchedule,
        fee_quality: DataQuality,
    ) -> Result<Self, ScanError> {
        Self::assemble(instrument, funding, book, fees, fee_quality, None, None)
    }

    /// Assemble a leg with the reverse-carry cost inputs `new` defaults to absent.
    ///
    /// Kept separate from [`LegInput::new`] so the common perp-leg call site is
    /// not forced to thread two `None`s, while the spot-leg call site that has a
    /// borrow quote and a rate history has a total constructor to pass them to.
    /// Both share the same internal validation.
    #[allow(clippy::too_many_arguments)]
    pub fn with_carry_inputs(
        instrument: Instrument,
        funding: Option<FundingSnapshot>,
        book: BookSnapshot,
        fees: FeeSchedule,
        fee_quality: DataQuality,
        borrow: Option<BorrowSnapshot>,
        history: Option<RateHistory>,
    ) -> Result<Self, ScanError> {
        Self::assemble(instrument, funding, book, fees, fee_quality, borrow, history)
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        instrument: Instrument,
        funding: Option<FundingSnapshot>,
        book: BookSnapshot,
        fees: FeeSchedule,
        fee_quality: DataQuality,
        borrow: Option<BorrowSnapshot>,
        history: Option<RateHistory>,
    ) -> Result<Self, ScanError> {
        let vkey = instrument.vkey;
        match (instrument.spec.kind.is_perp(), &funding) {
            (true, None) => return Err(ScanError::MissingFunding(vkey)),
            (false, Some(_)) => return Err(ScanError::UnexpectedFunding(vkey)),
            _ => {}
        }
        if let Some(f) = &funding {
            // Whole-key comparison, venue included. The commonest real form of
            // this bug is a symbol map that resolves `BTCUSDT` on the wrong
            // exchange, and that differs only in the venue field.
            if f.vkey != vkey {
                return Err(ScanError::FundingKeyMismatch(vkey));
            }
        }
        if book.vkey != vkey {
            return Err(ScanError::BookKeyMismatch(vkey));
        }
        if let Some(b) = &borrow {
            // The borrow quote must be for this venue's this instrument, and for
            // the base asset — the same class of mismatch as funding, with the
            // same silent-mispricing consequence.
            if b.vkey != vkey {
                return Err(ScanError::BorrowKeyMismatch(vkey));
            }
            if b.asset != vkey.key.base {
                return Err(ScanError::BorrowAssetMismatch(vkey));
            }
        }
        if let Some(h) = &history {
            if h.vkey != vkey {
                return Err(ScanError::RateHistoryKeyMismatch(vkey));
            }
        }
        Ok(Self {
            instrument,
            funding,
            book,
            fees,
            fee_quality,
            borrow,
            history,
        })
    }

    /// Ask an adapter for one instrument's data and assemble a leg.
    ///
    /// `fee_quality` is a parameter rather than derived, because
    /// [`ExchangeAdapter::fee_schedule`] returns a [`FeeSchedule`] with no
    /// provenance on it. `arb_venue::FixtureAdapter::fee_quality` produces the
    /// matching value for a fixture; a live adapter must produce its own. Passing
    /// [`DataQuality::MEASURED`] for numbers nobody measured is the one mistake
    /// this signature cannot prevent, and it is why the argument is not
    /// defaulted.
    ///
    /// The instrument is taken from the adapter's own catalogue rather than built
    /// here: the contract spec is the difference between a linear and an inverse
    /// notional, which is a ~68,000x error on BTC, and the venue is the only
    /// authority on it.
    pub fn gather<A: ExchangeAdapter + ?Sized>(
        adapter: &A,
        key: &InstrumentKey,
        depth: usize,
        fee_quality: DataQuality,
    ) -> Result<Self, ScanError> {
        let instruments = adapter.instruments()?;
        let instrument = instruments
            .into_iter()
            .find(|i| i.vkey.key == *key)
            .ok_or(ScanError::Venue(VenueError::NotFound))?;

        // Spot legitimately has no funding, and the adapter reports that as
        // `Unsupported` rather than as a zero rate. Distinguishing the two here
        // rather than swallowing every error keeps a genuine venue failure from
        // being read as "this is spot".
        let funding = if instrument.spec.kind.is_perp() {
            Some(adapter.funding(key)?)
        } else {
            None
        };

        let book = adapter.book(key, depth)?;
        let fees = adapter.fee_schedule(key)?;
        Self::new(instrument, funding, book, fees, fee_quality)
    }

    /// Venue plus instrument key.
    pub const fn vkey(&self) -> VenueKey {
        self.instrument.vkey
    }

    /// The instrument key alone, which is what joins two venues.
    pub const fn key(&self) -> InstrumentKey {
        self.instrument.vkey.key
    }

    /// Which exchange.
    pub const fn venue(&self) -> Venue {
        self.instrument.vkey.venue
    }

    /// Spot, linear perp, or inverse perp.
    pub const fn kind(&self) -> MarketKind {
        self.instrument.spec.kind
    }

    /// The funding snapshot, or the reason there is none.
    ///
    /// Callers that need funding should use this rather than matching on the
    /// field, so the error naming the venue is produced in one place.
    pub fn funding(&self) -> Result<&FundingSnapshot, ScanError> {
        self.funding
            .as_ref()
            .ok_or(ScanError::MissingFunding(self.vkey()))
    }

    /// Quality of every input on this leg, folded together.
    ///
    /// Book and funding contribute **flags and provenance**. The fee schedule
    /// contributes **flags only**, and that asymmetry is the most consequential
    /// line in this file.
    ///
    /// # Why the fee schedule's provenance is deliberately not folded
    ///
    /// [`DataQuality::combine`] keeps the *worst* provenance. No public endpoint on
    /// any venue reveals which fee tier an account is on, so an adapter that
    /// fetched a real book, a real ladder, a real funding rate and a real contract
    /// spec must still report the fee schedule as
    /// [`Provenance::SynthesizedUnverified`]. Folding that in marked the entire leg
    /// unverified, which sank every live row through
    /// [`DataQuality::is_trustworthy`] and left the scanner unable to rank a single
    /// thing it had actually measured.
    ///
    /// The two claims are not the same kind of claim:
    ///
    /// - `provenance` answers *where the market data came from*. Here: the venue.
    /// - The fee is a **cost-model input**, and the assumption made about it is
    ///   bounded and one-directional — adapters report the venue's worst published
    ///   tier, so the real fee can only be lower and `net_apr` can only be
    ///   understated. An assumption that can only make a row look worse than it is
    ///   is not the same risk as a price that was never measured.
    ///
    /// The caveat is not discarded. It travels as
    /// [`arb_types::QualityFlags::FEE_TIER_ASSUMED`], the flag that exists for
    /// exactly this, which [`arb_types::QualityFlags::BLOCKING`] deliberately
    /// excludes. The operator sees it on the row; the row is not silently deleted
    /// for it.
    ///
    /// This is the only place in the workspace that folds a `DataQuality`
    /// selectively. It is a decision about what *this* struct hands to `combine`,
    /// not a weakening of `combine` — which still cannot launder a synthesized
    /// provenance, and is still tested for it.
    pub fn quality(&self) -> DataQuality {
        let mut q = self.book.quality.with_flags(self.fee_quality.flags);
        if let Some(f) = &self.funding {
            q = q.combine(f.quality);
        }
        q
    }

    /// Where the fee schedule came from, unfolded.
    ///
    /// [`LegInput::quality`] drops this on purpose; a caller that needs to report
    /// the cost model's own standing — rather than the market data's — asks here.
    pub const fn fee_provenance(&self) -> Provenance {
        self.fee_quality.provenance
    }
}
