# 010 — The correctness adversary gets his own crate

**Decided:** Engineer 7 owns `crates/arb-adversary`. It contains no product code and
is never depended on by anything. Its `tests/` directory holds integration tests
that consume the other crates' *public* APIs. Failing tests he adds there are the
deliverable; the owning engineer fixes the defect in their own crate.

**Alternatives considered.** (a) Let him add `#[cfg(test)]` tests inside each crate
he audits — rejected: it directly violates hard rule 6, and worse, it puts two
authors in one file while five engineers are working in parallel, which is the
merge conflict rule 6 exists to prevent. (b) Have him file written defect reports
instead of tests — rejected: his mandate is executable proof. A prose report of a
precision loss is arguable; a failing assertion with a hand-computed expected value
is not. (c) One shared `tests/` at the workspace root — rejected: a root `tests/`
is not a crate and cannot express per-crate dependencies cleanly.

**Why.** It preserves both halves of his mandate at once. He may only *add* tests,
never edit product code, so he cannot rewrite someone's math to match his own
misunderstanding — which is the exact failure mode that would destroy his value as
an independent check. And because his crate depends on the others only through
their public surface, a defect he can express is by construction a defect a
customer could hit.

**What this breaks.** He can only reach public API. A precision bug in a private
helper is invisible to him unless it surfaces publicly — which is an acceptable
limit, because a bug with no observable public consequence cannot move money. Also,
his crate must remain listed in the workspace so `cargo test --workspace` runs it;
the lead owns the root manifest and has added it.
