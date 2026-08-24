# 012 — Where the stage-5 traits live

**Decided.** The stage-5 seams are split across two crates by *who they face*:

- `arb-venue` owns everything venue-facing: `ExecutionAdapter`, order lifecycle
  and state machine, `IdempotencyKey` and client order ID derivation,
  reconciliation against exchange state, and second-leg unwind.
- `arb-engine` owns everything operator-facing: risk limits and the kill switch.

Traits only this round. No implementations, no I/O beyond the fixture adapters
already sanctioned in `arb-venue`.

**Why the split falls here.** The round brief assigns `arb-venue` the
`ExchangeAdapter` trait "funding + book + later execution", so execution belongs
there by construction. Order lifecycle, idempotency, reconciliation and unwind
are all *conversations with a venue* — their shape is dictated by what venue APIs
accept and return, so putting them anywhere else would mean `arb-venue` defining
a trait whose vocabulary lives elsewhere.

Limits and the kill switch are different in kind: they are policy applied *across*
venues by whoever is running the machine. They do not belong in `arb-venue`
because a per-venue adapter cannot enforce a global exposure cap, and they must
not go in `arb-scan` because `arb-scan` is the crate we sell — shipping our risk
policy surface to customers tells them exactly what we will and will not do.

**Alternatives considered.** (a) An eleventh crate, `arb-risk`, which is the
cleanest home for limits and kill switch — rejected: the brief fixes the crate
list and one engineer per crate, and inventing a crate mid-round means inventing
an owner for it. Recorded in `OPEN.md` as the right refactor for the next round.
(b) All stage-5 traits in `arb-venue` — rejected: it makes the venue crate the
home of global policy, which is a layering inversion that would bite the moment a
limit needs to see two venues at once. (c) Limits in `arb-scan` — rejected on the
public/private boundary above.

**What this breaks.** `arb-engine` now carries public trait definitions rather
than being purely integration glue, so it is no longer a leaf that can be
rewritten freely. Accepted for one round.
