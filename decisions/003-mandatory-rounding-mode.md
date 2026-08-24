# 003 — Every lossy operation takes an explicit RoundMode; costs round against us

**Decided.** No operation in `arb-num` that can drop a sub-unit remainder has a
default rounding mode. `Price::mul_qty`, `Money::div_qty`, `Rate::mul_rate` and
the rest all require a `RoundMode`. Two named aliases carry the policy:
`RoundMode::COST` (away from zero — magnitude up) and `RoundMode::REVENUE`
(toward zero — magnitude down).

**Alternatives considered.**

1. A single implicit mode (truncate) everywhere, with `_round` variants for the
   careful caller. Rejected: the careless call site is the one that ships, and
   truncation flatters a cost by up to one unit in the last place per operation,
   always in our favour. Small per operation, systematically one-directional
   across thousands of rows — the worst possible shape for an error, because it
   survives eyeballing.
2. Banker's rounding everywhere. Rejected: statistically unbiased is the wrong
   target. We do not want our fee estimate centred on correct, we want it never
   below what the exchange actually charges. An unbiased scanner still surfaces
   its own rounding noise at the top of the ranking, because ranking *selects
   for favourable error*.
3. **Chosen:** mandatory explicit mode, plus two intention-revealing aliases.

**Why.** The product claim is that the number on screen equals the money that
arrives. That holds only if every rounding decision is deliberately
conservative. A number that is unbiased on average but sometimes optimistic
breaks the claim on the individual trade, which is the only unit that matters.
Requiring the mode at every call site also makes rounding intent visible in a
diff, which is what makes it auditable by the correctness adversary at all.

**The rule for consumers.** Costs — fees, slippage, funding paid, margin
required — round with `RoundMode::COST`. Proceeds — fill value, funding
received, PnL — round with `RoundMode::REVENUE`. When in doubt, ask which
direction makes the opportunity look worse, and pick that.

**What this breaks.** Call sites are verbose: `p.mul_qty(q, RoundMode::COST)`
rather than `p * q`. Accepted deliberately — there are no `Mul`/`Div` operator
impls on these types, and adding them would reintroduce a silent default. One
consequence worth stating plainly: computing a spread as `revenue - cost` with
both rounded against us makes the reported edge a **lower bound**, so the engine
systematically under-promises. That is the intended direction.
