# 004 — Notional formulas live in arb-types, not arb-math

**Decided:** `ContractSpec::notional` and `ContractSpec::base_qty` live in
`arb-types`. `arb-math` owns the economics — fees, funding APR, basis, VWAP,
slippage, liquidation distance, sizing — and calls into these for the
contract-count-to-amount conversion.

**Alternatives considered.** (a) Put both in `arb-math` as free functions —
rejected: `arb-book`, `arb-scan` and `arb-venue` all need notional too, so they
would either depend on `arb-math` (widening the dependency graph for a definition,
not a calculation) or re-derive it. Re-derivation is the failure mode: the
linear/inverse asymmetry is precisely where an independent derivation goes wrong.
(b) Put them on `Instrument` rather than `ContractSpec` — rejected: the formula
needs only the spec, and `arb-book` holds specs without full instruments.

**Why.** "How many quote units is one contract" is part of what the instrument
*is*, not a strategy choice. There is exactly one right answer per contract type
and it belongs next to the type that encodes the distinction. The boundary rule is:
if the answer is fixed by the exchange's contract definition it goes in
`arb-types`; if it depends on our fee tier, our size, or our risk appetite it goes
in `arb-math`.

**What this breaks.** `arb-types` now depends on `arb-num`, so the foundation is
two crates deep rather than two crates wide. Accepted. It also means `arb-types`
contains arithmetic that can fail (`Result<_, NumError>`), which slightly muddies
its "pure vocabulary" character; the alternative muddied correctness instead.
