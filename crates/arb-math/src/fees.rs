//! Fees: tier resolution and the charge on a notional.
//!
//! # Why an unknown tier resolves to the worst tier
//!
//! Real fee tiers depend on 30-day volume, VIP level, holdings of the venue's
//! own token, and promotions. We can rarely know the true tier from public data.
//! There are two defaults available: assume the best tier, or assume the worst.
//!
//! Assuming the best makes every opportunity look better than it is, and the
//! opportunities it flatters most are the marginal ones — precisely the rows
//! near the top of a ranking sorted by net return. Assuming the worst makes
//! every figure a pessimistic bound: a row that clears the bar under the worst
//! tier clears it under any tier. That asymmetry is the whole argument, and it
//! is why [`FeeTable::resolve`] returns `tier_known: false` alongside the worst
//! tier rather than silently picking something plausible.

use crate::error::MathError;
use arb_num::{Bps, Money, RoundMode};

/// Whether an order provides liquidity or takes it.
///
/// Not a bool: `is_maker: false` at a call site reads as "not a maker", which is
/// one negation away from the opposite meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeeRole {
    /// Resting order, filled by someone else's aggression.
    Maker,
    /// Aggressive order, crossing the spread.
    Taker,
}

impl FeeRole {
    /// Short tag for display.
    pub const fn tag(self) -> &'static str {
        match self {
            FeeRole::Maker => "maker",
            FeeRole::Taker => "taker",
        }
    }
}

/// The two fee rates that apply to us right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeeRates {
    /// Rate charged when we provide liquidity.
    pub maker: Bps,
    /// Rate charged when we take liquidity.
    pub taker: Bps,
    /// `false` when the tier was assumed rather than measured. The caller must
    /// map this onto [`arb_types::QualityFlags::FEE_TIER_ASSUMED`] so it reaches
    /// the screen.
    pub tier_known: bool,
}

impl FeeRates {
    /// Build a pair of rates, rejecting negative ones.
    ///
    /// Venue rebates for high-volume makers do exist, and they are not modelled:
    /// a negative fee turns a cost into revenue, which inverts every
    /// conservative rounding decision in this crate. Refusing is honest;
    /// silently treating a rebate as a cost is merely pessimistic, but silently
    /// treating it as revenue would flatter the edge.
    pub fn new(maker: Bps, taker: Bps, tier_known: bool) -> Result<Self, MathError> {
        if maker.is_negative() || taker.is_negative() {
            return Err(MathError::NegativeFeeRate);
        }
        Ok(Self {
            maker,
            taker,
            tier_known,
        })
    }

    /// The rate for a given role.
    pub const fn rate(&self, role: FeeRole) -> Bps {
        match role {
            FeeRole::Maker => self.maker,
            FeeRole::Taker => self.taker,
        }
    }

    /// Re-check the invariant [`Self::new`] established.
    ///
    /// `maker` and `taker` are `pub` and are read by `arb-adversary`, `arb-engine`,
    /// `arb-live`, `arb-scan` and `arb-venue`, so they cannot be made private and
    /// the constructor's check is a courtesy rather than a fence: one struct
    /// literal reaches a negative rate. Every function in this crate that does
    /// arithmetic on a `FeeRates` therefore checks for itself, at the last point
    /// the value is used and not only at the first point it was built — the same
    /// argument [`crate::borrow::net_apr_after_borrow`] makes for borrow.
    ///
    /// **Both** fields are checked regardless of which roles the configuration
    /// consults. A [`RoundTrip`] that happens to read only `taker` today is a
    /// configuration away from reading `maker`, and a schedule carrying a
    /// negative rate in the field this call does not touch is still a schedule we
    /// have no honest arithmetic for.
    fn check_non_negative(&self) -> Result<(), MathError> {
        if self.maker.is_negative() || self.taker.is_negative() {
            return Err(MathError::NegativeFeeRate);
        }
        Ok(())
    }
}

/// One row of a venue's fee schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VolumeTier {
    /// Inclusive lower bound of 30-day volume for this tier, in quote currency.
    pub min_volume_30d: Money,
    /// Maker rate in this tier.
    pub maker: Bps,
    /// Taker rate in this tier.
    pub taker: Bps,
}

/// A venue's fee schedule, sorted strictly ascending by volume threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeTable {
    tiers: Vec<VolumeTier>,
}

impl FeeTable {
    /// Build a table, rejecting an empty, unsorted, or ambiguous one.
    ///
    /// Sortedness is checked rather than fixed, because a caller who hands over
    /// an unsorted table has misunderstood the venue's documentation, and
    /// quietly sorting it hides that.
    ///
    /// Two rows at the *same* threshold are refused for a sharper reason: they
    /// are not unsorted, so a sortedness check accepts them, and [`Self::resolve`]
    /// then keeps whichever came last in the `Vec`. On a schedule that descends
    /// in price — the usual shape — that is the *cheaper* row, so a table with a
    /// duplicated threshold understates the fee. `<=` would have let that
    /// through; `<` does not.
    pub fn new(tiers: Vec<VolumeTier>) -> Result<Self, MathError> {
        if tiers.is_empty() {
            return Err(MathError::EmptyTierTable);
        }
        for w in tiers.windows(2) {
            match (w.first(), w.get(1)) {
                (Some(a), Some(b)) if a.min_volume_30d < b.min_volume_30d => {}
                (Some(a), Some(b)) if a.min_volume_30d == b.min_volume_30d => {
                    return Err(MathError::DuplicateTierThreshold)
                }
                (Some(_), Some(_)) => return Err(MathError::UnsortedTierTable),
                // `windows(2)` never yields a short window; this arm exists
                // only because rule 4 forbids indexing that can panic.
                _ => return Err(MathError::UnsortedTierTable),
            }
        }
        for t in &tiers {
            if t.maker.is_negative() || t.taker.is_negative() {
                return Err(MathError::NegativeFeeRate);
            }
        }
        Ok(Self { tiers })
    }

    /// A single-tier table, for a venue whose schedule we only know flatly.
    pub fn flat(maker: Bps, taker: Bps) -> Result<Self, MathError> {
        Self::new(vec![VolumeTier {
            min_volume_30d: Money::ZERO,
            maker,
            taker,
        }])
    }

    /// The tiers, ascending by threshold.
    pub fn tiers(&self) -> &[VolumeTier] {
        &self.tiers
    }

    /// The most expensive tier in the table.
    ///
    /// Computed by comparing taker rates rather than by taking the first row.
    /// The first row is the cheapest tier only if the venue's schedule is
    /// monotonic in the direction we expect, and a promotional row can break
    /// that. Comparing the actual numbers cannot be wrong.
    pub fn worst(&self) -> Result<FeeRates, MathError> {
        let mut worst: Option<&VolumeTier> = None;
        for t in &self.tiers {
            worst = match worst {
                None => Some(t),
                Some(w) if t.taker > w.taker => Some(t),
                // Tie on taker: break it on maker, so "worst" is total.
                Some(w) if t.taker == w.taker && t.maker > w.maker => Some(t),
                Some(w) => Some(w),
            };
        }
        match worst {
            Some(t) => FeeRates::new(t.maker, t.taker, false),
            None => Err(MathError::EmptyTierTable),
        }
    }

    /// Resolve the tier for a known 30-day volume.
    ///
    /// `None` volume means we do not know our tier, so this returns
    /// [`FeeTable::worst`] with `tier_known: false`. See the module docs for why
    /// that direction and not the other.
    pub fn resolve(&self, volume_30d: Option<Money>) -> Result<FeeRates, MathError> {
        let volume = match volume_30d {
            Some(v) => v,
            None => return self.worst(),
        };
        // Highest threshold that the volume reaches. Thresholds are strictly
        // ascending (`new` refuses ties), so the last qualifying row is the
        // unique deepest one rather than whichever duplicate came last.
        let mut chosen: Option<&VolumeTier> = None;
        for t in &self.tiers {
            if t.min_volume_30d <= volume {
                chosen = Some(t);
            }
        }
        match chosen {
            Some(t) => FeeRates::new(t.maker, t.taker, true),
            // Volume below every threshold. That means the table has no row
            // starting at zero, which is a gap in the table rather than a fact
            // about us — fall back to the worst tier and mark it unknown.
            None => self.worst(),
        }
    }
}

/// The fee charged on a notional, rounded against us.
///
/// Always [`RoundMode::COST`]: a fee is money leaving, and a sub-unit remainder
/// rounded down would show us keeping a fraction of a cent that the venue keeps.
/// Over a twelve basis point edge, the direction of that rounding is not
/// interesting on one trade and is not free across thousands.
pub fn fee_on_notional(notional: Money, rate: Bps) -> Result<Money, MathError> {
    if rate.is_negative() {
        return Err(MathError::NegativeFeeRate);
    }
    notional
        .mul_rate(rate.to_rate(), RoundMode::COST)
        .map_err(MathError::from)
}

/// The four fee legs of a two-leg position, opened and closed.
///
/// A carry trade is not one trade. It is two legs in, two legs out, and the cost
/// that matters is all four. Reporting a one-way, one-leg fee — which is what a
/// "spread minus fees" display usually does — understates the true cost by
/// roughly a factor of four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoundTrip {
    /// Role we expect on the first leg's entry.
    pub entry_a: FeeRole,
    /// Role we expect on the second leg's entry.
    pub entry_b: FeeRole,
    /// Role we expect on the first leg's exit.
    pub exit_a: FeeRole,
    /// Role we expect on the second leg's exit.
    pub exit_b: FeeRole,
}

impl RoundTrip {
    /// All four legs taker. The realistic default for a cross-venue carry,
    /// because a resting order that does not fill leaves the other leg naked,
    /// and being naked is a larger risk than the maker rebate is a saving.
    pub const ALL_TAKER: Self = Self {
        entry_a: FeeRole::Taker,
        entry_b: FeeRole::Taker,
        exit_a: FeeRole::Taker,
        exit_b: FeeRole::Taker,
    };

    /// Post the first entry leg, cross the second to hedge the fill, cross both
    /// exits.
    ///
    /// This is the configuration an operator can actually reach, and it is here
    /// because the gap between [`RoundTrip::ALL_TAKER`] and
    /// [`RoundTrip::ALL_MAKER`] is the largest lever in the economics (`OPEN.md`
    /// A6) and a report that shows only the endpoints shows only a choice between
    /// the pessimistic and the impossible.
    ///
    /// # Why only one leg rests, and why only on entry
    ///
    /// `ALL_TAKER`'s own reasoning is the constraint: a resting order that does not
    /// fill leaves the other leg naked. That argument bites on *both* legs resting
    /// and it bites hardest on exit.
    ///
    /// - **Entry, one leg resting:** we hold nothing yet. An unfilled post costs
    ///   only the opportunity, and the moment it fills we cross the second leg
    ///   immediately. Exposure is one hedge latency, not one unfilled order.
    /// - **Exit, resting:** we hold the position. We are exiting *because* the
    ///   edge decayed, so a post that does not fill keeps us in the thing we
    ///   decided to leave, for as long as it does not fill. Patience is cheap on
    ///   the way in and expensive on the way out, and this asymmetry is why the
    ///   middle configuration is not simply "maker where convenient".
    ///
    /// `[assumption]` The fill itself is assumed, and that is the load-bearing
    /// weakness: an entry post that never fills produces no position and therefore
    /// no row, so this configuration silently conditions on selection. Nothing here
    /// measures fill probability, and nothing can without recorded book data.
    pub const HEDGED_ENTRY: Self = Self {
        entry_a: FeeRole::Maker,
        entry_b: FeeRole::Taker,
        exit_a: FeeRole::Taker,
        exit_b: FeeRole::Taker,
    };

    /// All four legs maker. **A bound, not a recommendation.**
    ///
    /// Reaching it requires every leg to rest and fill, on both venues, in both
    /// directions — which is exactly the leg risk `ALL_TAKER` refuses, taken four
    /// times. It is reported because it answers "how much can fee optimisation
    /// possibly be worth?" with a number instead of a hope: at a 2/5.5 bp schedule
    /// it is 8 bp against all-taker's 22, which cuts the hold needed to break even
    /// at 1 bp per settlement from 7.3 days to 2.7 (`OPEN.md` A6).
    ///
    /// A row whose net APR is acceptable *only* in this configuration is a row
    /// whose thesis is an execution assumption, not a funding divergence.
    pub const ALL_MAKER: Self = Self {
        entry_a: FeeRole::Maker,
        entry_b: FeeRole::Maker,
        exit_a: FeeRole::Maker,
        exit_b: FeeRole::Maker,
    };

    /// The three configurations the report prints, most expensive first.
    ///
    /// Ordered so the conservative figure is what the eye lands on and the spread
    /// to its right is the lever. Reversing it would put the least attainable
    /// number in the position a reader treats as the headline.
    pub const REPORTED: [Self; 3] = [Self::ALL_TAKER, Self::HEDGED_ENTRY, Self::ALL_MAKER];

    /// A short label for display, or `"custom"` for a configuration with no name.
    ///
    /// `"custom"` rather than a rendering of the four roles: a column header is not
    /// the place to discover that someone constructed a fifth configuration, and a
    /// name that silently appears is worse than one that says it is unnamed.
    pub fn label(&self) -> &'static str {
        if *self == Self::ALL_TAKER {
            "all-taker"
        } else if *self == Self::HEDGED_ENTRY {
            "hedged-entry"
        } else if *self == Self::ALL_MAKER {
            "all-maker"
        } else {
            "custom"
        }
    }

    /// Total fee cost, in basis points of one leg's notional, assuming both legs
    /// carry the same notional and the same schedule.
    ///
    /// The "same notional" assumption is exactly true for a spot-perp carry on a
    /// linear contract and approximately true elsewhere; where it is not, use
    /// [`fee_on_notional`] per leg instead of this convenience.
    ///
    /// The "same schedule" assumption is **false for every cross-venue row in this
    /// product** — two venues, two fee tables. Use [`RoundTrip::total_bps_cross`]
    /// there. This one remains for the single-venue case and for tests.
    ///
    /// Refuses a `FeeRates` carrying a negative rate; see
    /// [`FeeRates::check_non_negative`]. Four all-taker legs at −5.5 bps summed to
    /// −22 bps here without an error, and a round trip that *pays* is subtracted
    /// from the net as revenue.
    pub fn total_bps(&self, rates: FeeRates) -> Result<Bps, MathError> {
        rates.check_non_negative()?;
        let mut total = Bps::ZERO;
        for role in [self.entry_a, self.entry_b, self.exit_a, self.exit_b] {
            total = total.checked_add(rates.rate(role))?;
        }
        Ok(total)
    }

    /// Total fee cost across two venues, each leg charged by its own schedule.
    ///
    /// `rates_a` prices the `_a` legs and `rates_b` the `_b` legs. Which venue is
    /// `a` is the caller's choice and it **matters** for any configuration whose
    /// roles are not symmetric: under [`RoundTrip::HEDGED_ENTRY`] the resting leg
    /// is `a`, so passing the venues in the other order prices the post at the
    /// wrong venue's maker rate. A symmetric configuration is unaffected, which is
    /// precisely why the asymmetric one needs saying out loud.
    ///
    /// Still one notional for both legs, for the reason [`RoundTrip::total_bps`]
    /// gives.
    ///
    /// **Both** schedules are checked, and both of each schedule's fields, even
    /// when the configuration reads only one role from one of them — see
    /// [`FeeRates::check_non_negative`].
    pub fn total_bps_cross(&self, rates_a: FeeRates, rates_b: FeeRates) -> Result<Bps, MathError> {
        rates_a.check_non_negative()?;
        rates_b.check_non_negative()?;
        let mut total = Bps::ZERO;
        for (role, rates) in [
            (self.entry_a, rates_a),
            (self.entry_b, rates_b),
            (self.exit_a, rates_a),
            (self.exit_b, rates_b),
        ] {
            total = total.checked_add(rates.rate(role))?;
        }
        Ok(total)
    }

    /// True when every leg is priced at the same role, so leg order cannot matter.
    ///
    /// Exists so a caller that cannot establish which venue is `a` can assert it
    /// does not need to, rather than guessing and being right by luck.
    pub const fn is_symmetric(&self) -> bool {
        matches!(
            (self.entry_a, self.entry_b, self.exit_a, self.exit_b),
            (FeeRole::Maker, FeeRole::Maker, FeeRole::Maker, FeeRole::Maker)
                | (FeeRole::Taker, FeeRole::Taker, FeeRole::Taker, FeeRole::Taker)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bps(s: &str) -> Bps {
        Bps::parse_bps(s).expect("test bps parses")
    }

    fn money(s: &str) -> Money {
        Money::parse(s).expect("test money parses")
    }

    fn table() -> FeeTable {
        // A plausible three-tier schedule. Values invented; the arithmetic is
        // what is under test, not the schedule.
        FeeTable::new(vec![
            VolumeTier {
                min_volume_30d: Money::ZERO,
                maker: bps("2"),
                taker: bps("5.5"),
            },
            VolumeTier {
                min_volume_30d: money("1000000"),
                maker: bps("1.6"),
                taker: bps("4.5"),
            },
            VolumeTier {
                min_volume_30d: money("5000000"),
                maker: bps("1.4"),
                taker: bps("4"),
            },
        ])
        .expect("table is valid")
    }

    #[test]
    fn fee_on_notional_is_hand_checkable() {
        // 100_000 USDT at 5.5 bps = 100000 * 0.00055 = 55 USDT exactly.
        let got = fee_on_notional(money("100000"), bps("5.5")).expect("ok");
        assert_eq!(got, money("55"));
    }

    #[test]
    fn fee_rounds_against_us() {
        // 1 USDT at 1 bp = 0.0001 exactly, no remainder. Pick a notional that
        // does leave one: 1 unit of the smallest representable money at 1 bp is
        // 1e-12 * 1e-4 = 1e-16, far below the 1e-12 scale, so the true answer
        // is a fraction of one ulp. Rounded against us that becomes one ulp,
        // not zero: we never report a fee of nothing on a non-zero notional.
        let got = fee_on_notional(Money::EPSILON, bps("1")).expect("ok");
        assert_eq!(got, Money::EPSILON);
    }

    #[test]
    fn zero_notional_pays_no_fee() {
        assert_eq!(fee_on_notional(Money::ZERO, bps("5.5")).expect("ok"), Money::ZERO);
    }

    #[test]
    fn negative_fee_is_refused_not_treated_as_revenue() {
        let rebate = bps("1").checked_neg().expect("ok");
        assert_eq!(
            fee_on_notional(money("100000"), rebate),
            Err(MathError::NegativeFeeRate)
        );
    }

    #[test]
    fn unknown_volume_resolves_to_the_worst_tier() {
        let r = table().resolve(None).expect("ok");
        // Worst taker in the table is 5.5 bps, in the zero-volume tier.
        assert_eq!(r.taker, bps("5.5"));
        assert_eq!(r.maker, bps("2"));
        assert!(!r.tier_known);
    }

    #[test]
    fn known_volume_resolves_to_the_right_tier() {
        let t = table();
        let low = t.resolve(Some(money("500000"))).expect("ok");
        assert_eq!(low.taker, bps("5.5"));
        assert!(low.tier_known);

        let mid = t.resolve(Some(money("1000000"))).expect("ok");
        assert_eq!(mid.taker, bps("4.5"));

        let high = t.resolve(Some(money("9999999"))).expect("ok");
        assert_eq!(high.taker, bps("4"));
    }

    #[test]
    fn threshold_is_inclusive_at_the_boundary() {
        // Exactly 5_000_000 qualifies for the 5_000_000 tier. One ulp below
        // does not. Off-by-one here silently applies the wrong fee forever.
        let t = table();
        let at = t.resolve(Some(money("5000000"))).expect("ok");
        assert_eq!(at.taker, bps("4"));
        let below = money("5000000").checked_sub(Money::EPSILON).expect("ok");
        assert_eq!(t.resolve(Some(below)).expect("ok").taker, bps("4.5"));
    }

    #[test]
    fn worst_tier_is_found_by_comparing_rates_not_by_position() {
        // A promotional top tier that is more expensive than the base tier.
        // Taking the first row would report 5 bps as the worst; the real worst
        // is 9 bps.
        let t = FeeTable::new(vec![
            VolumeTier {
                min_volume_30d: Money::ZERO,
                maker: bps("2"),
                taker: bps("5"),
            },
            VolumeTier {
                min_volume_30d: money("1000000"),
                maker: bps("3"),
                taker: bps("9"),
            },
        ])
        .expect("ok");
        assert_eq!(t.worst().expect("ok").taker, bps("9"));
    }

    #[test]
    fn unsorted_table_is_refused() {
        let got = FeeTable::new(vec![
            VolumeTier {
                min_volume_30d: money("1000000"),
                maker: bps("1"),
                taker: bps("2"),
            },
            VolumeTier {
                min_volume_30d: Money::ZERO,
                maker: bps("2"),
                taker: bps("5"),
            },
        ]);
        assert_eq!(got, Err(MathError::UnsortedTierTable));
    }

    #[test]
    fn a_duplicated_threshold_is_refused_rather_than_resolved_cheaply() {
        // Two rows starting at the same volume. Nothing about this pair is
        // out of order, so a `<=` sortedness check waves it through — and then
        // `resolve` keeps the LAST qualifying row, which here charges 3 bps
        // instead of 5.5. A transcription slip in a fixture would have shaved
        // 2.5 bps off every leg, i.e. 10 bps off a round trip, against a stated
        // edge of 12.
        let got = FeeTable::new(vec![
            VolumeTier {
                min_volume_30d: Money::ZERO,
                maker: bps("2"),
                taker: bps("5.5"),
            },
            VolumeTier {
                min_volume_30d: Money::ZERO,
                maker: bps("1"),
                taker: bps("3"),
            },
        ]);
        assert_eq!(got, Err(MathError::DuplicateTierThreshold));

        // Distinct from unsortedness, so a caller can tell a mis-ordered table
        // from an ambiguous one without matching on prose.
        assert_ne!(got, Err(MathError::UnsortedTierTable));

        // A duplicate deeper in the table is caught too, not just at the head.
        let deeper = FeeTable::new(vec![
            VolumeTier {
                min_volume_30d: Money::ZERO,
                maker: bps("2"),
                taker: bps("5.5"),
            },
            VolumeTier {
                min_volume_30d: money("1000000"),
                maker: bps("1.6"),
                taker: bps("4.5"),
            },
            VolumeTier {
                min_volume_30d: money("1000000"),
                maker: bps("1.4"),
                taker: bps("4"),
            },
        ]);
        assert_eq!(deeper, Err(MathError::DuplicateTierThreshold));
    }

    #[test]
    fn empty_table_is_refused() {
        assert_eq!(FeeTable::new(vec![]), Err(MathError::EmptyTierTable));
    }

    #[test]
    fn round_trip_is_four_legs_not_one() {
        // 5.5 bps taker, four taker legs = 22 bps. This is the number that
        // turns a "12 bps spread" into a loss, and it is why the round trip is
        // a type rather than a multiplication someone might forget.
        let rates = FeeRates::new(bps("2"), bps("5.5"), true).expect("ok");
        assert_eq!(RoundTrip::ALL_TAKER.total_bps(rates).expect("ok"), bps("22"));
    }

    #[test]
    fn mixed_roles_sum_correctly() {
        // maker 2 + taker 5.5 + maker 2 + taker 5.5 = 15 bps.
        let rates = FeeRates::new(bps("2"), bps("5.5"), true).expect("ok");
        let rt = RoundTrip {
            entry_a: FeeRole::Maker,
            entry_b: FeeRole::Taker,
            exit_a: FeeRole::Maker,
            exit_b: FeeRole::Taker,
        };
        assert_eq!(rt.total_bps(rates).expect("ok"), bps("15"));
    }

    #[test]
    fn flat_table_resolves_to_itself() {
        let t = FeeTable::flat(bps("1"), bps("6")).expect("ok");
        let r = t.resolve(Some(money("123"))).expect("ok");
        assert_eq!((r.maker, r.taker, r.tier_known), (bps("1"), bps("6"), true));
    }

    #[test]
    fn negative_rate_in_a_tier_is_refused() {
        let got = FeeTable::new(vec![VolumeTier {
            min_volume_30d: Money::ZERO,
            maker: bps("1").checked_neg().expect("ok"),
            taker: bps("5"),
        }]);
        assert_eq!(got, Err(MathError::NegativeFeeRate));
    }

    #[test]
    fn the_three_reported_configurations_are_hand_computed() {
        // Schedule 2 / 5.5 bps, the one `OPEN.md` A6 argues from.
        //   all-taker    5.5 * 4                 = 22   bps
        //   hedged-entry 2 + 5.5 + 5.5 + 5.5     = 18.5 bps
        //   all-maker    2 * 4                   =  8   bps
        let rates = FeeRates::new(bps("2"), bps("5.5"), true).expect("ok");
        let totals: Vec<Bps> = RoundTrip::REPORTED
            .iter()
            .map(|rt| rt.total_bps(rates).expect("ok"))
            .collect();
        assert_eq!(totals, vec![bps("22"), bps("18.5"), bps("8")]);
    }

    #[test]
    fn reported_configurations_are_ordered_most_expensive_first() {
        // The ordering is load-bearing, not cosmetic: the leftmost column is the
        // one a reader treats as the headline, and it must be the conservative one.
        let rates = FeeRates::new(bps("2"), bps("5.5"), true).expect("ok");
        let mut prev: Option<Bps> = None;
        for rt in RoundTrip::REPORTED {
            let total = rt.total_bps(rates).expect("ok");
            if let Some(p) = prev {
                assert!(total < p, "{} is not cheaper than its predecessor", rt.label());
            }
            prev = Some(total);
        }
    }

    #[test]
    fn all_maker_is_the_floor_and_all_taker_the_ceiling() {
        // No configuration of four legs can beat all-maker or exceed all-taker,
        // because every leg is one of the two rates. Checked over all 16.
        let rates = FeeRates::new(bps("2"), bps("5.5"), true).expect("ok");
        let floor = RoundTrip::ALL_MAKER.total_bps(rates).expect("ok");
        let ceiling = RoundTrip::ALL_TAKER.total_bps(rates).expect("ok");
        let roles = [FeeRole::Maker, FeeRole::Taker];
        let mut seen = 0;
        for ea in roles {
            for eb in roles {
                for xa in roles {
                    for xb in roles {
                        let rt = RoundTrip {
                            entry_a: ea,
                            entry_b: eb,
                            exit_a: xa,
                            exit_b: xb,
                        };
                        let t = rt.total_bps(rates).expect("ok");
                        assert!(t >= floor && t <= ceiling);
                        seen += 1;
                    }
                }
            }
        }
        assert_eq!(seen, 16);
    }

    #[test]
    fn labels_name_only_the_named() {
        assert_eq!(RoundTrip::ALL_TAKER.label(), "all-taker");
        assert_eq!(RoundTrip::HEDGED_ENTRY.label(), "hedged-entry");
        assert_eq!(RoundTrip::ALL_MAKER.label(), "all-maker");
        // A fifth configuration says it is unnamed rather than borrowing a name.
        let odd = RoundTrip {
            entry_a: FeeRole::Taker,
            entry_b: FeeRole::Maker,
            exit_a: FeeRole::Maker,
            exit_b: FeeRole::Taker,
        };
        assert_eq!(odd.label(), "custom");
    }

    #[test]
    fn cross_venue_charges_each_leg_its_own_schedule() {
        // Venue A cheap (1 / 4), venue B dear (3 / 7). All taker:
        //   entry_a 4 + entry_b 7 + exit_a 4 + exit_b 7 = 22 bps.
        // Using only A's schedule for both legs would give 16 — an understatement
        // of 6 bps, which is half the edge this project claims to hunt.
        let a = FeeRates::new(bps("1"), bps("4"), true).expect("ok");
        let b = FeeRates::new(bps("3"), bps("7"), true).expect("ok");
        assert_eq!(
            RoundTrip::ALL_TAKER.total_bps_cross(a, b).expect("ok"),
            bps("22")
        );
        assert_eq!(RoundTrip::ALL_TAKER.total_bps(a).expect("ok"), bps("16"));
    }

    #[test]
    fn leg_order_matters_for_an_asymmetric_configuration() {
        // HEDGED_ENTRY rests leg a. With A maker 1 and B maker 3:
        //   a rests: 1 + 7 + 4 + 7 = 19 bps
        //   b rests: 3 + 4 + 7 + 4 = 18 bps
        // One basis point apart on the same trade, decided purely by argument
        // order — which is why `total_bps_cross` documents it and `is_symmetric`
        // exists to let a caller assert it does not matter.
        let a = FeeRates::new(bps("1"), bps("4"), true).expect("ok");
        let b = FeeRates::new(bps("3"), bps("7"), true).expect("ok");
        assert_eq!(
            RoundTrip::HEDGED_ENTRY.total_bps_cross(a, b).expect("ok"),
            bps("19")
        );
        assert_eq!(
            RoundTrip::HEDGED_ENTRY.total_bps_cross(b, a).expect("ok"),
            bps("18")
        );
        assert!(!RoundTrip::HEDGED_ENTRY.is_symmetric());
    }

    #[test]
    fn leg_order_cannot_matter_for_a_symmetric_configuration() {
        let a = FeeRates::new(bps("1"), bps("4"), true).expect("ok");
        let b = FeeRates::new(bps("3"), bps("7"), true).expect("ok");
        for rt in [RoundTrip::ALL_TAKER, RoundTrip::ALL_MAKER] {
            assert!(rt.is_symmetric());
            assert_eq!(
                rt.total_bps_cross(a, b).expect("ok"),
                rt.total_bps_cross(b, a).expect("ok"),
            );
        }
        // And the asymmetric one is honest about not qualifying.
        assert!(!RoundTrip::HEDGED_ENTRY.is_symmetric());
    }
}
