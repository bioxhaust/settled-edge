# 001 — Fixed-point scale is 1e12 over i128

**Decided:** `Price`, `Qty`, `Money`, `Rate` are `i128` newtypes at a fixed scale of
1e12 (`SCALE_DIGITS = 12`).

**Alternatives considered.** (a) 1e8 over `i64`, satoshi-style — rejected: `i64`
at 1e8 caps magnitude near 9.2e10, and more importantly gives SHIB-class prices
(~8.1e-6) only three significant figures, so a price tick would be unrepresentable.
(b) A runtime-scale decimal carrying its own exponent — rejected: every binary
operation then needs scale reconciliation, which is exactly the class of bug this
crate exists to remove, and it costs a branch per multiply. (c) 1e18 over `i128` —
rejected: `mul_div` intermediates get within a factor of ~30 of the 256-bit
ceiling for large notionals, leaving no headroom.

**Why 1e12.** It holds every venue's quoted precision for the eight venues in
scope (max observed 8-10 decimals) with margin; funding rates at 8 decimals have
four spare digits; and `i128` at 1e12 spans ±1.7e26, far beyond any notional we
will ever size.

**What this breaks.** Assets priced below ~1e-9 lose significant figures — PEPE at
8.2e-9 retains four. Mitigation in practice is that venues quote such assets with
a multiplier (`1000PEPE`), and `Asset` has capacity for those names. Tracked in
`OPEN.md`. Anything requiring more than 12 decimals of *price* precision is
outside what this workspace can represent, and `Price::parse` returns
`NumError::PrecisionLoss` rather than truncating silently.
