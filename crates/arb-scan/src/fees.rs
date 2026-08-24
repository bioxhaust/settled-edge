//! The bridge from `arb-venue`'s [`FeeSchedule`] to `arb-math`'s [`FeeRates`],
//! and the four-leg cost of a **cross-venue** round trip.
//!
//! # Why the bridge exists and why it lives here
//!
//! Two crates hold one concept. `arb-venue` has [`FeeSchedule`] because it must
//! not depend on `arb-math`; `arb-math` has [`FeeRates`] because it must not
//! depend on `arb-venue`. `arb-scan` is the only crate that sees both, so the
//! conversion is ours (`OPEN.md` E7). The whole risk of a duplicated
//! representation is that a field is dropped in translation, and there is exactly
//! one field that matters: `tier_known`. Dropping it turns "these are the worst
//! rates we could imagine, we never read the account" into "these are our rates",
//! and the flag [`QualityFlags::FEE_TIER_ASSUMED`] never reaches the screen.
//! [`bridge`] therefore returns the rates *and* the quality contribution
//! together, so a caller cannot take one without the other.
//!
//! # Why a four-leg total is not `RoundTrip::total_bps`
//!
//! [`RoundTrip::total_bps`] sums four legs against **one** [`FeeRates`], which is
//! right for a two-leg position on one venue and wrong for the cross-venue carry
//! this crate ranks. Our fixtures already disagree by 1 bp between the two BTC
//! venues with the widest funding spread (MEXC 6 bps taker, OKX 5 bps), and 1 bp
//! is a twelfth of the edge.
//!
//! Two wrong ways to handle it, and why each is wrong:
//!
//! - Use one venue's schedule for both legs. Picking the cheaper understates cost
//!   and manufactures edge. Picking the dearer overstates cost, which is "safe"
//!   but is still a number that is not the money that will leave the account —
//!   and the product promise is equality, not conservatism for its own sake.
//! - Average the two. An average is neither leg's fee and is wrong on both.
//!
//! [`round_trip_bps`] charges each leg its own venue's rate for its own two
//! crossings. [`round_trip_bps`] reduces exactly to [`RoundTrip::total_bps`] when
//! the two schedules are equal, which is asserted by test so the two paths cannot
//! drift.

use arb_math::{FeeRates, MathError, RoundTrip};
use arb_num::Bps;
use arb_types::{DataQuality, Provenance, QualityFlags};
use arb_venue::FeeSchedule;

use crate::error::ScanError;

/// A venue's fees together with the quality mark they force onto any number
/// computed from them.
///
/// One struct rather than two return values because the pair must travel
/// together: the rates without the quality is exactly the mistake this module
/// exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgedFees {
    /// The rates, in `arb-math`'s representation.
    pub rates: FeeRates,
    /// Provenance and flags contributed by these fees.
    /// [`QualityFlags::FEE_TIER_ASSUMED`] whenever `tier_known` was false.
    pub quality: DataQuality,
}

/// Convert a venue fee schedule into economics-crate rates, preserving
/// `tier_known` as both a field and a flag.
///
/// `provenance` is a parameter because [`FeeSchedule`] has nowhere to carry it —
/// [`arb_venue::ExchangeAdapter::fee_schedule`] returns a plain value type shared
/// with a future live adapter. The caller must state where the numbers came from.
/// For anything read out of a `.fix` file that is
/// [`Provenance::SynthesizedUnverified`]; `arb_venue::FixtureAdapter::fee_quality`
/// supplies it directly and should be preferred over guessing here.
///
/// Refuses a negative rate, because [`FeeRates::new`] does: `arb-venue` permits a
/// maker rebate and `arb-math` does not model one. A rebate turns a cost into
/// revenue and inverts every conservative rounding decision downstream, so it is
/// refused rather than silently treated as a cost (merely pessimistic) or as
/// revenue (flattering, and therefore the dangerous one).
pub fn bridge(schedule: FeeSchedule, provenance: Provenance) -> Result<BridgedFees, ScanError> {
    let rates = FeeRates::new(schedule.maker, schedule.taker, schedule.tier_known)?;
    Ok(BridgedFees {
        rates,
        quality: DataQuality::new(schedule.flags(), provenance),
    })
}

/// Convert a venue fee schedule with a quality value already supplied by the
/// adapter.
///
/// Preferred over [`bridge`] wherever the adapter can produce the quality itself
/// (`arb_venue::FixtureAdapter::fee_quality` can), because it removes our chance
/// to attach the wrong provenance. The schedule's own flags are unioned in rather
/// than assumed to be present, so a caller that passes
/// [`DataQuality::MEASURED`] for a schedule whose tier is unknown still ends up
/// with [`QualityFlags::FEE_TIER_ASSUMED`] set.
pub fn bridge_with_quality(
    schedule: FeeSchedule,
    quality: DataQuality,
) -> Result<BridgedFees, ScanError> {
    let rates = FeeRates::new(schedule.maker, schedule.taker, schedule.tier_known)?;
    Ok(BridgedFees {
        rates,
        quality: quality.with_flags(schedule.flags()),
    })
}

/// The flags a bridged schedule forces, independent of the rest of the quality.
///
/// Exposed so a caller assembling a row's quality by hand cannot forget the one
/// flag this module is about.
pub const fn tier_flags(schedule: FeeSchedule) -> QualityFlags {
    schedule.flags()
}

/// Total fee cost of a two-leg round trip across two venues, in basis points of
/// one leg's notional.
///
/// Leg A pays venue A's rate on its entry and its exit; leg B pays venue B's. The
/// roles come from [`RoundTrip`], so a maker/taker assumption configured once
/// applies to both venues.
///
/// # The assumption in the unit
///
/// "Basis points of one leg's notional" is only a legitimate summary when both
/// legs carry the same notional. That is exactly true for the linear perp-perp
/// and spot-perp pairs this crate ranks, because both legs are sized to the same
/// [`crate::config::ScanConfig::target_notional`]. Where it is not true — an
/// inverse leg whose lot granularity forces a different realised notional — the
/// caller must charge each leg separately with
/// [`arb_math::fees::fee_on_notional`]. [`crate::funding`] reports both legs'
/// realised notionals so the discrepancy is visible rather than assumed away.
pub fn round_trip_bps(
    round_trip: RoundTrip,
    a: FeeRates,
    b: FeeRates,
) -> Result<Bps, ScanError> {
    // Summed as four separate additions rather than two doublings: the roles are
    // allowed to differ between entry and exit (rest on the way in, cross on the
    // way out is a real configuration), and `2 * entry` would silently ignore
    // `exit_a`.
    let leg_a = a
        .rate(round_trip.entry_a)
        .checked_add(a.rate(round_trip.exit_a))
        .map_err(MathError::from)?;
    let leg_b = b
        .rate(round_trip.entry_b)
        .checked_add(b.rate(round_trip.exit_b))
        .map_err(MathError::from)?;
    leg_a
        .checked_add(leg_b)
        .map_err(MathError::from)
        .map_err(ScanError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_math::FeeRole;

    fn bps(s: &str) -> Bps {
        Bps::parse_bps(s).expect("test bps parses")
    }

    #[test]
    fn tier_unknown_survives_the_bridge_as_field_and_flag() {
        // 0.02% maker / 0.055% taker, tier unknown: the shape every fixture uses.
        let s = FeeSchedule::assumed(bps("2"), bps("5.5"));
        let got = bridge(s, Provenance::SynthesizedUnverified).expect("bridges");
        assert!(!got.rates.tier_known);
        assert_eq!(got.rates.taker, bps("5.5"));
        assert!(got.quality.flags.contains(QualityFlags::FEE_TIER_ASSUMED));
        assert_eq!(got.quality.provenance, Provenance::SynthesizedUnverified);
    }

    #[test]
    fn tier_known_sets_no_flag() {
        let s = FeeSchedule::known(bps("2"), bps("5.5"));
        let got = bridge(s, Provenance::LiveMeasured).expect("bridges");
        assert!(got.rates.tier_known);
        assert!(got.quality.flags.is_empty());
    }

    #[test]
    fn measured_quality_cannot_launder_an_assumed_tier() {
        // The specific failure this guards: a caller hands over MEASURED quality
        // for a schedule whose tier nobody read. The flag must still appear.
        let s = FeeSchedule::assumed(bps("2"), bps("5.5"));
        let got = bridge_with_quality(s, DataQuality::MEASURED).expect("bridges");
        assert!(got.quality.flags.contains(QualityFlags::FEE_TIER_ASSUMED));
    }

    #[test]
    fn maker_rebate_is_refused_rather_than_re_signed() {
        let rebate = bps("1").checked_neg().expect("negates");
        let s = FeeSchedule::known(rebate, bps("5"));
        assert_eq!(
            bridge(s, Provenance::LiveMeasured),
            Err(ScanError::Math(MathError::NegativeFeeRate))
        );
    }

    #[test]
    fn cross_venue_round_trip_charges_each_leg_its_own_venue() {
        // MEXC 6 bps taker, OKX 5 bps taker, all four legs taker:
        //   entry MEXC 6 + exit MEXC 6 + entry OKX 5 + exit OKX 5 = 22 bps.
        // Using MEXC for both legs would give 24; using OKX for both, 20. The
        // truth is 22 and neither shortcut finds it.
        let mexc = FeeRates::new(bps("2"), bps("6"), false).expect("rates");
        let okx = FeeRates::new(bps("2"), bps("5"), false).expect("rates");
        let got = round_trip_bps(RoundTrip::ALL_TAKER, mexc, okx).expect("sums");
        assert_eq!(got, bps("22"));
    }

    #[test]
    fn equal_schedules_reduce_to_arb_maths_own_total() {
        // The anti-drift assertion. With one schedule on both legs our per-venue
        // sum must be arb-math's number to the last raw unit, or the two
        // implementations have diverged.
        let r = FeeRates::new(bps("2"), bps("5.5"), true).expect("rates");
        for rt in [
            RoundTrip::ALL_TAKER,
            RoundTrip {
                entry_a: FeeRole::Maker,
                entry_b: FeeRole::Taker,
                exit_a: FeeRole::Maker,
                exit_b: FeeRole::Taker,
            },
        ] {
            assert_eq!(
                round_trip_bps(rt, r, r).expect("sums"),
                rt.total_bps(r).expect("sums")
            );
        }
    }

    #[test]
    fn mixed_roles_charge_the_configured_leg() {
        // Rest leg A on the way in (maker 2), cross on the way out (taker 6);
        // cross leg B both ways (5 + 5). 2 + 6 + 5 + 5 = 18 bps.
        let a = FeeRates::new(bps("2"), bps("6"), true).expect("rates");
        let b = FeeRates::new(bps("1"), bps("5"), true).expect("rates");
        let rt = RoundTrip {
            entry_a: FeeRole::Maker,
            exit_a: FeeRole::Taker,
            entry_b: FeeRole::Taker,
            exit_b: FeeRole::Taker,
        };
        assert_eq!(round_trip_bps(rt, a, b).expect("sums"), bps("18"));
    }

    #[test]
    fn tier_flags_agrees_with_the_schedule() {
        assert!(tier_flags(FeeSchedule::assumed(bps("2"), bps("5")))
            .contains(QualityFlags::FEE_TIER_ASSUMED));
        assert!(tier_flags(FeeSchedule::known(bps("2"), bps("5"))).is_empty());
    }
}
