# 016 — liquidation_price returns Option<Price>

**Decided.**
`liquidation_price(&LiquidationInputs) -> Result<Option<Price>, MathError>`.
`None` means "this position cannot be liquidated by price alone" — a fact about
the position, not a failure of the computation.

**Alternatives considered.** (a) **Chosen:** `Option`. (b) An error variant —
rejected: it would say the computation failed when it succeeded. (c) Return
`Price::ZERO` or `Price::MAX` as a sentinel — rejected: a sentinel is a number,
and a number gets ranked.

**Why.** Two positions genuinely have no liquidation price, and both are
legitimate states to display: a linear long whose margin covers its whole notional
(the price can only fall to zero, and that loss is already funded), and a fully
coin-margined inverse short (its loss in coin terms converges as the price rises).
They are exact mirror images. Neither is an edge case to be swept into an error
channel, and neither can be represented as a price without lying about it.

**What this breaks.** Every caller must handle `None` explicitly — `arb-scan`
cannot just unwrap a distance-to-liquidation. That is the intended cost: the
`Option` is the mechanism that forces the display layer to say "cannot be
liquidated by price" instead of printing a plausible-looking figure.
