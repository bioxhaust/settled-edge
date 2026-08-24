# 013 — Ownership of the files that live outside crates/

**Decided.** Hard rule 6 ("you write only in your own directory") is unambiguous
for `crates/*`, and silent about the project root. It is hereby extended:

| Path | Owner |
| --- | --- |
| `crates/<name>/` | that crate's engineer |
| `fixtures/`, `FIXTURES-UNVERIFIED.md` | Engineer 4 (`arb-venue`) |
| `README.md` | Engineer 6 (`arb-engine`) |
| `OPEN.md`, `decisions/`, `FROZEN-API.md`, root `Cargo.toml` | lead |
| `crates/arb-adversary/` | Engineer 7, and he writes nowhere else |

**Why these assignments.** Fixtures are the same artifact as the adapters that
parse them — splitting them across two owners means the format and its instances
drift apart within a day. `FIXTURES-UNVERIFIED.md` goes with the fixtures for the
same reason: the person who invented a field shape is the only one who knows it
was invented. `README.md` goes to integration because the commands it documents
are the commands that engineer is the first to actually run; a README written by
anyone else documents intent rather than behaviour. The root `Cargo.toml` is lead-
owned because it is the one file where a stray edit can add an external
dependency and break hard rule 1 for everyone at once.

**Alternatives considered.** (a) Leave the root unassigned and rely on judgement
— rejected: eight agents with good judgement and no owner produce eight edits to
`README.md`, and the last writer wins silently. That is precisely the failure hard
rule 6 exists to prevent, so the rule needs to cover the root or it has a hole.
(b) Lead owns every root file, engineers propose diffs — rejected: it makes the
lead a serialisation point on the two files that change most during integration,
and the brief explicitly warns that a lead who becomes a bottleneck stops
resolving conflicts.

**What this breaks.** `fixtures/` is read by `arb-engine`'s replay tests but owned
by `arb-venue`, so a fixture change can break a test in a crate whose owner cannot
fix the cause. Accepted: that coupling is real and should be visible. The engine's
tests must therefore assert on *structure and determinism* rather than on specific
fixture values, or they will be brittle by construction.
