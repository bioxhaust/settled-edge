//! Order books: levels, order side, and depth access by intent.
//!
//! # Buying lifts asks
//!
//! [`BookSnapshot::taking_levels`] takes a [`Side`] and returns the levels a
//! taker actually consumes. This is the crate's answer to "depth taken from the
//! wrong side of the book": the wrong side is not merely discouraged, it is
//! inconvenient to reach. A VWAP computed from bids for a buy order looks
//! entirely reasonable and is optimistic by the full width of the spread —
//! typically 1-5 bps, against a claimed edge of 12.

use crate::instrument::VenueKey;
use crate::quality::{DataQuality, QualityFlags};
use crate::time::{LocalTs, WallTs};
use crate::TypeError;
use arb_num::{Price, Qty};
use core::fmt;

/// Side of an *order*.
///
/// Distinct from [`crate::PositionSide`], which describes a held perpetual
/// position. A `Buy` order may open a long or close a short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// Buy: pay the ask, consume ask levels.
    Buy,
    /// Sell: receive the bid, consume bid levels.
    Sell,
}

impl Side {
    /// The opposite side.
    #[inline]
    pub const fn opposite(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// Short tag for table output.
    pub const fn tag(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// One price level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level {
    /// Price of the level.
    pub price: Price,
    /// Size resting at that price, in contracts or base units per the
    /// instrument's [`crate::ContractSpec`].
    pub qty: Qty,
}

impl Level {
    /// Construct, rejecting non-positive values.
    ///
    /// A zero-size level is a *deletion* in most venues' delta protocols and must
    /// be applied by `arb-book`, never stored as depth. A zero price is always a
    /// parse error. Letting either into a book makes VWAP silently wrong.
    pub fn new(price: Price, qty: Qty) -> Result<Self, TypeError> {
        if !price.is_positive() || !qty.is_positive() {
            return Err(TypeError::NonPositiveLevel);
        }
        Ok(Self { price, qty })
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} x {}", self.price, self.qty)
    }
}

/// A point-in-time view of one instrument's book at one venue.
///
/// Invariants, enforced by [`BookSnapshot::new`]:
/// - `bids` strictly descending in price;
/// - `asks` strictly ascending in price;
/// - every level has positive price and positive size.
///
/// A crossed or empty book is *not* rejected — it is admitted with
/// [`QualityFlags::CROSSED_BOOK`] or [`QualityFlags::BOOK_EMPTY`] set. Rejecting
/// would hide the fault; flagging lets the scanner see the venue is broken and
/// skip it, which is the behaviour we want at 3am.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookSnapshot {
    /// Which instrument at which venue.
    pub vkey: VenueKey,
    /// Bid levels, strictly descending in price. Index 0 is the best bid.
    bids: Vec<Level>,
    /// Ask levels, strictly ascending in price. Index 0 is the best ask.
    asks: Vec<Level>,
    /// Venue update sequence number, where the venue supplies one.
    pub sequence: Option<u64>,
    /// The venue's own timestamp on this payload.
    pub exchange_ts: Option<WallTs>,
    /// When we observed it, on our clock.
    pub observed_at: LocalTs,
    /// Provenance and warnings.
    pub quality: DataQuality,
}

impl BookSnapshot {
    /// Build a snapshot, validating ordering and flagging structural faults.
    pub fn new(
        vkey: VenueKey,
        bids: Vec<Level>,
        asks: Vec<Level>,
        sequence: Option<u64>,
        exchange_ts: Option<WallTs>,
        observed_at: LocalTs,
        quality: DataQuality,
    ) -> Result<Self, TypeError> {
        for w in bids.windows(2) {
            if w[0].price <= w[1].price {
                return Err(TypeError::UnsortedLevels);
            }
        }
        for w in asks.windows(2) {
            if w[0].price >= w[1].price {
                return Err(TypeError::UnsortedLevels);
            }
        }
        for lvl in bids.iter().chain(asks.iter()) {
            if !lvl.price.is_positive() || !lvl.qty.is_positive() {
                return Err(TypeError::NonPositiveLevel);
            }
        }

        let mut flags = QualityFlags::NONE;
        if bids.is_empty() || asks.is_empty() {
            flags.insert(QualityFlags::BOOK_EMPTY);
        }
        if let (Some(b), Some(a)) = (bids.first(), asks.first()) {
            // Within one venue this is impossible in a healthy feed. Across
            // venues it is the opportunity — but that comparison happens in
            // arb-scan, over two separate books.
            if b.price >= a.price {
                flags.insert(QualityFlags::CROSSED_BOOK);
            }
        }

        Ok(Self {
            vkey,
            bids,
            asks,
            sequence,
            exchange_ts,
            observed_at,
            quality: quality.with_flags(flags),
        })
    }

    /// Bid levels, best first.
    #[inline]
    pub fn bids(&self) -> &[Level] {
        &self.bids
    }

    /// Ask levels, best first.
    #[inline]
    pub fn asks(&self) -> &[Level] {
        &self.asks
    }

    /// The levels a taker consumes when trading `side`.
    ///
    /// `Buy` returns asks. `Sell` returns bids. Use this instead of reaching for
    /// [`BookSnapshot::bids`] or [`BookSnapshot::asks`] whenever the side is a
    /// variable, which is nearly always.
    #[inline]
    pub fn taking_levels(&self, side: Side) -> &[Level] {
        match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        }
    }

    /// The price a taker of `side` pays for an infinitesimal size.
    #[inline]
    pub fn touch(&self, side: Side) -> Option<Price> {
        self.taking_levels(side).first().map(|l| l.price)
    }

    /// Best bid, if any.
    #[inline]
    pub fn best_bid(&self) -> Option<Level> {
        self.bids.first().copied()
    }

    /// Best ask, if any.
    #[inline]
    pub fn best_ask(&self) -> Option<Level> {
        self.asks.first().copied()
    }

    /// Mid price, as the unweighted average of the two touches.
    ///
    /// Provided for display only. Nothing tradeable should be priced off the
    /// mid: no order ever fills there. Sizing and PnL use
    /// [`BookSnapshot::taking_levels`].
    pub fn mid(&self) -> Option<Price> {
        let b = self.best_bid()?.price;
        let a = self.best_ask()?.price;
        let sum = b.checked_add(a).ok()?;
        sum.checked_div_int(2, arb_num::RoundMode::Trunc).ok()
    }

    /// True when either side is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() || self.asks.is_empty()
    }

    /// Total size resting on `side`, summed across all levels.
    pub fn total_depth(&self, side: Side) -> Result<Qty, arb_num::NumError> {
        self.taking_levels(side)
            .iter()
            .try_fold(Qty::ZERO, |acc, l| acc.checked_add(l.qty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::{InstrumentKey, MarketKind};
    use crate::venue::Venue;

    fn vkey() -> VenueKey {
        let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT")
            .unwrap_or(InstrumentKey::new(
                crate::Asset::default(),
                crate::Asset::default(),
                MarketKind::LinearPerp,
                crate::Asset::default(),
            ));
        VenueKey::new(Venue::Binance, key)
    }

    fn lvl(p: &str, q: &str) -> Level {
        Level {
            price: Price::parse(p).unwrap_or(Price::ONE),
            qty: Qty::parse(q).unwrap_or(Qty::ONE),
        }
    }

    fn book(bids: Vec<Level>, asks: Vec<Level>) -> Result<BookSnapshot, TypeError> {
        BookSnapshot::new(
            vkey(),
            bids,
            asks,
            Some(42),
            Some(WallTs::from_millis(1_700_000_000_000)),
            LocalTs::from_millis(1_700_000_000_040),
            DataQuality::SYNTHESIZED,
        )
    }

    fn healthy() -> BookSnapshot {
        // Bids descending, asks ascending, 1 bp wide at the touch.
        book(
            vec![lvl("68000", "1.5"), lvl("67999", "2.0"), lvl("67998", "3.0")],
            vec![lvl("68001", "1.0"), lvl("68002", "2.5"), lvl("68003", "4.0")],
        )
        .unwrap_or_else(|_| {
            // Unreachable for the literals above; keeps the helper total.
            BookSnapshot {
                vkey: vkey(),
                bids: Vec::new(),
                asks: Vec::new(),
                sequence: None,
                exchange_ts: None,
                observed_at: LocalTs::EPOCH,
                quality: DataQuality::SYNTHESIZED,
            }
        })
    }

    #[test]
    fn buy_takes_asks_and_sell_takes_bids() {
        // The central claim of this module.
        let b = healthy();
        assert_eq!(b.taking_levels(Side::Buy), b.asks());
        assert_eq!(b.taking_levels(Side::Sell), b.bids());
        assert_eq!(b.touch(Side::Buy), Some(Price::parse("68001").expect("ok")));
        assert_eq!(b.touch(Side::Sell), Some(Price::parse("68000").expect("ok")));
        // A buy must never be priced at or below the best bid.
        let buy_px = b.touch(Side::Buy).expect("present");
        let sell_px = b.touch(Side::Sell).expect("present");
        assert!(buy_px > sell_px, "taker buy price must exceed taker sell price");
    }

    #[test]
    fn best_levels_are_index_zero_on_both_sides() {
        let b = healthy();
        assert_eq!(b.best_bid().map(|l| l.price), Some(Price::parse("68000").expect("ok")));
        assert_eq!(b.best_ask().map(|l| l.price), Some(Price::parse("68001").expect("ok")));
        // And the best bid is the *highest* bid, not the first one supplied.
        assert!(b.bids()[0].price > b.bids()[1].price);
        assert!(b.asks()[0].price < b.asks()[1].price);
    }

    #[test]
    fn misordered_levels_are_rejected() {
        // Ascending bids: a venue payload transcribed without reversing.
        assert_eq!(
            book(vec![lvl("67999", "1"), lvl("68000", "1")], vec![lvl("68001", "1")]),
            Err(TypeError::UnsortedLevels)
        );
        // Descending asks.
        assert_eq!(
            book(vec![lvl("68000", "1")], vec![lvl("68002", "1"), lvl("68001", "1")]),
            Err(TypeError::UnsortedLevels)
        );
        // Duplicate price on one side is also misordered: it is not strict.
        assert_eq!(
            book(vec![lvl("68000", "1"), lvl("68000", "1")], vec![lvl("68001", "1")]),
            Err(TypeError::UnsortedLevels)
        );
    }

    #[test]
    fn non_positive_levels_are_rejected() {
        assert_eq!(
            book(vec![Level { price: Price::ZERO, qty: Qty::ONE }], vec![lvl("1", "1")]),
            Err(TypeError::NonPositiveLevel)
        );
        // Zero size is a deletion instruction, not depth.
        assert_eq!(
            book(vec![Level { price: Price::ONE, qty: Qty::ZERO }], vec![lvl("2", "1")]),
            Err(TypeError::NonPositiveLevel)
        );
        assert_eq!(
            Level::new(Price::ONE, Qty::ZERO),
            Err(TypeError::NonPositiveLevel)
        );
        assert!(Level::new(Price::ONE, Qty::ONE).is_ok());
    }

    #[test]
    fn crossed_book_is_flagged_not_rejected() {
        // Best bid above best ask: impossible within one venue.
        let b = book(vec![lvl("68005", "1")], vec![lvl("68001", "1")]).expect("admitted");
        assert!(b.quality.flags.contains(QualityFlags::CROSSED_BOOK));
        assert!(b.quality.flags.is_blocking());
        // Equal touch also counts as crossed (locked book).
        let locked = book(vec![lvl("68001", "1")], vec![lvl("68001", "1")]).expect("admitted");
        assert!(locked.quality.flags.contains(QualityFlags::CROSSED_BOOK));
        // A healthy book carries neither.
        assert!(!healthy().quality.flags.contains(QualityFlags::CROSSED_BOOK));
    }

    #[test]
    fn empty_side_is_flagged() {
        let b = book(vec![], vec![lvl("68001", "1")]).expect("admitted");
        assert!(b.quality.flags.contains(QualityFlags::BOOK_EMPTY));
        assert!(b.is_empty());
        assert_eq!(b.touch(Side::Sell), None);
        assert_eq!(b.mid(), None);
        // One-sided books must not be silently treated as crossed.
        assert!(!b.quality.flags.contains(QualityFlags::CROSSED_BOOK));
    }

    #[test]
    fn provenance_survives_construction() {
        // Rule 7: the synthesized stamp must not be lost by the constructor.
        let b = healthy();
        assert_eq!(b.quality.provenance, crate::Provenance::SynthesizedUnverified);
        assert!(!b.quality.is_tradeable());
    }

    #[test]
    fn total_depth_sums_the_right_side() {
        let b = healthy();
        // Asks: 1.0 + 2.5 + 4.0 = 7.5. Bids: 1.5 + 2.0 + 3.0 = 6.5. By hand.
        assert_eq!(
            b.total_depth(Side::Buy).expect("ok"),
            Qty::parse("7.5").expect("ok")
        );
        assert_eq!(
            b.total_depth(Side::Sell).expect("ok"),
            Qty::parse("6.5").expect("ok")
        );
    }

    #[test]
    fn mid_is_the_average_of_the_touches() {
        // (68000 + 68001)/2 = 68000.5
        assert_eq!(healthy().mid(), Some(Price::parse("68000.5").expect("ok")));
    }

    #[test]
    fn sides_are_involutive() {
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Side::Buy.opposite().opposite(), Side::Buy);
    }
}
