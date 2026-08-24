# 014 — Two methods added to arb-num after the Gate 0 freeze

**Decided.** Two names were added to `arb-num` *after* the Gate 0 freeze, by the
lead:

- `Qty::div_size(self, size: Qty, mode: RoundMode) -> Result<Qty, NumError>`
- `Bps::is_positive(self) -> bool`

**Alternatives considered.** (a) **Chosen:** add both. (b) Leave the gap and let
`arb-math` work around it — rejected: the only available workaround is the
conflation described below, which is the exact bug the foundation exists to make
unrepresentable. (c) Unfreeze `arb-num` for a general revision — rejected: the
freeze has to hold while a wave is in flight, which is the same principle that
keeps `Money::div_rate` out of the crate (`OPEN.md` C3).

**Why `div_size`.** `Qty::mul_qty` (contracts × contract_size → base units) had
no dimensional inverse. `Qty::div_qty` returns `Rate` — a *dimensionless ratio*,
the right type for filled/requested — and there is no `Rate` → `Qty` conversion.
So `contracts_for_notional` was literally inexpressible in the type system:
dividing an amount by a size-per-unit yields a count, and conflating that with a
fill fraction is how a contract count ends up displayed as a percentage.

**Why `is_positive`.** Every other numeric type has it; `Bps` did not, which
pushed callers into `!x.is_negative() && !x.is_zero()` — reads identically and is
one dropped clause away from treating zero as positive.

**What this breaks.** Nothing. Both are purely additive: no existing signature,
scale, or rounding behaviour changed, so work in flight is unaffected. The cost is
that "frozen" now means **additive-only**, and that has to be said out loud so it
is not later read as "the freeze was theatre".
