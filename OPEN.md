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

### A1. Borrow cost on any short-spot leg — WIRED AND BLOCKING; NOTHING MEASURED

`[fact, verified]` As of `decisions/025` `arb-types` carries `BorrowSnapshot` and
`BorrowInterval`: the accrual period travels with the rate, and a negative rate is
refused at construction. `[fact, verified]` `arb-math::borrow` annualises them
(`annualise_borrow`, `annual_borrow_cost`, `net_apr_after_borrow`), and
`arb-scan::scan::price_pair` now consumes that: a row whose short leg is spot
either has its `net_apr` reduced by the annualised borrow, or is blocked by
`Unrankable::BorrowCostUnknown` and flagged `QualityFlags::BORROW_COST_UNKNOWN`.
`[fact, verified]` **No fixture carries a `[borrow]` section**, so in practice
every reverse-carry row in a replay is blocked rather than priced. That is the
intended behaviour, not a gap: a blocked row is the honest report of a missing
cost term.

`[fact, verified]` The flag now has a producer. Both branches are under test in
`arb-scan`: `a_reverse_carry_with_no_borrow_quote_is_blocked_and_flagged` and
`supplying_the_borrow_quote_unblocks_the_row_and_can_invert_its_sign`. The second
is the argument for the flag being *blocking* rather than advisory — the same
fixture nets +14.38% at a 0.02%/day borrow and −3.87% at 0.07%/day, against
+21.68% with the term omitted. Both were verified to fail when the wiring is
removed, so the coverage is not self-confirming.

`[assumption]` The classic cash-and-carry we rank (long spot, short perp) needs
no borrow — you own the spot. But the *reverse* carry (short spot, long perp),
which is what a negative funding rate calls for, requires borrowing the base
asset. Margin-borrow rates on the venues in scope are commonly quoted at daily
rates that annualise into the tens of percent, i.e. one to three orders of
magnitude larger than 12 bps.

**What remains open** is the measurement, not the model: no venue adapter fetches
a borrow rate, so the term can only ever be supplied by hand. **Until a feed
supplies it, every reverse-carry row is blocked, and that is the correct output.**

**Three things the model deliberately does not cover, so they stay open:**
`[assumption]` **availability** — a rate is not inventory, and a borrow that
cannot be opened is an absent opportunity rather than an expensive one;
`[assumption]` **rate variability** — margin rates move, on some venues with
utilisation, so holding a week at today's quote is an assumption of the same kind
as A5's; and `[assumption]` **the borrow is charged against `net_apr` as an annual
rate**, not amortised over the horizon the way the round trip is, which is right
for a rate that accrues continuously and wrong if a venue charges it per-open.

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

### A5. Rate persistence is unmeasured, and it decides the outcome — TYPE EXISTS, NO DATA

`periods_to_breakeven` is printed. **The probability that a funding divergence
survives that many periods is not.** [do not know]

At default taker fees a four-leg round trip costs ~22 bp, so a typical 1 bp/8h
cross-venue divergence needs **22 periods — over seven days** — just to return the
round trip. Whether a divergence lasts seven days is the single number that decides
whether the strategy earns, and answering it needs a *history* of rates, not a
snapshot. A snapshot says what will be paid in eight hours; it says nothing about
three days from now.

`[fact, verified]` As of `decisions/025` `arb-types::RateHistory` can carry the
series — settled rates only, strictly increasing in time, with `span()` and
`current_sign_run()` reporting what is actually on record. It deliberately computes
no probability: the verdict is `arb-detect`'s and the arithmetic is `arb-math`'s.
`[fact, verified]` No fixture carries a `[history]` section, so every history in
the workspace today is empty. `[fact, verified]` `QualityFlags::RATE_HISTORY_MISSING`
now **has a producer**: `arb-scan::scan::price_pair` raises it for any funding leg
whose history is absent or empty, which is every replay row today. It warns rather
than blocks, deliberately — an unmeasured *probability of persistence* makes the
APR conditional, while an unmeasured *cost* makes it wrong, and only the second may
not be ranked. So the obligation in the next sentence is now met at the flag level:
every row does carry the mark.

**What remains open** is that a flag is not a probability. Nothing yet reads
`current_sign_run()` to say *how* conditional a given row is, so the report
distinguishes "we have no history" from nothing else — a row backed by thirty days
of one-signed funding and a row backed by nothing would both come back flagged the
moment a feed supplied either. Sizing the conditional is A5's real content and it
is untouched.

Until that data exists, every `net_apr` in the report is conditional on "if the
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
- **D6.** Inverse perpetuals exist with quote-denominated contract sizes on some
  venues in scope, and the per-venue contract size is unknown.

### D5. The fixture format declares its size unit, but nothing cross-checks it

An order book's ladder is denominated in base for linear and spot markets and in
contracts for inverse ones (`decisions/019`).

`[fact, verified]` As of `decisions/024` the format **does** carry the unit: the
parser requires `size_unit` and refuses a file without it with `MissingKey`, and
all 16 fixtures declare one (15 `base`, 1 `contracts`).

What stays open is the cross-check. `[fact, verified]` `ContractSpec::validate`
deliberately does **not** require `size_unit` to agree with `SizeUnit::implied_by`,
on the documented ground that disagreement is a real signal about the payload
rather than an occasion to paper over it — so a self-contradictory fixture is still
accepted, and nothing flags it. `[assumption]` Declared units are also still just
our memory of an API: `size_unit = base` asserts our carefulness, not the venue's
convention, and remains in `FIXTURES-UNVERIFIED.md` until measured.

---

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
  `arb-scan` owns that check and now performs it: `price_pair` compares the
  realised notional against each leg's `min_notional` and pushes
  `Unrankable::BelowMinNotional { vkey, available, required }` `[fact, verified]`.
  Five fixtures carry `min_notional = 5` and `kucoin-btcusdtm-perp` deliberately
  omits it (KuCoin gates on a minimum contract count, not a quote notional)
  `[fact, verified]`. **Still open:** at any target notional this project would
  realistically use — $40 and up on $5,000 of capital — 5 USDT is never binding, so
  the branch does not fire in a replay. It is coverage against a thin book shrinking
  the size to nothing, which is exactly when it matters and exactly what no fixture
  reproduces. `[assumption]` KuCoin's contract-count minimum is a *second* gate that
  nothing checks; `min_notional: None` there records the absence of a notional floor,
  not the absence of a floor.
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

