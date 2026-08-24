# 002 — Bps and Rate share one raw representation

**Decided.** `Bps` is `i64` at scale 1e8; `Rate` is `i128` at scale 1e12. The
scales are chosen so a quantity expressed either way has the **identical raw
integer**: 1 bp is `100_000_000` in both. `Bps::to_rate` is a widening cast,
`Rate::to_bps` a checked narrowing cast — both exact, neither taking a
`RoundMode`.

**The arithmetic.** rate = bps/1e4. Stored bps = `b·1e8`; stored rate =
`(b/1e4)·1e12 = b·1e8`. Same integer, so conversion is lossless in both
directions with no multiply, no divide, no rounding, and no error case beyond
`i64` range.

**Alternatives considered.**

1. `Bps` at scale 1 (whole basis points only). Rejected: VIP taker tiers are
   1.8 bps and our claimed edge is 12 bps, so integer bps would misstate a real
   fee schedule by up to 55%.
2. `Bps` at scale 1e6 (micro-bp). Rejected: the conversion factor to `Rate`
   becomes 100, so `Rate → Bps` divides and can drop a remainder. A lossy
   conversion between two representations of the same quantity is a bug
   generator, and it would drag a `RoundMode` into what should be a cast.
3. No `Bps` type at all, ratios everywhere. Rejected: humans write fee
   schedules and thresholds in bps, and forcing every config value to be typed
   as `0.00018` is exactly how a factor-of-10,000 error gets entered.
4. **Chosen:** a distinct `Bps` type at the scale that makes conversion free.

**Why.** The factor-of-10,000 confusion between a ratio and a basis point is the
most common unit error in this domain, and our entire claimed edge is 12 bps. Two
types make the distinction visible; a free lossless conversion removes any
incentive to bypass them.

**The asymmetric constructor names are deliberate.** `Bps::parse_bps("1")` is one
basis point. `Bps::parse_ratio("0.0001")` is also one basis point. They are named
so no call site can be read without knowing which unit the input string carries.
Exchange fee and funding fields are *ratios*, so adapters want `parse_ratio`;
config we write ourselves is in bps, so it wants `parse_bps`. The test
`bps_constructors_are_not_interchangeable` asserts the exact 10,000x factor
between them, so the trap is documented in executable form.

**What this breaks.** The two scales are now coupled: changing `SCALE_DIGITS` or
`BPS_SCALE_DIGITS` independently silently breaks the identity, so
`bps_and_rate_share_a_raw_representation` asserts it and both constants are
documented as load-bearing. `Bps` being `i64` also makes `Rate::to_bps`
fallible; rates beyond ±9.2e10 bps are unrepresentable, which is 9.2e6 as a
ratio — far outside any real rate.
