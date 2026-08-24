# 015 — The lead wrote arb-math

**Decided.** The lead implemented `crates/arb-math` himself — funding, basis,
fees, execution, liquidation, sizing, 133 tests — against the brief's explicit
rule that the lead does not write feature code beyond the foundation.

**Alternatives considered.** (a) **Chosen:** the lead writes it. (b) Hand it to
Engineer 1 "Quant" as chartered. (c) Split it.

**Why.** Definition-of-done item 4 requires every formula to be covered by a
numeric vector "computed by hand, INDEPENDENTLY of the implementation — a test
fitted to the code's output does not count". The person who verifies a number by
hand is doing the lead's adjudication job, not an engineer's. And `arb-math` is
the critical path for `arb-scan`, so the same person being both author and
adjudicator shortens the path that everything else waits on.

That reasoning is **not unlimited.** It justifies the honesty core — the formulas
whose correctness the lead must personally vouch for — and not a habit. Cited
again for the next module, it is an excuse.

**What this breaks.** The brief's warning that "a tech lead who goes off to write
`arb-math` becomes the bottleneck" — and it partly happened. `arb-venue`'s Rust
side failed four delegated attempts while the lead was inside `arb-math`. That
cost is recorded here next to the justification rather than only the
justification, because a decision that lists only its upside is not a decision on
the record.
