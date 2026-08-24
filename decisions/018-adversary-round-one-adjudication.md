# 018 — Adjudication of the adversary's first ten failing tests

**Decided.** Of the ten failing tests engineer 7 delivered, **eight name real
defects** in library code and **two are arithmetic errors in the test's own
expectation**. The eight are fixed in library code. The two are corrected in
`arb-adversary`, keeping the property each test asserts and repairing only the
number it compares against. No correct arithmetic in `arb-num` or `arb-math` is
changed to make a red test green.

This file exists because the split is the whole decision. A red test is an
accusation, and the temptation in both directions is real: to dismiss a test that
is inconvenient, and to "fix" working code until a wrong test passes. Rule: a
disputed expectation is only overruled with the exact rational derivation written
down here, so that the next person can check the derivation instead of trusting
the verdict.

## Method

Every one of the ten was re-derived as an **exact rational**, by hand, from the
venue arithmetic — not by running the code and reading its output. Where code and
test disagreed, the rational answer decided which was wrong. Nine of the ten
derivations are reproduced below or in the fixing commit's test comments.

## The eight real defects

| # | Test (in `arb-adversary`) | Defect in | Defect |
|---|------|-------|--------|
| 1 | `depth::capacity_solves_for_the_average_not_for_the_deepest_qualifying_level` | `arb-math::execution` | capacity walk |
| 2 | `depth::capacity_on_a_btc_scale_ladder_is_understated_by_one_hundred_and_sixty_nine_coins` | `arb-math::execution` | capacity walk |
| 3 | `depth::capacity_mirrors_on_the_sell_side` | `arb-math::execution` | capacity walk |
| 4 | `depth::any_size_larger_than_the_reported_capacity_must_breach_the_budget` | `arb-math::execution` | capacity walk |
| 5 | `rounding::reported_capacity_must_actually_fit_inside_the_budget` | `arb-math::execution` | capacity cost mode |
| 6 | `rounding::an_ambiguous_fee_table_must_not_resolve_in_our_favour` | `arb-math::fees` | duplicate tier threshold |
| 7 | `rounding::a_negative_round_trip_cost_must_not_become_positive_annual_return` | `arb-math::funding` | missing domain check |
| 8 | `staleness::a_book_snapshot_may_not_be_stamped_fresher_than_its_newest_data` | `arb-book::state` | timestamp laundering |

**1–4, the capacity walk.** `max_size_within_slippage` solved for the size whose
*average* fill price sits on the slippage limit, which is the right question. But
when the subsidy from the cheap levels covered a dear level **whole**, the walk
added that level's quantity to `q`, never charged its cost to `c`, and then
`break`ed unconditionally. So the walk stopped at the first level past the limit
and did so with an under-counted cost. On the 68 000-base one-BTC-per-tick ladder
at 25 bps this reported **172 BTC** where **341** fit — capacity understated
without bound as the ladder deepens. Understated capacity is not a safe error: it
is size we own, cannot see, and will not trade.

Fixed by charging a whole-consumed level to `c` and continuing the walk; the
`break` now happens only where the partial solve genuinely lands inside a level,
or where the numerator or denominator stops being positive.

**5, the cost mode.** `c` accumulated with `RoundMode::HalfEven`. HalfEven is the
one mode that can round a **cost down**, and a cost rounded down is a **capacity
rounded up** — so the function could return a size whose executed average
breached the very budget it was given, by an ulp. It now accumulates with
`conservative_mode(side)`, the same mode `executable_price` charges with, so the
solve and the execution agree to the last ulp. Verified: on the adversary's
ladder the correct answer is `0.199999999998` at exactly 50.0 bps; HalfEven
returned `0.2` at 50.00000001 bps, which is the failure message verbatim.

The complementary rounding — `limit * q` on the other side of the subtraction —
must go the *opposite* way to shrink the numerator, and is now side-dependent.

**6, duplicate tier threshold.** `FeeTable::new` validated sortedness with
`a.min_volume_30d <= b.min_volume_30d`, so two rows at the same threshold passed.
`resolve` then kept the last qualifying row, which on a schedule that descends in
price is the **cheaper** one: 3 bps charged where 5.5 bps is owed, i.e. 10 bps off
a round trip against a claimed 12 bps edge. Now `<`, with a distinct
`MathError::DuplicateTierThreshold` so a caller can tell a mis-ordered table from
an ambiguous one. The adversary's test is fix-agnostic (it accepts either a
refusal or the worse row) and passes.

**7, negative round-trip cost.** `net_apr_over_horizon` *subtracts* the
annualised cost, so a negative cost is **added** to the return:
`0.1095 - (-0.0022 * 31_536_000 / 28_800) = 0.1095 + 2.409 = +2.5185`. A −22 bp
cost renders a +10.95% row as **+251.85% APR** — the error runs in the flattering
direction and lands the corrupted row at the **top** of the ranking, which is the
one place an error is certain to be acted on. `fee_on_notional`, `FeeRates::new`
and `SlippageBudget::new` all refuse a negative rate; this was the hole in that
fence. Now `MathError::NegativeFeeRate`.

**8, timestamp laundering.** `BookState::snapshot(&self, observed_at: LocalTs)`
stamped the emitted `BookSnapshot` with the **caller's** argument while
`self.observed_at` held the truth. `st.snapshot(now)` therefore produced a
snapshot whose age is zero forever, defeating every staleness predicate
downstream — on data that may be minutes old. The parameter is removed:
`snapshot(&self)` stamps `self.observed_at`. This is an API break in `arb-book`,
authorised here; all call sites are tests.

## The two adversary errors

Both hold as *properties*; both compare against a number the adversary computed
wrongly. The library answers are right.

**`a_net_apr_never_understates_the_cost_it_amortises`.** The disputed case is a
**22 bp** round trip over a **7-day** (604 800 s) horizon, chosen because the
scaling is inexact. The annualised cost is exactly

    0.0022 * 31_536_000 / 604_800 = 69_379.2 / 604_800
                                  = 693_792 / 6_048_000
                                  = 803 / 7000
                                  = 0.114714285714285714...

`RoundMode::COST` gives raw `114_714_285_715`, so
`net = 0.1095 - 0.114714285715` = raw **−5_214_285_715**, which is exactly what
the code returns. The adversary's expected `−5_206_349_207` implies an annualised
cost of `0.114706349206349...` = `14453/126000`, i.e. exactly `803/7000 - 1/126000`
— an off-by-one in the numerator of his own long division of `69_379.2/604_800`.
Which step produced it is **[не знаю]**; that it is low by exactly `1/126000` is
**[факт, проверено]**, and that is enough to place the error in the test rather
than in the code. Corrected to `−5_214_285_715`, the paying-side figure to
`−224_214_285_715`, and the threshold to `rate("-0.005214285714")`. The property —
that the amortised cost is never rounded in our favour — is preserved and still
fails if the cost term is ever rounded toward zero.

**`margin_times_leverage_is_never_below_the_notional`.** `1/7` at scale 1e12
rounds `COST` to `142_857_142_858`, `m7 * 7 = 1_000_000.000000000006 >= 1_000_000`,
so the invariant **holds** — the code is correct. Two separate slips in the
expectation: the test subtracted `money("142857.142857")`, the truth truncated at
**six** decimal places, while asserting the **twelve**-place drift; and the
twelve-place difference is raw `857_143`, not `857_142`
(`142_857_142_858_000_000 - 142_857_142_857_142_857`). Corrected to subtract
`money("142857.142857142857")` and expect `857_143`. The real drift from the
unrounded `1/7` is `0.00000085714285714...` USDT, of which `857_143` raw is the
representable ceiling — the safe side, as the whole test asserts.

Both of these are the double-rounding already recorded as `OPEN.md` C3, and both
run in the **safe** direction: `margin_required` rounds twice, both away from
zero, so it is at or above the true requirement by about `2e-12` relative. That
remains open as a documented conservatism, not a defect.

## Consequences

- The adversary keeps his mandate: he may only add failing tests, and he may not
  edit library code. Correcting his own expectation is his to do, on this record.
- Two tests were overruled without anybody moving the code they accused. That is
  only legitimate because the derivation is written here; a verdict without one
  would be indistinguishable from convenience.
- The round's earlier adjudication of these same ten tests was never written to
  `decisions/` before the session ended. Per the round's own rule — a decision
  that never reaches `decisions/` counts as not taken — this file *is* the
  decision, re-derived from scratch rather than inherited.
