# 009 — Three quality predicates: rankable, tradeable, trustworthy

**Decided.** `DataQuality` answers three separate questions, not one.

| Predicate | Condition | Question it answers |
|---|---|---|
| `is_rankable()` | no blocking flags | May this row appear in the table? |
| `is_tradeable()` | no blocking flags **and** provenance is not `SynthesizedUnverified` | May an order be routed against it? |
| `is_trustworthy()` | no flags at all **and** provenance is `LiveMeasured` | May this be presented as a fact about the market? |

**How this arose.** A single `is_usable()` predicate checking only flags let a
clean-flagged fixture row pass as actionable. A failing test in `arb-types::book`
(`provenance_survives_construction`) asserted that a synthesized book must not be
tradeable, which contradicted it. The test was right and the predicate was wrong.

**Options considered.**

1. One predicate, flags only. Rejected: makes invented data actionable. This is
   the exact failure mode hard rule 7 exists to prevent — we would build the core
   on fabricated fixture shapes and discover the problem with live money.
2. One predicate, flags plus provenance. Rejected: then fixture rows cannot be
   ranked, and the entire replay harness — the deliverable for this round — has
   nothing to display.
3. **Chosen:** three predicates, because there are genuinely three questions and
   different crates ask different ones.

**Who uses which.** `arb-scan` gates rows on `is_rankable()`. Any future execution
path gates on `is_tradeable()`. The CLI marks a row's provenance from
`is_trustworthy()` so a fixture-derived APR is never printed as though measured.

**Note on `RecordedFixture`.** A clean recording *is* tradeable-in-principle: it
came from a real response, so its shape is real, and execution paths need to be
exercisable in replay. Staleness of a recording is caught by `QualityFlags::STALE`,
not by provenance.

**Blocking set.** `STALE | SEQ_GAP | CROSSED_BOOK | PAINTED_DEPTH | BOOK_EMPTY |
SIZE_EXCEEDS_DEPTH | PRICE_UNAVAILABLE` — the flags where the number is *wrong*.
`THIN_DEPTH` and `PARTIAL_BOOK` are excluded because they make an estimate
conservative rather than false. `FEE_TIER_ASSUMED`, `FUNDING_INTERVAL_ASSUMED`,
`CLOCK_SKEW` and `FUNDING_RATE_HISTORICAL` are excluded because they are the
normal state before an account is connected; they degrade confidence, and
`arb-scan` scores them, but they do not falsify the row.

**What this breaks.** Three predicates invite picking the wrong one. Mitigated by
naming them after the *decision* each authorises rather than after a quality
level, and by `DataQuality::default()` being the pessimistic value so an
undescribed datum never claims to be measured.
