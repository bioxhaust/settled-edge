# 008 — Two timestamp types, no conversion between them

**Decided:** `WallTs` (what the exchange said) and `LocalTs` (when we observed it)
are separate types with no `From` impl either way. The only bridge is
`WallTs::apparent_skew_from`, which returns a `Millis` duration rather than a
timestamp, and `LocalTs::assume_no_skew`, which is named to be uncomfortable.
Canonical unit is the millisecond, with no constructor taking any other unit.

**Alternatives considered.** (a) One `Timestamp` type — rejected: staleness must be
measured on our clock, because that is the clock our order will be submitted on,
while funding-interval boundaries are defined on the venue's clock. One type makes
mixing them a silent success. (b) Keep one type plus a `source` field — rejected:
a runtime tag does not stop `a - b` from compiling. (c) Nanoseconds as the canonical
unit, for future latency work — rejected: all eight venues report milliseconds, so
nanoseconds would add a 1e6 multiply at every adapter boundary purely as a place to
forget a factor.

**Why.** The specific failure this prevents: computing quote age as
`exchange_ts_of_quote - exchange_ts_of_something_else` and comparing it against a
freshness budget. That expression is dimensionally fine and answers the wrong
question, because a venue whose clock runs 300 ms behind produces quotes that look
permanently fresh. With two types it does not compile.

**What this breaks.** `arb-detect` needs both clocks on every snapshot, so snapshot
types carry `exchange_ts` *and* `observed_at`; adapters must supply both, and a
fixture missing one has to say so rather than defaulting to zero. Also
`apparent_skew_from` deliberately conflates genuine skew with network latency —
it is an upper bound on skew, not a measurement, and is named `apparent_` so the
call site cannot forget. Separating the two requires round-trip timing we do not
have this round; see `OPEN.md`.
