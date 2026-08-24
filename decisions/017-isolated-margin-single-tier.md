# 017 — Liquidation is modelled for isolated margin and a single tier

**Decided.** Liquidation is modelled for **isolated** margin, **one-way** mode, a
**single** position, and a **single** maintenance-margin tier. Nothing else is
attempted, and the limitation is stated wherever a liquidation number is
presented.

**Alternatives considered.** (a) **Chosen:** model isolated only, and say so.
(b) Attempt cross margin from memory of eight venues' rules with no network to
check any of it — rejected: that is invention presented as a risk figure, which
is the failure hard rule 7 exists to prevent.

**Why.** Cross margin pools collateral across positions and therefore gives a
materially *later* liquidation price than the isolated formula returns. Tiered
maintenance margin rises with position size on every venue in scope, so feeding a
single maintenance rate is only correct within one tier.

**What this breaks, and in which direction.** State this precisely, because the two
errors are **not symmetric**:

- A **cross-margined** account fed these numbers is told it is **closer** to
  liquidation than it truly is. Wrong number, safe direction.
- Using the **tier of a smaller position than you actually hold** reports the
  cliff **further away** than the truth. Wrong number, **unsafe direction** — the
  operator is told there is room that does not exist.

The second is the one that costs money, so a liquidation figure is a lower bound
on distance only for a single-tier isolated position; above the first tier
boundary it is an overstatement of safety. Tracked in `OPEN.md` A3.
