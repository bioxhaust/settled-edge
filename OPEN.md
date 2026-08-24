# OPEN — unresolved questions, and why each is still open

Maintained by the lead. An item leaves this file only by becoming a decision in
`decisions/` or by being measured against a live API.

Every claim below is tagged `[fact, verified]`, `[assumption]`, or `[do not know]`
per hard rule 8. "Verified" in this round means *verified against the code or the
toolchain in front of me* — it never means verified against an exchange, because
no exchange was reachable.

---

## A. Economics that could eat the entire claimed edge

These are first in the file because they matter more than anything else in it.
The round's stated edge is ~12 bps. Each item here is capable of exceeding that
on its own, and none of them is modelled yet.

### A1. Borrow cost on any short-spot leg — NOT MODELLED

`[fact, verified]` `arb-math` models exchange fees and slippage. It does not
model borrow.

`[assumption]` The classic cash-and-carry we rank (long spot, short perp) needs
no borrow — you own the spot. But the *reverse* carry (short spot, long perp),
which is what a negative funding rate calls for, requires borrowing the base
asset. Margin-borrow rates on the venues in scope are commonly quoted at daily
rates that annualise into the tens of percent, i.e. one to three orders of
magnitude larger than 12 bps.

**Consequence if wrong:** every negative-funding opportunity we surface is
overstated, possibly to the point of sign inversion. **Until this is modelled,
negative-funding rows must not be presented as actionable.** Whether the current
`arb-scan` excludes them or merely ranks them lower is `[do not know]` pending
the round's integration report.

### A2. Inter-venue transfer cost and latency — NOT MODELLED

`[assumption]` A cross-venue position needs capital on both venues. Rebalancing
costs a withdrawal fee plus chain time, and during that time the position is
un-hedged or the capital is idle. Neither the fee nor the delay is in any cost
model in this workspace `[fact, verified]`.

**Consequence:** the reported APR is a *steady-state* figure that ignores the
cost of reaching steady state. For a short holding period, that omission can
dominate. The engine must not present the number as a return on deployed capital
until this is either modelled or explicitly excluded in the output.

### A3. Margin mode and maintenance tier both change liquidation distance

`[assumption]` Cross margin and isolated margin give materially different
liquidation prices for the same nominal position, and venues differ in which is
default. `[fact, verified]` `arb-math`'s liquidation module takes margin as an
input, which is the right shape, but the mode a given number represents is **not**
carried in the type — there is no `MarginMode` anywhere in the workspace, and
`LiquidationInputs` has no field for it. Decision 017 closes the ambiguity by
fixing the model at **isolated margin, one-way mode, one position, one maintenance
tier**; it does not close the risk, which is why this item stays open.

`[assumption]` Tiered maintenance margin rises with position size on every venue
in scope, so the single `maintenance_rate` input is correct only within one tier.

**Consequence, and the asymmetry that matters:** the two error directions are not
equally bad.

- A **cross-margined** account fed these numbers is told it is **closer** to
  liquidation than it truly is — a wrong number in the safe direction.
- Using the **tier of a smaller position than is actually held** reports the cliff
  **further away** than the truth — a wrong number in the **unsafe** direction,
  telling the operator there is room that does not exist.

Distance-to-liquidation is a headline column in the required output, so it can be
wrong by a large factor while looking entirely plausible. Until per-venue tier
tables are measured, the column is a lower bound on distance only for a
single-tier isolated position. See decisions 016 (why `None` is a legitimate
answer) and 017 (what is and is not modelled).

### A5. Rate persistence is unmeasured, and it decides the outcome — NOT MODELLED

`periods_to_breakeven` is printed. **The probability that a funding divergence
survives that many periods is not.** [do not know]

At default taker fees a four-leg round trip costs ~22 bp, so a typical 1 bp/8h
cross-venue divergence needs **22 periods — over seven days** — just to return the
round trip. Whether a divergence lasts seven days is the single number that decides
whether the strategy earns, and answering it needs a *history* of rates, not a
snapshot. A snapshot says what will be paid in eight hours; it says nothing about
three days from now.

Until that history exists, every `net_apr` in the report is conditional on "if the
rate does not move", and the report must say so rather than imply it. Worked
through in `decisions/020`.

### A6. Fee tier is the largest available lever and it is the least measured

Same arithmetic, from `decisions/020`: going from all-taker (22 bp) to all-maker
(8 bp) cuts the hold needed to break even at 1 bp/period from 7.3 days to 2.7 — a
2.7× improvement that no plausible gain in divergence detection can match.

And `FEE_ASSUMED` is set on every row in the report right now. The most important
multiplier in the economics is the one we have not measured. [fact, verified]

### A4. Capital efficiency is not in the ranking

`[fact, verified]` Ranking is by net-of-costs APR. Two rows with identical APR
can require very different margin, so identical-looking rows are not equally
good. Required margin is displayed but does not enter the rank.

**Open:** rank by return on margin instead of raw APR? That changes the product's
central number, so it is a decision, not a tweak. Deferred deliberately.

---

## B. The honesty question the round cannot answer

`[fact, verified]` Every fixture in this repository was invented from memory of
API shapes. No live endpoint was contacted; the sandbox blocks all of them.

**Therefore: this round can demonstrate that the machinery is correct, and cannot
say anything whatsoever about whether the edge exists.** Any APR the CLI prints is
a property of numbers someone made up. The engine is required to print a banner
saying exactly that, and `FIXTURES-UNVERIFIED.md` is the list of what must be
measured before a single figure means anything.

This is the point on which the brief demands we speak up rather than reassure, so
stating it plainly: **we do not yet know whether the strategy earns.** We have
built the instrument that can answer it. Answering it requires network access —
one of the four conditions under which work stops and asks.

---

## C. Numerical representation

### C1. Precision floor at scale 1e12

`[fact, verified]` `Price` holds 12 decimal places. A token at 0.0000000082
(PEPE-class) therefore retains about 4 significant figures, and one unit in the
last place is ~1.2 bps of its own price — which is a tenth of our claimed edge.

`[assumption]` Venues mitigate this by quoting multiplier instruments
(`1000PEPE`), and `Asset` accommodates those names.

**Open:** accept the floor and refuse to scan instruments whose tick is within N
ulp of the scale, or move to a per-instrument scale? The former is a half-day and
honest; the latter is a redesign of `arb-num`. Not decided because no measured
tick sizes exist yet to say how many instruments are actually affected.

### C2. Simple vs compounded annualisation

`[fact, verified]` Decision 005 fixes simple annualisation over a 365-day year.

`[assumption]` Funding is realised every interval and can be re-deployed, so the
compounded figure is the economically truthful one for a held position; at 0.01%
per 8h the two differ by roughly 60 bps of APR, which is five times our edge.

**Open:** display both, or switch the headline to compounded? Blocked on a
product call about which number customers will compare against competitors —
competitors mostly show simple.

### C3. `margin_required` rounds twice, both away from zero

`[fact, verified]` `margin_required` computes `notional * (1/leverage)` in two
steps — `leverage.recip(RoundMode::COST)` then `notional.mul_rate(inverse,
RoundMode::COST)` — so the remainder is rounded twice, both times away from
zero. The figure is therefore at or above the true requirement, never below it.

`[assumption]` The relative error is bounded by roughly 2e-12, i.e. about two
units in the last place at scale 1e12. On a 1,000,000 USD notional that is a fifth
of a cent. This bound is derived from the fixed scale, not measured against any
venue's own margin figure.

**Open:** the clean fix is a `Money::div_rate` in `arb-num` that divides once. It
was deliberately **not** added, because the freeze should hold while a wave is in
flight (contrast decision 014, where the missing operation made a correct formula
inexpressible; here it only makes a correct formula slightly pessimistic). Written
down rather than hidden: the direction is safe and the magnitude is far below the
12 bps edge, so it waits for the next round.

---

## D. Venue conventions asserted but unmeasured

Each of these is `[assumption]`. All are itemised for verification in
`FIXTURES-UNVERIFIED.md`; they are repeated here because each is load-bearing for
a decision already taken.

- **D1.** All eight venues publish funding such that a positive rate means longs
  pay shorts (decision 006 depends on this). If any venue inverts it, that
  venue's rows invert — and an inverted carry ranks *first*, because ranking
  selects for favourable error.
- **D2.** Depth updates are absolute-size-per-level, with zero meaning delete
  (`arb-book` depends on this). A venue sending true increments would corrupt the
  ladder silently.
- **D3.** Funding intervals are discoverable per instrument on every venue
  (decision 007 depends on it). Where they are not, the flag
  `FUNDING_INTERVAL_ASSUMED` fires — but a systematically wrong interval is an
  8x APR error.
- **D4.** Exchange timestamps are milliseconds. A venue emitting seconds or
  microseconds parsed as millis is a 1000x staleness error in the direction that
  makes stale data look fresh.
- **D5.** Inverse perpetuals exist with quote-denominated contract sizes on some
  venues in scope, and the per-venue contract size is unknown.

---

### D5. The fixture format does not declare its own size unit

An order book's ladder is denominated in base for linear and spot markets and in
contracts for inverse ones (`decisions/019`). **No field in the fixture format says
which**, so the unit is inferred from `ContractSize` and the convention survives
only as a comment inside each file.

That is an assertion about our carefulness, not about the data, and it is exactly
what produced the units defect that decision records — a defect invisible on
Binance, where one contract is one coin. The format should carry the unit
explicitly, and the parser should refuse a file that does not state it.
[assumption]

## E. Structural debt accepted for this round

- **E1.** Risk limits and the kill switch live in `arb-engine` (decision 012). An
  eleventh crate, `arb-risk`, is the correct home. Deferred because the brief
  fixes the crate list and one owner per crate.
- **E2.** Dated futures are out of scope, so `MarketKind` has no expiring
  variant. Basis on dated futures is a real and larger opportunity than perp
  basis; adding it later touches `arb-types`, which is frozen — so it is a
  lead-owned change, not an engineer-owned one.
- **E3.** Painted-depth detection in `arb-detect` is heuristic and was validated
  against nothing `[fact, verified]`. A confident "depth is real" verdict is more
  dangerous than an admitted unknown, so the scoring must stay conservative until
  it can be calibrated on recorded feeds.
- **E4.** Fee tiers are static tables. Real tiers depend on 30-day volume, VIP
  level, holdings of the venue token, and promotions. Unknown tiers resolve to
  the worst tier by decision, which is the safe direction but makes every figure
  a pessimistic bound rather than an estimate.
- **E5.** Rate limits are not modelled, so how many instruments can actually be
  polled per venue per second is `[do not know]`. This bounds the real scanner's
  breadth and therefore how many opportunities it can see at all.
- **E6.** `contracts_for_notional` deliberately does **not** check
  `ContractSpec::min_notional` `[fact, verified]`. It answers "how many contracts
  fit in this much money"; a venue minimum is a reason to drop the row from the
  ranking, and the caller needs the size in order to *explain* why it was dropped.
  `arb-scan` owns that check. `[fact, verified]` `arb-scan` currently contains no
  reference to `min_notional` at all. If it does not implement the check, we will
  submit legs venues reject.
- **E7.** Duplicated fee representation: `arb-math` has `FeeRates`/`FeeTable` and
  `arb-venue` has `FeeSchedule` `[fact, verified]`. Both exist so that `arb-venue`
  does not depend on `arb-math`; `arb-scan` bridges them. Two types for one
  concept can drift — a tier added on one side and not the other is a silent
  mismatch between the fee we quote and the fee we would be charged.

---

## F. Closed by decision 018, recorded here so the direction is not forgotten

The adversary's first ten failing tests are adjudicated in
`decisions/018-adversary-round-one-adjudication.md`: eight real defects, two
arithmetic errors in the tests' own expectations. All ten are resolved and the
workspace is green `[fact, verified]`. Four consequences outlive the fixes:

- **F1.** `max_size_within_slippage` understated capacity **without bound** —
  172 BTC where 341 fit, on a 68 000-base one-BTC-per-tick ladder at 25 bps
  `[fact, verified]`. It is fixed, but note what kind of error it was: a
  *conservative-looking* one. Every rounding in that function is documented as
  understating capacity, and that documentation is exactly why nobody looked at
  it for two rounds. **An error in the safe direction is still an error, and it
  hides better than the other kind.** The lesson is recorded because the same
  reasoning protects other functions in this workspace.
- **F2.** `BookState::snapshot` no longer takes an `observed_at` argument
  `[fact, verified]`. This is an API break, authorised in 018. Any future code
  that needs a snapshot stamped at a time other than the newest datum's arrival
  must justify it in `decisions/` first; there is no longer a parameter to do it
  by accident.
- **F3.** `FeeTable::new` now refuses duplicate volume thresholds with
  `MathError::DuplicateTierThreshold` `[fact, verified]`. This tightens E4: a
  fixture that transcribes a venue's schedule with a repeated threshold is now
  rejected rather than silently resolved to the cheaper row. It does **not**
  address E4's substance, which is that the tables are static and invented.
- **F4.** `net_apr_over_horizon` refuses a negative round-trip cost
  `[fact, verified]`. Every rate-taking entry point in `arb-math` now refuses a
  negative rate. `[assumption]` That fence is complete; it was checked by reading
  the crate, not by a test that enumerates entry points, so a new function can
  still be added without the guard.

