# 011 — arb-book reports sequence facts; arb-detect owns every threshold

**Decided:** The boundary between the two crates that both touch "sequence gaps"
runs along fact versus judgement. `arb-book` maintains book state and reports
*what happened*: the last applied sequence, whether an update was contiguous, how
many updates were skipped, whether a resync is required by the venue's own
protocol. `arb-detect` decides *whether it matters*: every numeric threshold, and
every `QualityFlags` bit that encodes a policy, is set there.

**Alternatives considered.** (a) `arb-book` sets `QualityFlags::SEQ_GAP` itself —
rejected: it would then need a staleness budget and a gap tolerance, i.e. policy,
and two crates would own overlapping constants that drift apart. (b) `arb-detect`
reimplements sequence tracking so `arb-book` needs no sequence awareness at all —
rejected: applying a delta *requires* knowing whether it is contiguous, so the
tracking must live where the deltas are applied. Duplicating it guarantees the two
copies disagree during a resync.

**Why.** The rule generalises: mechanical consequences of a venue's own protocol
belong to `arb-book`; anything with a number a human chose belongs to `arb-detect`,
where hard rule "every threshold carries its justification beside the constant"
applies. This keeps all tunable policy auditable in one file rather than scattered
across two crates.

**What this breaks.** `arb-book`'s public API must expose enough sequence detail for
`arb-detect` to judge — a gap count and a contiguity flag, not just a boolean —
which makes its surface slightly wider than a pure book store would need.
`arb-detect` does **not** depend on `arb-book`: it consumes `arb-types` snapshots
plus its own state, so the two can be built in parallel. The engine wires them.
