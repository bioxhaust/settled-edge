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
    annual_borrow_cost, executable_price, liquidation_price, margin_required,
    max_size_within_slippage, net_apr_after_borrow, net_apr_over_horizon, periods_to_breakeven,
    round_down_to_lot, slippage_bps, CarryLeg, LiquidationInputs, MathError,
};
use arb_num::{Bps, Money, Price, Qty, Rate, RoundMode};
use arb_types::{
    ContractSize, ContractSpec, DataQuality, MarketKind, PositionSide, Provenance, QualityFlags,
    Side, VenueKey,
};

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

    // Two checks, not one. This used to be a single `!is_trustworthy()`, which is
    // `flags.is_empty() && provenance == LiveMeasured` — a conjunction that hid two
    // unrelated verdicts behind one blocker, and whose flag term was far stricter
    // than `QualityFlags::BLOCKING`. Any live leg carries `FEE_TIER_ASSUMED`, so
    // under that gate no measured row could ever rank. Splitting it keeps every
    // falsifying flag blocking while letting a stated, one-directional assumption
    // ride on the row where the operator can see it.
    //
    // `q.flags` is the snapshots' own flags, unioned with the fee schedule's. It is
    // a different set from the `flags` local above, which holds only what this
    // function derived (staleness, skew, crossing, paint) and whose blockers were
    // already pushed with their specific reasons. Nothing is double-counted: the
    // derived flags are never written back into the snapshot's quality.
    let q = leg.quality();
    if q.flags.is_blocking() {
        // Catches what has no dedicated check above: SEQ_GAP, BOOK_EMPTY,
        // PRICE_UNAVAILABLE. The Bybit adapter really does set the last of these
        // when a ticker arrives without a mark price, so this is a live path and
        // not a defensive hypothetical.
        blocked.push(Unrankable::BlockingFlags(
            vkey,
            q.flags.intersection(QualityFlags::BLOCKING),
        ));
    }
    if q.provenance != Provenance::LiveMeasured {
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

    // Borrow cost on a reverse carry, and the two ways it can go.
    //
    // A short **spot** leg means selling the base asset we do not own, which means
    // borrowing it. Cash-and-carry (long spot, short perp) owns the spot and needs
    // no borrow; only the reverse carry a negative funding rate calls for does. The
    // cost is not small: a CEX margin borrow annualises into the tens of percent
    // against an edge of twelve basis points, so it can invert the sign of the
    // number above (`OPEN.md` A1, `decisions/023`).
    //
    // - With a borrow snapshot, charge it as an **annual** rate via
    //   `net_apr_after_borrow` — never amortised into the round trip, which would
    //   divide by the horizon it should be multiplied by (`arb_math::borrow`).
    // - Without one, the term is missing and the sign is in doubt, so the row is
    //   blocked with `BORROW_COST_UNKNOWN` rather than ranked on a cost it never
    //   charged. `net_apr` is left at its pre-borrow value — the block is the
    //   honest signal; subtracting an unknown is not an option.
    let mut carry_flags = QualityFlags::NONE;
    let net_apr = if short.kind().is_perp() {
        net_apr
    } else {
        match short.borrow.as_ref() {
            Some(snap) => net_apr_after_borrow(net_apr, annual_borrow_cost(snap)?)?,
            None => {
                carry_flags = carry_flags.union(QualityFlags::BORROW_COST_UNKNOWN);
                blocked.push(Unrankable::BorrowCostUnknown(short.vkey()));
                net_apr
            }
        }
    };

    // Rate history. A funding leg with no settled-rate history makes `net_apr` a
    // conditional — "this is the return *if* the rate holds" — rather than a
    // forecast. That conditional is honestly useful, so this warns rather than
    // blocks (`OPEN.md` A5); the report's job is to state the condition. A leg with
    // an empty history is as unmeasured as one with none.
    for leg in [short, long] {
        if leg.funding.is_some() && leg.history.as_ref().is_none_or(|h| h.is_empty()) {
            carry_flags = carry_flags.union(QualityFlags::RATE_HISTORY_MISSING);
        }
    }

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

    // The borrow and history verdicts are pair-level — they depend on which leg
    // ended up short — so they are folded in here rather than in `assess_leg`.
    let quality = short_leg
        .quality
        .combine(long_leg.quality)
        .with_flags(carry_flags);

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
    use arb_types::{
        Asset, BookSnapshot, BorrowInterval, BorrowSnapshot, ContractSize, ContractSpec,
        FundingInterval, FundingSnapshot, Instrument, InstrumentKey, Level, LocalTs, MarketKind,
        Millis, RateHistory, SettledRate, SizeUnit, Symbol, Venue, WallTs,
    };
    use arb_venue::FeeSchedule;

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

    // -- the whole pipeline, end to end -----------------------------------
    //
    // Every test above stops at a function boundary. The tests below run
    // `price_pair` from two `LegInput`s to a finished row, which until now was
    // the only path through this module that nothing exercised — and it is the
    // path that ships.
    //
    // # Why these fixtures claim `DataQuality::MEASURED`
    //
    // `assess_leg` pushes `Unrankable::Unverified` for any leg whose *market data*
    // is not live-measured, and `Opportunity::is_rankable` is false while anything
    // is in `blocked`. So a `SYNTHESIZED` fixture — the honest mark for every
    // number in this repository, and what `arb_venue::FixtureAdapter` stamps —
    // *cannot* reach the end of the pipeline rankable. That is the guard working
    // as designed, and it is also why the guard hid the fact that nothing had
    // ever run the rankable branch.
    //
    // These legs therefore lie about their provenance, deliberately and only
    // here, to reach the branch. The lie is confined to this module: no fixture
    // on disk is `MEASURED`, no adapter produces it, and the assertions below are
    // about *arithmetic*, not about any market. Nothing here makes a shipped
    // number real.

    /// The ladder from `arb_detect::painted`'s own `healthy_book`, reused
    /// verbatim.
    ///
    /// asks 0.5 + 0.8 + 1.2 + 1.5 + 2.0 = 6.0, bids 0.6 + 0.9 + 1.3 + 1.2 + 1.5
    /// = 5.5, and two tests over there assert it fires no paint signal from
    /// either side. Reusing the proven shape means a `PaintedDepth` block
    /// appearing in these tests is a change in *this* crate's wiring, not a
    /// carelessly shaped fixture.
    ///
    /// `PaintLimits::CONSERVATIVE` carries `tick_size: Price::ZERO`, so the gap
    /// check falls to its 25 bps branch rather than the tick one; a 0.01 step on
    /// a 100.00 touch is 1 bp, so it stays silent there too.
    ///
    /// The exchange stamp equals the observation instant, so `assess_skew` sees
    /// zero skew and returns `Aligned`. A `None` stamp would also give `Aligned`,
    /// which is why supplying one matters: it makes the clock path *exercised*
    /// rather than defaulted.
    fn clean_book(vkey: VenueKey) -> BookSnapshot {
        book_with_quality(vkey, DataQuality::MEASURED)
    }

    /// [`clean_book`]'s ladder under a different quality claim.
    ///
    /// Split out so a test that is about the *gate* changes exactly one thing —
    /// what the snapshot says about itself — and the ladder stays the proven shape
    /// documented above.
    fn book_with_quality(vkey: VenueKey, quality: DataQuality) -> BookSnapshot {
        fn lvl(price: &str, qty: &str) -> Level {
            Level::new(
                Price::parse(price).expect("price literal"),
                Qty::parse(qty).expect("qty literal"),
            )
            .expect("level")
        }
        BookSnapshot::new(
            vkey,
            vec![
                lvl("99.99", "0.6"),
                lvl("99.98", "0.9"),
                lvl("99.97", "1.3"),
                lvl("99.96", "1.2"),
                lvl("99.95", "1.5"),
            ],
            vec![
                lvl("100.00", "0.5"),
                lvl("100.01", "0.8"),
                lvl("100.02", "1.2"),
                lvl("100.03", "1.5"),
                lvl("100.04", "2.0"),
            ],
            Some(1),
            Some(WallTs::EPOCH),
            LocalTs::EPOCH,
            quality,
        )
        .expect("book")
    }

    fn vkey_of(venue: Venue, kind: MarketKind) -> VenueKey {
        VenueKey::new(
            venue,
            InstrumentKey::parse("BTC", "USDT", kind, "USDT").expect("key"),
        )
    }

    /// One contract is one coin and the lot is 0.001, so book units, contracts
    /// and base all coincide and no unit conversion can hide inside these
    /// assertions. The unit-boundary tests above are where that conversion is
    /// under test.
    fn instrument_of(venue: Venue, kind: MarketKind) -> Instrument {
        Instrument::new(
            vkey_of(venue, kind),
            Symbol::new("BTCUSDT").expect("symbol"),
            spec(kind, base_per("1"), Qty::parse("0.001").expect("lot")),
        )
    }

    fn funding_every(vkey: VenueKey, rate: &str, interval: FundingInterval) -> FundingSnapshot {
        FundingSnapshot {
            vkey,
            rate: Rate::parse(rate).expect("rate literal"),
            interval,
            next_funding_at: None,
            mark: None,
            index: None,
            exchange_ts: WallTs::EPOCH,
            observed_at: LocalTs::EPOCH,
            quality: DataQuality::MEASURED,
        }
    }

    fn funding_at(vkey: VenueKey, rate: &str) -> FundingSnapshot {
        funding_every(vkey, rate, FundingInterval::EIGHT_HOURS)
    }

    /// A one-entry history. Its contents are never read here; what matters is
    /// that it is non-empty, so `RATE_HISTORY_MISSING` stays off and the row's
    /// flags are exactly the ones under test.
    fn history_at(vkey: VenueKey, rate: &str) -> RateHistory {
        RateHistory::new(
            vkey,
            vec![SettledRate {
                settled_at: WallTs::EPOCH,
                rate: Rate::parse(rate).expect("rate literal"),
            }],
            DataQuality::MEASURED,
        )
        .expect("history")
    }

    /// `known`, not `assumed`: an assumed tier sets `FEE_TIER_ASSUMED`, and while
    /// that flag does not block, it would put a flag on a row these tests assert
    /// is flagless.
    fn fees_taking(taker_bps: &str) -> FeeSchedule {
        FeeSchedule::known(
            Bps::parse_bps("1").expect("maker bps"),
            Bps::parse_bps(taker_bps).expect("taker bps"),
        )
    }

    /// `now` equals every `observed_at` in these fixtures, so both staleness
    /// checks see an age of zero and return `Fresh`.
    ///
    /// `horizon` is a parameter because it is the only lever that changes the
    /// answer while the fixture stays fixed — see
    /// [`the_round_trip_is_amortised_over_the_horizon_not_charged_once`].
    fn cfg_holding_for(horizon_secs: i64) -> ScanConfig {
        ScanConfig::conservative(
            Money::from_int(40).expect("target notional"),
            Rate::from_int(3).expect("leverage"),
            Millis::from_secs(horizon_secs),
            LocalTs::EPOCH,
        )
        .expect("config")
    }

    /// One year, so `net_apr_over_horizon`'s amortisation factor is exactly 1
    /// and `net_apr` is `gross_apr` minus `cost_bps` read as a rate. Isolating
    /// the subtraction is the point; the factor gets its own test.
    const ONE_YEAR_SECS: i64 = 31_536_000;

    /// Two perp legs with opposite-signed funding, so *both* sides are paid.
    ///
    /// MEXC at +0.0001 per 8h pays its shorts; OKX at −0.0001 per 8h pays its
    /// longs. `assign_sides` must therefore short MEXC and hold OKX long, and it
    /// must reach that from the annualised receipts rather than from the
    /// published rates.
    fn clean_perp_pair() -> (LegInput, LegInput) {
        let mexc = vkey_of(Venue::Mexc, MarketKind::LinearPerp);
        let okx = vkey_of(Venue::Okx, MarketKind::LinearPerp);
        let a = LegInput::with_carry_inputs(
            instrument_of(Venue::Mexc, MarketKind::LinearPerp),
            Some(funding_at(mexc, "0.0001")),
            clean_book(mexc),
            fees_taking("6"),
            DataQuality::MEASURED,
            None,
            Some(history_at(mexc, "0.0001")),
        )
        .expect("mexc leg");
        let b = LegInput::with_carry_inputs(
            instrument_of(Venue::Okx, MarketKind::LinearPerp),
            Some(funding_at(okx, "-0.0001")),
            clean_book(okx),
            fees_taking("5"),
            DataQuality::MEASURED,
            None,
            Some(history_at(okx, "-0.0001")),
        )
        .expect("okx leg");
        (a, b)
    }

    /// The row that ships: nothing blocked, nothing flagged, and every number a
    /// hand computation.
    ///
    /// Until this test existed the rankable branch of `price_pair` had never
    /// run — every fixture in the repository is `SYNTHESIZED` and therefore
    /// blocked at `Unrankable::Unverified` before reaching it.
    #[test]
    fn a_clean_perp_pair_ranks_and_every_number_is_the_hand_computation() {
        let (a, b) = clean_perp_pair();
        let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");

        // ---- the row is rankable, which is the whole point ----------------
        assert!(
            opp.blocked.is_empty(),
            "clean inputs must block nothing, got {:?}",
            opp.blocked
        );
        assert_eq!(opp.quality.flags, QualityFlags::NONE);
        assert!(opp.is_rankable());

        // ---- sides ---------------------------------------------------------
        // Shorting MEXC and holding OKX long earns +0.1095 + +0.1095; the
        // reverse earns −0.1095 + −0.1095.
        //
        // Both legs settle on the same interval here, so ordering them by
        // published rate would reach the same answer — this fixture does *not*
        // discriminate between the two rules. See
        // [`the_sides_are_ordered_by_annualised_receipt_not_by_published_rate`]
        // for the case where the two rules disagree.
        assert_eq!(opp.short.vkey, vkey_of(Venue::Mexc, MarketKind::LinearPerp));
        assert_eq!(opp.long.vkey, vkey_of(Venue::Okx, MarketKind::LinearPerp));
        assert_eq!(opp.short.side, PositionSide::Short);
        assert_eq!(opp.long.side, PositionSide::Long);

        // The venue's own number, carried through untouched so it can be
        // reconciled against the exchange's screen.
        assert_eq!(opp.short.published_rate, Rate::parse("0.0001").expect("rate"));
        assert_eq!(opp.long.published_rate, Rate::parse("-0.0001").expect("rate"));

        // ---- size and notional -------------------------------------------
        // 40 USDT at the ask touch of 100.00 is 0.4 base, and 0.4 is a whole
        // multiple of the 0.001 lot, so nothing rounds off it.
        assert_eq!(opp.size, Qty::parse("0.4").expect("qty"));
        // Realised at the long leg's VWAP, which *is* the touch here: 0.4 fits
        // inside the 0.5 resting at 100.00, so 0.4 * 100.00 = 40 exactly, and
        // the budget is met rather than overshot.
        assert_eq!(opp.notional, Money::from_int(40).expect("money"));

        // ---- gross ---------------------------------------------------------
        // 31_536_000 / 28_800 = 1095 settlements a year, exactly.
        //   short MEXC: +0.0001 * 1095 = +0.1095
        //   long  OKX : −(−0.0001) * 1095 = +0.1095
        // Both are receipts, so the sum is 0.2190 — raw 219_000_000_000 at
        // Rate's 1e12 scale.
        assert_eq!(opp.short.annual_receipt.raw(), 109_500_000_000);
        assert_eq!(opp.long.annual_receipt.raw(), 109_500_000_000);
        assert_eq!(opp.gross_apr.raw(), 219_000_000_000);

        // ---- costs ---------------------------------------------------------
        // Four crossings, each at its own venue's taker rate: 6 + 6 on the MEXC
        // leg and 5 + 5 on the OKX leg. 22 bps — not 4x either rate, which is
        // the error `round_trip_bps` exists to prevent, and not 4x the maker
        // rate either, which is what a non-`ALL_TAKER` round trip would give.
        assert_eq!(opp.fee_bps, Bps::from_whole(22).expect("bps"));
        // Both legs fill entirely at the touch — 0.4 against 0.6 resting on the
        // bid and 0.5 on the ask — so VWAP equals touch and there is no depth
        // cost to charge. This is what isolates the fee arithmetic below.
        assert_eq!(opp.short.slippage, Bps::ZERO);
        assert_eq!(opp.long.slippage, Bps::ZERO);
        assert_eq!(opp.slippage_bps, Bps::ZERO);
        assert_eq!(opp.cost_bps, Bps::from_whole(22).expect("bps"));

        // ---- net -----------------------------------------------------------
        // Held one year: 22 bps * 31_536_000 / 31_536_000 = 22 bps = 0.0022.
        // 0.2190 − 0.0022 = 0.2168.
        assert_eq!(opp.net_apr.raw(), 216_800_000_000);

        // ---- breakeven -----------------------------------------------------
        // Per settlement the pair receives 0.0001 + 0.0001 = 0.0002 = 2 bps,
        // and 22 bps of round trip needs ceil(22 / 2) = 11 of them. Both legs
        // settle on 8h, so the count has a unit.
        assert_eq!(opp.periods_to_breakeven, Some(11));

        // Both legs are perps, so both can be liquidated and the row must carry
        // the nearer distance rather than nothing.
        assert!(opp.distance_to_liq.is_some());
    }

    /// The same fixture held for one settlement instead of a year.
    ///
    /// This is the number the rest of the market does not show: nothing about
    /// the market changed between this test and the one above, only how long we
    /// intend to hold, and a +21.90% headline is a −219.00% trade.
    ///
    /// And it is still **rankable**. A negative net is a fact about the market;
    /// `blocked` is for facts about the data. Conflating them would hide either
    /// the bad trade or the bad feed.
    #[test]
    fn the_round_trip_is_amortised_over_the_horizon_not_charged_once() {
        let (a, b) = clean_perp_pair();
        // One 8h settlement.
        let opp = price_pair(&a, &b, &cfg_holding_for(28_800)).expect("row");

        // Unchanged: the horizon is not an input to either of these.
        assert_eq!(opp.gross_apr.raw(), 219_000_000_000);
        assert_eq!(opp.cost_bps, Bps::from_whole(22).expect("bps"));

        // 0.0022 * 31_536_000 / 28_800 = 0.0022 * 1095 = 2.4090 of annual drag,
        // against 0.2190 of annual receipt: 0.2190 − 2.4090 = −2.1900.
        assert_eq!(opp.net_apr.raw(), -2_190_000_000_000);
        assert!(opp.net_apr.is_negative());

        assert!(opp.blocked.is_empty(), "got {:?}", opp.blocked);
        assert!(
            opp.is_rankable(),
            "a losing trade is still a priced trade; only bad data is unrankable"
        );
    }

    /// Two perps on different settlement clocks, where the two candidate rules
    /// give **opposite** answers.
    ///
    /// MEXC publishes +0.0001 every hour; OKX publishes +0.0003 every eight.
    /// OKX's printed number is three times MEXC's, so "short the venue with the
    /// higher published rate" shorts OKX. Annualised, MEXC pays 8760 times a
    /// year and OKX 1095:
    ///
    /// - short MEXC, long OKX: +0.8760 + −0.3285 = **+0.5475**
    /// - short OKX, long MEXC: +0.3285 + −0.8760 = **−0.5475**
    ///
    /// So the correct short is the leg with the *lower* published rate, and the
    /// naive rule does not merely mis-rank this pair — it takes the wrong side of
    /// it, turning +54.75% into −54.75%. This is the arithmetic
    /// [`crate::opportunity::Opportunity::gross_apr`] warns about, under test.
    #[test]
    fn the_sides_are_ordered_by_annualised_receipt_not_by_published_rate() {
        let mexc = vkey_of(Venue::Mexc, MarketKind::LinearPerp);
        let okx = vkey_of(Venue::Okx, MarketKind::LinearPerp);
        let a = LegInput::with_carry_inputs(
            instrument_of(Venue::Mexc, MarketKind::LinearPerp),
            Some(funding_every(mexc, "0.0001", FundingInterval::ONE_HOUR)),
            clean_book(mexc),
            fees_taking("6"),
            DataQuality::MEASURED,
            None,
            Some(history_at(mexc, "0.0001")),
        )
        .expect("mexc leg");
        let b = LegInput::with_carry_inputs(
            instrument_of(Venue::Okx, MarketKind::LinearPerp),
            Some(funding_every(okx, "0.0003", FundingInterval::EIGHT_HOURS)),
            clean_book(okx),
            fees_taking("5"),
            DataQuality::MEASURED,
            None,
            Some(history_at(okx, "0.0003")),
        )
        .expect("okx leg");

        let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");

        assert!(opp.blocked.is_empty(), "got {:?}", opp.blocked);
        assert!(opp.is_rankable());

        // The lower published rate is the one we short.
        assert_eq!(opp.short.vkey, mexc);
        assert_eq!(opp.long.vkey, okx);
        assert!(opp.short.published_rate < opp.long.published_rate);

        // 0.0001 * 31_536_000 / 3_600 = 0.0001 * 8760 = 0.8760 received.
        assert_eq!(opp.short.annual_receipt.raw(), 876_000_000_000);
        // 0.0003 * 31_536_000 / 28_800 = 0.0003 * 1095 = 0.3285, paid out.
        assert_eq!(opp.long.annual_receipt.raw(), -328_500_000_000);
        // 0.8760 − 0.3285 = 0.5475, and its sign is the whole argument.
        assert_eq!(opp.gross_apr.raw(), 547_500_000_000);
        assert!(opp.gross_apr.is_positive());

        // 22 bps over a year again: 0.5475 − 0.0022 = 0.5453.
        assert_eq!(opp.cost_bps, Bps::from_whole(22).expect("bps"));
        assert_eq!(opp.net_apr.raw(), 545_300_000_000);

        // No breakeven count: the two legs settle on different clocks, so
        // "periods" has no shared unit and a number here would be meaningless.
        // Reported as absent rather than as a count in one leg's periods.
        assert_eq!(opp.periods_to_breakeven, None);
    }

    /// A reverse carry: Bybit's perp funds at −0.0002 per 8h, so its *longs* are
    /// paid, and the hedge is a **short spot** leg — which means borrowing BTC.
    ///
    /// `assign_sides` must reach that on its own: spot receives no funding at
    /// all, so shorting spot scores 0 + 0.2190 while shorting the perp scores
    /// −0.2190 + 0.
    fn reverse_carry_pair(borrow: Option<BorrowSnapshot>) -> (LegInput, LegInput) {
        let spot = vkey_of(Venue::Binance, MarketKind::Spot);
        let perp = vkey_of(Venue::Bybit, MarketKind::LinearPerp);
        let a = LegInput::with_carry_inputs(
            instrument_of(Venue::Binance, MarketKind::Spot),
            None,
            clean_book(spot),
            fees_taking("5"),
            DataQuality::MEASURED,
            borrow,
            None,
        )
        .expect("spot leg");
        let b = LegInput::with_carry_inputs(
            instrument_of(Venue::Bybit, MarketKind::LinearPerp),
            Some(funding_at(perp, "-0.0002")),
            clean_book(perp),
            fees_taking("6"),
            DataQuality::MEASURED,
            None,
            Some(history_at(perp, "-0.0002")),
        )
        .expect("perp leg");
        (a, b)
    }

    fn borrow_per_day(rate: &str) -> BorrowSnapshot {
        BorrowSnapshot::new(
            vkey_of(Venue::Binance, MarketKind::Spot),
            Asset::new("BTC").expect("asset"),
            Rate::parse(rate).expect("rate literal"),
            BorrowInterval::DAILY,
            Some(WallTs::EPOCH),
            LocalTs::EPOCH,
            DataQuality::MEASURED,
        )
        .expect("borrow")
    }

    /// A reverse carry with no borrow quote is blocked, and its `net_apr` is
    /// left alone.
    ///
    /// Both halves matter. Blocking is the honest signal that a cost term is
    /// missing; *not* adjusting `net_apr` is the refusal to subtract a number
    /// nobody supplied. Subtracting a guess would be a second invented figure
    /// dressed as a correction. `OPEN.md` A1, `decisions/023`.
    #[test]
    fn a_reverse_carry_with_no_borrow_quote_is_blocked_and_flagged() {
        let (a, b) = reverse_carry_pair(None);
        let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");

        let spot = vkey_of(Venue::Binance, MarketKind::Spot);
        assert_eq!(opp.short.vkey, spot, "the spot leg is the one being shorted");
        // Spot cannot be liquidated, which is exactly why it must be borrowed.
        assert!(opp.short.liquidation.is_none());

        // Exactly one blocker, naming the leg that owes the borrow.
        assert_eq!(opp.blocked, vec![Unrankable::BorrowCostUnknown(spot)]);
        // And exactly one flag: the spot leg has no funding, so the rate-history
        // check skips it, and the perp leg's history is present.
        assert_eq!(opp.quality.flags, QualityFlags::BORROW_COST_UNKNOWN);
        assert!(!opp.is_rankable());

        // −(−0.0002) * 1095 = +0.2190 on the long perp, nothing on spot, less
        // 22 bps of round trip over a year: 0.2190 − 0.0022 = 0.2168. Reported
        // as-is, with no borrow term guessed into it.
        assert_eq!(opp.gross_apr.raw(), 219_000_000_000);
        assert_eq!(opp.net_apr.raw(), 216_800_000_000);
    }

    /// Supplying the quote unblocks the row — and can invert its sign.
    ///
    /// The two borrow rates here are the whole argument for
    /// `QualityFlags::BORROW_COST_UNKNOWN` being *blocking* rather than a
    /// warning. At 0.0002 per day the borrow eats a third of the edge; at 0.0007
    /// it eats all of it and 3.87 points more. A row that omitted the term would
    /// have printed +21.68% both times and sorted at the top of the table.
    #[test]
    fn supplying_the_borrow_quote_unblocks_the_row_and_can_invert_its_sign() {
        // 0.0002 per day: 0.0002 * 31_536_000 / 86_400 = 0.0002 * 365 = 0.0730.
        // 0.2168 − 0.0730 = 0.1438.
        let (a, b) = reverse_carry_pair(Some(borrow_per_day("0.0002")));
        let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");
        assert!(opp.blocked.is_empty(), "got {:?}", opp.blocked);
        assert_eq!(opp.quality.flags, QualityFlags::NONE);
        assert!(opp.is_rankable());
        // Gross and cost are untouched by the borrow: it is charged as an annual
        // rate against the net, never amortised into the round trip.
        assert_eq!(opp.gross_apr.raw(), 219_000_000_000);
        assert_eq!(opp.cost_bps, Bps::from_whole(22).expect("bps"));
        assert_eq!(opp.net_apr.raw(), 143_800_000_000);

        // 0.0007 per day: 0.0007 * 365 = 0.2555, and 0.2168 − 0.2555 = −0.0387.
        // Same market, same fees, same size — the sign of the answer is decided
        // by the term the unblocked row was missing.
        let (a, b) = reverse_carry_pair(Some(borrow_per_day("0.0007")));
        let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");
        assert!(opp.blocked.is_empty(), "got {:?}", opp.blocked);
        assert!(opp.is_rankable());
        assert_eq!(opp.net_apr.raw(), -38_700_000_000);
        assert!(opp.net_apr.is_negative());
    }

    // -- the leg gate: which caveats sink a row and which ride on it -------
    //
    // The tests below pin the gate that decides whether a *measured* row is
    // allowed into the table at all, and they exist because that gate was wrong
    // for most of this crate's life. It read:
    //
    //     if !leg.quality().is_trustworthy() { blocked.push(Unverified(vkey)) }
    //
    // and `is_trustworthy` is `flags.is_empty() && provenance == LiveMeasured`.
    // An empty flag set is a far stricter demand than `QualityFlags::BLOCKING`,
    // which deliberately excludes `FEE_TIER_ASSUMED`, `PARTIAL_BOOK` and
    // `THIN_DEPTH`. Since no venue publishes which fee tier an account is on,
    // every leg a public adapter can build carries `FEE_TIER_ASSUMED` — so the
    // answer to "does this arbitrage pay" was "unverified", on every venue, at
    // every size, for ever. The product did not work because of one conjunction.
    //
    // Splitting it into a flags check and a provenance check is the fix, and the
    // risk it introduces is the obvious one: a blocking flag that arrives already
    // set on an adapter's snapshot, and that `assess_leg` has no dedicated check
    // for, must still block. `blocking_book` below is that case.

    /// The same leg, with the fee schedule an honest public adapter produces.
    ///
    /// Rebuilt rather than parameterised so the clean pair above stays the
    /// arithmetic baseline: the *only* difference between the two is which fee
    /// tier claim is made, which is what these tests are about.
    fn with_assumed_fees(leg: &LegInput) -> LegInput {
        LegInput::with_carry_inputs(
            leg.instrument,
            leg.funding,
            leg.book.clone(),
            FeeSchedule::assumed(leg.fees.maker, leg.fees.taker),
            DataQuality::new(
                QualityFlags::FEE_TIER_ASSUMED,
                Provenance::SynthesizedUnverified,
            ),
            leg.borrow,
            leg.history.clone(),
        )
        .expect("changing the fee tier claim cannot make a valid leg invalid")
    }

    /// A measured book that says, in its own flags, that something about it is
    /// broken.
    ///
    /// `PRICE_UNAVAILABLE` is not hypothetical: `arb_live`'s Bybit adapter sets it
    /// when a ticker arrives without a mark price. It is in
    /// `QualityFlags::BLOCKING` and it has no dedicated check in `assess_leg`, so
    /// it is exactly the flag a careless split of the old gate would drop.
    fn blocking_book(vkey: VenueKey, flag: QualityFlags) -> BookSnapshot {
        book_with_quality(vkey, DataQuality::new(flag, Provenance::LiveMeasured))
    }

    /// An assumed fee tier does not sink a measured leg.
    ///
    /// This is the assertion the product rests on. The market data was measured
    /// and says so; the cost model was assumed and says so, on a flag
    /// `QualityFlags::BLOCKING` excludes. And the direction of the assumption is
    /// what makes that safe: the worst published tier is charged, so a row that
    /// clears the gate has been charged *at least* what it will really pay.
    #[test]
    fn an_assumed_fee_tier_flags_a_measured_leg_without_sinking_it() {
        let (a, b) = clean_perp_pair();
        let (a, b) = (with_assumed_fees(&a), with_assumed_fees(&b));

        // The fee schedule is still honest about itself, unfolded.
        assert_eq!(a.fee_provenance(), Provenance::SynthesizedUnverified);
        // The leg is not: its market data came off a venue.
        assert_eq!(a.quality().provenance, Provenance::LiveMeasured);
        assert_eq!(a.quality().flags, QualityFlags::FEE_TIER_ASSUMED);
        assert!(a.quality().is_rankable());
        // Still not *trustworthy* — that gate wants a clean flag set, and reports
        // are entitled to keep saying so. It is simply not the ranking gate.
        assert!(!a.quality().is_trustworthy());

        let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");
        assert!(
            opp.blocked.is_empty(),
            "an assumed fee tier must not block a measured row, got {:?}",
            opp.blocked
        );
        assert!(opp.is_rankable());
        // The caveat survives onto the row, where an operator reads it.
        assert_eq!(opp.quality.flags, QualityFlags::FEE_TIER_ASSUMED);
        assert_eq!(opp.quality.provenance, Provenance::LiveMeasured);

        // And the number is the clean pair's number: this changed what the gate
        // permits, not what the engine computes.
        let (ca, cb) = clean_perp_pair();
        let clean = price_pair(&ca, &cb, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");
        assert_eq!(opp.net_apr, clean.net_apr);
        assert_eq!(opp.cost_bps, clean.cost_bps);
    }

    /// A blocking flag on the snapshot still blocks, even with the fee gate gone.
    ///
    /// Three flags in `QualityFlags::BLOCKING` — `SEQ_GAP`, `BOOK_EMPTY` and
    /// `PRICE_UNAVAILABLE` — have no dedicated `Unrankable` variant, because they
    /// are produced by adapters rather than derived by `assess_leg`. Before the
    /// split they were caught by `is_trustworthy`'s `flags.is_empty()` term as a
    /// side effect. This test is the reason the replacement is two checks and not
    /// one: with a single provenance check, a book that told us its feed had
    /// skipped a sequence number would have ranked.
    #[test]
    fn a_blocking_flag_on_a_measured_snapshot_still_sinks_the_row() {
        for flag in [
            QualityFlags::SEQ_GAP,
            QualityFlags::BOOK_EMPTY,
            QualityFlags::PRICE_UNAVAILABLE,
        ] {
            let (a, b) = clean_perp_pair();
            let mexc = vkey_of(Venue::Mexc, MarketKind::LinearPerp);
            let a = LegInput::with_carry_inputs(
                a.instrument,
                a.funding,
                blocking_book(mexc, flag),
                FeeSchedule::assumed(a.fees.maker, a.fees.taker),
                DataQuality::new(
                    QualityFlags::FEE_TIER_ASSUMED,
                    Provenance::SynthesizedUnverified,
                ),
                None,
                a.history.clone(),
            )
            .expect("leg");
            let b = with_assumed_fees(&b);

            let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");
            assert!(
                !opp.is_rankable(),
                "{flag} is in BLOCKING and must sink the row"
            );
            // Named, and named *precisely*: the payload carries the blocking
            // subset only. Listing FEE_ASSUMED as a reason the row was sunk —
            // when it explicitly is not one — would send the operator to buy an
            // API key to fix a broken feed.
            assert_eq!(
                opp.blocked,
                vec![Unrankable::BlockingFlags(mexc, flag)],
                "expected exactly one blocker naming {flag}"
            );
            assert!(
                !opp.blocked[0].to_string().contains("FEE_ASSUMED"),
                "the reason must not name a non-blocking flag: {}",
                opp.blocked[0]
            );
        }
    }

    /// Synthesized *market data* is still unverified, and still blocked.
    ///
    /// The safety property the split had to preserve. Every fixture in this
    /// repository is `SYNTHESIZED`, `arb_venue::FixtureAdapter` stamps it, and a
    /// replay must never be able to present an invented number as a ranked one.
    /// The fee carve-out is narrow by construction: it applies to the fee schedule
    /// alone, and the book and funding provenance still fold in as before.
    #[test]
    fn synthesized_market_data_is_still_unverified_however_good_the_fees_are() {
        let mexc = vkey_of(Venue::Mexc, MarketKind::LinearPerp);
        let (a, b) = clean_perp_pair();
        // A *measured* fee schedule on a *synthesized* book. If the carve-out
        // leaked in the other direction, this would rank.
        let a = LegInput::with_carry_inputs(
            a.instrument,
            a.funding,
            book_with_quality(mexc, DataQuality::SYNTHESIZED),
            fees_taking("6"),
            DataQuality::MEASURED,
            None,
            a.history.clone(),
        )
        .expect("leg");

        assert_eq!(a.quality().provenance, Provenance::SynthesizedUnverified);
        let opp = price_pair(&a, &b, &cfg_holding_for(ONE_YEAR_SECS)).expect("row");
        assert!(!opp.is_rankable());
        assert!(
            opp.blocked.contains(&Unrankable::Unverified(mexc)),
            "an invented book must be refused by name, got {:?}",
            opp.blocked
        );
    }
}
