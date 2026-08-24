//! Data quality and provenance — the machinery behind hard rule 7.
//!
//! A number on screen is only as good as its worst input. This module makes
//! that arithmetic automatic: [`DataQuality::combine`] unions warning flags and
//! keeps the *least* trustworthy [`Provenance`], so a result computed from one
//! measured price and one invented one is stamped invented. Nobody has to
//! remember to propagate it, which is the only reason it will still be correct
//! in three months.

use core::fmt;

/// Where a datum came from, ordered from most to least trustworthy.
///
/// The ordering is the point: [`Provenance::worst`] is `max`, so combining
/// provenances can only ever move downward in trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Provenance {
    /// Read from a live venue API response during this process's lifetime.
    #[default]
    LiveMeasured,
    /// Replayed from a fixture that was itself captured from a live response.
    /// Trustworthy as to *shape*; possibly stale as to *value*.
    RecordedFixture,
    /// Written by a human from memory of an API's shape, never validated
    /// against the live endpoint.
    ///
    /// Every fixture currently in this repository is this. See
    /// `FIXTURES-UNVERIFIED.md`. Anything derived from such data must not be
    /// presented as a fact about the market, and the CLI marks it accordingly.
    SynthesizedUnverified,
}

impl Provenance {
    /// The less trustworthy of two provenances.
    #[inline]
    pub fn worst(self, other: Self) -> Self {
        if self >= other {
            self
        } else {
            other
        }
    }

    /// Short tag for table output.
    pub const fn tag(self) -> &'static str {
        match self {
            Provenance::LiveMeasured => "LIVE",
            Provenance::RecordedFixture => "REC",
            Provenance::SynthesizedUnverified => "SYNTH",
        }
    }
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// A set of warnings attached to a datum.
///
/// Hand-rolled bitflags: hard rule 1 forbids the `bitflags` crate, and a `u32`
/// with associated constants is no worse for a set this small.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct QualityFlags(u32);

impl QualityFlags {
    /// No warnings.
    pub const NONE: Self = Self(0);

    /// Observation is older than the configured freshness budget.
    pub const STALE: Self = Self(1 << 0);
    /// A gap in the venue's update sequence numbers: we missed messages, so the
    /// book may not reflect reality.
    pub const SEQ_GAP: Self = Self(1 << 1);
    /// Venue clock and our clock disagree by more than the configured budget.
    pub const CLOCK_SKEW: Self = Self(1 << 2);
    /// Best bid is at or above best ask within a single venue's book. Legal
    /// across venues, impossible within one — so this means bad data.
    pub const CROSSED_BOOK: Self = Self(1 << 3);
    /// Total visible depth is small relative to the size being priced.
    pub const THIN_DEPTH: Self = Self(1 << 4);
    /// Depth looks fabricated — see `arb-detect` for the specific signature.
    pub const PAINTED_DEPTH: Self = Self(1 << 5);
    /// Fewer levels than requested were available; depth beyond the last level
    /// is unknown rather than absent.
    pub const PARTIAL_BOOK: Self = Self(1 << 6);
    /// The book had no levels on at least one side.
    pub const BOOK_EMPTY: Self = Self(1 << 7);
    /// The requested size exhausted all visible depth: the fill price is a
    /// lower bound on cost, not an estimate of it.
    pub const SIZE_EXCEEDS_DEPTH: Self = Self(1 << 8);
    /// The funding interval was assumed rather than read from the venue.
    pub const FUNDING_INTERVAL_ASSUMED: Self = Self(1 << 9);
    /// The fee tier was assumed rather than read from the authenticated
    /// account endpoint.
    pub const FEE_TIER_ASSUMED: Self = Self(1 << 10);
    /// A required price (mark, index) was missing from the venue payload.
    pub const PRICE_UNAVAILABLE: Self = Self(1 << 11);
    /// Funding rate is the last settled rate, not the rate that will apply at
    /// the next settlement. Sign and magnitude may both differ.
    pub const FUNDING_RATE_HISTORICAL: Self = Self(1 << 12);
    /// This leg borrows an asset it does not own — a short spot leg — and the
    /// borrow rate is **not known**.
    ///
    /// Blocking, unlike the other two `_ASSUMED` flags. Those degrade a number
    /// that is still the right number; this one means a whole cost term is
    /// missing from it, and the term is large enough to invert the sign: a CEX
    /// USDT borrow rate in stress passes 50% APR against a best-row net of
    /// 26.79%. A row that silently omits it is not imprecise, it is wrong in the
    /// direction that loses money. See `decisions/023`.
    pub const BORROW_COST_UNKNOWN: Self = Self(1 << 13);
    /// No history of past settled funding rates accompanies this snapshot, so
    /// nothing can be said about whether the rate will persist.
    ///
    /// Not blocking. The row's arithmetic is correct as a conditional — "this is
    /// the return *if* the rate holds" — and that conditional is honestly useful.
    /// It becomes dishonest only when presented as a forecast, which is the
    /// report's job to prevent, not this flag's. `arb-scan` scores it.
    pub const RATE_HISTORY_MISSING: Self = Self(1 << 14);

    /// Flags that disqualify an opportunity from being acted on.
    ///
    /// Chosen as the set where the *number itself is wrong*, as opposed to
    /// merely imprecise. `THIN_DEPTH` and `PARTIAL_BOOK` are excluded: they make
    /// an estimate conservative, not false. `FEE_TIER_ASSUMED` and
    /// `FUNDING_INTERVAL_ASSUMED` are excluded because they are the normal
    /// state before an account is connected — they degrade confidence, and
    /// `arb-scan` scores them, but they do not falsify the row.
    pub const BLOCKING: Self = Self(
        Self::STALE.0
            | Self::SEQ_GAP.0
            | Self::CROSSED_BOOK.0
            | Self::PAINTED_DEPTH.0
            | Self::BOOK_EMPTY.0
            | Self::SIZE_EXCEEDS_DEPTH.0
            | Self::PRICE_UNAVAILABLE.0
            // See the flag's own docs: a missing cost term is not imprecision.
            | Self::BORROW_COST_UNKNOWN.0,
    );

    /// All named flags, with their labels, in bit order.
    pub const NAMED: [(Self, &'static str); 15] = [
        (Self::STALE, "STALE"),
        (Self::SEQ_GAP, "SEQ_GAP"),
        (Self::CLOCK_SKEW, "CLOCK_SKEW"),
        (Self::CROSSED_BOOK, "CROSSED"),
        (Self::THIN_DEPTH, "THIN"),
        (Self::PAINTED_DEPTH, "PAINTED"),
        (Self::PARTIAL_BOOK, "PARTIAL"),
        (Self::BOOK_EMPTY, "EMPTY"),
        (Self::SIZE_EXCEEDS_DEPTH, "OVER_DEPTH"),
        (Self::FUNDING_INTERVAL_ASSUMED, "IVL_ASSUMED"),
        (Self::FEE_TIER_ASSUMED, "FEE_ASSUMED"),
        (Self::PRICE_UNAVAILABLE, "NO_PRICE"),
        (Self::FUNDING_RATE_HISTORICAL, "RATE_HIST"),
        (Self::BORROW_COST_UNKNOWN, "BORROW_UNKNOWN"),
        (Self::RATE_HISTORY_MISSING, "NO_HISTORY"),
    ];

    /// The raw bit pattern.
    #[inline]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// True when no flags are set.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when *every* flag in `other` is set in `self`.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// True when *any* flag in `other` is set in `self`.
    #[inline]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// Set union.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Add flags in place.
    #[inline]
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Remove flags in place.
    #[inline]
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// True when at least one [`QualityFlags::BLOCKING`] flag is present.
    #[inline]
    pub const fn is_blocking(self) -> bool {
        self.intersects(Self::BLOCKING)
    }
}

impl fmt::Display for QualityFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("-");
        }
        let mut first = true;
        for (flag, label) in Self::NAMED {
            if self.contains(flag) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(label)?;
                first = false;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for QualityFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QualityFlags({self})")
    }
}

/// Warnings plus provenance, travelling together with every datum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataQuality {
    /// Warnings raised about this datum.
    pub flags: QualityFlags,
    /// How the datum was obtained.
    pub provenance: Provenance,
}

impl DataQuality {
    /// Clean and measured — the only combination that may be presented as fact.
    pub const MEASURED: Self = Self {
        flags: QualityFlags::NONE,
        provenance: Provenance::LiveMeasured,
    };

    /// Clean in shape but invented in value. The correct starting quality for
    /// every fixture in this repository.
    pub const SYNTHESIZED: Self = Self {
        flags: QualityFlags::NONE,
        provenance: Provenance::SynthesizedUnverified,
    };

    /// Construct explicitly.
    pub const fn new(flags: QualityFlags, provenance: Provenance) -> Self {
        Self { flags, provenance }
    }

    /// Merge the quality of two inputs to a calculation.
    ///
    /// Flags union; provenance takes the worse. This is the whole mechanism of
    /// rule 7 — a spread computed from a measured Binance book and a
    /// synthesized Bybit book is synthesized.
    #[inline]
    pub fn combine(self, other: Self) -> Self {
        Self {
            flags: self.flags.union(other.flags),
            provenance: self.provenance.worst(other.provenance),
        }
    }

    /// Add flags, returning the updated quality.
    #[inline]
    pub fn with_flags(mut self, flags: QualityFlags) -> Self {
        self.flags.insert(flags);
        self
    }

    /// True only for clean, live-measured data.
    ///
    /// Deliberately strict: fixture-derived numbers are never "trustworthy" in
    /// the sense this project means, however clean their flags.
    #[inline]
    pub fn is_trustworthy(self) -> bool {
        self.flags.is_empty() && self.provenance == Provenance::LiveMeasured
    }

    /// True when nothing *falsifies* the datum, whatever its provenance.
    ///
    /// The gate `arb-scan` uses to decide whether a row belongs in the table at
    /// all. Fixture-derived rows are rankable — that is the whole point of the
    /// replay harness — but see [`DataQuality::is_tradeable`] for why being in
    /// the table does not make a row actionable.
    #[inline]
    pub fn is_rankable(self) -> bool {
        !self.flags.is_blocking()
    }

    /// True when an order may be sent against this datum.
    ///
    /// Stricter than [`DataQuality::is_rankable`] by one term, and the extra
    /// term is the one that matters: **synthesized data is never tradeable.**
    /// A number invented from memory of an API's shape may be ranked, displayed,
    /// and reasoned about, but routing real money against it is the specific
    /// catastrophe hard rule 7 exists to prevent.
    ///
    /// [`Provenance::RecordedFixture`] *is* tradeable-in-principle: it came from
    /// a real response, so its shape is real, and the replay harness needs to be
    /// able to exercise execution paths. Staleness of a recording is caught by
    /// [`QualityFlags::STALE`], not here.
    #[inline]
    pub fn is_tradeable(self) -> bool {
        !self.flags.is_blocking() && self.provenance != Provenance::SynthesizedUnverified
    }
}

impl Default for DataQuality {
    /// Defaults to the *pessimistic* value, not the optimistic one: a datum
    /// nobody bothered to describe is not assumed to be measured.
    fn default() -> Self {
        Self::SYNTHESIZED
    }
}

impl fmt::Display for DataQuality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provenance.tag(), self.flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_worst_is_monotone_downward() {
        use Provenance::*;
        assert_eq!(LiveMeasured.worst(RecordedFixture), RecordedFixture);
        assert_eq!(RecordedFixture.worst(SynthesizedUnverified), SynthesizedUnverified);
        assert_eq!(LiveMeasured.worst(SynthesizedUnverified), SynthesizedUnverified);
        assert_eq!(LiveMeasured.worst(LiveMeasured), LiveMeasured);
        // Commutative, so combination order cannot change a verdict.
        for a in [LiveMeasured, RecordedFixture, SynthesizedUnverified] {
            for b in [LiveMeasured, RecordedFixture, SynthesizedUnverified] {
                assert_eq!(a.worst(b), b.worst(a));
            }
        }
    }

    #[test]
    fn synthesized_provenance_contaminates_any_calculation() {
        // The core guarantee of rule 7.
        let measured = DataQuality::MEASURED;
        let invented = DataQuality::SYNTHESIZED;
        let combined = measured.combine(invented);
        assert_eq!(combined.provenance, Provenance::SynthesizedUnverified);
        assert!(!combined.is_trustworthy());
        // ...and it cannot be laundered by combining again with clean data.
        assert_eq!(
            combined.combine(measured).provenance,
            Provenance::SynthesizedUnverified
        );
    }

    #[test]
    fn default_quality_is_pessimistic() {
        // A struct built with `..Default::default()` must not claim to be live.
        assert_eq!(DataQuality::default().provenance, Provenance::SynthesizedUnverified);
        assert!(!DataQuality::default().is_trustworthy());
    }

    #[test]
    fn flags_union_across_combine() {
        let a = DataQuality::new(QualityFlags::STALE, Provenance::LiveMeasured);
        let b = DataQuality::new(QualityFlags::SEQ_GAP, Provenance::LiveMeasured);
        let c = a.combine(b);
        assert!(c.flags.contains(QualityFlags::STALE));
        assert!(c.flags.contains(QualityFlags::SEQ_GAP));
        assert!(!c.flags.contains(QualityFlags::CLOCK_SKEW));
    }

    #[test]
    fn contains_requires_all_bits_intersects_requires_any() {
        let both = QualityFlags::STALE.union(QualityFlags::SEQ_GAP);
        assert!(both.contains(QualityFlags::STALE));
        assert!(both.contains(both));
        assert!(!QualityFlags::STALE.contains(both));
        assert!(QualityFlags::STALE.intersects(both));
        assert!(!QualityFlags::CLOCK_SKEW.intersects(QualityFlags::STALE));
    }

    #[test]
    fn blocking_set_is_exactly_as_documented() {
        for f in [
            QualityFlags::STALE,
            QualityFlags::SEQ_GAP,
            QualityFlags::CROSSED_BOOK,
            QualityFlags::PAINTED_DEPTH,
            QualityFlags::BOOK_EMPTY,
            QualityFlags::SIZE_EXCEEDS_DEPTH,
            QualityFlags::PRICE_UNAVAILABLE,
        ] {
            assert!(f.is_blocking(), "{f} must block");
        }
        // Imprecise but not false — must not block.
        for f in [
            QualityFlags::THIN_DEPTH,
            QualityFlags::PARTIAL_BOOK,
            QualityFlags::CLOCK_SKEW,
            QualityFlags::FEE_TIER_ASSUMED,
            QualityFlags::FUNDING_INTERVAL_ASSUMED,
            QualityFlags::FUNDING_RATE_HISTORICAL,
        ] {
            assert!(!f.is_blocking(), "{f} must not block");
        }
        assert!(!QualityFlags::NONE.is_blocking());
    }

    #[test]
    fn ranking_trading_and_trusting_are_three_different_questions() {
        // Fixture data: rankable, never tradeable, never presentable as fact.
        let synth = DataQuality::SYNTHESIZED;
        assert!(synth.is_rankable(), "fixture rows must still reach the table");
        assert!(!synth.is_tradeable(), "invented data must never route an order");
        assert!(!synth.is_trustworthy());

        // Live but stale: measured, yet must not be traded.
        let stale = DataQuality::new(QualityFlags::STALE, Provenance::LiveMeasured);
        assert!(!stale.is_rankable());
        assert!(!stale.is_tradeable());
        assert!(!stale.is_trustworthy());

        // A clean recording: tradeable, but not a fact about the market now.
        let rec = DataQuality::new(QualityFlags::NONE, Provenance::RecordedFixture);
        assert!(rec.is_rankable());
        assert!(rec.is_tradeable());
        assert!(!rec.is_trustworthy());

        // Only clean live data is all three.
        let live = DataQuality::MEASURED;
        assert!(live.is_rankable() && live.is_tradeable() && live.is_trustworthy());

        // Non-blocking flags degrade confidence without disqualifying the row.
        let assumed = DataQuality::new(QualityFlags::FEE_TIER_ASSUMED, Provenance::LiveMeasured);
        assert!(assumed.is_rankable() && assumed.is_tradeable());
        assert!(!assumed.is_trustworthy());
    }

    #[test]
    fn insert_and_remove_are_inverses() {
        let mut f = QualityFlags::NONE;
        f.insert(QualityFlags::STALE);
        assert!(f.contains(QualityFlags::STALE));
        f.remove(QualityFlags::STALE);
        assert!(f.is_empty());
    }

    #[test]
    fn every_named_flag_is_a_distinct_single_bit() {
        let mut seen = 0u32;
        for (flag, label) in QualityFlags::NAMED {
            assert_eq!(flag.bits().count_ones(), 1, "{label} is not a single bit");
            assert_eq!(seen & flag.bits(), 0, "{label} duplicates another flag");
            seen |= flag.bits();
        }
        // Deliberately a literal and not `NAMED.len()`, which would make the
        // assertion vacuous. Adding a flag has to force a reader here.
        assert_eq!(seen.count_ones(), 15);
        assert_eq!(QualityFlags::NAMED.len(), 15);
    }

    /// `BORROW_COST_UNKNOWN` blocks and `RATE_HISTORY_MISSING` does not, and the
    /// difference is the whole reason there are two flags. A missing cost term
    /// inverts the sign of the number; a missing history leaves the number
    /// correct as a conditional. See `decisions/023`.
    #[test]
    fn a_missing_cost_term_blocks_but_a_missing_history_does_not() {
        assert!(QualityFlags::BLOCKING.contains(QualityFlags::BORROW_COST_UNKNOWN));
        assert!(!QualityFlags::BLOCKING.contains(QualityFlags::RATE_HISTORY_MISSING));
        // And the two `_ASSUMED` flags stay non-blocking, so the new blocking
        // flag cannot be read as "anything unmeasured blocks".
        assert!(!QualityFlags::BLOCKING.contains(QualityFlags::FEE_TIER_ASSUMED));
        assert!(!QualityFlags::BLOCKING.contains(QualityFlags::FUNDING_INTERVAL_ASSUMED));

        let borrowed = DataQuality::new(
            QualityFlags::BORROW_COST_UNKNOWN,
            Provenance::LiveMeasured,
        );
        assert!(!borrowed.is_tradeable(), "a row missing a cost term is not tradeable");
    }

    #[test]
    fn display_lists_flags_and_marks_empty() {
        assert_eq!(QualityFlags::NONE.to_string(), "-");
        assert_eq!(QualityFlags::STALE.to_string(), "STALE");
        assert_eq!(
            QualityFlags::STALE.union(QualityFlags::SEQ_GAP).to_string(),
            "STALE|SEQ_GAP"
        );
        assert_eq!(DataQuality::SYNTHESIZED.to_string(), "SYNTH/-");
        assert_eq!(
            DataQuality::new(QualityFlags::STALE, Provenance::LiveMeasured).to_string(),
            "LIVE/STALE"
        );
    }
}
