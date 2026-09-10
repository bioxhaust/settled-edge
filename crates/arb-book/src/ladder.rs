//! One side of an order book, kept sorted best-price-first.

use arb_num::{Price, Qty};
use arb_types::{Level, Side, TypeError};

use crate::error::BookError;

/// Hard cap on the number of levels we keep per side.
///
/// Why a cap at all: a full-depth stream can carry tens of thousands of dust
/// levels far away from the touch. We never price against them — every quote we
/// compute walks from the touch until our size is filled — but each insert into
/// a `Vec` past them costs a memmove of everything after it, and each side costs
/// unbounded memory times instruments times venues.
///
/// Why 4096: a `Level` is two `i128`s, so 4096 levels is 128 KiB per side, which
/// bounds an eight-venue book set at a few MiB. 4096 levels from the touch is
/// orders of magnitude more depth than any size we would send. `[assumption]`
/// the depth actually needed was NOT measured against a live venue; if a
/// strategy ever wants deeper, raise this and say why here.
pub const MAX_LEVELS: usize = 4096;

/// Refuse any single message carrying more levels than this.
///
/// No venue publishes 65 536 levels for a pair worth trading. A message that
/// large is corruption, a decoder bug, or someone probing us; sorting it would
/// stall the hot path for every other instrument. Note this is deliberately far
/// above [`MAX_LEVELS`]: a legitimate deep REST snapshot that we then truncate is
/// normal, so it must not be an error.
pub const MAX_INPUT_LEVELS: usize = 65_536;

/// One side of a book: levels sorted in the natural order for that side.
///
/// Bids descend (best = highest), asks ascend (best = lowest). The side is
/// stored so the ordering invariant cannot be applied to the wrong ladder — a
/// bug that produces a book that looks sorted and prices everything backwards.
#[derive(Debug)]
pub struct Ladder {
    side: Side,
    levels: Vec<Level>,
    /// Set when we discarded levels to stay inside [`MAX_LEVELS`]. Raises
    /// `PARTIAL_BOOK` on any snapshot assembled from this ladder: our depth is
    /// a lower bound, not the venue's book.
    truncated: bool,
}

impl Ladder {
    /// Empty ladder for `side`. `Side::Buy` holds bids, `Side::Sell` holds asks.
    pub fn new(side: Side) -> Self {
        Ladder {
            side,
            levels: Vec::new(),
            truncated: false,
        }
    }

    /// Which side's resting orders this ladder holds.
    pub fn side(&self) -> Side {
        self.side
    }

    /// Levels, best first.
    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    /// Number of levels held.
    pub fn len(&self) -> usize {
        self.levels.len()
    }

    /// True when no levels are held.
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// True when levels were dropped to respect [`MAX_LEVELS`].
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Drop every level. Also clears the truncation fact: an empty ladder is not
    /// a partial one.
    pub fn clear(&mut self) {
        self.levels.clear();
        self.truncated = false;
    }

    /// Best level on this side, if any.
    pub fn best(&self) -> Option<Level> {
        self.levels.first().cloned()
    }

    /// Size standing at exactly `price`, or `None` if this side does not hold it.
    ///
    /// A lookup, not a walk: `None` means "no level at this price", which is a
    /// different fact from `Some(Qty::ZERO)` — the latter cannot occur, because
    /// [`Self::apply`] removes a level rather than storing a zero in it. Callers
    /// that conflate the two turn "the venue never mentioned this price" into
    /// "the venue says this price is empty".
    pub fn qty_at(&self, price: Price) -> Option<Qty> {
        match self.position(price) {
            Ok(i) => self.levels.get(i).map(|l| l.qty),
            Err(_) => None,
        }
    }

    /// True when `price` is at least as good as `reference`, in this side's own
    /// sense of "good".
    ///
    /// For bids, better means higher; for asks, lower. Exposed because code that
    /// compares two prices on a ladder must not re-derive that rule with a bare
    /// `<=` — the [`Self::position`] comment below is about the same inversion,
    /// and getting it backwards there produced a book that quoted prices nobody
    /// offered.
    pub fn is_no_worse_than(&self, price: Price, reference: Price) -> bool {
        match self.side {
            Side::Buy => price >= reference,
            Side::Sell => price <= reference,
        }
    }

    /// Apply one absolute-size level update.
    ///
    /// `qty == 0` deletes the level (and deleting a level we do not hold is a
    /// no-op, which is normal: venues re-send deletes). `qty < 0` is an error.
    /// Otherwise insert-or-replace, keeping sort order.
    pub fn apply(&mut self, price: Price, qty: Qty) -> Result<(), BookError> {
        if !price.is_positive() {
            return Err(BookError::Type(TypeError::NonPositivePrice));
        }
        if qty.is_negative() {
            return Err(BookError::NegativeQty);
        }
        match (self.position(price), qty.is_zero()) {
            (Ok(i), true) => {
                // Guarded rather than indexed: `Ok(i)` from a binary search is
                // always in bounds, but library code does not get to rely on
                // "always" for a panic.
                if i < self.levels.len() {
                    self.levels.remove(i);
                }
            }
            // Delete of a level we do not hold. Nothing to do.
            (Err(_), true) => {}
            (Ok(i), false) => {
                if let Some(slot) = self.levels.get_mut(i) {
                    slot.qty = qty;
                }
            }
            (Err(i), false) => {
                let level = Level::new(price, qty)?;
                // `insert` panics for i > len; a binary search never returns
                // that, and the clamp makes it impossible instead of unlikely.
                let i = i.min(self.levels.len());
                self.levels.insert(i, level);
                if self.levels.len() > MAX_LEVELS {
                    self.truncate_to(MAX_LEVELS);
                }
            }
        }
        Ok(())
    }

    /// Full refresh. Sorts into this side's order and rejects duplicate prices.
    ///
    /// A zero size has no meaning in a full refresh — there is nothing to delete
    /// — so `Level`'s own positivity invariant is what validates the input.
    pub fn replace_all(&mut self, mut levels: Vec<Level>) -> Result<(), BookError> {
        if levels.len() > MAX_INPUT_LEVELS {
            return Err(BookError::LevelLimitExceeded);
        }
        for l in &levels {
            if !l.price.is_positive() {
                return Err(BookError::Type(TypeError::NonPositivePrice));
            }
            if !l.qty.is_positive() {
                return Err(BookError::Type(TypeError::NonPositiveLevel));
            }
        }
        match self.side {
            Side::Buy => levels.sort_by(|a, b| b.price.cmp(&a.price)),
            Side::Sell => levels.sort_by(|a, b| a.price.cmp(&b.price)),
        }
        // After sorting, the only way to break strict monotonicity is an equal
        // pair, and equal pairs are adjacent. Two sizes at one price means one
        // of them is wrong and we cannot know which, so we refuse the refresh
        // instead of picking a winner.
        for w in levels.windows(2) {
            if let (Some(a), Some(b)) = (w.first(), w.get(1)) {
                if a.price == b.price {
                    return Err(BookError::DuplicatePrice);
                }
            }
        }
        self.truncated = false;
        if levels.len() > MAX_LEVELS {
            levels.truncate(MAX_LEVELS);
            self.truncated = true;
        }
        self.levels = levels;
        Ok(())
    }

    /// Sum of sizes across every level held.
    pub fn total_qty(&self) -> Result<Qty, BookError> {
        let mut acc = Qty::ZERO;
        for l in &self.levels {
            acc = acc.checked_add(l.qty)?;
        }
        Ok(acc)
    }

    /// Keep at most `depth` levels, dropping the worst. Records the truncation.
    pub fn truncate_to(&mut self, depth: usize) {
        if depth < self.levels.len() {
            self.levels.truncate(depth);
            self.truncated = true;
        }
    }

    /// Index of `price`, or the index it would be inserted at.
    fn position(&self, price: Price) -> Result<usize, usize> {
        match self.side {
            // Bids are stored DESCENDING. `binary_search_by` requires the slice
            // to be sorted ascending with respect to the comparator it is given,
            // so for bids we invert the comparison: `price.cmp(&l.price)` instead
            // of `l.price.cmp(&price)`. Get this backwards and every probe on a
            // populated ladder returns `Err(0)`, which turns insert-or-replace
            // into "prepend a duplicate at the touch" — a book that looks full,
            // quotes a price nobody offered, and never errors.
            Side::Buy => self.levels.binary_search_by(|l| price.cmp(&l.price)),
            Side::Sell => self.levels.binary_search_by(|l| l.price.cmp(&price)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Price {
        Price::parse(s).unwrap()
    }
    fn q(s: &str) -> Qty {
        Qty::parse(s).unwrap()
    }
    fn lvl(price: &str, qty: &str) -> Level {
        Level::new(p(price), q(qty)).unwrap()
    }

    #[test]
    fn apply_then_delete_removes_the_level() {
        let mut l = Ladder::new(Side::Sell);
        l.apply(p("100.5"), q("1.5")).unwrap();
        assert_eq!(l.len(), 1);
        // 100.5 at scale 1e12 = 100_500_000_000_000
        assert_eq!(l.best().unwrap().price.raw(), 100_500_000_000_000);
        l.apply(p("100.5"), Qty::ZERO).unwrap();
        assert!(l.is_empty());
        assert!(l.best().is_none());
    }

    #[test]
    fn deleting_an_absent_level_is_a_no_op() {
        let mut l = Ladder::new(Side::Sell);
        l.apply(p("100"), q("1")).unwrap();
        l.apply(p("101"), Qty::ZERO).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l.best().unwrap().price.raw(), 100_000_000_000_000);
    }

    #[test]
    fn bids_stay_descending_after_out_of_order_inserts() {
        let mut l = Ladder::new(Side::Buy);
        for (price, qty) in [("98", "4"), ("99.5", "1"), ("99", "2.5"), ("97", "9")] {
            l.apply(p(price), q(qty)).unwrap();
        }
        let prices: Vec<i128> = l.levels().iter().map(|x| x.price.raw()).collect();
        assert_eq!(
            prices,
            vec![
                99_500_000_000_000,
                99_000_000_000_000,
                98_000_000_000_000,
                97_000_000_000_000
            ]
        );
    }

    #[test]
    fn asks_stay_ascending_after_out_of_order_inserts() {
        let mut l = Ladder::new(Side::Sell);
        for (price, qty) in [("101", "4"), ("100", "1"), ("100.5", "2.5"), ("99.25", "9")] {
            l.apply(p(price), q(qty)).unwrap();
        }
        let prices: Vec<i128> = l.levels().iter().map(|x| x.price.raw()).collect();
        assert_eq!(
            prices,
            vec![
                99_250_000_000_000,
                100_000_000_000_000,
                100_500_000_000_000,
                101_000_000_000_000
            ]
        );
    }

    #[test]
    fn duplicate_price_replaces_rather_than_duplicating() {
        let mut l = Ladder::new(Side::Buy);
        l.apply(p("99"), q("1")).unwrap();
        l.apply(p("98"), q("1")).unwrap();
        l.apply(p("99"), q("7.25")).unwrap();
        assert_eq!(l.len(), 2);
        // 7.25 at 1e12 = 7_250_000_000_000
        assert_eq!(l.best().unwrap().qty.raw(), 7_250_000_000_000);
    }

    #[test]
    fn negative_qty_is_an_error() {
        let mut l = Ladder::new(Side::Sell);
        let neg = Qty::from_raw(-1);
        assert!(matches!(
            l.apply(p("100"), neg),
            Err(BookError::NegativeQty)
        ));
        assert!(l.is_empty());
    }

    #[test]
    fn non_positive_price_is_an_error() {
        let mut l = Ladder::new(Side::Sell);
        assert!(matches!(
            l.apply(Price::ZERO, q("1")),
            Err(BookError::Type(_))
        ));
    }

    #[test]
    fn replace_all_sorts_both_sides() {
        let mut bids = Ladder::new(Side::Buy);
        bids.replace_all(vec![lvl("98", "1"), lvl("100", "2"), lvl("99", "3")])
            .unwrap();
        assert_eq!(bids.best().unwrap().price.raw(), 100_000_000_000_000);

        let mut asks = Ladder::new(Side::Sell);
        asks.replace_all(vec![lvl("98", "1"), lvl("100", "2"), lvl("99", "3")])
            .unwrap();
        assert_eq!(asks.best().unwrap().price.raw(), 98_000_000_000_000);
    }

    #[test]
    fn replace_all_rejects_duplicate_prices() {
        let mut l = Ladder::new(Side::Sell);
        let r = l.replace_all(vec![lvl("100", "1"), lvl("100", "2")]);
        assert!(matches!(r, Err(BookError::DuplicatePrice)));
        // Rejected refresh must not have installed anything.
        assert!(l.is_empty());
    }

    #[test]
    fn total_qty_sums_by_hand() {
        let mut l = Ladder::new(Side::Sell);
        l.replace_all(vec![lvl("100", "1.5"), lvl("100.5", "2"), lvl("101", "3")])
            .unwrap();
        // 1.5 + 2 + 3 = 6.5 exactly -> 6_500_000_000_000 at scale 1e12
        assert_eq!(l.total_qty().unwrap().raw(), 6_500_000_000_000);
    }

    #[test]
    fn truncate_to_records_the_truncation() {
        let mut l = Ladder::new(Side::Sell);
        l.replace_all(vec![lvl("100", "1"), lvl("101", "1"), lvl("102", "1")])
            .unwrap();
        assert!(!l.truncated());
        l.truncate_to(2);
        assert_eq!(l.len(), 2);
        assert!(l.truncated());
        // Truncating to more than we hold changes nothing.
        let mut m = Ladder::new(Side::Sell);
        m.replace_all(vec![lvl("100", "1")]).unwrap();
        m.truncate_to(50);
        assert!(!m.truncated());
    }

    #[test]
    fn clear_drops_levels_and_truncation() {
        let mut l = Ladder::new(Side::Buy);
        l.replace_all(vec![lvl("100", "1"), lvl("99", "1")]).unwrap();
        l.truncate_to(1);
        assert!(l.truncated());
        l.clear();
        assert!(l.is_empty());
        assert!(!l.truncated());
        assert_eq!(l.total_qty().unwrap().raw(), 0);
    }

    #[test]
    fn oversized_refresh_is_refused() {
        let mut l = Ladder::new(Side::Sell);
        // One level per tick from 1, so prices are unique and positive.
        let levels: Vec<Level> = (1..=(MAX_INPUT_LEVELS as i64 + 1))
            .map(|i| Level::new(Price::from_int(i).unwrap(), Qty::ONE).unwrap())
            .collect();
        assert!(matches!(
            l.replace_all(levels),
            Err(BookError::LevelLimitExceeded)
        ));
    }

    #[test]
    fn side_is_reported_back() {
        assert!(matches!(Ladder::new(Side::Buy).side(), Side::Buy));
        assert!(matches!(Ladder::new(Side::Sell).side(), Side::Sell));
    }
}
