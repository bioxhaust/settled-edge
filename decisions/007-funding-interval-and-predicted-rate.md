# 007 — Funding intervals are per-instrument; the rate is the predicted one

Two conventions about *which* funding number a snapshot carries. Both are places
where a plausible reading is off by a multiple or a sign, and both would put the
most-wrong row at the top of the ranking.

## Intervals are per-instrument, never per-venue

**Decided.** `FundingInterval` is a validated newtype stored on each
`FundingSnapshot`, rejecting zero. There is no per-venue default interval
anywhere in the workspace.

**Alternatives considered.** (a) A per-venue constant, since 8h is the common
case — rejected: several venues run 8h on majors and 4h or 1h on volatile alts,
and some switch dynamically during stress. Assuming 8h for a 1h instrument
**overstates its APR by 8x**. Because we rank by APR, the instrument we got most
wrong is precisely the one that appears first. (b) A workspace-wide 8h constant —
rejected for the same reason, more so.

**Why.** The error is not merely large, it is adversarially selected by our own
ranking. An adapter that cannot read the true interval from the venue must set
`QualityFlags::FUNDING_INTERVAL_ASSUMED`, and the CLI must show it.

**What this breaks.** Every adapter must source the interval per instrument,
which for some venues means a second endpoint. Where that endpoint is unknown
this round, the flag is set and the item is listed in `FIXTURES-UNVERIFIED.md`.

## The rate is the predicted rate, not the last settled one

**Decided.** `FundingSnapshot::rate` is the rate that will apply at
`next_funding_at`. It is not the last settled rate.

**Alternatives considered.** (a) Store the last settled rate, which every venue
exposes reliably — rejected: ranking on a realised rate while presenting it as a
forecast is a silent, plausible-looking error. The two routinely differ in
magnitude and sometimes in sign. (b) Store both and let consumers choose —
rejected for the snapshot's primary field, because "choose" means "half the
consumers choose wrong"; an adapter that can only obtain the historical rate sets
`QualityFlags::FUNDING_RATE_HISTORICAL` instead, which is visible rather than
silent.

**Why.** A carry trade is entered *before* a settlement and paid *at* it. The only
rate that describes money we will actually receive is the forward one. Using the
backward one is the exact failure this project exists to eliminate: a confident
number that does not match the money.

**What this breaks.** Predicted rates move until the settlement window closes, so
a snapshot's APR is a moving estimate rather than a realised fact — the freshness
flags on the snapshot, not the rate field, are what make that honest. Related:
`OPEN.md` on whether to clamp predicted rates to venue-published caps.
