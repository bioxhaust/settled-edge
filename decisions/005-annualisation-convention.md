# 005 — Annualisation uses exactly 365 days, and never a rounded period count

**Decided:** `SECONDS_PER_YEAR = 365 * 24 * 60 * 60 = 31_536_000`. Every
annualisation is `rate * SECONDS_PER_YEAR / interval_secs` by exact
`mul_div`, **not** `rate * periods_per_year` where the period count was rounded.

**Alternatives considered.** (a) 365.25 days, averaging leap years — rejected: it
makes `SECONDS_PER_YEAR` non-integral at second resolution and changes a headline
APR by ~7 bps per 100% APR, which is a real difference at a 12 bp edge; more
importantly, no venue documents a leap-year convention, so 365.25 is a fabricated
precision. (b) 360 days, money-market convention — rejected: crypto funding is not
quoted on an ACT/360 basis and using it would make our APR ~1.4% relative-optimistic
against every competitor's number, which looks like an edge and is not.
(c) `periods_per_year` as a plain integer multiplier — rejected as the *default*
path: it is exact only when the interval divides the year, and silently discards
the remainder otherwise.

**Why.** Both parts matter. 365 is the convention every venue's own APR display
uses, so our number is comparable to theirs. The exact-rational form means a
venue with a 7-hour or 90-minute interval annualises correctly instead of losing
the remainder; `FundingInterval::periods_per_year_exact` returns `None` precisely
so nobody reaches for a rounded count by accident.

**What this breaks.** Our APR will differ from a competitor using 365.25 by ~7 bps
per 100% APR, and from one using compounding by much more — see `OPEN.md` on
simple vs compounded annualisation, which is still open. The figure is a *simple*
annualised rate, not a compounded yield, and the CLI must label it as such.
