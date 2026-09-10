//! Is this timestamp even in milliseconds?
//!
//! # The hole this closes
//!
//! `OPEN.md` D4: every venue timestamp in this workspace is *assumed* to be
//! Unix milliseconds. Nothing checks it. A venue publishing seconds, parsed as
//! millis, yields a `WallTs` around 1.7 × 10⁹ instead of 1.7 × 10¹² — a value
//! forty-six years after the epoch, i.e. 1970. Read as an age, the data is a
//! thousand times *fresher* than it is, and "fresher" is the direction that draws
//! an opportunity onto the screen.
//!
//! # Why no existing detector can see it
//!
//! Because every other timestamp check in this crate is a **difference**, and a
//! difference is unit-blind. `WallTs::apparent_skew_from` subtracts two stamps;
//! [`crate::staleness::age_of`] subtracts two stamps. If *both* operands are in
//! the wrong unit, the difference is wrong by the same factor and never looks
//! impossible — a seconds-valued feed compared against a seconds-valued clock
//! reports ages a thousand times too small, monotonically, forever, with no
//! reading anywhere that contradicts itself.
//!
//! Mixing units *is* visible, and painfully so, but not usefully:
//!
//! - seconds against our millisecond clock → `Suspicious(1_698_300_000_050 ms)`,
//!   a penalty of 30, score 70 — above `MIN_TRADEABLE`.
//! - micros against our millisecond clock →
//!   `ExchangeAhead(1_698_299_999_999_950 ms)`, capped at 40, score exactly 60 —
//!   `MIN_TRADEABLE`, and the gate is `>=`.
//!
//! `[fact, verified]` — both are pinned by
//! `arb_adversary::staleness`. A clock reading is capped precisely because a
//! clock reading cannot distinguish a bad clock from a slow network, and capping
//! it is right; the consequence is that no clock verdict can express "this
//! number is not a millisecond count". That is what this module is for, and it
//! reaches its verdict from **one absolute magnitude**, with no clock involved.
//!
//! # Why magnitude alone is enough
//!
//! The four candidate units are separated by factors of a thousand, while the
//! plausible range of real dates spans only a factor of about 2.8
//! ([`PLAUSIBLE_FROM_MILLIS`] to [`PLAUSIBLE_UNTIL_MILLIS`]). The bands
//! therefore do not touch: the widest gap between the top of one band and the
//! bottom of the next is a factor of ~361. A stamp cannot be ambiguous between
//! two units unless it is also not a plausible date in either, and that case has
//! its own answer, [`EpochUnit::Unrecognised`].
//!
//! This is the rare check that is both cheap and decisive, which is why it is a
//! comparison and not a heuristic. Contrast [`crate::painted`], where the
//! evidence is genuinely weak and the scoring stays deliberately unsure.

use arb_types::WallTs;

/// Earliest millisecond timestamp we will call plausible: 2017-01-01T00:00:00Z.
///
/// Why an era bound rather than "any positive number": the whole detection
/// depends on knowing roughly *when* now is. Without a lower bound, a
/// seconds-valued 1.7 × 10⁹ is a perfectly well-formed millisecond timestamp —
/// it just points at 1970 — and no amount of arithmetic objects.
///
/// Why 2017 specifically: it is comfortably before any exchange we connect to
/// published the endpoints we read, so no live message can legitimately fall
/// below it, and it is late enough that a seconds-valued stamp for any date from
/// 1970 to 2100 lands underneath it. Recorded fixtures are the only thing that
/// could be older, and a fixture stamped before 2017 would be describing a market
/// that no longer exists.
///
/// `[fact, verified]` — 1_483_228_800 seconds is 2017-01-01T00:00:00Z: from
/// 1970-01-01 to 2017-01-01 is 47 years containing 12 leap days (1972, 1976, …,
/// 2016), so 47 × 365 + 12 = 17_167 days, and 17_167 × 86_400 =
/// 1_483_228_800. Times 1000 for milliseconds.
///
/// Raising this is safe as long as it stays below the oldest fixture in the
/// workspace. Lowering it toward 4 × 10⁹ would break the band disjointness this
/// module depends on, and
/// `the_four_bands_do_not_touch_anywhere` is the test that would say so.
pub const PLAUSIBLE_FROM_MILLIS: i64 = 1_483_228_800_000;

/// Latest millisecond timestamp we will call plausible: 2100-01-01T00:00:00Z.
///
/// Why an upper bound at all: it is the half of the check that catches the
/// dangerous direction. Micro- and nanosecond stamps read as millis land in the
/// far future, and a future timestamp is what understates an age.
///
/// Why 2100: far enough out that nothing about this system will be running, and
/// low enough that a microsecond stamp for any date after 1970 exceeds it. This
/// is not a Y2K-style cliff — a real 2100 timestamp would be
/// [`EpochUnit::Unrecognised`], which is refused rather than misread.
pub const PLAUSIBLE_UNTIL_MILLIS: i64 = 4_102_444_800_000;

/// Lower bound of the seconds band: [`PLAUSIBLE_FROM_MILLIS`] in seconds.
const SECONDS_FROM: i64 = PLAUSIBLE_FROM_MILLIS / 1_000;
/// Upper bound of the seconds band, exclusive.
const SECONDS_UNTIL: i64 = PLAUSIBLE_UNTIL_MILLIS / 1_000;
/// Lower bound of the microseconds band.
const MICROS_FROM: i64 = PLAUSIBLE_FROM_MILLIS * 1_000;
/// Upper bound of the microseconds band, exclusive.
const MICROS_UNTIL: i64 = PLAUSIBLE_UNTIL_MILLIS * 1_000;
/// Lower bound of the nanoseconds band.
///
/// 1_483_228_800_000 × 10⁶ ≈ 1.483 × 10¹⁸, and the top of the band is
/// ≈ 4.102 × 10¹⁸, both inside `i64::MAX` ≈ 9.223 × 10¹⁸. Const evaluation
/// would refuse to compile these if they overflowed, so the bound is checked by
/// the compiler and not by this comment.
const NANOS_FROM: i64 = PLAUSIBLE_FROM_MILLIS * 1_000_000;
/// Upper bound of the nanoseconds band, exclusive.
const NANOS_UNTIL: i64 = PLAUSIBLE_UNTIL_MILLIS * 1_000_000;

/// What unit a timestamp's magnitude says it is in.
///
/// Not ordered: these are kinds, not severities. Every non-`Millis` variant is
/// equally disqualifying, because each one means the number we have been doing
/// arithmetic on is not the quantity we thought.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EpochUnit {
    /// A plausible date when read as milliseconds. The unit we assume everywhere.
    Millis,
    /// A plausible date when read as **seconds**, so as millis it points at the
    /// early 1970s. Ages computed from it are 1000× too small: the favourable
    /// direction, and the reason this module exists.
    Seconds,
    /// A plausible date when read as **microseconds**, so as millis it is tens of
    /// thousands of years in the future.
    Micros,
    /// A plausible date when read as **nanoseconds**.
    Nanos,
    /// Not a plausible date in any of the four units.
    ///
    /// Zero, negative, a sentinel, a parse that landed on a sequence number, or a
    /// genuine date outside 2017–2100. Deliberately not "probably millis": a
    /// number we cannot explain is not a number to price against, and guessing the
    /// most convenient unit is how the original defect would come back.
    Unrecognised,
}

impl EpochUnit {
    /// True only for [`EpochUnit::Millis`].
    ///
    /// Exists so call sites read `if !unit.is_millis()` instead of
    /// `if unit != EpochUnit::Millis`, which is one `!` away from inverting the
    /// gate, and to keep `Unrecognised` on the failing side by construction —
    /// a match that forgot to handle it would otherwise default to passing.
    pub const fn is_millis(self) -> bool {
        matches!(self, EpochUnit::Millis)
    }

    /// Short label for logs and screens.
    pub const fn tag(self) -> &'static str {
        match self {
            EpochUnit::Millis => "ms",
            EpochUnit::Seconds => "s",
            EpochUnit::Micros => "us",
            EpochUnit::Nanos => "ns",
            EpochUnit::Unrecognised => "unrecognised",
        }
    }
}

impl std::fmt::Display for EpochUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

/// Classify one venue timestamp by magnitude alone.
///
/// Bands are checked in ascending order and are disjoint, so the order is for
/// readability rather than precedence. Each band is `[from, until)`: half-open,
/// so a stamp exactly on an upper bound falls out of that band and into
/// [`EpochUnit::Unrecognised`] rather than into the next one, which keeps the
/// boundary on the refusing side.
///
/// Takes a [`WallTs`] rather than an `i64` so it cannot be pointed at a
/// [`arb_types::LocalTs`] or at an age. Our own clock is not the thing under
/// suspicion here — the venue's field is.
pub fn classify_epoch(ts: WallTs) -> EpochUnit {
    let v = ts.millis();
    if v >= SECONDS_FROM && v < SECONDS_UNTIL {
        EpochUnit::Seconds
    } else if v >= PLAUSIBLE_FROM_MILLIS && v < PLAUSIBLE_UNTIL_MILLIS {
        EpochUnit::Millis
    } else if v >= MICROS_FROM && v < MICROS_UNTIL {
        EpochUnit::Micros
    } else if v >= NANOS_FROM && v < NANOS_UNTIL {
        EpochUnit::Nanos
    } else {
        // Includes zero, every negative, and every date outside 2017–2100 in all
        // four units.
        EpochUnit::Unrecognised
    }
}

/// A venue's timestamp field, classified — the form [`crate::trust`] consumes.
///
/// Three states rather than two because "the venue sent no timestamp" and "the
/// venue sent a number in the wrong unit" are different facts with different
/// remedies. Several venues legitimately omit a timestamp; none legitimately
/// sends seconds where its documentation says milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TsUnit {
    /// The message carried no exchange timestamp. Not a defect: it means the
    /// clock and staleness checks have nothing to work from, which they already
    /// report themselves.
    NoTimestamp,
    /// A plausible millisecond timestamp. The assumption held for this sample.
    Millis,
    /// A timestamp that is **not** milliseconds. Fatal in
    /// [`crate::trust::assess_venue`]: every age and every skew computed from
    /// this feed is wrong by the unit's factor, and in the seconds case wrong in
    /// the direction that manufactures opportunity.
    NotMillis(EpochUnit),
}

impl TsUnit {
    /// Classify whatever the parse boundary produced, `None` included.
    ///
    /// The whole point of accepting an `Option` is that this is the one call an
    /// adapter has to make, so there is no correct-usage rule left to get wrong:
    /// hand it the field, store the answer.
    ///
    /// Feed it the timestamp **as parsed**, before any normalisation. A caller
    /// that multiplies a seconds field by 1000 and then classifies the result
    /// gets `Millis`, which is true of the value it passed and useless — the
    /// question is what the venue sent.
    pub fn of(ts: Option<WallTs>) -> Self {
        match ts {
            None => TsUnit::NoTimestamp,
            Some(t) => match classify_epoch(t) {
                EpochUnit::Millis => TsUnit::Millis,
                other => TsUnit::NotMillis(other),
            },
        }
    }

    /// True when a timestamp arrived and was not in milliseconds.
    pub const fn is_wrong_unit(self) -> bool {
        matches!(self, TsUnit::NotMillis(_))
    }
}

impl std::fmt::Display for TsUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TsUnit::NoTimestamp => f.write_str("no exchange timestamp"),
            TsUnit::Millis => f.write_str("timestamp in ms"),
            TsUnit::NotMillis(u) => {
                write!(f, "timestamp is not ms, it looks like {u}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// True wall time used throughout: 2023-10-26T06:00:00Z-ish, the same instant
    /// `arb_adversary::staleness` uses for the units tests.
    const NOW_MS: i64 = 1_700_000_000_000;

    #[test]
    fn a_real_millisecond_stamp_is_recognised() {
        assert_eq!(classify_epoch(WallTs::from_millis(NOW_MS)), EpochUnit::Millis);
        assert!(classify_epoch(WallTs::from_millis(NOW_MS)).is_millis());
    }

    #[test]
    fn the_same_instant_in_seconds_is_caught() {
        // The defect: 1_700_000_000 read as millis is 1970-01-20. Nothing in a
        // difference-based check can see this, because a seconds feed compared
        // against a seconds clock is self-consistent.
        let secs = WallTs::from_millis(NOW_MS / 1_000);
        assert_eq!(classify_epoch(secs), EpochUnit::Seconds);
        assert!(!classify_epoch(secs).is_millis());
    }

    #[test]
    fn the_same_instant_in_micros_and_nanos_is_caught() {
        assert_eq!(
            classify_epoch(WallTs::from_millis(NOW_MS * 1_000)),
            EpochUnit::Micros
        );
        assert_eq!(
            classify_epoch(WallTs::from_millis(NOW_MS * 1_000_000)),
            EpochUnit::Nanos
        );
    }

    #[test]
    fn the_four_bands_do_not_touch_anywhere() {
        // The property the whole module rests on. Between the top of each band
        // and the bottom of the next there is a factor of ~361, so no value is
        // ambiguous. If someone widens the plausible era far enough to close one
        // of these gaps, this test is what says so.
        for (top, bottom) in [
            (SECONDS_UNTIL, PLAUSIBLE_FROM_MILLIS),
            (PLAUSIBLE_UNTIL_MILLIS, MICROS_FROM),
            (MICROS_UNTIL, NANOS_FROM),
        ] {
            assert!(top < bottom, "bands {top} and {bottom} overlap");
            // Integer ratio, floored, so the assertion understates the real gap.
            assert!(
                bottom / top >= 100,
                "the gap between {top} and {bottom} is under 100x, so a stamp \
                 could plausibly be read in two units"
            );
        }
    }

    #[test]
    fn the_whole_nanosecond_band_fits_in_the_type() {
        // Const evaluation already refuses to compile an overflowing product;
        // this pins that the top of the band is representable with room left, so
        // a later widening of the era cannot silently wrap.
        assert!(NANOS_UNTIL > 0);
        assert!(NANOS_UNTIL < i64::MAX / 2);
    }

    #[test]
    fn boundaries_fall_on_the_refusing_side() {
        // Exactly on a lower bound is inside the band.
        assert_eq!(
            classify_epoch(WallTs::from_millis(PLAUSIBLE_FROM_MILLIS)),
            EpochUnit::Millis
        );
        // Exactly on an upper bound is out of it, and lands nowhere rather than
        // in the next band up.
        assert_eq!(
            classify_epoch(WallTs::from_millis(PLAUSIBLE_UNTIL_MILLIS)),
            EpochUnit::Unrecognised
        );
        assert_eq!(
            classify_epoch(WallTs::from_millis(SECONDS_UNTIL)),
            EpochUnit::Unrecognised
        );
    }

    #[test]
    fn zero_and_negative_are_unrecognised_and_not_quietly_millis() {
        // A venue that sends 0, or a parse that produced a default, must not be
        // read as "1970 in milliseconds, therefore millis, therefore fine".
        for v in [0, -1, 1, -1_700_000_000_000, i64::MIN, i64::MAX] {
            assert_eq!(
                classify_epoch(WallTs::from_millis(v)),
                EpochUnit::Unrecognised,
                "{v} must not be classified as a unit"
            );
        }
    }

    #[test]
    fn a_sequence_number_mistaken_for_a_timestamp_is_unrecognised() {
        // The realistic adapter defect: a field lookup that returned the update
        // id. These are far too small to be a date in any unit.
        for v in [1_i64, 42, 1_000_000, 999_999_999] {
            assert_eq!(
                classify_epoch(WallTs::from_millis(v)),
                EpochUnit::Unrecognised
            );
        }
    }

    #[test]
    fn a_missing_timestamp_is_not_a_wrong_unit() {
        assert_eq!(TsUnit::of(None), TsUnit::NoTimestamp);
        assert!(!TsUnit::of(None).is_wrong_unit());
    }

    #[test]
    fn ts_unit_carries_which_unit_it_actually_looked_like() {
        assert_eq!(TsUnit::of(Some(WallTs::from_millis(NOW_MS))), TsUnit::Millis);
        assert_eq!(
            TsUnit::of(Some(WallTs::from_millis(NOW_MS / 1_000))),
            TsUnit::NotMillis(EpochUnit::Seconds)
        );
        assert_eq!(
            TsUnit::of(Some(WallTs::from_millis(0))),
            TsUnit::NotMillis(EpochUnit::Unrecognised)
        );
        assert!(TsUnit::of(Some(WallTs::from_millis(0))).is_wrong_unit());
    }

    #[test]
    fn labels_are_distinct_so_a_log_line_is_actionable() {
        let tags = [
            EpochUnit::Millis.tag(),
            EpochUnit::Seconds.tag(),
            EpochUnit::Micros.tag(),
            EpochUnit::Nanos.tag(),
            EpochUnit::Unrecognised.tag(),
        ];
        for (i, a) in tags.iter().enumerate() {
            for b in tags.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
        assert_eq!(
            TsUnit::NotMillis(EpochUnit::Seconds).to_string(),
            "timestamp is not ms, it looks like s"
        );
    }
}
