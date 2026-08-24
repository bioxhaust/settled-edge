//! Staleness: how old is this observation, and does that age still permit a
//! bet?
//!
//! Age is measured on our own receive clock only ([`LocalTs`] minus
//! [`LocalTs`]). We never mix in the exchange's clock here — that mixture is
//! [`crate::clock`]'s problem and it answers a different question.
//!
//! One rule governs every boundary in this module: **the boundary belongs to
//! the worse bucket.** An age of exactly `stale_after` reads Stale, not Warn.
//! If a threshold is placed correctly, sitting exactly on it is already the bad
//! case, and one millisecond of luck should not decide whether we send an
//! order.

use arb_types::{LocalTs, Millis};

/// The three ages at which an observation changes meaning.
///
/// Held as three separate numbers rather than one because the useful response
/// differs: Warn means "rank it lower", Stale means "do not trade it", Dead
/// means "do not even display it as a price".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StalenessLimits {
    /// At or past this age, downgrade but keep using.
    pub warn_after: Millis,
    /// At or past this age, do not trade off it.
    pub stale_after: Millis,
    /// At or past this age, the observation carries no information.
    pub dead_after: Millis,
}

impl StalenessLimits {
    /// Limits for an order book.
    ///
    /// The physical fact behind these numbers is venue publish cadence. The
    /// major venues push depth updates on a fixed tick — Binance's depth
    /// streams are 100 ms (with a 1000 ms variant), Bybit and OKX publish in
    /// the 20–100 ms range. Take 100 ms as the realistic normal case.
    ///
    /// - `warn_after = 250 ms`: two or three publish ticks have gone by
    ///   unobserved. The book has almost certainly changed; whether it changed
    ///   in a way that matters is unknown, so we keep the row but rank it
    ///   behind an equal-priced fresh row. Below 250 ms we are inside the
    ///   normal jitter of a healthy feed and flagging would fire constantly.
    /// - `stale_after = 500 ms`: five publish ticks. At this point it is a
    ///   different book, not an old copy of the same book — during a move, top
    ///   of book on a liquid perp turns over several times per second, so the
    ///   level we priced against is likely gone. Above this line the edge we
    ///   compute is fiction and the fill we get is whatever the market has
    ///   moved to.
    /// - `dead_after = 2000 ms`: twenty publish ticks. Nothing about the
    ///   original snapshot survives; showing it on screen invites a human to
    ///   act on a price that has no relationship to the market.
    ///
    /// `[assumption]` — the cadences above are recalled API behaviour, not
    /// measured on our own connections. If measurement shows a venue publishing
    /// slower than 100 ms, `warn_after` is too tight for that venue and should
    /// be widened per venue rather than globally.
    pub const BOOK: StalenessLimits = StalenessLimits {
        warn_after: Millis::from_millis(250),
        stale_after: Millis::from_millis(500),
        dead_after: Millis::from_millis(2_000),
    };

    /// Limits for a funding snapshot.
    ///
    /// Funding is a slow variable and the limits reflect that. The fastest
    /// interval in production is one hour, so the *settled* rate can only
    /// change once every 3 600 000 ms. What moves faster is the *predicted*
    /// rate, which tracks the premium index and therefore the basis.
    ///
    /// - `warn_after = 60 000 ms` (1 min): a minute-old prediction is the same
    ///   forecast within noise for anything but a violent move. Warning earlier
    ///   would flag data that is genuinely fine and train us to ignore the
    ///   flag.
    /// - `stale_after = 300 000 ms` (5 min): in five minutes the basis can move
    ///   enough to change the predicted rate materially, and funding APR is the
    ///   number our ranking sorts on. Trading a five-minute-old prediction is
    ///   betting that the premium index stood still.
    /// - `dead_after = 3 600 000 ms` (1 h): one full interval at the fastest
    ///   cadence. Past this we may have crossed a settlement boundary, so the
    ///   rate in hand may describe a period that has already paid out — which
    ///   is not merely imprecise, it is about a different event.
    ///
    /// `[assumption]` — "one hour is the fastest interval" is recalled venue
    /// behaviour. If any instrument settles more often than hourly,
    /// `dead_after` is too loose for it and must come from the instrument's own
    /// `FundingInterval` instead of this constant.
    pub const FUNDING: StalenessLimits = StalenessLimits {
        warn_after: Millis::from_millis(60_000),
        stale_after: Millis::from_millis(300_000),
        dead_after: Millis::from_millis(3_600_000),
    };

    /// True when `warn_after <= stale_after <= dead_after`, which is the only
    /// ordering in which the three limits mean what their names say.
    ///
    /// [`assess_age`] does not require this — it tests worst-first so that a
    /// misordered set can only ever produce a *harsher* verdict — but a caller
    /// building limits from config should check it and reject rather than ship
    /// thresholds nobody can reason about.
    pub fn is_ordered(&self) -> bool {
        self.warn_after.millis() <= self.stale_after.millis()
            && self.stale_after.millis() <= self.dead_after.millis()
    }
}

/// How much an observation's age is allowed to affect our decisions.
///
/// Ordered worst-last, so `Ord`/`max` give "the worse of two verdicts".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Staleness {
    /// Young enough that age is not a consideration.
    Fresh,
    /// Old enough to rank lower, young enough to keep.
    Warn,
    /// Too old to trade against.
    Stale,
    /// Carries no information about the current market.
    Dead,
}

impl Staleness {
    /// The worse of two verdicts.
    pub const fn worse_of(self, other: Self) -> Self {
        if (self as u8) >= (other as u8) {
            self
        } else {
            other
        }
    }

    /// Short label for logs and screens.
    pub const fn tag(self) -> &'static str {
        match self {
            Staleness::Fresh => "fresh",
            Staleness::Warn => "warn",
            Staleness::Stale => "stale",
            Staleness::Dead => "dead",
        }
    }
}

impl std::fmt::Display for Staleness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

/// Age of an observation on our own clock: `now - observed_at`.
///
/// The result is negative when `observed_at` is in the future relative to
/// `now`. That is never a property of the market; it is a bug in our own clock
/// handling (a re-used timestamp, a clock step, a snapshot stamped by a
/// different thread) and [`assess_age`] treats it as such.
pub fn age_of(observed_at: LocalTs, now: LocalTs) -> Millis {
    now.since(observed_at)
}

/// Classify an age against limits.
///
/// Tests run worst-first, deliberately: with misordered limits the answer can
/// only come out harsher than intended, never softer.
///
/// A negative age returns [`Staleness::Dead`]. It must not read Fresh, and
/// "Fresh" is exactly what a naive `age < warn_after` comparison would return
/// for it — that is the whole reason this function exists instead of a
/// comparison at each call site. The reasoning for Dead rather than Warn: a
/// negative age means our clock arithmetic is broken, so *every* age we compute
/// in that moment is untrustworthy, including this one. We do not know the true
/// age, and the honest response to an unknown age is the worst bucket.
pub fn assess_age(age: Millis, limits: StalenessLimits) -> Staleness {
    let ms = age.millis();

    // An observation from the future means our clock, not the venue's, is
    // wrong. We cannot recover the real age from here.
    if ms < 0 {
        return Staleness::Dead;
    }

    if ms >= limits.dead_after.millis() {
        Staleness::Dead
    } else if ms >= limits.stale_after.millis() {
        Staleness::Stale
    } else if ms >= limits.warn_after.millis() {
        Staleness::Warn
    } else {
        Staleness::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOK: StalenessLimits = StalenessLimits::BOOK;
    const FUNDING: StalenessLimits = StalenessLimits::FUNDING;

    fn ms(v: i64) -> Millis {
        Millis::from_millis(v)
    }

    #[test]
    fn presets_are_ordered() {
        assert!(BOOK.is_ordered());
        assert!(FUNDING.is_ordered());
    }

    #[test]
    fn age_of_is_now_minus_observed() {
        // observed at 1_000, now 1_250 -> 250 ms old.
        let age = age_of(LocalTs::from_millis(1_000), LocalTs::from_millis(1_250));
        assert_eq!(age.millis(), 250);
    }

    #[test]
    fn age_of_future_observation_is_negative() {
        // observed at 2_000, now 1_900 -> -100 ms. Our clock, not the market.
        let age = age_of(LocalTs::from_millis(2_000), LocalTs::from_millis(1_900));
        assert_eq!(age.millis(), -100);
    }

    #[test]
    fn negative_age_is_not_fresh() {
        assert_eq!(assess_age(ms(-1), BOOK), Staleness::Dead);
        assert_eq!(assess_age(ms(-100_000), BOOK), Staleness::Dead);
        assert_ne!(assess_age(ms(-1), BOOK), Staleness::Fresh);
    }

    #[test]
    fn book_zero_age_is_fresh() {
        assert_eq!(assess_age(ms(0), BOOK), Staleness::Fresh);
    }

    #[test]
    fn book_one_ms_below_warn_is_fresh() {
        // warn_after = 250; 249 < 250.
        assert_eq!(assess_age(ms(249), BOOK), Staleness::Fresh);
    }

    #[test]
    fn book_exactly_on_warn_is_warn() {
        // The boundary belongs to the worse bucket.
        assert_eq!(assess_age(ms(250), BOOK), Staleness::Warn);
    }

    #[test]
    fn book_one_ms_above_warn_is_warn() {
        assert_eq!(assess_age(ms(251), BOOK), Staleness::Warn);
    }

    #[test]
    fn book_one_ms_below_stale_is_warn() {
        // stale_after = 500; 499 < 500.
        assert_eq!(assess_age(ms(499), BOOK), Staleness::Warn);
    }

    #[test]
    fn book_exactly_on_stale_is_stale() {
        assert_eq!(assess_age(ms(500), BOOK), Staleness::Stale);
    }

    #[test]
    fn book_one_ms_above_stale_is_stale() {
        assert_eq!(assess_age(ms(501), BOOK), Staleness::Stale);
    }

    #[test]
    fn book_one_ms_below_dead_is_stale() {
        // dead_after = 2000; 1999 < 2000.
        assert_eq!(assess_age(ms(1_999), BOOK), Staleness::Stale);
    }

    #[test]
    fn book_exactly_on_dead_is_dead() {
        assert_eq!(assess_age(ms(2_000), BOOK), Staleness::Dead);
    }

    #[test]
    fn book_one_ms_above_dead_is_dead() {
        assert_eq!(assess_age(ms(2_001), BOOK), Staleness::Dead);
    }

    #[test]
    fn funding_boundaries() {
        // warn 60_000, stale 300_000, dead 3_600_000.
        assert_eq!(assess_age(ms(59_999), FUNDING), Staleness::Fresh);
        assert_eq!(assess_age(ms(60_000), FUNDING), Staleness::Warn);
        assert_eq!(assess_age(ms(299_999), FUNDING), Staleness::Warn);
        assert_eq!(assess_age(ms(300_000), FUNDING), Staleness::Stale);
        assert_eq!(assess_age(ms(3_599_999), FUNDING), Staleness::Stale);
        assert_eq!(assess_age(ms(3_600_000), FUNDING), Staleness::Dead);
    }

    #[test]
    fn funding_tolerates_an_age_that_kills_a_book() {
        // 30 s: dead for a book, fresh for a funding prediction.
        assert_eq!(assess_age(ms(30_000), BOOK), Staleness::Dead);
        assert_eq!(assess_age(ms(30_000), FUNDING), Staleness::Fresh);
    }

    #[test]
    fn misordered_limits_can_only_read_worse() {
        // warn 1000 > stale 500: nonsense config. 700 ms is past stale_after,
        // so worst-first testing reports Stale, which is harsher than the Warn
        // a naive best-first walk would have produced.
        let bad = StalenessLimits {
            warn_after: ms(1_000),
            stale_after: ms(500),
            dead_after: ms(2_000),
        };
        assert!(!bad.is_ordered());
        assert_eq!(assess_age(ms(700), bad), Staleness::Stale);
    }

    #[test]
    fn worse_of_picks_the_worse() {
        assert_eq!(Staleness::Fresh.worse_of(Staleness::Stale), Staleness::Stale);
        assert_eq!(Staleness::Dead.worse_of(Staleness::Warn), Staleness::Dead);
        assert_eq!(Staleness::Warn.worse_of(Staleness::Warn), Staleness::Warn);
        assert!(Staleness::Dead > Staleness::Stale);
    }
}
