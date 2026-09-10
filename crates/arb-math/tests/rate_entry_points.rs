//! `OPEN.md` F4, made mechanical: every public entry point of this crate that
//! can be handed a rate is enumerated **from the source text**, and each one
//! must be classified and probed.
//!
//! # What was wrong with F4 before this file
//!
//! `OPEN.md` F4 recorded the fence as `[assumption]`-complete, and said so
//! plainly: it was "verified by reading the crate, not by a test enumerating the
//! entry points". A fence checked by reading holds until the next function is
//! added. Nothing failed when one was; nothing even noticed.
//!
//! Two holes were sitting behind that wording when this file was written, and
//! both are named in the registry below with the fix that closed them:
//! [`funding::periods_to_breakeven`] returned `Ok(0)` — "breakeven at settlement
//! zero", the single most flattering answer available — for a *negative* round
//! trip cost, and [`fees::RoundTrip::total_bps`] summed four negative fee legs
//! into a negative total. Both were reachable without touching a private field
//! and without `unsafe`.
//!
//! # What "mechanical" means here, precisely
//!
//! Three properties, and it is worth being exact about which of them the machine
//! checks and which a reader still has to:
//!
//! 1. **The enumeration is derived, not written.** [`public_signatures`] parses
//!    every module's source text for `pub fn` signatures. A new public function
//!    appears in that list the moment it is written, whatever anyone remembered.
//! 2. **Every parameter type must be classified.** [`PARAM_TYPES`] says, for
//!    each type name that may stand in a parameter position, whether it can
//!    carry a rate. An unlisted type fails the test, so a new type cannot enter
//!    a signature without someone deciding what it is.
//! 3. **Every rate-accepting entry point must be in [`REGISTRY`] with a probe.**
//!    The probe is a required field, so an entry cannot be registered without
//!    executable evidence of the behaviour claimed for it.
//!
//! What the machine does **not** check is whether a probe's own assertion is the
//! right one. That is a reader's job, and it is why each probe asserts a
//! hand-computed value rather than "is_err()".
//!
//! # Why classification, and not simply "everything refuses"
//!
//! Because "every entry point refuses a negative rate" is false and must stay
//! false. A funding rate is negative roughly half the time — that is what
//! `decisions/006` is about — and [`funding::annualise`] annualising `-0.0001`
//! into `-0.1095` is the correct behaviour, pinned by its own test. A fence that
//! refused it would refuse half the market.
//!
//! So the fence is over **cost-like** rates: a fee, a slippage budget, a borrow
//! rate, a round trip. Those cannot be negative, and the reason they cannot is
//! always the same one: this crate *subtracts* them, so a negative cost is added
//! back as revenue and lifts the row to the top of the ranking. Charter section 0
//! — an error in the favourable direction is the expensive kind, because ranking
//! selects for it.

use arb_math::{
    annual_borrow_cost, annualise, annualise_borrow, annualise_realised, borrow_cost,
    borrow_rate_over, convergence_capture, coverage, fee_on_notional, funding_payment,
    liquidation_price, margin_required, max_size_within_slippage, mean_settled_rate,
    net_apr_after_borrow, net_apr_over_horizon, periods_to_breakeven,
    realised_receipt_per_period, sign_flips, CarryLeg, FeeRates, FeeRole, FeeTable,
    LiquidationInputs, MathError, RoundTrip, SlippageBudget, VolumeTier,
};
use arb_num::{Bps, Money, Price, Qty, Rate, RoundMode};
use arb_types::{
    Asset, BorrowInterval, BorrowSnapshot, BookSnapshot, DataQuality, FundingInterval,
    FundingSnapshot, InstrumentKey, Level, LocalTs, MarketKind, Millis, PositionSide, Provenance,
    QualityFlags, RateHistory, SettledRate, Side, Venue, VenueKey, WallTs,
};

// ---------------------------------------------------------------------------
// Test data. No `f64` anywhere, including here: hard rule 2 covers tests, on the
// grounds that a test comparing through a float does not prove exactness.
// ---------------------------------------------------------------------------

fn rate(s: &str) -> Rate {
    Rate::parse(s).expect("test rate parses")
}

fn bps(s: &str) -> Bps {
    Bps::parse_bps(s).expect("test bps parses")
}

fn money(s: &str) -> Money {
    Money::parse(s).expect("test money parses")
}

fn price(s: &str) -> Price {
    Price::parse(s).expect("test price parses")
}

fn qty(s: &str) -> Qty {
    Qty::parse(s).expect("test qty parses")
}

fn vkey() -> VenueKey {
    let key = InstrumentKey::parse("BTC", "USDT", MarketKind::LinearPerp, "USDT").unwrap_or(
        InstrumentKey::new(
            Asset::default(),
            Asset::default(),
            MarketKind::LinearPerp,
            Asset::default(),
        ),
    );
    VenueKey::new(Venue::Binance, key)
}

/// A negative fee rate, in the shape a venue rebate would arrive in.
const NEGATIVE_BPS: &str = "-5.5";
/// A negative rate at the scale funding and borrow rates live at.
const NEGATIVE_RATE: &str = "-0.0001";

fn funding_snapshot(r: &str) -> FundingSnapshot {
    FundingSnapshot {
        vkey: vkey(),
        rate: rate(r),
        interval: FundingInterval::EIGHT_HOURS,
        next_funding_at: Some(WallTs::from_millis(1_700_000_000_000)),
        mark: None,
        index: None,
        exchange_ts: WallTs::from_millis(1_700_000_000_000),
        observed_at: LocalTs::from_millis(1),
        quality: DataQuality::SYNTHESIZED,
    }
}

/// A `BorrowSnapshot` holding a negative rate.
///
/// `BorrowSnapshot::new` refuses one — `arb-types` fences its own constructor —
/// but the fields are `pub`, so the struct literal reaches the value anyway.
/// That is exactly why [`annual_borrow_cost`] must re-check at the point of use
/// instead of trusting that it was built properly, and this helper is the proof
/// that the bypass is one line long.
fn borrow_snapshot_literal(r: &str) -> BorrowSnapshot {
    BorrowSnapshot {
        vkey: vkey(),
        asset: Asset::default(),
        rate: rate(r),
        interval: BorrowInterval::HOURLY,
        exchange_ts: None,
        observed_at: LocalTs::from_millis(1),
        quality: DataQuality::SYNTHESIZED,
    }
}

/// Two settlements, both at the given rate, one interval apart.
fn history(r: &str) -> RateHistory {
    let entries = vec![
        SettledRate {
            settled_at: WallTs::from_millis(0),
            rate: rate(r),
        },
        SettledRate {
            settled_at: WallTs::from_millis(28_800_000),
            rate: rate(r),
        },
    ];
    RateHistory::new(vkey(), entries, DataQuality::SYNTHESIZED).expect("stamps increase")
}

/// A `FeeRates` carrying a negative rate, built by struct literal.
///
/// `FeeRates::new` refuses this. The fields are `pub` and are read from four
/// other crates, so they cannot be made private without breaking them — which
/// means the constructor's check is a courtesy, not a fence, and every function
/// that does arithmetic on a `FeeRates` has to check for itself.
fn fee_rates_literal(maker: &str, taker: &str) -> FeeRates {
    FeeRates {
        maker: bps(maker),
        taker: bps(taker),
        tier_known: true,
    }
}

fn carry_leg(r: &str, side: PositionSide) -> CarryLeg {
    CarryLeg {
        vkey: vkey(),
        side,
        rate: rate(r),
        interval: FundingInterval::EIGHT_HOURS,
        quality: DataQuality::SYNTHESIZED,
    }
}

fn liq_inputs(maintenance: &str) -> LiquidationInputs {
    LiquidationInputs {
        kind: MarketKind::LinearPerp,
        side: PositionSide::Long,
        entry: price("100"),
        notional: money("100"),
        margin: money("25"),
        maintenance_rate: rate(maintenance),
    }
}

fn one_level_book() -> BookSnapshot {
    BookSnapshot::new(
        vkey(),
        vec![Level {
            price: price("100"),
            qty: qty("1"),
        }],
        vec![Level {
            price: price("101"),
            qty: qty("1"),
        }],
        None,
        Some(WallTs::from_millis(1)),
        LocalTs::from_millis(1),
        DataQuality::new(QualityFlags::NONE, Provenance::SynthesizedUnverified),
    )
    .expect("one level each side is a legal book")
}

// ---------------------------------------------------------------------------
// The classification
// ---------------------------------------------------------------------------

/// What kind of fence stands between a negative rate and this entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// The rate is cost-like. A negative one is refused with exactly this error.
    Refuses(MathError),
    /// The rate is legitimately signed, and the sign is carried through. A fence
    /// here would refuse real market data.
    Signed,
    /// The rate arrives inside a type whose field is private and whose only
    /// constructor refuses a negative. The named constructor must itself be a
    /// [`Class::Refuses`] entry in this registry, so the fence is checked rather
    /// than asserted.
    FencedBy(&'static str),
    /// No arithmetic is performed on the rate and it does not reach the result.
    /// The probe proves it by flipping the sign and observing no change.
    Ignores,
    /// The stored value is returned unchanged. Nothing is checked here on
    /// purpose; `why` must name the functions that do the checking.
    Inert,
}

/// What actually happened when the entry point was handed a negative rate.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Probe {
    Refused(MathError),
    /// Accepted, and the probe's hand-computed signed expectation held.
    SignedThrough,
    /// The result did not move when the rate's sign was flipped.
    Unaffected,
    /// The value came back exactly as it went in.
    Identity,
    /// A negative rate cannot be got into this entry point at all.
    Unreachable,
}

struct Entry {
    /// `module::Type::function`, matching what [`public_signatures`] derives.
    name: &'static str,
    class: Class,
    /// Why this classification and not another. For [`Class::Refuses`], the
    /// money consequence of *not* refusing.
    why: &'static str,
    probe: fn() -> Probe,
}

/// Every public entry point of this crate that can be handed a rate.
///
/// Not written from memory: the enumeration test below derives the same list
/// from the source text and fails on any disagreement in either direction, so a
/// function that is added, renamed or removed shows up here as a failure rather
/// than as silence.
const REGISTRY: &[Entry] = &[
    // -- borrow -------------------------------------------------------------
    Entry {
        name: "borrow::annualise_borrow",
        class: Class::Refuses(MathError::NegativeBorrowRate),
        why: "A borrow cost is subtracted from net APR. Annualising -0.0001 hourly \
              yields -87.6% and subtracting that adds 87.6 points of invented income.",
        probe: || {
            expect_err(annualise_borrow(
                rate(NEGATIVE_RATE),
                BorrowInterval::HOURLY,
            ))
        },
    },
    Entry {
        name: "borrow::annual_borrow_cost",
        class: Class::Refuses(MathError::NegativeBorrowRate),
        why: "BorrowSnapshot::new refuses a negative rate, but its fields are pub, so \
              the constructor is bypassable by a struct literal. The fence has to exist \
              at the point of use as well as the point of construction.",
        probe: || expect_err(annual_borrow_cost(&borrow_snapshot_literal(NEGATIVE_RATE))),
    },
    Entry {
        name: "borrow::borrow_rate_over",
        class: Class::Refuses(MathError::NegativeBorrowRate),
        why: "Accrual over a horizon. A negative rate makes a longer hold cheaper, \
              which inverts the one thing the borrow module exists to say.",
        probe: || {
            expect_err(borrow_rate_over(
                rate(NEGATIVE_RATE),
                BorrowInterval::HOURLY,
                Millis::from_secs(8 * 3600),
            ))
        },
    },
    Entry {
        name: "borrow::borrow_cost",
        class: Class::Refuses(MathError::NegativeBorrowRate),
        why: "Same accrual, denominated in money. A negative cost is a credit that no \
              venue pays.",
        probe: || {
            expect_err(borrow_cost(
                money("100000"),
                rate(NEGATIVE_RATE),
                BorrowInterval::HOURLY,
                Millis::from_secs(8 * 3600),
            ))
        },
    },
    Entry {
        name: "borrow::net_apr_after_borrow",
        class: Class::Refuses(MathError::NegativeBorrowRate),
        why: "The subtraction itself. `net_apr` is signed by design — a losing row is a \
              real answer — but `annual_borrow` is the term being subtracted, so a \
              negative one is added to the return.",
        probe: || {
            expect_err(net_apr_after_borrow(
                rate("0.1095"),
                rate(NEGATIVE_RATE),
            ))
        },
    },
    // -- fees ---------------------------------------------------------------
    Entry {
        name: "fees::FeeRates::new",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "Maker rebates exist and are deliberately not modelled: a negative fee \
              turns a cost into revenue and inverts every conservative rounding choice \
              downstream.",
        probe: || expect_err(FeeRates::new(bps("2"), bps(NEGATIVE_BPS), true)),
    },
    Entry {
        name: "fees::FeeRates::rate",
        class: Class::Inert,
        why: "A const accessor. It returns the stored rate without arithmetic, and \
              cannot return a Result. Every arithmetic consumer of a FeeRates in this \
              crate checks for itself: fees::fee_on_notional, fees::RoundTrip::total_bps \
              and fees::RoundTrip::total_bps_cross are all Refuses entries here.",
        probe: || {
            let r = fee_rates_literal("2", NEGATIVE_BPS);
            if r.rate(FeeRole::Taker) == bps(NEGATIVE_BPS) && r.rate(FeeRole::Maker) == bps("2") {
                Probe::Identity
            } else {
                Probe::Refused(MathError::NegativeFeeRate)
            }
        },
    },
    Entry {
        name: "fees::FeeTable::new",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "The only constructor of a FeeTable, whose tier Vec is private. Everything \
              the table can later be asked is fenced here.",
        probe: || {
            expect_err(FeeTable::new(vec![VolumeTier {
                min_volume_30d: Money::ZERO,
                maker: bps("2"),
                taker: bps(NEGATIVE_BPS),
            }]))
        },
    },
    Entry {
        name: "fees::FeeTable::flat",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "Delegates to FeeTable::new, and the delegation is the thing worth pinning: \
              a second constructor that forgot to is how this class of hole appears.",
        probe: || expect_err(FeeTable::flat(bps("2"), bps(NEGATIVE_BPS))),
    },
    Entry {
        name: "fees::FeeTable::tiers",
        class: Class::FencedBy("fees::FeeTable::new"),
        why: "Returns a slice of the private Vec. No FeeTable exists holding a negative \
              rate, so there is none to hand out.",
        probe: || unreachable_if_negative_tier_is_refused(),
    },
    Entry {
        name: "fees::FeeTable::worst",
        class: Class::FencedBy("fees::FeeTable::new"),
        why: "Reads the private Vec the constructor validated.",
        probe: || unreachable_if_negative_tier_is_refused(),
    },
    Entry {
        name: "fees::FeeTable::resolve",
        class: Class::FencedBy("fees::FeeTable::new"),
        why: "Reads the private Vec the constructor validated. An unknown tier resolves \
              to the worst row, which is still a validated row.",
        probe: || unreachable_if_negative_tier_is_refused(),
    },
    Entry {
        name: "fees::fee_on_notional",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "Charges a rate against money with RoundMode::COST. A negative rate would \
              produce a negative fee, i.e. money arriving on a trade that costs money.",
        probe: || expect_err(fee_on_notional(money("100000"), bps(NEGATIVE_BPS))),
    },
    Entry {
        name: "fees::RoundTrip::total_bps",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "This was the second hole behind F4. FeeRates fields are pub, so a literal \
              carrying -5.5 bps reaches here, and four all-taker legs summed to -22 bps. \
              That total is the round trip cost the whole crate subtracts, so the sign \
              error propagates into net APR and into breakeven at once.",
        probe: || {
            expect_err(RoundTrip::ALL_TAKER.total_bps(fee_rates_literal("2", NEGATIVE_BPS)))
        },
    },
    Entry {
        name: "fees::RoundTrip::total_bps_cross",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "Same hole, two venues. A rebate at either venue is enough, so both \
              arguments are checked and not only the one whose legs happen to be taker.",
        probe: || {
            expect_err(RoundTrip::ALL_TAKER.total_bps_cross(
                fee_rates_literal("2", "5.5"),
                fee_rates_literal("2", NEGATIVE_BPS),
            ))
        },
    },
    // -- execution ----------------------------------------------------------
    Entry {
        name: "execution::SlippageBudget::new",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "A negative tolerance is not a tight budget, it is a budget that demands \
              a price better than the touch, and the walk would return zero size while \
              reporting no error.",
        probe: || expect_err(SlippageBudget::new(bps(NEGATIVE_BPS))),
    },
    Entry {
        name: "execution::SlippageBudget::bps",
        class: Class::FencedBy("execution::SlippageBudget::new"),
        why: "Reads the private tuple field. No SlippageBudget exists holding a negative.",
        probe: || unreachable_if_negative_budget_is_refused(),
    },
    Entry {
        name: "execution::max_size_within_slippage",
        class: Class::FencedBy("execution::SlippageBudget::new"),
        why: "Takes the newtype rather than a bare Bps precisely so that the check \
              cannot be skipped at the call site. The book is walked against a budget \
              that is non-negative by construction.",
        probe: || {
            // Belt as well as braces: the fenced type still has to work, or
            // "unreachable" would be true for the wrong reason.
            let budget = SlippageBudget::new(bps("25")).expect("25 bps is a legal budget");
            let _ = max_size_within_slippage(&one_level_book(), Side::Buy, budget)
                .expect("a one-level book fills something");
            unreachable_if_negative_budget_is_refused()
        },
    },
    // -- funding ------------------------------------------------------------
    Entry {
        name: "funding::annualise",
        class: Class::Signed,
        why: "decisions/006 and 007: a published funding rate is negative roughly half \
              the time, and that is the case where shorts pay longs. Refusing it would \
              refuse half the market. Pinned independently by \
              funding::tests::annualising_a_negative_rate_keeps_the_sign.",
        probe: || {
            // -0.0001 per 8h * 1095 settlements = -0.1095, i.e. -10.95% APR.
            assert_eq!(
                annualise(
                    rate(NEGATIVE_RATE),
                    FundingInterval::EIGHT_HOURS,
                    RoundMode::COST
                ),
                Ok(rate("-0.1095")),
                "annualise must carry the sign, not clamp it"
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "funding::funding_payment",
        class: Class::Signed,
        why: "The sign law itself. A negative rate means shorts pay longs, so a long \
              holder receives. Clamping here would delete the entire reverse-carry half \
              of the product.",
        probe: || {
            // rate -0.0001, long: receipt sign is -1, so signed rate is +0.0001.
            // 100000 * 0.0001 = +10, a receipt.
            assert_eq!(
                funding_payment(money("100000"), rate(NEGATIVE_RATE), PositionSide::Long),
                Ok(money("10")),
                "a negative rate pays the long holder"
            );
            // The short side of the same rate pays out.
            assert_eq!(
                funding_payment(money("100000"), rate(NEGATIVE_RATE), PositionSide::Short),
                Ok(money("-10")),
                "and charges the short holder"
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "funding::periods_to_breakeven",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "This was the first hole behind F4, and the worse of the two. A negative \
              round trip cost fell into the `not positive` arm and returned Ok(0) — \
              breakeven at settlement zero, the most flattering answer the column can \
              print, and the row sorts first on it. net_apr_over_horizon had already \
              been hardened against exactly this input; its sibling had not.",
        probe: || {
            expect_err(periods_to_breakeven(
                rate("0.0001"),
                bps(NEGATIVE_BPS),
            ))
        },
    },
    Entry {
        name: "funding::net_apr_over_horizon",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "The cost is annualised and subtracted, so a -22 bp round trip over one 8h \
              settlement turns a +10.95% row into +251.85%. annual_receipt beside it is \
              signed by design and stays so.",
        probe: || {
            expect_err(net_apr_over_horizon(
                rate("0.1095"),
                bps("-22"),
                Millis::from_secs(8 * 3600),
            ))
        },
    },
    Entry {
        name: "funding::CarryLeg::from_snapshot",
        class: Class::Signed,
        why: "Carries the venue's published rate through unchanged — the field docs say \
              `do not pre-negate it`, and a fence here would be a pre-negation by \
              another name.",
        probe: || {
            let leg = CarryLeg::from_snapshot(&funding_snapshot(NEGATIVE_RATE), PositionSide::Long);
            assert_eq!(
                leg.rate,
                rate(NEGATIVE_RATE),
                "the leg must hold the venue's rate verbatim"
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "funding::CarryLeg::receipt_per_period",
        class: Class::Signed,
        why: "Applies the sign law to the leg's own signed rate. Both signs of the \
              result are real answers.",
        probe: || {
            // -0.0001 held long: receipt sign -1, so +0.0001 received per period.
            assert_eq!(
                carry_leg(NEGATIVE_RATE, PositionSide::Long).receipt_per_period(),
                Ok(rate("0.0001"))
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "funding::CarryLeg::annual_receipt",
        class: Class::Signed,
        why: "Annualises the signed per-period receipt. Each leg is annualised \
              separately and summed by the caller; decisions/006 forbids rate_a - rate_b.",
        probe: || {
            // +0.0001 per 8h * 1095 = +0.1095.
            assert_eq!(
                carry_leg(NEGATIVE_RATE, PositionSide::Long).annual_receipt(),
                Ok(rate("0.1095"))
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "funding::CarryLeg::payment",
        class: Class::Signed,
        why: "One settlement in money, signed. Same reason as funding::funding_payment, \
              which it delegates to.",
        probe: || {
            assert_eq!(
                carry_leg(NEGATIVE_RATE, PositionSide::Long).payment(money("100000")),
                Ok(money("10"))
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "funding::CarryLeg::periods_to_breakeven",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "Delegates to funding::periods_to_breakeven and so inherits the fix. Listed \
              separately because a delegation that stops delegating is invisible in a \
              diff of the callee. The leg's own rate stays signed: a receipt running \
              against us yields NoBreakeven, which is an answer about the market.",
        probe: || {
            // Short holder of a positive rate receives, so the receipt is positive
            // and the cost is the only thing left that can be malformed.
            expect_err(
                carry_leg("0.0001", PositionSide::Short).periods_to_breakeven(bps(NEGATIVE_BPS)),
            )
        },
    },
    Entry {
        name: "funding::CarryLeg::time_to_breakeven",
        class: Class::Refuses(MathError::NegativeFeeRate),
        why: "Same delegation, converted to wall time. A zero here would read as \
              `profitable immediately` on the screen.",
        probe: || {
            expect_err(
                carry_leg("0.0001", PositionSide::Short).time_to_breakeven(bps(NEGATIVE_BPS)),
            )
        },
    },
    // -- persistence --------------------------------------------------------
    Entry {
        name: "persistence::mean_settled_rate",
        class: Class::Signed,
        why: "A history whose signs were normalised away would make an inverted carry \
              look like a persistent one. SettledRate::rate documents the sign as \
              load-bearing.",
        probe: || {
            // Two settlements of -0.0001: mean -0.0001, exactly.
            assert_eq!(
                mean_settled_rate(&history(NEGATIVE_RATE), RoundMode::Trunc),
                Ok(rate(NEGATIVE_RATE))
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "persistence::realised_receipt_per_period",
        class: Class::Signed,
        why: "The realised counterpart of the sign law. Both signs are real: it is what \
              the history would actually have paid the side we would have held.",
        probe: || {
            // mean -0.0001, long receipt sign -1 => +0.0001.
            assert_eq!(
                realised_receipt_per_period(&history(NEGATIVE_RATE), PositionSide::Long),
                Ok(rate("0.0001"))
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "persistence::annualise_realised",
        class: Class::Signed,
        why: "Annualises the signed realised receipt. Refusing a negative history would \
              silently drop every instrument that paid shorts.",
        probe: || {
            assert_eq!(
                annualise_realised(
                    &history(NEGATIVE_RATE),
                    FundingInterval::EIGHT_HOURS,
                    PositionSide::Long
                ),
                Ok(rate("0.1095"))
            );
            Probe::SignedThrough
        },
    },
    Entry {
        name: "persistence::coverage",
        class: Class::Ignores,
        why: "Counts settlements and spans time. It never reads SettledRate::rate, so \
              the rate's sign cannot reach the answer — and the probe proves that rather \
              than trusting the reading.",
        probe: || {
            let negative = coverage(&history(NEGATIVE_RATE), FundingInterval::EIGHT_HOURS, 3);
            let positive = coverage(&history("0.0001"), FundingInterval::EIGHT_HOURS, 3);
            if negative == positive {
                Probe::Unaffected
            } else {
                Probe::SignedThrough
            }
        },
    },
    Entry {
        name: "persistence::sign_flips",
        class: Class::Signed,
        why: "Counting sign changes is the entire function. A fence against negative \
              rates would leave it nothing to count.",
        probe: || {
            let entries = [
                SettledRate {
                    settled_at: WallTs::from_millis(0),
                    rate: rate(NEGATIVE_RATE),
                },
                SettledRate {
                    settled_at: WallTs::from_millis(28_800_000),
                    rate: rate("0.0001"),
                },
            ];
            assert_eq!(sign_flips(&entries), 1, "one crossing, one flip");
            Probe::SignedThrough
        },
    },
    // -- basis --------------------------------------------------------------
    Entry {
        name: "basis::convergence_capture",
        class: Class::Signed,
        why: "Basis is signed at both ends, and a negative capture is the common \
              outcome. Clamping it at zero would delete the loss the position actually \
              took.",
        probe: || {
            // entry -0.002, exit 0: moved -0.002, a loss. 100000 * -0.002 = -200.
            assert_eq!(
                convergence_capture(rate("-0.002"), Rate::ZERO, money("100000")),
                Ok(money("-200"))
            );
            Probe::SignedThrough
        },
    },
    // -- sizing -------------------------------------------------------------
    Entry {
        name: "sizing::margin_required",
        class: Class::Refuses(MathError::NonPositiveLeverage),
        why: "Leverage is a rate here. A negative one reciprocates to a negative \
              fraction and reports a negative margin requirement, i.e. that the position \
              funds itself.",
        probe: || expect_err_with(margin_required(money("100000"), rate("-3"))),
    },
    // -- liquidation --------------------------------------------------------
    Entry {
        name: "liquidation::liquidation_price",
        class: Class::Refuses(MathError::MaintenanceRateOutOfRange),
        why: "LiquidationInputs has pub fields, so the maintenance rate is checked here \
              rather than at construction. A negative rate moves the cliff past the \
              entry price and reports a position that liquidates in the wrong direction.",
        probe: || expect_err_with(liquidation_price(&liq_inputs("-0.005"))),
    },
    Entry {
        name: "liquidation::LiquidationInputs::distance_bps",
        class: Class::Refuses(MathError::MaintenanceRateOutOfRange),
        why: "Delegates to liquidation::liquidation_price, so it inherits check(). \
              Listed separately for the same reason the CarryLeg delegations are.",
        probe: || expect_err_with(liq_inputs("-0.005").distance_bps()),
    },
];

/// Collapse a `Result` whose error is the thing under test.
fn expect_err<T>(r: Result<T, MathError>) -> Probe {
    match r {
        Err(e) => Probe::Refused(e),
        Ok(_) => panic!("a negative rate was accepted where the registry says it is refused"),
    }
}

/// [`expect_err`] for entry points whose `Ok` type is not `Debug`-free of noise.
fn expect_err_with<T>(r: Result<T, MathError>) -> Probe {
    expect_err(r)
}

fn unreachable_if_negative_tier_is_refused() -> Probe {
    match FeeTable::new(vec![VolumeTier {
        min_volume_30d: Money::ZERO,
        maker: bps("2"),
        taker: bps(NEGATIVE_BPS),
    }]) {
        Err(MathError::NegativeFeeRate) => Probe::Unreachable,
        other => panic!("a FeeTable holding a negative rate was constructible: {other:?}"),
    }
}

fn unreachable_if_negative_budget_is_refused() -> Probe {
    match SlippageBudget::new(bps(NEGATIVE_BPS)) {
        Err(MathError::NegativeFeeRate) => Probe::Unreachable,
        other => panic!("a SlippageBudget holding a negative was constructible: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The scanner
// ---------------------------------------------------------------------------

/// Every module of this crate, by the name the registry uses, and its source.
///
/// `include_str!` rather than reading the filesystem at runtime: the file has to
/// exist at compile time, so a module that is renamed breaks the build instead
/// of silently dropping out of the enumeration.
const SOURCES: &[(&str, &str)] = &[
    ("basis", include_str!("../src/basis.rs")),
    ("borrow", include_str!("../src/borrow.rs")),
    ("error", include_str!("../src/error.rs")),
    ("execution", include_str!("../src/execution.rs")),
    ("fees", include_str!("../src/fees.rs")),
    ("funding", include_str!("../src/funding.rs")),
    ("liquidation", include_str!("../src/liquidation.rs")),
    ("persistence", include_str!("../src/persistence.rs")),
    ("sizing", include_str!("../src/sizing.rs")),
];

const LIB_RS: &str = include_str!("../src/lib.rs");

/// Whether a type standing in a parameter position can carry a rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bearing {
    /// Carries, or is, a `Rate` or a `Bps`.
    Rate,
    /// Cannot carry one.
    No,
}

/// Every type name that may appear in a parameter position of a public function
/// in this crate, and whether it can carry a rate.
///
/// The test fails on any name it does not find here. That is the point: a new
/// parameter type cannot enter a public signature without someone deciding, in
/// writing, whether a rate can travel inside it.
///
/// The `Rate` entries defined by *this* crate are cross-checked against a
/// transitive closure computed from the struct definitions, so they cannot drift.
/// The ones defined by `arb-types` are declared by hand — that crate is frozen
/// and not mine to parse — and each is backed by a probe above:
/// `BorrowSnapshot` by `borrow::annual_borrow_cost`, `FundingSnapshot` by
/// `funding::CarryLeg::from_snapshot`, `RateHistory` and `SettledRate` by
/// `persistence::mean_settled_rate` and `persistence::sign_flips`.
const PARAM_TYPES: &[(&str, Bearing)] = &[
    // The two rate representations. decisions/002: same raw integer, two scales.
    ("Rate", Bearing::Rate),
    ("Bps", Bearing::Rate),
    // arb-types aggregates that carry one.
    ("BorrowSnapshot", Bearing::Rate),
    ("FundingSnapshot", Bearing::Rate),
    ("RateHistory", Bearing::Rate),
    ("SettledRate", Bearing::Rate),
    // This crate's aggregates that carry one. Cross-checked below.
    ("FeeRates", Bearing::Rate),
    ("FeeTable", Bearing::Rate),
    ("VolumeTier", Bearing::Rate),
    ("SlippageBudget", Bearing::Rate),
    ("CarryLeg", Bearing::Rate),
    ("LiquidationInputs", Bearing::Rate),
    // Money-like and market-like types that cannot.
    ("Price", Bearing::No),
    ("Qty", Bearing::No),
    ("Money", Bearing::No),
    ("BookSnapshot", Bearing::No),
    ("ContractSpec", Bearing::No),
    ("Execution", Bearing::No),
    ("HistoryCoverage", Bearing::No),
    ("RoundTrip", Bearing::No),
    ("FeeRole", Bearing::No),
    ("MathError", Bearing::No),
    // Modes, sides, cadences, clocks.
    ("RoundMode", Bearing::No),
    ("Side", Bearing::No),
    ("PositionSide", Bearing::No),
    ("MarketKind", Bearing::No),
    ("FundingInterval", Bearing::No),
    ("BorrowInterval", Bearing::No),
    ("Millis", Bearing::No),
    // Containers and primitives. A container is transparent: the scan looks at
    // every identifier in the type, so `Option<Rate>` is caught by `Rate`.
    ("Option", Bearing::No),
    ("Vec", Bearing::No),
    ("bool", Bearing::No),
    ("u64", Bearing::No),
    ("usize", Bearing::No),
    ("i64", Bearing::No),
    ("i128", Bearing::No),
    ("str", Bearing::No),
];

fn bearing(name: &str) -> Option<Bearing> {
    PARAM_TYPES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, b)| *b)
}

/// One public function, as read out of the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sig {
    /// `module::Type::function` or `module::function`.
    qualified: String,
    /// One entry per parameter, the text of its type. A `self` receiver is
    /// rendered as the enclosing type's name.
    param_types: Vec<String>,
    line: usize,
}

/// Everything after the first top-level `#[cfg(test)]` is test code.
///
/// Crude on purpose: every module in this crate puts its tests in exactly one
/// trailing `#[cfg(test)] mod tests`, and a module that stops doing so will show
/// up as test helpers appearing in the enumeration, which fails loudly rather
/// than quietly widening the fence.
fn product_code(src: &str) -> &str {
    match src.find("\n#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    }
}

/// Identifiers in a type expression, lifetimes excluded.
fn idents(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut lifetime = false;
    let mut prev = ' ';
    for c in s.chars() {
        if c.is_alphanumeric() || c == '_' {
            if cur.is_empty() && prev == '\'' {
                lifetime = true;
            }
            cur.push(c);
        } else if !cur.is_empty() {
            if !lifetime {
                out.push(core::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            lifetime = false;
        }
        prev = c;
    }
    if !cur.is_empty() && !lifetime {
        out.push(cur);
    }
    out
}

/// Split a parameter list on commas that are not inside brackets.
fn split_params(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in list.chars() {
        match c {
            '<' | '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            '>' | ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => out.push(core::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The type half of one parameter, or the owner's name for a `self` receiver.
fn param_type(param: &str, owner: Option<&str>) -> String {
    let mut depth = 0i32;
    for (i, c) in param.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ':' if depth == 0 => return param[i + 1..].trim().to_string(),
            _ => {}
        }
    }
    // No colon at depth zero: `self`, `&self`, `&mut self`.
    owner.unwrap_or("Self").to_string()
}

/// The name a top-level `impl` block is for. `impl X for Y` yields `Y`.
fn impl_owner(line: &str) -> Option<String> {
    let rest = line.strip_prefix("impl ")?;
    let target = match rest.find(" for ") {
        Some(i) => &rest[i + 5..],
        None => rest,
    };
    let name: String = target
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Parse the public function signatures out of one module's product code.
fn public_signatures(module: &str, src: &str) -> Vec<Sig> {
    let lines: Vec<&str> = product_code(src).lines().collect();
    let mut out = Vec::new();
    let mut owner: Option<String> = None;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("impl ") {
            owner = impl_owner(line);
        } else if line == "}" {
            owner = None;
        }

        let trimmed = line.trim_start();
        let is_fn = trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub const fn ")
            || trimmed.starts_with("pub(crate) fn ");
        if is_fn && !trimmed.starts_with("pub(crate) fn ") {
            // Accumulate until the body opens.
            let mut text = String::new();
            let mut j = i;
            while j < lines.len() {
                text.push(' ');
                text.push_str(lines[j].trim());
                if lines[j].contains('{') || lines[j].trim_end().ends_with(';') {
                    break;
                }
                j += 1;
            }
            if let Some(sig) = parse_sig(module, owner.as_deref(), &text, i + 1) {
                out.push(sig);
            }
            i = j;
        }
        i += 1;
    }
    out
}

fn parse_sig(module: &str, owner: Option<&str>, text: &str, line: usize) -> Option<Sig> {
    let after_fn = text.find(" fn ")? + 4;
    let open = text[after_fn..].find('(')? + after_fn;
    let name = text[after_fn..open].trim().to_string();

    // Matching close paren for the parameter list.
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;

    let param_types = split_params(&text[open + 1..close])
        .iter()
        .map(|p| param_type(p, owner))
        .collect();

    let qualified = match owner {
        Some(o) => format!("{module}::{o}::{name}"),
        None => format!("{module}::{name}"),
    };
    Some(Sig {
        qualified,
        param_types,
        line,
    })
}

fn all_signatures() -> Vec<Sig> {
    SOURCES
        .iter()
        .flat_map(|(m, src)| public_signatures(m, src))
        .collect()
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// The scanner has to actually find things, or every test below passes vacuously.
///
/// This is the test that guards the others. A parser that silently returns an
/// empty list would make "every rate-accepting entry point is registered" true
/// and meaningless — which is the precise failure mode of the reading-based
/// check that F4 relied on before.
#[test]
fn the_scanner_finds_the_functions_it_is_supposed_to_find() {
    let sigs = all_signatures();
    assert!(
        sigs.len() >= 40,
        "the scanner found only {} public functions across {} modules, which is fewer \
         than this crate has. The parser is broken, and a broken parser makes every \
         other test in this file pass for free.",
        sigs.len(),
        SOURCES.len()
    );

    // Named landmarks: a free function, an inherent method, a `const fn`, and a
    // multi-line signature. Each exercises a different branch of the parser.
    for expected in [
        "funding::annualise",
        "funding::CarryLeg::receipt_per_period",
        "fees::FeeRates::rate",
        "execution::max_size_within_slippage",
        "sizing::contracts_for_notional",
        "persistence::HistoryCoverage::covers",
    ] {
        assert!(
            sigs.iter().any(|s| s.qualified == expected),
            "the scanner did not find {expected}; found: {:?}",
            sigs.iter().map(|s| &s.qualified).collect::<Vec<_>>()
        );
    }

    // And the parameter list of a known signature, so a parser that finds names
    // but loses types cannot pass either.
    let annualise = sigs
        .iter()
        .find(|s| s.qualified == "funding::annualise")
        .expect("just asserted it is there");
    assert_eq!(
        annualise.param_types,
        vec!["Rate", "FundingInterval", "RoundMode"],
        "parameter types were not read correctly"
    );

    let from_snapshot = sigs
        .iter()
        .find(|s| s.qualified == "funding::CarryLeg::from_snapshot")
        .expect("CarryLeg::from_snapshot exists");
    assert_eq!(
        from_snapshot.param_types,
        vec!["&FundingSnapshot", "PositionSide"],
        "a reference parameter was not read correctly"
    );

    let receipt = sigs
        .iter()
        .find(|s| s.qualified == "funding::CarryLeg::receipt_per_period")
        .expect("CarryLeg::receipt_per_period exists");
    assert_eq!(
        receipt.param_types,
        vec!["CarryLeg"],
        "a self receiver must resolve to the owning type, or every method on a \
         rate-bearing type drops out of the enumeration"
    );
}

/// Every module in `lib.rs` is scanned.
///
/// Without this, adding a module and forgetting to add it to [`SOURCES`] would
/// exempt the whole module from the fence.
#[test]
fn every_module_of_this_crate_is_scanned() {
    let declared: Vec<&str> = LIB_RS
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub mod "))
        .filter_map(|l| l.strip_suffix(';'))
        .collect();
    assert!(
        !declared.is_empty(),
        "no `pub mod` found in lib.rs; the parse is wrong"
    );
    for m in &declared {
        assert!(
            SOURCES.iter().any(|(name, _)| name == m),
            "module `{m}` is public in lib.rs but is not in SOURCES, so nothing in it \
             is enumerated. Add it."
        );
    }
    for (name, _) in SOURCES {
        assert!(
            declared.contains(name),
            "SOURCES names `{name}`, which lib.rs does not declare"
        );
    }
}

/// Every type appearing in a public parameter position is classified.
#[test]
fn no_parameter_type_is_unclassified() {
    let mut unknown: Vec<String> = Vec::new();
    for sig in all_signatures() {
        for ty in &sig.param_types {
            for id in idents(ty) {
                // `mut`, `dyn`, `impl` and friends are not types.
                if matches!(id.as_str(), "mut" | "dyn" | "impl" | "Self") {
                    continue;
                }
                if bearing(&id).is_none() && !unknown.contains(&id) {
                    unknown.push(format!("{id} (in {})", sig.qualified));
                }
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "these parameter types are not in PARAM_TYPES, so nobody has decided whether a \
         rate can travel inside them: {unknown:?}"
    );
}

/// The rate-bearing types *this crate defines* are derived, not remembered.
///
/// Scans the struct definitions for fields whose types mention a rate, takes the
/// transitive closure, and compares it with what [`PARAM_TYPES`] claims. A new
/// struct in this crate that carries a `Rate` therefore becomes rate-bearing
/// automatically, and any function taking it starts failing the registry test
/// until it is classified.
#[test]
fn the_rate_bearing_types_of_this_crate_are_computed_not_declared() {
    // name -> identifiers mentioned in its field types
    let mut fields: Vec<(String, Vec<String>)> = Vec::new();
    for (_, src) in SOURCES {
        let code = product_code(src);
        let lines: Vec<&str> = code.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let l = lines[i];
            if let Some(rest) = l.strip_prefix("pub struct ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let mut mentioned = Vec::new();
                if rest.contains('(') {
                    // Tuple struct: the whole line carries the field types.
                    mentioned.extend(idents(rest.trim_start_matches(&name as &str)));
                } else {
                    let mut j = i + 1;
                    while j < lines.len() && lines[j] != "}" {
                        let f = lines[j].trim();
                        if !f.starts_with("//") && f.contains(':') {
                            if let Some(k) = f.find(':') {
                                mentioned.extend(idents(&f[k + 1..]));
                            }
                        }
                        j += 1;
                    }
                    i = j;
                }
                fields.push((name, mentioned));
            }
            i += 1;
        }
    }
    assert!(
        fields.iter().any(|(n, _)| n == "FeeRates"),
        "the struct scan found no FeeRates; it is broken. Found: {:?}",
        fields.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

    // Seeds: the rate types themselves, plus the arb-types aggregates declared
    // by hand (that crate is frozen and is not parsed here).
    let mut bearing_set: Vec<String> = PARAM_TYPES
        .iter()
        .filter(|(n, b)| {
            *b == Bearing::Rate && !fields.iter().any(|(name, _)| name == n)
        })
        .map(|(n, _)| (*n).to_string())
        .collect();

    // Fixpoint.
    loop {
        let mut grew = false;
        for (name, mentioned) in &fields {
            if bearing_set.contains(name) {
                continue;
            }
            if mentioned.iter().any(|m| bearing_set.contains(m)) {
                bearing_set.push(name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    for (name, _) in &fields {
        let computed = bearing_set.contains(name);
        // Only types that actually stand in a parameter position are declared.
        if let Some(declared) = bearing(name) {
            assert_eq!(
                computed,
                declared == Bearing::Rate,
                "PARAM_TYPES says `{name}` is {declared:?}, but its field types say \
                 rate-bearing = {computed}. One of the two is wrong, and the field \
                 types are the source of truth."
            );
        } else {
            assert!(
                !computed,
                "`{name}` carries a rate but is not in PARAM_TYPES. It will become \
                 invisible to the fence the moment a public function takes one."
            );
        }
    }
}

/// **F4.** Every public entry point that can be handed a rate is registered,
/// and every registered entry point still exists.
#[test]
fn every_rate_accepting_entry_point_is_registered() {
    let sigs = all_signatures();

    let mut missing: Vec<String> = Vec::new();
    for sig in &sigs {
        let takes_rate = sig.param_types.iter().any(|ty| {
            idents(ty)
                .iter()
                .any(|id| bearing(id) == Some(Bearing::Rate))
        });
        if !takes_rate {
            continue;
        }
        if !REGISTRY.iter().any(|e| e.name == sig.qualified) {
            missing.push(format!("{} (line {})", sig.qualified, sig.line));
        }
    }
    assert!(
        missing.is_empty(),
        "these public entry points accept a rate and are not classified in REGISTRY. \
         Each one is a place a negative rate can enter unchecked, which is OPEN.md F4 \
         reopening: {missing:?}"
    );

    // The other direction: a registry entry naming a function that no longer
    // exists is a fence guarding nothing, and it would keep passing forever.
    let mut stale: Vec<&str> = Vec::new();
    for e in REGISTRY {
        if !sigs.iter().any(|s| s.qualified == e.name) {
            stale.push(e.name);
        }
    }
    assert!(
        stale.is_empty(),
        "REGISTRY names entry points the scanner cannot find. Renamed, or moved? \
         A stale entry is a fence around nothing: {stale:?}"
    );
}

/// Every registered entry point behaves the way its classification claims.
#[test]
fn every_registered_entry_point_behaves_as_classified() {
    for e in REGISTRY {
        assert!(
            !e.why.trim().is_empty(),
            "{} carries no justification; hard rule 5 applies to a fence as much as to \
             a threshold",
            e.name
        );
        let got = (e.probe)();
        match e.class {
            Class::Refuses(expected) => assert_eq!(
                got,
                Probe::Refused(expected),
                "{} is classified as refusing a negative rate with {expected:?}",
                e.name
            ),
            Class::Signed => assert_eq!(
                got,
                Probe::SignedThrough,
                "{} is classified as carrying the sign through",
                e.name
            ),
            Class::Ignores => assert_eq!(
                got,
                Probe::Unaffected,
                "{} is classified as ignoring the rate, so flipping its sign must not \
                 move the answer",
                e.name
            ),
            Class::Inert => assert_eq!(
                got,
                Probe::Identity,
                "{} is classified as returning the stored value unchanged",
                e.name
            ),
            Class::FencedBy(ctor) => {
                assert_eq!(
                    got,
                    Probe::Unreachable,
                    "{} is classified as unreachable with a negative rate",
                    e.name
                );
                let guard = REGISTRY.iter().find(|c| c.name == ctor);
                match guard {
                    Some(c) => assert!(
                        matches!(c.class, Class::Refuses(_)),
                        "{} is fenced by {ctor}, but {ctor} is classified {:?} rather \
                         than Refuses, so the fence does not refuse anything",
                        e.name,
                        c.class
                    ),
                    None => panic!(
                        "{} claims to be fenced by {ctor}, which is not in REGISTRY at \
                         all",
                        e.name
                    ),
                }
            }
        }
    }
}

/// No two registry entries name the same function.
///
/// A duplicate would let a weaker classification sit beside a stronger one and
/// the stronger one would look satisfied.
#[test]
fn the_registry_has_no_duplicates() {
    for (i, a) in REGISTRY.iter().enumerate() {
        for b in REGISTRY.iter().skip(i + 1) {
            assert_ne!(a.name, b.name, "{} is registered twice", a.name);
        }
    }
}

/// The two holes this file was written to close, asserted directly.
///
/// The registry already covers them, but a registry entry can be argued away by
/// reclassification. These two cannot: they name the exact input and the exact
/// wrong answer that was being returned before.
#[test]
fn a_negative_round_trip_cost_never_reads_as_breakeven_at_settlement_zero() {
    // Before the fix: Ok(0). A row whose breakeven column reads 0 sorts first and
    // reads as "profitable from the moment of entry".
    assert_eq!(
        periods_to_breakeven(rate("0.0001"), bps("-22")),
        Err(MathError::NegativeFeeRate),
        "a negative round trip cost must be refused, not treated as free"
    );
    // A genuinely zero cost stays a legal, if unrealistic, input.
    assert_eq!(
        periods_to_breakeven(rate("0.0001"), Bps::ZERO),
        Ok(0),
        "zero cost is nothing to pay off, which is not the same as a negative cost"
    );
    // And a receipt running against us is still an answer about the market.
    assert_eq!(
        periods_to_breakeven(rate("-0.0001"), bps("22")),
        Err(MathError::NoBreakeven)
    );
}

#[test]
fn four_negative_fee_legs_never_sum_to_a_negative_round_trip() {
    // Before the fix: Ok(-22 bps), from a FeeRates literal that never saw
    // FeeRates::new. -22 bps then flows into net_apr_over_horizon (which refuses
    // it) and into periods_to_breakeven (which did not).
    let rebated = fee_rates_literal("2", "-5.5");
    assert_eq!(
        RoundTrip::ALL_TAKER.total_bps(rebated),
        Err(MathError::NegativeFeeRate)
    );
    // A negative maker rate is refused even under an all-taker configuration:
    // which legs happen to consult the schedule is not a reason to accept a
    // malformed one.
    assert_eq!(
        RoundTrip::ALL_TAKER.total_bps(fee_rates_literal("-2", "5.5")),
        Err(MathError::NegativeFeeRate)
    );
    // The honest schedule still totals what it did: 4 x 5.5 = 22 bps.
    let honest = FeeRates::new(bps("2"), bps("5.5"), true).expect("legal schedule");
    assert_eq!(RoundTrip::ALL_TAKER.total_bps(honest), Ok(bps("22")));
}
