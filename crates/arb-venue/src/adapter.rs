//! Stage 1 + 2 read-side traits: what a venue can tell us.
//!
//! Five methods. Each returns owned, already-validated `arb-types` values with
//! provenance attached, so a caller can never accidentally consume a raw wire
//! number.

use arb_num::{Bps, NumError};
use arb_types::{
    BookSnapshot, FundingSnapshot, Instrument, InstrumentKey, QualityFlags, Venue,
};

use crate::error::VenueError;

/// Everything the read side of the engine is allowed to ask a venue.
///
/// Deliberately synchronous and deliberately snapshot-shaped. A live WebSocket
/// adapter keeps its own book internally and answers [`ExchangeAdapter::book`]
/// from that state; the strategy never learns whether the number came from a
/// socket, a REST poll, or a file. That is the whole seam.
///
/// Deliberately *not* here:
/// - no `subscribe`/callback: streaming is an implementation detail of an
///   adapter, not a contract the strategy can see. Decision 012 keeps the
///   strategy pull-based so replay and live share one code path.
/// - no `now()`: an adapter must never be the clock. `observed_at` comes from
///   whoever built the snapshot.
pub trait ExchangeAdapter {
    /// Which venue this adapter speaks for. One adapter, one venue — a
    /// multi-venue adapter would let a symbol collision (`BTCUSDT` on five
    /// venues) route an order to the wrong exchange.
    fn venue(&self) -> Venue;

    /// Every instrument this adapter can serve, with its native symbol and
    /// contract spec. This is the only sanctioned place a native symbol
    /// string enters the system.
    fn instruments(&self) -> Result<Vec<Instrument>, VenueError>;

    /// Funding for a perp.
    ///
    /// Returns [`VenueError::Unsupported`] for spot: spot does not fund, and a
    /// zero rate would be indistinguishable from a real zero.
    fn funding(&self, key: &InstrumentKey) -> Result<FundingSnapshot, VenueError>;

    /// Top-`depth` levels of the book.
    ///
    /// `depth` is per side. An implementation that returns fewer levels than
    /// the venue holds MUST set [`QualityFlags::PARTIAL_BOOK`], because a
    /// truncated book makes a size look fillable when it is not.
    fn book(&self, key: &InstrumentKey, depth: usize) -> Result<BookSnapshot, VenueError>;

    /// Maker/taker fees for this instrument.
    fn fee_schedule(&self, key: &InstrumentKey) -> Result<FeeSchedule, VenueError>;
}

/// Maker and taker fee for one instrument, plus whether we actually know the
/// tier.
///
/// Fees are [`Bps`], not [`arb_num::Rate`], because every venue quotes them as
/// a ratio with at most a few significant digits and `Bps` is the narrower
/// type — a fee that does not fit in `Bps` is a fee we have misparsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSchedule {
    /// Fee paid when resting. May be negative on venues that rebate makers;
    /// nothing here assumes it is positive.
    pub maker: Bps,
    /// Fee paid when crossing the spread.
    pub taker: Bps,
    /// `false` means: nobody measured our account's tier, these numbers are the
    /// worst plausible tier, and the caller must set
    /// [`QualityFlags::FEE_TIER_ASSUMED`].
    ///
    /// Why the *worst* tier is the right default: our edge is twelve basis
    /// points and a two-leg round trip pays taker four times (open both legs,
    /// close both legs). Assuming a better tier than we have manufactures edge
    /// that does not exist and the engine trades on it. Assuming a worse tier
    /// than we have only makes us skip trades — we lose opportunity, not money.
    /// Those two errors are not symmetric, so the default is not symmetric.
    pub tier_known: bool,
}

impl FeeSchedule {
    /// A measured schedule: someone read the account's actual tier off the API.
    ///
    /// Nothing in this crate calls this. It exists so a live adapter has an
    /// obvious constructor and so the *absence* of calls to it is visible.
    pub const fn known(maker: Bps, taker: Bps) -> Self {
        Self {
            maker,
            taker,
            tier_known: true,
        }
    }

    /// A guessed schedule. `maker`/`taker` must be the worst plausible tier for
    /// this venue — see [`FeeSchedule::tier_known`].
    pub const fn assumed(maker: Bps, taker: Bps) -> Self {
        Self {
            maker,
            taker,
            tier_known: false,
        }
    }

    /// The quality flags this schedule forces onto any calculation using it.
    pub const fn flags(&self) -> QualityFlags {
        if self.tier_known {
            QualityFlags::NONE
        } else {
            QualityFlags::FEE_TIER_ASSUMED
        }
    }

    /// Taker cost of crossing on both legs of one arb *entry*, in bps.
    ///
    /// Two legs, both taken, so twice the taker fee. Closing the position pays
    /// it again; the caller decides whether the trade is round-tripped or held
    /// to funding, so this deliberately does not multiply by four.
    pub fn taker_both_legs(&self) -> Result<Bps, NumError> {
        self.taker.checked_mul_int(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assumed_tier_forces_the_flag() {
        // 0.02% maker / 0.055% taker, the shape Binance USD-M advertises.
        // 0.0002 ratio == 2 bps; Bps scale is 1e8 so raw == 2 * 1e8.
        let maker = Bps::parse_ratio("0.0002").unwrap();
        let taker = Bps::parse_ratio("0.00055").unwrap();
        assert_eq!(maker.raw(), 200_000_000);
        // 0.00055 / 0.0001 == 5.5 bps; 5.5 * 1e8 == 550_000_000.
        assert_eq!(taker.raw(), 550_000_000);

        let assumed = FeeSchedule::assumed(maker, taker);
        assert!(!assumed.tier_known);
        assert!(assumed.flags().contains(QualityFlags::FEE_TIER_ASSUMED));

        let known = FeeSchedule::known(maker, taker);
        assert!(known.tier_known);
        assert!(known.flags().is_empty());
    }

    #[test]
    fn taker_both_legs_doubles() {
        // 5.5 bps taker, two legs taken: 5.5 * 2 = 11 bps exactly.
        // 11 bps at scale 1e8 == 1_100_000_000.
        let s = FeeSchedule::assumed(Bps::ZERO, Bps::parse_ratio("0.00055").unwrap());
        assert_eq!(s.taker_both_legs().unwrap().raw(), 1_100_000_000);
    }

    #[test]
    fn negative_maker_rebate_survives() {
        // -0.0001 ratio == -1 bp. A rebate must not be clamped to zero: it is
        // real revenue on venues that pay it.
        let maker = Bps::parse_ratio("-0.0001").unwrap();
        assert_eq!(maker.raw(), -100_000_000);
        assert!(maker.is_negative());
        let s = FeeSchedule::known(maker, Bps::from_whole(5).unwrap());
        assert!(s.maker.is_negative());
    }
}
