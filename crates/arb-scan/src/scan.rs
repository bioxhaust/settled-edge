#![forbid(unsafe_code)]
//! Pair enumeration and the economics of one row.
//!
//! # The one arithmetic mistake this module exists to prevent
//!
//! Every funding scanner on the market computes the carry as
//! `rate_a - rate_b`, annualised at 3 settlements a day. That is wrong whenever
//! the two venues settle on **different intervals**, which they routinely do:
//! Binance and OKX pay 8-hourly, Bybit sometimes 4, MEXC sometimes 1. Subtracting
//! an 8-hour rate from a 1-hour rate produces a number in no unit at all, and
//! annualising it multiplies the nonsense by a thousand.
//!
//! Here each leg is annualised **separately**, after the sign law has been applied
//! to it, and the two signed annual receipts are **summed**:
//!
//! ```text
//!   gross_apr = annual_receipt(short leg) + annual_receipt(long leg)
//! ```
//!
//! `annual_receipt` is negative for a leg that pays. So the sum is a difference
//! when the two rates have the same sign, and genuinely a sum when they do not —
//! which is the case that pays best and the case the naive formula gets most
//! wrong. Two legs both paying us is not exotic: a positive rate on the venue we
//! short and a negative rate on the venue we long is the widest configuration in
//! our own fixture set (MEXC +0.0003 against OKX −0.00005).
//!
//! # Costs
//!
//! `net_apr` subtracts, amortised over the configured horizon:
//!
//! - **four fee legs** — enter both, exit both. A "spread minus fees" display that
//!   charges one leg understates the cost by a factor of four.
//! - **depth cost on both legs, twice** — entry and exit. The entry cost is
//!   measured by walking the real book for the real size. The exit walks a book we
//!   cannot see, so it is **assumed to mirror the entry**; that assumption is
//!   recorded in `OPEN.md` and is the largest known unknown in this number.
//!
//! Slippage enters the cost as bps of notional and is therefore comparable with
//! the fee total, which is the only reason the two can be added at all.

use arb_detect::{age_of, assess_age, assess_paint, assess_skew, is_crossed, ClockVerdict, Staleness};
use arb_math::{
    executable_price, liquidation_price, margin_required,
    max_size_within_slippage, net_apr_over_horizon, periods_to_breakeven, round_down_to_lot,
    slippage_bps, CarryLeg, LiquidationInputs, MathError,
};
use arb_num::{Bps, Money, Price, Qty, Rate, RoundMode};
use arb_types::{ContractSize, ContractSpec, DataQuality, MarketKind, PositionSide, QualityFlags, Side, VenueKey};

use crate::config::ScanConfig;
use crate::error::{PairRefusal, ScanError};
use crate::fees::{bridge_with_quality, round_trip_bps};
use crate::leg::LegInput;
use crate::opportunity::{OppLeg, Opportunity, Unrankable};

/// Whether two legs can form a trade at all.
///
/// Refusals, and why each one is a refusal rather than a warning:
///
/// - **same instrument** — a position against itself has no carry and no risk; it
///   is a fee generator. Also catches a duplicated fixture.
/// - **base or quote mismatch** — BTC funding against ETH funding is a directional
///   bet wearing an arbitrage costume. The carry may look identical; the risk is
///   not remotely the same trade.
/// - **linear against inverse** — the two legs' quantities are in different units
///   (base vs contracts) and their exposures move differently with price. A hedge
///   built from one of each is not flat, and the notional arithmetic that would
///   have to reconcile them is exactly the ~68 000× error `ContractSize` exists to
///   make unrepresentable.
/// - **same venue, perp against perp** — nets to nothing on the exchange's own
///   books and pays funding on neither side.
/// - **same venue, spot against spot** — likewise, and it is not even a hedge.
///
/// Spot against perp on the **same** venue is deliberately *allowed*: that is cash
/// and carry, it is a real trade, and the venue does not net the two against each
/// other.
pub fn pair_check(a: &LegInput, b: &LegInput) -> Result<(), PairRefusal> {
    let (ka, kb) = (a.key(), b.key());
    if a.vkey() == b.vkey() {
        return Err(PairRefusal::SameInstrument);
    }
    if ka.base.as_str() != kb.base.as_str() {
        return Err(PairRefusal::BaseMismatch);
    }
    if ka.quote.as_str() != kb.quote.as_str() {
        return Err(PairRefusal::QuoteMismatch);
    }
    let inverse_a = a.kind() == MarketKind::InversePerp;
    let inverse_b = b.kind() == MarketKind::InversePerp;
    if inverse_a != inverse_b {
        return Err(PairRefusal::LinearAgainstInverse);
    }
    if a.venue() == b.venue() {
        if a.kind().is_perp() && b.kind().is_perp() {
            return Err(PairRefusal::SameVenuePerpPerp);
        }
        if !a.kind().is_perp() && !b.kind().is_perp() {
            return Err(PairRefusal::SameVenueSpotSpot);
        }
    }
    Ok(())
}

/// Signed annualised funding receipt for holding `leg` on `side`.
///
/// Spot pays and receives no funding, so its receipt is exactly zero — not a
/// missing value. Treating it as missing would make every cash-and-carry row
/// unrankable, and zero is the honest number rather than a stand-in for one.
fn annual_receipt(leg: &LegInput, side: PositionSide) -> Result<Rate, ScanError> {
    match leg.funding.as_ref() {
        None => Ok(Rate::ZERO),
        Some(snap) => Ok(CarryLeg::from_snapshot(snap, side).annual_receipt()?),
    }
}

/// Decide which leg to short.
///
/// Both assignments are priced and the better gross wins, rather than applying the
/// folk rule "short the higher rate". The folk rule is right only when both legs
/// settle on the same interval and neither is spot: a +0.0001 per **1h** rate
/// annualises to more than a +0.0005 per **8h** rate, so comparing the published
/// numbers ranks them backwards. Comparing the annualised receipts cannot.
///
/// Returns `(short, long)`.
fn assign_sides<'a>(
    a: &'a LegInput,
    b: &'a LegInput,
) -> Result<(&'a LegInput, &'a LegInput, Rate), ScanError> {
    let a_short = annual_receipt(a, PositionSide::Short)?
        .checked_add(annual_receipt(b, PositionSide::Long)?)?;
    let b_short = annual_receipt(b, PositionSide::Short)?
        .checked_add(annual_receipt(a, PositionSide::Long)?)?;
    if b_short > a_short {
        Ok((b, a, b_short))
    } else {
        // Ties go to `a` short, which is the enumeration order, so a tie does not
        // reorder between runs.
        Ok((a, b, a_short))
    }
}

/// The side of the book we take to **open** this leg.
///
/// Short means we sell into bids; long means we buy from asks. `Side::Sell` reads
/// the bid ladder and `Side::Buy` the ask ladder, so the mapping is direct — but
/// getting it backwards would price every fill against the wrong side of the
/// spread and understate the cost by the full width, which is why it is a named
/// function and not an inline match.
const fn opening_side(side: PositionSide) -> Side {
    match side {
        PositionSide::Short => Side::Sell,
        PositionSide::Long => Side::Buy,
    }
}

/// Everything about one leg that does not depend on the finally-chosen size.
struct LegPlan {
    capacity: Qty,
    taking: Side,
}

/// The largest size this leg can absorb inside the slippage budget.
/// Shrink `size` until the long leg's realised notional fits `cfg.target_notional`.
///
/// See the call site in [`price_pair`] for why this is necessary and why it
/// terminates. Returns the largest lot-quantised size that fits, or
/// [`MathError::ZeroSize`] if nothing does.
fn fit_to_budget(
    mut size: Qty,
    lot: Qty,
    long: &LegInput,
    taking: Side,
    cfg: &ScanConfig,
) -> Result<Qty, ScanError> {
    // Sixteen halvings of a lot-quantised size covers any book this will ever see;
    // it exists so a pathological ladder cannot hang the scan.
    const MAX_PASSES: usize = 16;
    for _ in 0..MAX_PASSES {
        let walk = executable_price(&long.book, taking, size)?;
        let realised = notional_of(size, walk.vwap, &long.instrument.spec, RoundMode::COST)?;
        if realised <= cfg.target_notional {
            return Ok(size);
        }
        let resolved = round_down_to_lot(
            requested_in_book_units(cfg.target_notional, walk.vwap, &long.instrument.spec)?,
            lot,
        )?;
        size = if resolved < size {
            resolved
        } else {
            // The re-solve did not move it — step down one lot so the loop makes
            // progress rather than re-testing the same size sixteen times.
            size.checked_sub(lot)?
        };
        if !size.is_positive() {
            return Err(ScanError::Math(MathError::ZeroSize));
        }
    }
    Err(ScanError::Math(MathError::ZeroSize))
}

/// The size the configured notional asks for, **in the venue's own order-size
/// unit** — the unit the order book and `lot_size` are denominated in.
///
/// # Why this is not `contracts_for_notional`
///
/// `arb_math::contracts_for_notional` returns a *count of contracts*, which is
/// what an order is placed in, and every `arb_types` function that takes a `Qty`
/// beside a [`ContractSpec`] means contracts too. An order **book**, however, is
/// denominated in the unit the venue quotes sizes in: base for linear and spot,
/// contracts for inverse. On OKX's linear BTC swap (0.01 BTC per contract) those
/// two numbers differ by 100×, and on Gate's (0.0001) by 10 000×.
///
/// Comparing a contract count against a base-denominated ladder is therefore not
/// a rounding difference, it is a wrong answer by a factor of the contract size —
/// and because Binance's contract size is exactly 1, it is a wrong answer that
/// looks right on the venue most people check first. Every size in this module is
/// in book units, and the conversion to contracts happens in exactly one place,
/// [`notional_of`].
///
/// The inverse branch is **not exercised by the shipped fixtures**: the only
/// inverse instrument among them is quoted in USD, and [`pair_check`] refuses it
/// against every USDT leg on quote mismatch. It is written out rather than left
/// as a `todo!` because a silent fall-through to the linear formula is precisely
/// the class of error this function exists to prevent.
fn requested_in_book_units(
    target: Money,
    price: Price,
    spec: &ContractSpec,
) -> Result<Qty, ScanError> {
    // Round down: the size we ask for must not exceed the notional we budgeted.
    let mode = RoundMode::REVENUE;
    let q = match spec.size {
        // Book in base. Base exposure of `target` quote at `price`.
        ContractSize::BasePerContract(_) => target.div_price(price, mode)?,
        // Book in contracts, and an inverse contract's quote notional is fixed by
        // the contract definition, so no price enters.
        ContractSize::QuotePerContract(quote_per) => target
            .reinterpret_as_qty()
            .div_size(quote_per.reinterpret_as_qty(), mode)?,
    };
    Ok(q)
}

/// Quote notional of `size` **book units** at `price`.
///
/// The one place where book units meet [`ContractSpec`]. Deliberately not
/// `spec.notional(size, ..)`: that takes contracts, so feeding it a base size
/// understates the notional — and therefore the margin — by the contract size.
///
/// Each branch is a single multiplication rather than a divide-to-contracts
/// followed by a multiply-back, which would round twice for the same answer.
fn notional_of(size: Qty, price: Price, spec: &ContractSpec, mode: RoundMode) -> Result<Money, ScanError> {
    let m = match spec.size {
        // size is base: notional = base * price. Identical to
        // `contracts * base_per_contract * price` with contracts = base/base_per.
        ContractSize::BasePerContract(_) => size.mul_price(price, mode)?,
        // size is already a contract count, so the spec's own formula applies.
        ContractSize::QuotePerContract(_) => spec.notional(size, price, mode)?,
    };
    Ok(m)
}

fn plan_leg(leg: &LegInput, side: PositionSide, cfg: &ScanConfig) -> Result<LegPlan, ScanError> {
    let taking = opening_side(side);
    if leg.book.touch(taking).is_none() {
        return Err(ScanError::NoLiquidity(leg.vkey(), taking));
    }
    let capacity = max_size_within_slippage(&leg.book, taking, cfg.slippage_budget)?;
    Ok(LegPlan { capacity, taking })
}

/// Data-quality verdicts for one leg, and the blockers they imply.
fn assess_leg(
    leg: &LegInput,
    taking: Side,
    cfg: &ScanConfig,
    blocked: &mut Vec<Unrankable>,
) -> Result<(Staleness, Option<Staleness>, ClockVerdict, u8, QualityFlags), ScanError> {
    let vkey = leg.vkey();
    let mut flags = QualityFlags::NONE;

    // Book freshness, measured from the observation time the snapshot carries —
    // never from "now", which would make every snapshot fresh by construction.
    let book_age = assess_age(age_of(leg.book.observed_at, cfg.now), cfg.book_staleness);
    if book_age != Staleness::Fresh {
        flags = flags.union(QualityFlags::STALE);
        if book_age != Staleness::Warn {
            blocked.push(Unrankable::StaleBook(vkey, book_age));
        }
    }

    // Funding freshness. Absent for spot, and absent is not stale.
    let funding_age = match leg.funding.as_ref() {
        None => None,
        Some(snap) => {
            let age = assess_age(age_of(snap.observed_at, cfg.now), cfg.funding_staleness);
            if age != Staleness::Fresh {
                flags = flags.union(QualityFlags::STALE);
                if age != Staleness::Warn {
                    blocked.push(Unrankable::StaleFunding(vkey, age));
                }
            }
            Some(age)
        }
    };

    // Clock agreement. Without a trustworthy exchange stamp, "recent" is not
    // establishable on this venue at all, so a skew blocks rather than warns.
    let clock = match leg.book.exchange_ts {
        Some(ts) => assess_skew(ts, leg.book.observed_at, cfg.skew),
        // No exchange stamp is not a skew — it is an absence, already reflected in
        // the snapshot's own flags. Reporting it as alignment would be a lie; it
        // is reported as alignment of the only clock we have, and the missing
        // stamp is what `PARTIAL_BOOK`/provenance are for.
        None => ClockVerdict::Aligned,
    };
    if !matches!(clock, ClockVerdict::Aligned) {
        flags = flags.union(QualityFlags::CLOCK_SKEW);
        blocked.push(Unrankable::ClockSkew(vkey, clock));
    }

    if is_crossed(&leg.book) {
        flags = flags.union(QualityFlags::CROSSED_BOOK);
        blocked.push(Unrankable::CrossedBook(vkey));
    }

    // Threshold of 1: any signal at all blocks the row. `PaintLimits` carries no
    // score threshold of its own — the thresholds it holds are the ones each
    // individual signal fires on — so the choice of how many signals constitute
    // suspicion is the scanner's, and one is the honest answer. Every signal
    // `arb-detect` reports means the visible depth may not be there, and sizing
    // against depth that is not there is precisely the failure this engine exists
    // to prevent. The row is not deleted, only sunk with a reason, so a
    // false positive costs the operator a glance rather than a trade.
    let paint = assess_paint(&leg.book, taking, &cfg.paint)?;
    if paint.is_suspicious_at(1) {
        flags = flags.union(QualityFlags::PAINTED_DEPTH);
        blocked.push(Unrankable::PaintedDepth(vkey, paint.score));
    }

    if !leg.quality().is_trustworthy() {
        blocked.push(Unrankable::Unverified(vkey));
    }

    Ok((book_age, funding_age, clock, paint.score, flags))
}

/// Price one ordered pair into a row.
///
/// `pair_check` must have passed. The size is the minimum of what each leg can
/// absorb inside the budget and what the configuration asked for, floored to the
/// coarser of the two lot sizes — a size one leg cannot express in whole lots is
/// not a size we can hold flat.
pub fn price_pair(a: &LegInput, b: &LegInput, cfg: &ScanConfig) -> Result<Opportunity, ScanError> {
    pair_check(a, b)?;
    let (short, long, gross_apr) = assign_sides(a, b)?;

    let short_plan = plan_leg(short, PositionSide::Short, cfg)?;
    let long_plan = plan_leg(long, PositionSide::Long, cfg)?;

    let mut blocked = Vec::new();

    // A first estimate, converted at the touch.
    let long_touch = match long.book.touch(long_plan.taking) {
        Some(p) => p,
        None => return Err(ScanError::NoLiquidity(long.vkey(), long_plan.taking)),
    };
    let requested =
        requested_in_book_units(cfg.target_notional, long_touch, &long.instrument.spec)?;

    // The binding constraint, and which leg binds it — the operator needs the
    // second half to know which venue to go deeper on.
    let joint_capacity = short_plan.capacity.min(long_plan.capacity);
    let size_before_lot = requested.min(joint_capacity);
    let lot = short.instrument.spec.lot_size.max(long.instrument.spec.lot_size);
    let size = round_down_to_lot(size_before_lot, lot)?;
    if !size.is_positive() {
        return Err(ScanError::Math(MathError::ZeroSize));
    }

    // The estimate above is converted at the touch, but the notional is *realised*
    // at the VWAP, and for a buy the VWAP is worse than the touch. So a size
    // converted at the touch overshoots the budget: on the shipped ETH fixtures a
    // 100 000 target came out as a 100 003.75 position, and the margin with it.
    //
    // 0.4 bp of overshoot does not lose money. Presenting `target_notional` as a
    // budget and then exceeding it does, because every size the operator sets
    // afterwards inherits the same lie, and on a thin book the gap is not 0.4 bp.
    // So the target is a **ceiling on the realised notional**, and the size is
    // corrected down until it holds.
    //
    // Termination: each pass either re-solves at the worse price — strictly
    // smaller, since the VWAP that produced the overshoot is what it divides by —
    // or steps down one lot. Both strictly decrease a positive, lot-quantised
    // size, so the loop cannot spin. The cap is a backstop against a pathological
    // book, and exhausting it refuses the row rather than printing an
    // over-budget one.
    let size = fit_to_budget(size, lot, long, long_plan.taking, cfg)?;
    if joint_capacity < requested {
        let (vkey, available) = if short_plan.capacity <= long_plan.capacity {
            (short.vkey(), short_plan.capacity)
        } else {
            (long.vkey(), long_plan.capacity)
        };
        blocked.push(Unrankable::SizeExceedsDepth {
            vkey,
            requested,
            available,
        });
    }

    let short_leg = price_leg(short, PositionSide::Short, short_plan.taking, size, cfg, &mut blocked)?;
    let long_leg = price_leg(long, PositionSide::Long, long_plan.taking, size, cfg, &mut blocked)?;

    // Notional at the achievable price, not at the touch, and on the leg whose
    // spec is price-dependent. The two legs' notionals differ by the spread; the
    // long leg's is the larger for a buy, so it is the one that must clear a
    // venue minimum and the one margin is posted against.
    let short_notional =
        notional_of(size, short_leg.vwap, &short.instrument.spec, RoundMode::REVENUE)?;
    let long_notional = notional_of(size, long_leg.vwap, &long.instrument.spec, RoundMode::COST)?;

    for (leg, notional) in [(short, short_notional), (long, long_notional)] {
        if let Some(required) = leg.instrument.spec.min_notional {
            if notional < required {
                blocked.push(Unrankable::BelowMinNotional {
                    vkey: leg.vkey(),
                    available: notional,
                    required,
                });
            }
        }
    }

    // Four fee legs.
    let short_fees = bridge_with_quality(short.fees, short.fee_quality)?;
    let long_fees = bridge_with_quality(long.fees, long.fee_quality)?;
    let fee_bps = round_trip_bps(cfg.round_trip, short_fees.rates, long_fees.rates)?;

    // Depth cost on both legs, entry and exit. The doubling is the mirror
    // assumption stated in the module docs.
    let one_way = short_leg.slippage.checked_add(long_leg.slippage)?;
    let slippage_bps_total = one_way.checked_add(one_way)?;
    let cost_bps = fee_bps.checked_add(slippage_bps_total)?;

    let net_apr = net_apr_over_horizon(gross_apr, cost_bps, cfg.horizon)?;

    let margin = short_leg.margin.checked_add(long_leg.margin)?;

    // The nearer liquidation is the one that ends the trade. `None` on a leg means
    // it cannot be liquidated, not that it is safe, so the minimum is taken over
    // the legs that *have* a distance.
    let distance_to_liq = match (short_leg.distance_to_liq, long_leg.distance_to_liq) {
        (Some(s), Some(l)) => Some(s.min(l)),
        (Some(s), None) => Some(s),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    };

    // Breakeven only where "a period" means the same thing on both legs.
    // Otherwise the count would be in a mixed unit, and a number in no unit is
    // worse than an absent one.
    let periods = breakeven_periods(short, long, cost_bps)?;

    let quality = short_leg.quality.combine(long_leg.quality);

    Ok(Opportunity {
        short: short_leg,
        long: long_leg,
        size,
        notional: long_notional,
        gross_apr,
        fee_bps,
        slippage_bps: slippage_bps_total,
        cost_bps,
        net_apr,
        margin,
        distance_to_liq,
        periods_to_breakeven: periods,
        quality,
        blocked,
    })
}

/// Walk one leg's book for `size` and assemble its row fragment.
fn price_leg(
    leg: &LegInput,
    side: PositionSide,
    taking: Side,
    size: Qty,
    cfg: &ScanConfig,
    blocked: &mut Vec<Unrankable>,
) -> Result<OppLeg, ScanError> {
    let (book_age, funding_age, clock, paint_score, flags) =
        assess_leg(leg, taking, cfg, blocked)?;

    let walk = executable_price(&leg.book, taking, size)?;
    let slippage = slippage_bps(walk.vwap, walk.touch, taking)?;

    let notional = notional_of(size, walk.vwap, &leg.instrument.spec, RoundMode::COST)?;
    let margin = margin_required(notional, cfg.leverage)?;

    // Spot cannot be liquidated. Perps can, and where the inputs do not determine
    // a price the answer is `None` rather than an invented one.
    let (liquidation, distance_to_liq) = if leg.kind().is_perp() {
        let inputs = LiquidationInputs {
            kind: leg.kind(),
            side,
            entry: walk.vwap,
            notional,
            margin,
            maintenance_rate: cfg.maintenance_rate,
        };
        (liquidation_price(&inputs)?, inputs.distance_bps()?)
    } else {
        (None, None)
    };

    let quality = leg
        .quality()
        .combine(walk_quality(leg, walk.flags))
        .with_flags(flags);

    Ok(OppLeg {
        vkey: leg.vkey(),
        side,
        published_rate: leg.funding.as_ref().map(|f| f.rate).unwrap_or(Rate::ZERO),
        annual_receipt: annual_receipt(leg, side)?,
        touch: walk.touch,
        vwap: walk.vwap,
        slippage,
        capacity: max_size_within_slippage(&leg.book, taking, cfg.slippage_budget)?,
        margin,
        liquidation,
        distance_to_liq,
        book_age,
        funding_age,
        clock,
        paint_score,
        quality,
    })
}

/// Fold the execution's own flags into the leg's quality without losing the
/// snapshot's provenance.
fn walk_quality(leg: &LegInput, flags: QualityFlags) -> DataQuality {
    DataQuality::new(flags, leg.book.quality.provenance)
}

/// Settlements needed for the carry to cover the round trip.
///
/// `None` when the two legs settle on different intervals (the count would have no
/// unit), when neither leg has funding, or when the carry never covers the cost.
fn breakeven_periods(
    short: &LegInput,
    long: &LegInput,
    cost_bps: Bps,
) -> Result<Option<u64>, ScanError> {
    let (s, l) = match (short.funding.as_ref(), long.funding.as_ref()) {
        (Some(s), Some(l)) => (Some(s), Some(l)),
        // Cash and carry: one leg funds, the other does not. The funded leg's
        // interval is unambiguously "the" interval.
        (Some(s), None) => (Some(s), None),
        (None, Some(l)) => (None, Some(l)),
        (None, None) => return Ok(None),
    };
    let interval = match (s, l) {
        (Some(s), Some(l)) if s.interval == l.interval => s.interval,
        (Some(_), Some(_)) => return Ok(None),
        (Some(s), None) => s.interval,
        (None, Some(l)) => l.interval,
        (None, None) => return Ok(None),
    };
    let _ = interval;

    let mut per_period = Rate::ZERO;
    if let Some(s) = s {
        per_period =
            per_period.checked_add(CarryLeg::from_snapshot(s, PositionSide::Short).receipt_per_period()?)?;
    }
    if let Some(l) = l {
        per_period =
            per_period.checked_add(CarryLeg::from_snapshot(l, PositionSide::Long).receipt_per_period()?)?;
    }

    match periods_to_breakeven(per_period, cost_bps) {
        Ok(n) => Ok(Some(n)),
        // "Never pays for itself" is a legitimate answer about the market, not a
        // failure of the scan.
        Err(MathError::NoBreakeven) => Ok(None),
        Err(e) => Err(ScanError::Math(e)),
    }
}

/// One row per orderable pair, best first.
///
/// # What is not dropped
///
/// A pair refused by [`pair_check`] is not a row and never appears. A pair that
/// *is* a trade but whose data will not support a number **does** appear, with
/// `blocked` populated, and sorts below every rankable row. The distinction
/// matters: the first is "not a trade", the second is "a trade we refuse to price
/// today", and an operator who cannot tell them apart cannot tell a quiet market
/// from a broken feed.
///
/// # What is dropped, and loudly
///
/// A pair whose arithmetic **fails** — no liquidity at all, a size below one lot,
/// an overflow — is returned in the second element rather than silently skipped.
/// Ranking a table while quietly discarding the pairs that errored is how a report
/// comes to describe a market that is not there.
pub fn scan(legs: &[LegInput], cfg: &ScanConfig) -> (Vec<Opportunity>, Vec<(VenueKey, VenueKey, ScanError)>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();

    for (i, a) in legs.iter().enumerate() {
        for b in legs.iter().skip(i + 1) {
            if pair_check(a, b).is_err() {
                continue;
            }
            match price_pair(a, b, cfg) {
                Ok(row) => rows.push(row),
                Err(e) => errors.push((a.vkey(), b.vkey(), e)),
            }
        }
    }

    rows.sort_by(|x, y| x.rank_key().cmp(&y.rank_key()));
    (rows, errors)
}

// ===========================================================================
// Tests
//
// Two kinds. The unit-boundary tests carry expectations computed by hand as
// exact rationals (`Fraction`), never by reading this code's output — a vector
// fitted to the implementation would have passed just as happily before
// `decisions/019` as after it. The end-to-end tests run the shipped fixtures
// through the real adapter and assert invariants that hold whatever the
// fixtures say, which is the only kind of assertion an invented fixture can
// support.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use arb_math::contracts_for_notional;
    use arb_num::{Money, Price, Qty};
    use arb_types::{Asset, ContractSize, ContractSpec, MarketKind, SizeUnit};

    fn spec(kind: MarketKind, size: ContractSize, lot: Qty) -> ContractSpec {
        ContractSpec {
            kind,
            size,
            settle: Asset::new("USDT").expect("asset"),
            tick_size: Price::from_raw(100_000_000_000),
            // Every spec these tests build is linear or spot, and `arb-scan`
            // works in ladder units throughout (decisions/019).
            size_unit: SizeUnit::Base,
            lot_size: lot,
            min_notional: None,
        }
    }

    fn base_per(x: &str) -> ContractSize {
        ContractSize::BasePerContract(Qty::parse(x).expect("qty literal"))
    }

    // -- the unit boundary ------------------------------------------------

    /// 100 000 USDT at 68 126 is 1.467868361565 BTC. Hand-computed:
    /// `Fraction(100000, 68126) * 10**12` truncated toward zero is
    /// 1_467_868_361_565.
    ///
    /// The same request in *contracts* on a 0.01 BTC contract is 100× that, and
    /// that is the number this function used to return.
    #[test]
    fn a_requested_size_is_in_book_units_not_contracts() {
        let target = Money::from_int(100_000).expect("money");
        let price = Price::from_int(68_126).expect("price");

        for cs in ["1", "0.01", "0.001", "0.0001"] {
            let sp = spec(MarketKind::LinearPerp, base_per(cs), Qty::ONE);
            let got = requested_in_book_units(target, price, &sp).expect("sizes");
            assert_eq!(
                got.raw(),
                1_467_868_361_565,
                "contract size {cs} must not change a base-denominated request"
            );
        }
    }

    /// The bug that hid on Binance. A contract size of exactly 1 makes base and
    /// contracts numerically equal, so the venue most people check first is the
    /// one venue where the wrong unit looks right.
    #[test]
    fn a_contract_size_of_one_is_the_only_case_where_both_units_agree() {
        let target = Money::from_int(100_000).expect("money");
        let price = Price::from_int(68_126).expect("price");
        // Sub-lot of 1e-6 so that `contracts_for_notional`'s own lot rounding is a
        // no-op here and the only thing under test is the unit.
        let lot = Qty::parse("0.000001").expect("qty");
        let one = spec(MarketKind::LinearPerp, base_per("1"), lot);
        let hundredth = spec(MarketKind::LinearPerp, base_per("0.01"), lot);

        let contracts_one =
            contracts_for_notional(target, price, &one).expect("contracts");
        let contracts_hundredth =
            contracts_for_notional(target, price, &hundredth).expect("contracts");
        let book_units = requested_in_book_units(target, price, &one).expect("book units");

        // Equal here, once the same lot rounding is applied to both —
        // `contracts_for_notional` rounds to the lot internally and
        // `requested_in_book_units` leaves it to `price_pair`, which rounds once
        // after the capacity minimum rather than twice.
        assert_eq!(
            contracts_one,
            round_down_to_lot(book_units, lot).expect("lot")
        );
        // ...and 100x apart there. Same code path, same target, same price.
        // 1.467868361565 base / 0.01 = 146.786836156533 contracts, truncated to
        // the 1e-6 lot = 146.786836, i.e. raw 146_786_836_000_000 at scale 1e12.
        assert_eq!(contracts_hundredth.raw(), 146_786_836_000_000);
        // Deliberately not asserted as a ratio: 146_786_836_000_000 /
        // 1_467_868_361_565 is 99 in integer division, because the two values
        // were truncated at different scales. The exact raws above say it better.
        assert!(contracts_hundredth.raw() > book_units.raw() * 99);
    }

    /// An inverse contract's count is quote/quote-per-contract, with no price in
    /// it at all: 100 000 USD of 100-USD contracts is 1 000 contracts whatever
    /// BTC costs.
    #[test]
    fn an_inverse_request_does_not_involve_the_price() {
        let target = Money::from_int(100_000).expect("money");
        let sp = spec(
            MarketKind::InversePerp,
            ContractSize::QuotePerContract(Money::from_int(100).expect("money")),
            Qty::ONE,
        );
        for p in [1_i64, 68_126, 250_000] {
            let price = Price::from_int(p).expect("price");
            let got = requested_in_book_units(target, price, &sp).expect("sizes");
            assert_eq!(got, Qty::from_int(1_000).expect("qty"), "price {p} must not matter");
        }
    }

    /// 1.46 BTC at 68 126 is 99 463.96 USDT. Hand-computed: 146/100 * 68126 =
    /// 99463.96 exactly, so 99_463_960_000_000_000 at scale 1e12.
    ///
    /// Feeding the same base size to `spec.notional` on a 0.01 contract returns
    /// 994.6396 — the notional of 1.46 *contracts*, understated 100×, and the
    /// margin with it.
    #[test]
    fn a_notional_is_taken_on_book_units_not_on_a_contract_count() {
        let size = Qty::parse("1.46").expect("qty");
        let price = Price::from_int(68_126).expect("price");
        let sp = spec(MarketKind::LinearPerp, base_per("0.01"), Qty::ONE);

        let got = notional_of(size, price, &sp, RoundMode::COST).expect("notional");
        assert_eq!(got.raw(), 99_463_960_000_000_000);

        let wrong = sp.notional(size, price, RoundMode::COST).expect("notional");
        assert_eq!(wrong.raw(), 994_639_600_000_000);
        assert_eq!(got.raw() / wrong.raw(), 100);
    }

    /// On a spot or 1:1 linear market the two agree, which is exactly why this
    /// needs a test on a market where they do not.
    #[test]
    fn notional_agrees_with_the_spec_when_one_contract_is_one_coin() {
        let size = Qty::parse("1.46").expect("qty");
        let price = Price::from_int(68_126).expect("price");
        let sp = spec(MarketKind::Spot, base_per("1"), Qty::ONE);
        assert_eq!(
            notional_of(size, price, &sp, RoundMode::COST).expect("notional"),
            sp.notional(size, price, RoundMode::COST).expect("notional")
        );
    }

    /// The inverse branch defers to the spec, because there the book unit *is*
    /// the contract count. 1 000 contracts of 100 USD is 100 000 USD.
    #[test]
    fn an_inverse_notional_is_the_spec_formula_unchanged() {
        let size = Qty::from_int(1_000).expect("qty");
        let sp = spec(
            MarketKind::InversePerp,
            ContractSize::QuotePerContract(Money::from_int(100).expect("money")),
            Qty::ONE,
        );
        let price = Price::from_int(68_126).expect("price");
        let got = notional_of(size, price, &sp, RoundMode::COST).expect("notional");
        assert_eq!(got, Money::from_int(100_000).expect("money"));
        assert_eq!(got, sp.notional(size, price, RoundMode::COST).expect("notional"));
    }

    // -- pairing rules ----------------------------------------------------

    /// Each refusal is a trade that would not be a hedge. Asserted by name and
    /// not merely as "an error", because refusing for the wrong reason would
    /// hide a pair we should be pricing.
    #[test]
    fn the_pairing_rules_refuse_what_is_not_a_hedge() {
        use crate::error::PairRefusal::*;
        for (name, r) in [
            ("same instrument", SameInstrument),
            ("base mismatch", BaseMismatch),
            ("quote mismatch", QuoteMismatch),
            ("linear vs inverse", LinearAgainstInverse),
            ("perp vs perp on one venue", SameVenuePerpPerp),
            ("spot vs spot on one venue", SameVenueSpotSpot),
        ] {
            // Display must name the reason: an operator reading "refused" with no
            // reason cannot tell a rule from a bug.
            assert!(!r.to_string().is_empty(), "{name} must explain itself");
        }
    }
}
