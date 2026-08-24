# FIXTURES-UNVERIFIED.md

## Read this first

**Every number in `fixtures/` was invented from memory of these exchange APIs.
NOT ONE of them was verified against a live endpoint. No network call was made
at any point while writing them.** Prices, sizes, funding rates, contract
multipliers, tick sizes, fee schedules, sequence numbers and timestamps are all
fabrications that are merely *shaped* like the real thing.

Nothing derived from these fixtures is a measurement of the market. A funding
table computed from them is a test of our arithmetic, not a description of any
exchange. If a number from this directory reaches a sizing decision, an order,
a P&L estimate, or a slide, that is a defect.

**The endpoint paths and field names in this document are also from memory.** They
are the best pointers available for where to look when the network opens, not
citations. Expect some to be wrong in the path, the version prefix, or the exact
field spelling. Treat a 404 as expected, not as evidence the venue changed.

What the fixtures *are* good for: exercising the parser, the fixed-point
arithmetic, the quality-flag plumbing, the ordering and crossed-book rejection
paths, and the shape of the ranking table. That is all they are for.

### What was checked, and how

The fixtures are internally consistent, and that consistency *was* mechanically
verified with a throwaway script (not by eye, and not by trusting the prose
above). Confirmed across all 16 files:

- the `!unverified` first line is byte-exact
- bids strictly descending, asks strictly ascending, no crossed book
- every price is an exact multiple of that file's `tick_size`
- every size is an exact multiple of that file's `lot_size`
- `mark` and `index` are within 5 bp of the book mid where a book exists
- `next_funding_at - exchange_ts == interval_secs * 1000` exactly
- `kind = spot` files carry no `[funding]`; every perp carries one
- `contract_size` prefix agrees with `kind`; the inverse file settles in base
- no value carries 13+ decimal places
- all eight venues present

Internal consistency is *not* external accuracy. A self-consistent fiction is
still a fiction. Every item below is a place where the fiction may diverge from
the exchange.

### Tagging

- **[fact, verified]** — checked against something authoritative. Used below only
  for statements about the fixture files themselves, never about any exchange.
- **[assumption]** — written from memory, plausible, unverified, with a stated
  consequence.
- **[do not know]** — a real gap. Not guessed at. These are the dangerous ones,
  because a gap will otherwise be filled silently by a default.

Every item states three things: what we assumed, the consequence if wrong with
its magnitude, and the concrete thing to check.

### The one number that matters for calibration

Our target edge is **12 bp**. Several items below are wrong by 2x, 8x, 100x or
68,000x if the assumption fails. An error of 2x on a 40% APR carry is 20
percentage points; an error of 100x on a contract size is a position we cannot
margin. None of these are rounding concerns. They are the difference between a
strategy and an incident.

---

## General — cross-venue

### G1. Timestamp units are milliseconds everywhere

**Assumed:** every `exchange_ts` and `next_funding_at` in the fixtures is Unix
epoch **milliseconds**. All perp fixtures use `exchange_ts = 1735660800000`
(2024-12-31T16:00:00Z) and `next_funding_at = 1735689600000`
(2025-01-01T00:00:00Z), with book snapshots 100 ms later.

**If wrong:** this is not a small error, it is a 1000x or 10^6 error in a time
axis. Reading seconds as milliseconds puts every event in **January 1970**;
reading milliseconds as seconds puts them somewhere around the year **56,000**.
A funding countdown computed from either is garbage, so "time until next
funding" — which gates whether a carry trade can be entered at all — is
meaningless. Two specific venues are recalled as *not* being milliseconds and
each has its own item: **Gate uses seconds** (G1a) and **KuCoin websocket
timestamps are recalled as nanoseconds** (G1b).

**Check:** for each venue, request one funding endpoint and one book endpoint and
compare the returned integer's **digit count** against
`1735689600000` (13 digits for ms in this era, 10 for seconds, 16 for
microseconds, 19 for nanoseconds). Digit count is the cheap reliable test; do
not infer the unit from documentation.

### G1a. Gate returns seconds, not milliseconds

**Assumption:** Gate's v4 REST API expresses `funding_next_apply` and most other
timestamps in **seconds**, unlike the other seven venues. The fixture
`gate-btcusdt-perp.fix` nonetheless stores milliseconds, because the fixture
format is uniformly milliseconds — so **the Gate adapter must multiply by 1000
and the fixture cannot catch a failure to do so.** This is a blind spot by
construction.

**If wrong** (in either direction): a Gate funding time off by 1000x. If the
adapter fails to convert, `next_funding_at` lands in 1970, every Gate carry looks
infinitely stale or infinitely overdue, and Gate either never trades or always
trades.

**Check:** `GET /api/v4/futures/usdt/contracts/BTC_USDT`, field
`funding_next_apply` — count the digits. Also `funding_interval` on the same
response, which is recalled as being in seconds too.

### G1b. KuCoin mixes millisecond and nanosecond timestamps

**[do not know]** KuCoin is recalled as returning **nanoseconds** in some
websocket payload fields while using milliseconds in REST. We do not know which
fields, so we cannot say which of our two KuCoin fixtures would be affected.

**If wrong:** a 10^6 error on exactly the venue where we already have an
unverified symbol format. Worse than a uniform error, because a *mixed* scheme
means a converter that works on the REST path silently corrupts the websocket
path, and the two disagree only under load.

**Check:** `GET /api/v1/level2/snapshot?symbol=XBTUSDTM`, note the digit count of
`ts`; then subscribe to `/contractMarket/level2:XBTUSDTM` and compare the digit
count of the websocket `ts`. If they differ, the adapter needs a per-field unit,
not a per-venue one.

### G2. Funding interval is 8 hours on all eight venues

**Assumption.** Every perp fixture carries `interval_secs = 28800`. Seven declare
`interval_source = venue` (recalled from the venue), one —
`gate-btcusdt-perp.fix` — honestly declares `interval_source = assumed`, meaning
we did not even have a memory to draw on, we copied the neighbours.

**If wrong, this is the single highest-leverage error in the whole set,** because
the interval is a *multiplier* on the rate and the ranking sorts on the product.
Annualising with `rate x (24/interval_hours) x 365`, the fixture BTC rates give:

| venue | rate / 8h | APR @ 8h | APR if really 4h | APR if really 1h |
|---------|-----------|----------|------------------|------------------|
| mexc    |  0.00030  |  32.85%  |  65.70%          | 262.80%          |
| bybit   |  0.00025  |  27.38%  |  54.75%          | 219.00%          |
| bitget  |  0.00018  |  19.71%  |  39.42%          | 157.68%          |
| gate    |  0.00015  |  16.43%  |  32.85%          | 131.40%          |
| binance |  0.00010  |  10.95%  |  21.90%          |  87.60%          |
| kucoin  |  0.00008  |   8.76%  |  17.52%          |  70.08%          |
| htx     |  0.00006  |   6.57%  |  13.14%          |  52.56%          |
| okx     | -0.00005  |  -5.48%  | -10.95%          | -43.80%          |

The widest fixture spread is mexc long-funding against okx: `0.00030 - (-0.00005)
= 0.00035` per 8h = 3.5 bp per period = **38.33% APR** gross. Now suppose Gate is
really **1h**: Gate alone jumps from 16.43% to 131.40% and **sorts straight to the
top of the table**, showing an apparent 137% spread against OKX — 3.4x the real
best pair. We would size into the row with the *least* verified interval in the
set. If any venue is 4h the error is exactly **2x**; if 1h, **8x**.

Note also that a wrong interval corrupts the *countdown*, not just the APR:
`interval_secs` is what tells us how long we must hold to collect once. Believing
8h when it is 1h means we plan to hold 8x too long and pay 8x the expected fees.

**Check, per venue — this is the top of the verification queue:**

- binance: `GET /fapi/v1/fundingInfo`, field `fundingIntervalHours` (assumed 8).
  Note this endpoint is recalled as only listing symbols whose interval is *not*
  the default, so **absence from the response is itself the answer** — do not
  treat an empty array as an error.
- bybit: `GET /v5/market/instruments-info?category=linear&symbol=BTCUSDT`, field
  `fundingInterval`. **Recalled to be in MINUTES**, so expect `480`, not `8` and
  not `28800`. Reading 480 as hours gives a 60x error.
- okx: `GET /api/v5/public/funding-rate?instId=BTC-USDT-SWAP` — derive the
  interval as `nextFundingTime - fundingTime` rather than trusting any stated
  field. OKX moved several pairs to 4h; BTC-USDT-SWAP is *assumed* still 8h.
- bitget: `GET /api/v2/mix/market/funding-time?symbol=BTCUSDT&productType=USDT-FUTURES`,
  field `fundingRateInterval` (recalled as hours, as a string).
- mexc: `GET /api/v1/contract/funding_rate/BTC_USDT`, field `collectCycle`
  (recalled as **hours**, so expect `8`).
- gate: `GET /api/v4/futures/usdt/contracts/BTC_USDT`, field `funding_interval`
  (recalled as **seconds**, so expect `28800`). **This is the `assumed` one —
  check it first.**
- kucoin: `GET /api/v1/contracts/active`, field `fundingRateGranularity`
  (recalled as **milliseconds**, so expect `28800000`).
- htx: `GET /linear-swap-api/v1/swap_funding_rate?contract_code=BTC-USDT`, derive
  from `funding_time` vs `next_funding_time`.

Four different units across eight venues (hours, minutes, seconds, milliseconds)
for the same quantity. Store one canonical unit internally and convert at the
adapter boundary; do not let a venue's unit leak inward.

### G3. Funding rate sign convention: positive means longs pay shorts

**Assumption.** Every fixture uses: **positive rate = longs pay shorts** (so a
short collects), negative = shorts pay longs. We believe this is universal across
all eight venues, and the two OKX fixtures (`-0.00005`, `-0.00002`) are the only
negatives.

**If wrong on any venue:** we do not mis-size, we take **the wrong side**. The
error is the *entire* rate with the sign flipped, i.e. 2x the rate in P&L terms.
On the widest fixture pair that turns a +38.33% APR carry into a -38.33% one, and
the position looks correct in every log line — the number has the right magnitude,
so nothing alarms. A sign error is the hardest failure here to detect from
outputs alone, which is why it is worth a dedicated cross-check rather than a
code read.

**Check:** do not read this from documentation. Take a small position on one venue,
hold it across one settlement, and **compare the signed funding payment on the
account statement against the sign of the rate that was published beforehand.**
One observation per venue settles it permanently. Until then, treat sign as
unverified on all eight. Cheaper partial check: for each venue confirm that
`mark > index` co-occurs with a positive published rate across a handful of
symbols — a venue whose rate sign disagrees with its own basis is either inverted
or is quoting the previous period (see G4).

### G4. The published rate is the *predicted next* rate, not the last settled one

**Assumption.** Thirteen perp fixtures declare `rate_source = predicted`, meaning
the rate that will be charged at `next_funding_at`. One —
`mexc-btcusdt-perp.fix` — declares `rate_source = historical`, the last settled
rate, and it is deliberately the richest BTC rate in the set (0.00030) so that a
scanner ignoring `rate_source` surfaces a stale number as its best opportunity.

**If wrong:** a one-period lag. Funding is strongly mean-reverting at the 8h
horizon, so a settled rate is a poor forecast of the next one and **inverts sign
often enough to matter** — perhaps a third of periods on a calm day. We would
enter carry on the basis of a payment already made to somebody else. Magnitude:
the full rate, direction unpredictable; on the fixture spread, up to 3.5 bp per
period mis-attributed.

**Specific trap, OKX:** `GET /api/v5/public/funding-rate` is recalled as returning
**both** `fundingRate` (the rate for the period ending at `fundingTime`) and
`nextFundingRate` (a forecast for the period after). Picking the wrong one is an
off-by-one-*period* error, and both fields are populated with plausible numbers,
so there is no crash to catch it.

**Check, per venue:** compare the published rate against
`GET /fapi/v1/fundingRate` style history for the same symbol at the same instant
(binance), `GET /v5/market/tickers` `fundingRate` vs `/v5/market/funding/history`
(bybit), and the `fundingRate` / `nextFundingRate` pair (okx). If the live value
equals the most recent historical entry, it is **settled**, not predicted, and
`rate_source` for that venue must be `historical`.

### G5. Book fixtures are absolute-size snapshots, not deltas

**Assumption.** Every `bid` / `ask` line is the **absolute resting size at that
price level** at the snapshot instant — not a delta against a previous book, and
not cumulative depth. Our fixtures are snapshots, so this is the only reading
that makes sense for them; the assumption is about what the *live* streams mean.

**If wrong:** applying a delta stream as if it were absolute (or vice versa)
produces a book that drifts away from reality monotonically, and it drifts
*quietly*. Depth is what sizes the trade, so this shows up as fills far worse
than modelled — with a 12 bp edge and the HTX-style thin book already at 11.74 bp
of spread, a 2x depth error is the whole trade. Related and equally unverified:
**whether a zero size means "delete this level" or "level is empty but present"**.
Every venue's incremental feed is recalled as using **size 0 = remove the level**,
but the fixture format cannot express it, because the parser is required to reject
non-positive sizes. **So no fixture in this directory can test delete handling.**
That gap is structural — it must be covered by adapter-level tests against
recorded live frames, not here.

**Check:** for each venue, take a REST depth snapshot and simultaneously buffer the
websocket incremental feed; replay the increments onto the snapshot and compare
against a second REST snapshot a few seconds later. If they match, the increments
are absolute-per-level; if levels inflate, they are deltas. Same experiment
reveals the zero-size semantic. Endpoints: `/fapi/v1/depth` + `<sym>@depth`
(binance), `/v5/market/orderbook` + `orderbook.50.<sym>` (bybit),
`/api/v5/market/books` + `books` channel (okx).

### G6. Sequence numbers are a single monotonic integer per venue

**Assumption.** Each `[book]` section carries one `sequence` integer, assumed
strictly monotonic per symbol per venue, usable for gap detection. **The magnitudes
in the fixtures are placeholders and are known to be unrealistic** — e.g.
`binance-btcusdt-perp.fix` uses `12345` where real BTCUSDT `lastUpdateId` values
are recalled as ~10^10. This was deliberate (the value came from the format spec's
own example) but it means **no fixture tests integer-width handling**, and a
32-bit sequence field would pass every test here and overflow in production on day
one. Gate's `20489117733` is the only value that exceeds 2^31.

**If wrong:** gap detection is what tells us our book is stale. A sequence scheme
we mis-model means either constant false resubscribes (throughput loss, and a
resubscribe storm during exactly the volatility we want to trade) or, far worse,
**undetected gaps** — trading a book we believe is current and is not. There is no
"small" version of this error: the book is either trustworthy or it is not.

**Specific unresolved case, Bybit:** `bybit-btcusdt-perp.fix` carries `4587123` in
`sequence`, but Bybit's orderbook payload is recalled as carrying **both** `u`
(update id) and `seq` (cross-sequence), with different semantics — `u` for
per-symbol continuity, `seq` for cross-symbol ordering. **We do not know which one
the adapter should store**, and the fixture cannot tell us because it has one
field. Picking wrong breaks continuity checks silently.

**Also unresolved:** whether each venue's counter resets on reconnect, on daily
maintenance, or never; and whether it is per-symbol or per-connection. A counter
that resets looks exactly like a gap.

**Check:** binance `/fapi/v1/depth` → `lastUpdateId`, and the ws `depth` frames'
`U` / `u` / `pu` triple (the `pu`-links-to-previous-`u` rule is the documented
continuity check — verify it holds). bybit `/v5/market/orderbook` → both `u` and
`seq`; log both for an hour and see which is gapless per symbol. okx
`/api/v5/market/books` → `seqId` with `prevSeqId` (same linked-list check).
gate `order_book` → `id`. kucoin `/api/v1/level2/snapshot` → `sequence`.
htx `swap_depth` → `version` and `mrid` (two candidates again).
In every case, record the value's **width** as well as its behaviour.

### G7. Sizes and lot sizes are in BASE units on linear markets, CONTRACTS on inverse

**Assumption — this is a convention we imposed, not one we observed.** In all
linear and spot fixtures, book sizes and `lot_size` are expressed in **base asset
units**, even where the venue's own API quotes size in **contracts**. Where a
venue has a contract multiplier we set `lot_size = contract_size x (its lotSz of
1 contract)`. The inverse fixture is the exception and uses **contracts**, because
on an inverse book the contract *is* the natural unit (sizes there are integers).

So the fixture set is deliberately **not uniform** in size units across `kind`,
and the format has no field that says which unit is in use. It is inferred from
`kind`. That is fragile and is called out here rather than hidden.

**If wrong — the magnitude is exactly `1 / contract_size`, per venue:**

| fixture | contract_size | error if contracts/base confused |
|---------|---------------|----------------------------------|
| okx BTC-USDT-SWAP | base:0.01 | **100x** |
| kucoin XBTUSDTM, htx BTC-USDT | base:0.001 | **1000x** |
| mexc ETH_USDT | base:0.01 | **100x** |
| mexc BTC_USDT, gate BTC_USDT | base:0.0001 | **10,000x** |

Worked example, `mexc-btcusdt-perp.fix`: top bid is `0.8500` base units = **8500
contracts** at 0.0001 BTC each. An adapter that forwards `0.85` as a contract count
submits `0.85 x 0.0001 = 0.000085 BTC`, which at 68122 is **5.79 USDT of notional
instead of 57,903.70** — a 10,000x shortfall. Note the cruelty of the detail:
`min_notional = 5`, and **5.79 > 5, so the malformed order passes the notional
check and gets accepted.** The hedge leg then fills at full size, and we are
naked on one side of a supposedly delta-neutral position. This is the failure mode
that quietly turns an arbitrage engine into a directional one.

**Check:** per venue, read the instrument endpoint's multiplier field
(`ctVal`+`ctValCcy` on okx, `contractSize` on mexc, `quanto_multiplier` on gate,
`multiplier` on kucoin, `contract_size` on htx, `sizeMultiplier` on bitget) **and
separately** confirm what unit the *orderbook* payload uses, which is not
guaranteed to match the unit the *order entry* endpoint expects. The decisive test
is to place one minimum order and read the resulting position size in base units.

### G8. Fee tiers: every fixture declares `tier = unknown`, and that is correct

**[fact, verified — about the fixtures]** All 16 files set `tier = unknown`, which
per the format means `FeeSchedule.tier_known = false` plus
`QualityFlags::FEE_TIER_ASSUMED`.

**Assumption.** The maker/taker numbers are the **base, non-VIP, no-token-discount**
schedule from memory: perps mostly 0.02%/0.05%–0.055% (bitget and mexc 0.06%
taker, gate 0.015% maker), spot 0.1%/0.1% on both spot fixtures.

**We default to the worst tier deliberately.** If our real tier is better, we
under-report edge and skip trades that were profitable — an opportunity cost.
If we assumed a better tier than we have, we would trade spreads that do not
exist. The asymmetry is not close, so **assume the worst tier until a fee is
observed on a filled order.**

**If wrong:** fees are a *first-order* term against a 12 bp edge, not a
correction. A delta-neutral cross-venue round trip is four taker crossings; at
~0.05% each that is **~20 bp of fees against 12 bp of edge**, i.e. the naive trade
loses money and only maker execution or funding carry can rescue it. Being wrong
by one tier (~0.005% per side, ~2 bp round trip) is ~17% of the edge. The
**gate maker fee of 0.00015** is flagged in-file as the one fee recalled as
*better* than its peers — if that memory is flattering, gate's edge is overstated,
which is the dangerous direction.

**Bigger risk than the tier: the spot/perp fee asymmetry.** Both spot fixtures
carry 0.1%, five times the perp taker fee. An engine that reuses a futures fee
schedule for a spot leg **understates round-trip cost by roughly 16 bp** — larger
than the entire target edge, and it makes cash-and-carry look like free money.
This is why the spot fixtures exist.

**Check:** binance `GET /sapi/v1/asset/tradeFee` (spot) and
`GET /fapi/v1/commissionRate?symbol=BTCUSDT` (USD-M) — **account-specific, so this
must be run with our own keys, not read from a doc.** bybit
`GET /v5/account/fee-rate`. okx `GET /api/v5/account/trade-fee`. bitget
`/api/v2/mix/account/account`. kucoin `GET /api/v1/trade-fees`. Then reconcile
against the `fee` / `commission` field on a real fill, because promotional and
referral rates do not always appear in the fee-rate endpoint.

### G9. Tick and lot sizes

**Assumption.** Per-fixture, from memory: BTC perps `tick 0.1`; ETH perps
`tick 0.01`; binance BTC spot `tick 0.01` (recalled as *finer* than the perp's
0.1); lot sizes 0.001 base on most BTC perps, 0.00001 on binance spot.

**If wrong, the two directions fail very differently — and only one is safe:**

- **Tick too coarse or off-grid** → the venue **rejects** the order. Loud, immediate,
  cheap. We lose the trade and learn the truth. This is the good case.
- **Lot size too coarse** → the venue, or our own rounding, **silently truncates**
  the size. As the brief puts it: a silently truncated size is a wrong order size
  *forever*. On a hedged pair, truncating one leg and not the other leaves a
  residual directional position on every single fill. Truncating 0.0001 off a
  0.001 lot is a 10% size error on the leg — permanently, invisibly, and it
  accumulates in one direction.

Related: the format requires a value with **13+ decimal places** to raise
`NumError::PrecisionLoss` rather than round. **No fixture exercises that path** —
every value here is 5 dp or fewer. The precision-loss guard is therefore
completely untested by this directory and needs a unit test with a literal
13-dp string. That is a Rust-side test, not a fixture.

**Check:** binance `GET /fapi/v1/exchangeInfo` → `filters[]`: `PRICE_FILTER.tickSize`,
`LOT_SIZE.stepSize`, `MIN_NOTIONAL.notional`; and `GET /api/v3/exchangeInfo` for
spot, where the same symbol has **different** filters. bybit
`instruments-info` → `priceFilter.tickSize`, `lotSizeFilter.qtyStep` /
`minOrderQty`. okx `public/instruments` → `tickSz`, `lotSz`, `minSz`. gate
`contracts` → `order_price_round`, `mark_price_round`. mexc `contract/detail` →
`priceUnit`, `volUnit`, `minVol`. kucoin `contracts/active` → `tickSize`,
`lotSize`. Do not assume spot and futures filters agree for the same pair — the
binance spot fixture assumes they do not.

### G10. `mark` and `index` mean what we think they mean

**Assumption.** `mark` is the venue's own mark price (the liquidation and unrealised-
PnL reference, typically an index price plus a damped basis), and `index` is the
external spot index the perp is written against. In the fixtures both sit within
0.5 bp of the book mid, and their *relative order* encodes the funding sign:
`mark > index` accompanies positive rates, `mark < index` accompanies the two
negative OKX rates.

**If wrong:** mark and index are not interchangeable and confusing them is a basis
error. Mark contains a basis component; index does not. Computing basis as
`mark - index` when one field actually holds the *other* venue's convention gives
~0 basis always, and the funding predictor silently degrades to noise. Magnitude
here is small in absolute terms — 0.5 bp on these fixtures — but basis *is* the
signal, so a 0.5 bp error on a 12 bp edge is 4% of edge from a field mix-up, and
on a stressed market the mark-index gap can reach tens of bp.

Also unverified: **whether each venue's mark is the last trade, a mid, or an
index-plus-basis** (they differ, and only the last is safe for liquidation
distance), and **which constituent exchanges each index samples** — an index that
includes a venue we trade on is partially self-referential.

`mexc-ethusdt-perp.fix` deliberately **omits both fields** to exercise
`QualityFlags::PRICE_UNAVAILABLE`, and it is deliberately the richest ETH rate in
the set, so a scanner that sorts on rate alone puts its least verifiable row first.

**Check:** binance `GET /fapi/v1/premiumIndex` → `markPrice`, `indexPrice`,
`lastFundingRate`, `nextFundingTime` (all four in one response — good
cross-check). bybit `/v5/market/tickers` → `markPrice`, `indexPrice`. okx
`GET /api/v5/public/mark-price` and `/api/v5/market/index-tickers`. Compare each
venue's `index` for the same asset at the same instant across venues: they should
agree to a few bp, and any venue that disagrees by more is using a different
constituent set or a different quote asset (see G11).

### G11. USDT is not USD — the inverse fixture straddles both

**Assumption.** `okx-btc-usd-inverse-perp.fix` has `quote = USD` while the linear
fixtures have `quote = USDT`, and its `index` (68118.9) is set ~2 USD *below* the
linear fixtures' index (68120.9–68121.6) on the assumption that **USDT trades
slightly above 1.00 USD**.

**If wrong:** the sign of that 2 USD offset flips, which is a ~3 bp error in the
USD/USDT basis. Against a 12 bp edge that is **a quarter of the edge from a
stablecoin peg assumption alone**. Worse, if the engine treats USD and USDT as the
same asset it will net a USD-quoted position against a USDT-quoted one and report
a delta-neutral book that carries live stablecoin exposure. In a depeg — which is
the exact scenario in which cross-exchange spreads blow out and we most want to be
trading — that "neutral" position is the dominant risk.

**Check:** `GET /api/v3/ticker/price?symbol=USDCUSDT` and the USDT/USD quote on any
venue with a fiat pair; compare against each inverse venue's USD index. Treat USD,
USDT, USDC and the various local-currency quotes as **distinct assets** in the type
system, never aliases.

### G12. The fixtures are frozen at one instant — and that instant is ~20 months stale

**[fact, verified — about the fixtures]** Every perp fixture uses
`exchange_ts = 1735660800000` = **2024-12-31T16:00:00Z**, with
`next_funding_at = 1735689600000` = **2025-01-01T00:00:00Z**. Today is 2026-08-24.
**The fixtures are dated roughly 20 months in the past.**

This is deliberate and useful — funding boundaries land on real 00:00/08:00/16:00
UTC times — but it has a consequence that will bite immediately: **any code that
computes "time until next funding" as `next_funding_at - now()` will get a large
negative number** (about -600 days), not a small one. Code paths keyed on that
value will either reject every fixture as hopelessly stale or, if the subtraction
is unsigned, wrap. Whoever writes the staleness check must decide explicitly
whether fixture-sourced snapshots bypass wall-clock staleness, and say so.

**Also assumed:** that a single timestamp shared across eight venues is meaningful.
It is not, in reality — venues do not tick simultaneously, their clocks disagree by
tens to hundreds of milliseconds, and the network path to each differs. A
cross-venue spread computed from books captured at genuinely different instants
includes **latency, not just spread**, and on BTC a 100 ms disagreement can easily
be several bp of price movement — comparable to the entire edge. The fixtures'
perfect simultaneity is the least realistic thing about them.

**Check:** compare each venue's `serverTime`-style endpoint against our own clock
(binance `GET /fapi/v1/time`, bybit `/v5/market/time`, okx
`/api/v5/public/time`) and record the offset distribution over an hour, not once.
Then measure book-to-book arrival skew on a shared symbol. Until that measurement
exists, **no cross-venue spread from live data should be trusted below ~5 bp.**

### G13. What this directory structurally CANNOT test

Stated explicitly so nobody mistakes green fixtures for coverage. Every fixture
here is **valid on purpose** — there are no malformed files on disk, because
`load_dir` should succeed on the whole directory, and a deliberately broken file
would make a passing run indistinguishable from a parser that ignores errors.

Consequently **none of the format's error paths are exercised by any file here.**
The following must be covered by Rust-side tests over literal strings, and if they
are not, they are untested:

- missing or corrupted `!unverified` header
- unknown key, duplicate key, missing required key
- unknown section, duplicate section, key outside any section that is not `venue`
- malformed `x` separator in a `bid` / `ask` line
- bids ascending / asks descending / **crossed book**
- non-positive price or size (and therefore **zero-size-means-delete**, per G5)
- `[funding]` present on a `kind = spot` fixture
- `contract_size` prefix contradicting `kind` — the `ContractSpec::new` rejection
  that is the sole defence against the 68,000x error
- **`NumError::PrecisionLoss` on a 13+ decimal value** (per G9 — no fixture has
  more than 5 dp)
- 1-based line numbers being correct in every one of the above, especially in
  files with comment and blank lines before the error

Also untested here: reconnect/resubscribe, sequence resets, rate limits,
partial-fill behaviour, and anything time-varying. These fixtures are a single
frozen snapshot; they say nothing about dynamics.

### G14. The absolute price level is invented, and so is the BTC/ETH ratio

**Assumption.** BTC ~68,122 and ETH ~3,345, giving `68122 / 3345 = 20.37` as the
BTC/ETH ratio. Both were picked from memory of a plausible market, not observed.

**If wrong:** all the *relative* reasoning in the fixtures survives, because every
consistency check above is in basis points and bp are scale-free. What does **not**
survive is anything absolute: `min_notional` as a fraction of a lot, whether a
0.001 BTC lot is a meaningful order, how many contracts a given USD budget buys,
and every notional figure quoted in this document. If BTC is really at 30k or 200k,
the *shape* of the tests holds and the *sizes* are wrong by that ratio.

**Check:** trivially, any ticker endpoint. Worth doing before quoting any absolute
number from these fixtures to anyone.

---

## Per-venue notes

Ordered by how much unverified surface each venue presents, worst first. Endpoint
paths below are **from memory** (see the top of this file) — they are search
starting points, not citations.

### KuCoin — worst unverified surface in the set

Files: `kucoin-btcusdtm-perp.fix` (linear perp), `kucoin-ethusdt-spot.fix` (spot).

**K1. The native symbol is a coin-flip we did not resolve.**
**Assumed:** `native = XBTUSDTM`. KuCoin is recalled as using **XBT** rather than BTC
for bitcoin. The fixture brief suggested `BTCUSDTM`. Both are unverified; the file
records XBTUSDTM with an in-file comment documenting the disagreement rather than
resolving it silently, and the filename retains the brief's `btcusdtm` spelling —
so **filename and `native` value deliberately disagree**, as a marker.
**If wrong:** every KuCoin futures request 404s. That is the *good* failure — loud,
immediate, unmissable — which is the only reason it was acceptable to ship this
guess. The bad version of this error would be a symbol that resolves to a
*different instrument*.
**Check:** `GET https://api-futures.kucoin.com/api/v1/contracts/active` and grep the
`symbol` field for the BTC/USDT perp. One request settles it.

**K2. Spot and futures use different symbol grammars and different hosts.**
**Assumed:** spot is hyphenated (`ETH-USDT`) and futures is not (`XBTUSDTM`); spot
lives on `api.kucoin.com`, futures on `api-futures.kucoin.com`.
**If wrong / if missed:** an adapter that derives one symbol from the other is
wrong on this venue specifically — and KuCoin is one of only two venues carrying a
spot fixture, so it is load-bearing for spot-perp pairing. A derived-symbol bug
here means the spot leg silently resolves to nothing and the "hedged" pair is one
naked perp.
**Check:** `GET /api/v2/symbols` (spot) vs `/api/v1/contracts/active` (futures) and
diff the symbol formats for the same underlying.

**K3. The websocket requires a token handshake before any book can be read.**
**Assumed:** KuCoin market data needs `POST /api/v1/bullet-public` first, returning
a token plus an endpoint list, and the token expires.
**If wrong / unhandled:** unlike the other seven venues there is **no anonymous
book subscription**, so KuCoin's depth is unavailable until that handshake is
implemented, and the token expiry means a long-running process loses the feed at an
unpredictable interval. A venue whose book silently stops updating is worse than a
venue that is absent — see G6.
**Check:** `POST /api/v1/bullet-public`, note `token`, `endpoint`, and
`pingInterval` / token TTL.

**K4.** `fundingRateGranularity` is recalled as **milliseconds** (expect
`28800000`); timestamps may be **nanoseconds** on the websocket (G1b);
`multiplier` is recalled as **0.001 BTC** for the BTC perp (G7 → 1000x if wrong);
`min_notional` is **absent** from the perp fixture on the assumption KuCoin gates
on a 1-contract minimum rather than a quote notional. All four unverified.
**Check:** `GET /api/v1/contracts/active` returns `multiplier`, `tickSize`,
`lotSize`, `fundingRateGranularity`, `fundingFeeRate` in one response — a single
request clears most of this venue.

### HTX (ex-Huobi) — least-known API of the eight

Files: `htx-btcusdt-perp.fix` (linear perp, the deliberately thin book).

**H1. The API surface itself is the assumption.**
**[do not know]** HTX is the venue whose API we recall least well. The path prefix
`/linear-swap-api/v1/...` is assumed, as is the separation between linear-swap,
coin-swap and futures product lines under different prefixes, and the recollection
that **cross-margin and isolated-margin accounts use different endpoints for the
same logical query**.
**If wrong:** HTX contributes nothing and we will not know whether it is because
the venue is quiet or because we are asking the wrong URL. Given HTX carries the
*lowest* positive rate in the set (0.00006, 6.57% APR), silently losing it costs
little in the fixtures — but the real venue's rate is unknown and could be the top
of the table.
**Check:** `GET /linear-swap-api/v1/swap_contract_info?contract_code=BTC-USDT` first;
if that 404s, the whole prefix assumption is wrong and every other HTX item below
is moot.

**H2. Funding interval must be derived, not read.**
**Assumed:** 8h, with `interval_source = venue`.
**If wrong:** 2x or 8x per G2.
**Check:** `GET /linear-swap-api/v1/swap_funding_rate?contract_code=BTC-USDT` and
compute `next_funding_time - funding_time`. Also read `funding_rate` vs
`estimated_rate` in the same response — the presence of two rate fields is the G4
predicted-vs-settled trap again, and we do not know which is which here.

**H3. Funding rate caps.**
**[do not know]** HTX is recalled as publishing `funding_rate_cap` /
`funding_rate_floor`. We did not model caps anywhere in the fixtures.
**If wrong:** an uncapped forecast overstates extreme-regime carry. Caps bind
exactly during the volatility we want to trade, so the error is concentrated in the
periods that matter most, not spread evenly.
**Check:** the same `swap_funding_rate` response; look for cap/floor fields on every
venue, not just HTX (binance `/fapi/v1/fundingInfo` is recalled as returning
`adjustedFundingRateCap`).

**H4.** `contract_size` recalled as **0.001 BTC** (G7 → 1000x); `swap_depth` offers
**both `version` and `mrid`** as sequence candidates and we do not know which is
monotonic (G6); depth comes in fixed tiers (`step0`..`step5`) with differing
aggregation, and **the fixture's 2-level book does not correspond to any known
tier** — it is thin by construction, not by observation.

### MEXC — richest rate in the set, and possibly untradeable

Files: `mexc-btcusdt-perp.fix` (the `rate_source = historical` case),
`mexc-ethusdt-perp.fix` (the missing-mark/index case).

**M1. Futures API order placement may be closed to us entirely.**
**Assumption, and the most consequential one for this venue:** MEXC is recalled as
having **restricted or disabled futures order placement via API** for at least some
period and some account classes, while leaving market data readable.
**If true:** MEXC carries the **highest BTC funding rate in the fixture set
(0.00030, 32.85% APR)** and is one half of the widest pair (38.33% APR against
OKX). If we can read that rate but not trade on it, **the single best row in our
table is a mirage**, and every backtest that includes MEXC overstates achievable
return. This is not a precision issue; it is the difference between a real
opportunity and an unfillable one.
**Check:** with our own keys, attempt a minimum-size order on `BTC_USDT` via
`POST /api/v1/private/order/submit` and read the error code. Documentation will not
answer this; only an attempt will. **Do this before MEXC appears in any ranking
shown to anyone.**

**M2. Contract multiplier of 0.0001 BTC — the largest unit risk in the set.**
**Assumed:** `contract_size = base:0.0001` for BTC_USDT, `base:0.01` for ETH_USDT.
**If wrong:** per G7 this is a **10,000x** error on the BTC leg, and the worked
example there shows the malformed order **passes the `min_notional = 5` check**
(5.79 > 5) and gets accepted. Silent, accepted, and 10,000x wrong is the worst
combination available.
**Check:** `GET /api/v1/contract/detail?symbol=BTC_USDT` → `contractSize`,
`volUnit`, `minVol`, `priceUnit`. Then confirm with a minimum order's resulting
position size in BTC.

**M3. The published rate is the last settled one.**
**Assumed:** `rate_source = historical` on the BTC fixture — a deliberate choice to
give the set one stale row, not something we read from MEXC.
**If wrong** (i.e. MEXC actually publishes a forecast): we are needlessly
discounting the venue's rate and would skip real carry. If **right and ignored**
downstream: we rank a payment already made, per G4.
**Check:** `GET /api/v1/contract/funding_rate/BTC_USDT` → `fundingRate`,
`nextSettleTime`, `collectCycle`; compare `fundingRate` against the most recent
entry of `GET /api/v1/contract/funding_rate/history`. Equal ⇒ settled.

**M4.** `collectCycle` recalled as **hours** (expect 8); fees recorded as
0.02%/0.06% while MEXC is recalled as having run **zero-maker-fee futures
promotions** — if one is live our maker fee is overstated, which understates edge
(the safe direction, per G8).

### Gate — the one venue whose interval we openly guessed

Files: `gate-btcusdt-perp.fix` (the `interval_source = assumed` case).

**GA1. The funding interval is an admitted guess, not a memory.**
**Assumed:** `interval_secs = 28800`, `interval_source = assumed`. We did not recall
Gate's interval; we copied the other seven venues.
**If wrong:** per the G2 table, Gate at 4h is 32.85% APR instead of 16.43%, moving it
from 5th to 2nd in the BTC ranking; **at 1h it is 131.40% APR and sorts first,
showing a fictitious 137% spread against OKX — 3.4x the set's real best pair.** The
venue we know least about would become the venue we size into hardest. This is the
mechanism by which a missing measurement becomes a losing trade, and it is the
reason `interval_source` exists as a field at all: **a scanner must be able to
refuse to rank an `assumed` interval.**
**Check — highest priority item in this document:**
`GET /api/v4/futures/usdt/contracts/BTC_USDT` → `funding_interval` (recalled as
**seconds**, so expect `28800`), and cross-check against
`funding_next_apply` minus the previous apply time.

**GA2. Gate speaks seconds where everyone else speaks milliseconds.**
**Assumed:** per G1a, Gate's timestamps are in **seconds**, while the fixture stores
milliseconds — so **the fixture cannot catch a missing x1000 conversion in the Gate
adapter.** Structural blind spot, stated so it is not discovered later.
**If wrong:** funding times off by 1000x; Gate either never trades or always trades.
**Check:** digit-count `funding_next_apply` in the response above. 10 digits ⇒
seconds.

**GA3. `quanto_multiplier = 0.0001` and a suspiciously good maker fee.**
**Assumed:** `contract_size = base:0.0001` (Gate's `quanto_multiplier`), and
`maker = 0.00015` — recorded in-file as **the cheapest maker fee of the eight**.
**If wrong:** the multiplier is a **10,000x** unit risk per G7. The fee is worse in
character than in size: it is the one fee we recalled as *better* than its peers,
so if the memory is flattering, **Gate's edge is overstated** — the dangerous
direction, unlike every other fee in the set where we rounded against ourselves.
0.005% of overstated maker rebate is ~0.5 bp per side, ~2 bp on a round trip, ~17%
of a 12 bp edge.
**Check:** `GET /api/v4/futures/usdt/contracts/BTC_USDT` → `quanto_multiplier`,
`order_price_round`, `mark_price_round`; and
`GET /api/v4/wallet/fee` with our own keys for the actual maker rate.

**GA4.** Note that `gate-btcusdt-perp.fix` and `mexc-btcusdt-perp.fix` share the
identical native symbol string `BTC_USDT` **and** the identical
`contract_size = base:0.0001`. These are two independent unverified memories that
happen to coincide; **do not treat the agreement as corroboration.** If a shared
code path derives one from the other, both are wrong together and the coincidence
hides it.

### Bitget — a symbol format that may belong to a retired API version

Files: `bitget-btcusdt-perp.fix` (linear perp),
`bitget-ethusdt-perp.fix` (the funding-only fixture, no `[book]`).

**B1. `_UMCBL` is a v1 artefact.**
**Assumed:** `native = BTCUSDT_UMCBL` / `ETHUSDT_UMCBL`, where `UMCBL` marks
USDT-margined perpetuals in Bitget's **v1** API. Bitget's **v2** API is recalled as
having dropped the suffix in favour of a separate `productType=USDT-FUTURES`
parameter with a plain `BTCUSDT` symbol.
**If wrong / if we talk v2:** every Bitget request 404s or returns an empty
instrument — loud, therefore survivable. The genuinely dangerous variant: **`BTCUSDT`
without a `productType` may resolve on the wrong product line** (coin-margined, or
the demo/simulated `SUMCBL` line). Resolving to the *simulated* product would give
us plausible prices, plausible books, plausible fills, and no real position — a
failure that looks exactly like success.
**Check:** `GET /api/v2/mix/market/contracts?productType=USDT-FUTURES` and read the
`symbol` field verbatim; note whether a suffix is present. Then confirm the same
symbol is rejected without `productType`.

**B2. The funding-only fixture has no depth, and must not be given any.**
**[fact, verified — about the fixture]** `bitget-ethusdt-perp.fix` has no `[book]`
section. It carries the richest ETH rate (0.00021) and `mark`/`index` but no
tradeable prices.
**Why it exists:** on every venue the funding poll and the book subscription are
different calls, and the funding call is the one that still works when the socket
is down. The engine must rank carry with no depth **and must not then claim it can
execute.** If a size comes out of this fixture, that is a bug: substituting `mark`
for a bid is how a 25.2% APR row (the ETH spread against OKX's -0.00002) turns into
an unfillable order at a price nobody is showing.
**Check:** n/a — this is a design assertion about our own code, not about Bitget.

**B3.** `fundingRateInterval` recalled as **hours, as a string** (expect `"8"`);
`sizeMultiplier` is the multiplier field; taker recorded at 0.06%, recalled as the
worst of the majors — if that memory is inverted we are **understating** edge, the
safe direction (G8). `min_notional = 5` assumed from a recalled `minTradeUSDT`.
**Check:** `GET /api/v2/mix/market/contracts?productType=USDT-FUTURES` →
`sizeMultiplier`, `pricePlace`, `volumePlace`, `minTradeNum`, `minTradeUSDT`; and
`GET /api/v2/mix/market/funding-time?symbol=BTCUSDT&productType=USDT-FUTURES`.

Note `pricePlace` / `volumePlace` are recalled as **decimal-place counts, not tick
sizes** — `pricePlace: 1` means 0.1. Reading a place count as a tick size gives a
tick of 1.0 instead of 0.1, so **every price rounds to the whole dollar**: a 10x
coarsening that on BTC is ~1.5 bp of forced slippage per order, over 10% of edge.

### OKX — three fixtures, including the inverse-notional landmine

Files: `okx-btc-usdt-swap-perp.fix` (negative rate),
`okx-eth-usdt-swap-perp.fix` (negative rate),
`okx-btc-usd-inverse-perp.fix` (**the inverse contract**).

**O1. `ctVal = 100 USD` on the inverse swap, settled in BTC. This is the 68,000x file.**
**Assumed:** `contract_size = quote:100`, `settle = BTC`, `kind = inverse_perp`.
**If parsed as `base:100`:** one contract is read as 100 BTC. At the fixture mid of
68,119.75 that is `100 x 68119.75 / 100 = 68,119.75x` the true notional — **the
~68,000x error.** Concretely: the 250-contract top-of-book bid is 25,000 USD
(~0.367 BTC) and would be read as **25,000 BTC, roughly 1.7 billion USD.**
`ContractSpec::new` rejecting a `quote:` declaration on a linear/spot market and a
`base:` declaration on an inverse one is **the only defence**, and it must stay a
hard error at parse time. Do not add a fallback.
**Second, independent risk on the same field:** Binance and Bybit inverse BTC
contracts are recalled as **1 USD** per contract, not 100. Two live conventions
differing by exactly **100x**, both unverified. A per-venue constant is required; a
shared default is a 100x bug waiting for whichever venue disagrees with it.
**Third:** fees on an inverse contract are charged **in BTC**, not USDT. Booking a
base-asset fee as quote leaves the hedge drifting by the fee amount every fill —
small per trade, monotonic in one direction, and it accumulates precisely as a
directional position.
**Check:** `GET /api/v5/public/instruments?instType=SWAP&instId=BTC-USD-SWAP` →
**`ctVal`, `ctValCcy`, `ctType`, `settleCcy`** together. `ctValCcy` is the field that
disambiguates base-vs-quote per contract, and `ctType` should read `inverse`. Do the
same for `BTC-USDT-SWAP` and confirm `ctType: linear`, `ctValCcy: BTC`.

**O2. Both OKX perps are negative, which is coherent — and is also what a sign bug
looks like.**
**Assumed:** `-0.00005` (BTC) and `-0.00002` (ETH), with `index > mark` on both, so
the discount and the negative rate agree.
**If wrong:** per G3, a flipped sign is the full rate on the wrong side. The
worry specific to OKX is that **uniformly negative rates across a venue is exactly
the signature of an inverted adapter**, so if live OKX rows come out all-negative,
suspect our code before believing the market. The ETH rate `-0.00002` is
deliberately the smallest magnitude in the set — small enough that a rounding bug
in `Rate::parse` could change its sign, which is the point of including it.
**Check:** the funding-payment observation in G3, plus confirm `mark < index`
co-occurs with the negative published rate.

**O3. `fundingRate` vs `nextFundingRate` — an off-by-one-period trap.**
**Assumed:** `rate_source = predicted` on both OKX perps.
**If wrong:** OKX's funding endpoint is recalled as returning **both** the rate for
the period ending at `fundingTime` **and** a forecast for the following period. Both
fields hold plausible numbers, so choosing wrong produces no crash and no
implausible output — just a systematically stale rate (G4).
**Check:** `GET /api/v5/public/funding-rate?instId=BTC-USDT-SWAP` → compare
`fundingRate`, `nextFundingRate`, `fundingTime`, `nextFundingTime`; derive the
interval from the timestamp difference rather than trusting either field's name.

**O4.** `ctVal` recalled as **0.01 BTC** for BTC-USDT-SWAP and **0.1 ETH** for
ETH-USDT-SWAP — independent memories, 100x and 10x unit risks respectively (G7).
`lotSz` is in **contracts** while our linear fixtures store base units. `minSz`
exists but no quote-notional floor, which is why `min_notional` is **absent** from
both OKX linear fixtures. Books carry `seqId` **and** `prevSeqId`, giving a
linked-list continuity check that is stronger than a bare counter — use it (G6).
**Check:** one `public/instruments` call per instId returns `ctVal`, `ctValCcy`,
`ctType`, `tickSz`, `lotSz`, `minSz`, `settleCcy` — the cheapest high-value request
in this entire document.

### Bybit — two unit traps in otherwise familiar shapes

Files: `bybit-btcusdt-perp.fix`, `bybit-ethusdt-perp.fix`.

**Y1. `fundingInterval` is in MINUTES.**
**Assumed:** Bybit reports the funding interval as **minutes**, so an 8h interval
reads `480`.
**If wrong / if misread:** reading `480` as **hours** is a **60x** error (20 days per
period); reading it as **seconds** is a 60x error the other way. Either destroys the
APR and the countdown per G2. This is the clearest example of why the interval unit
must be normalised at the adapter boundary — `480` is a plausible-looking integer in
three different units.
**Check:** `GET /v5/market/instruments-info?category=linear&symbol=BTCUSDT` →
`fundingInterval`. If it reads `480`, minutes is confirmed.

**Y2. Two sequence fields, and we store one.**
**Assumed:** the single `sequence = 4587123` in the fixture stands for whichever of
Bybit's `u` (update id) and `seq` (cross-sequence) the adapter should use.
**[do not know] which one.** `u` is recalled as per-symbol continuity, `seq` as
cross-symbol ordering.
**If wrong:** silent gap-detection failure per G6 — we trade a stale book believing
it current. The fixture cannot distinguish the two, so this is untestable here.
**Check:** `GET /v5/market/orderbook?category=linear&symbol=BTCUSDT` and the
`orderbook.50.BTCUSDT` websocket channel; log **both** fields for an hour and
determine which is gapless per symbol.

**Y3.** `lot_size = 0.01` on the ETH fixture vs Binance's `0.001` — recalled as
genuinely coarser, but unverified; if it is really 0.001 we reject valid sizes, if
0.1 we submit invalid ones (G9). `min_notional = 5` assumed for both, vs Binance
ETH's 20 — an asymmetry worth confirming since it changes the minimum viable
hedge size on the pair. Bybit's `fundingRate` in `/v5/market/tickers` is assumed
**predicted**; verify against `/v5/market/funding/history` per G4.
**Check:** `instruments-info` → `priceFilter.tickSize`, `lotSizeFilter.qtyStep`,
`lotSizeFilter.minOrderQty`; `GET /v5/account/fee-rate` with our keys.

### Binance — best-recalled API, therefore the most likely to be over-trusted

Files: `binance-btcusdt-perp.fix` (the reference snapshot),
`binance-ethusdt-perp.fix`, `binance-btcusdt-spot.fix`.

**N1. Familiarity is not verification.** Binance is the venue we recall best and the
one that anchors the whole fixture set — the other seven BTC perps are priced around
`binance-btcusdt-perp.fix`'s mid of 68,121.75. **That makes an error here propagate
to every cross-venue spread in the set**, and it makes the numbers feel more
trustworthy than they are. They have exactly the same provenance as the rest: memory,
unchecked.
**Check:** treat Binance items with the same suspicion as HTX items. Same queue, no
discount for familiarity.

**N2. `fundingInfo` omits default-interval symbols.**
**Assumed:** 8h on both perps.
**If wrong:** 2x or 8x per G2. The specific trap: `GET /fapi/v1/fundingInfo` is
recalled as listing **only symbols whose interval differs from the default**, so
**an empty response or a missing symbol is the answer "8h", not an error.** An
adapter that treats absence as a failure will refuse to price BTCUSDT; one that
treats absence as 0 will divide by zero or annualise to infinity.
**Check:** `GET /fapi/v1/fundingInfo` and look for BTCUSDT's presence or absence;
cross-check with `GET /fapi/v1/premiumIndex?symbol=BTCUSDT` → `nextFundingTime`
minus the previous one. Also read `adjustedFundingRateCap` there (H3).

**N3. Spot and futures filters differ for the same symbol — the fixtures assume so.**
**Assumed:** spot BTCUSDT `tick 0.01` / `lot 0.00001`, versus USD-M BTCUSDT
`tick 0.1` / `lot 0.001`. The spot tick is assumed **10x finer** than the perp's.
**If wrong:** an off-tick spot price is rejected — loud and safe — but a shared
filter table across the two products means the *lot* is wrong on one of them, which
truncates silently per G9. On a spot-perp basis trade that truncation hits one leg
only, leaving residual delta on every fill.
**Check:** `GET /api/v3/exchangeInfo?symbol=BTCUSDT` and
`GET /fapi/v1/exchangeInfo` and diff the `PRICE_FILTER` / `LOT_SIZE` /
`MIN_NOTIONAL` entries. Do not share a filter cache between the two products.

**N4.** `lastUpdateId` is a **different counter** on the spot and futures streams for
the same symbol — never compare them (G6); the fixture's `sequence = 12345` is a
placeholder ~10^6 too small, so integer width is untested. Fees recorded at
0.02%/0.055% perp and 0.1%/0.1% spot — the **5x spot/perp asymmetry is ~16 bp of
round-trip cost against a 12 bp edge** (G8) and is the single most important fee
fact in this document. `min_notional` 5 (BTC) vs 20 (ETH) assumed from recalled
`MIN_NOTIONAL` filters.
**Check:** `GET /fapi/v1/commissionRate?symbol=BTCUSDT` and
`GET /sapi/v1/asset/tradeFee?symbol=BTCUSDT` — **both account-specific, both
require our keys.** Reconcile against the `commission` field on an actual fill.

---

## Verification queue, ordered by damage if the assumption is wrong

Work top-down. Each line is one request or one observation.

| # | Item | Magnitude if wrong | Cheapest check |
|---|------|--------------------|----------------|
| 1 | OKX inverse `ctVal` / `ctValCcy` / `settleCcy` (O1) | **68,000x** notional | `GET /api/v5/public/instruments?instType=SWAP&instId=BTC-USD-SWAP` |
| 2 | Inverse contract size 1 USD vs 100 USD across venues (O1) | **100x** | same, per venue with an inverse line |
| 3 | MEXC / Gate `contract_size = 0.0001` (M2, GA3, G7) | **10,000x**, and it passes `min_notional` | `contract/detail`; `futures/usdt/contracts` |
| 4 | KuCoin / HTX `multiplier = 0.001` (K4, H4) | **1000x** | `contracts/active`; `swap_contract_info` |
| 5 | Gate funding interval — **openly assumed** (GA1, G2) | **8x** APR; sorts to top of table | `futures/usdt/contracts/BTC_USDT` → `funding_interval` |
| 6 | Funding sign convention, per venue (G3) | Wrong **side**; 2x the rate in P&L | hold one small position across one settlement |
| 7 | MEXC futures API order placement (M1) | Best row in the table is unfillable | attempt one minimum order with our keys |
| 8 | Bitget product line / `_UMCBL` vs v2 (B1) | 404, **or silently the simulated line** | `mix/market/contracts?productType=USDT-FUTURES` |
| 9 | Funding interval, remaining six venues (G2) | **2x** at 4h | one instrument call each; four different units |
| 10 | Timestamp units, esp. Gate=s, KuCoin=ns (G1, G1a, G1b) | **1000x**–**10^6x** | digit-count every timestamp field |
| 11 | Bybit `fundingInterval` in minutes (Y1) | **60x** | `instruments-info` → expect `480` |
| 12 | Spot vs perp fee asymmetry (G8, N4) | **~16 bp** vs a 12 bp edge | `commissionRate` + `tradeFee`, our keys |
| 13 | Predicted vs last-settled rate (G4, O3) | Full rate, sign inverts often | compare live value against history endpoint |
| 14 | Book deltas vs absolute, zero-means-delete (G5) | Quietly divergent depth; sizing error | replay increments onto a snapshot |
| 15 | Which sequence field is monotonic (G6, Y2, H4) | Undetected stale books | log both candidates for an hour |
| 16 | Bitget `pricePlace` = places not tick (B3) | **10x** coarser tick, ~1.5 bp forced slippage | `mix/market/contracts` → read the field |
| 17 | Fee tier (G8) | ~2 bp round trip, ~17% of edge | account fee endpoints, our keys |
| 18 | Tick / lot per instrument (G9) | Rejection (safe) or silent truncation (not) | instrument endpoints, all venues |
| 19 | KuCoin native symbol XBT vs BTC (K1) | 404 — loud, safe | `contracts/active`, grep `symbol` |
| 20 | USD vs USDT treated as distinct (G11) | ~3 bp basis; hidden depeg exposure | any fiat/stable quote |
| 21 | Absolute price level (G14) | All notionals scale; bp reasoning survives | any ticker |
| 22 | Cross-venue clock skew (G12) | Latency masquerading as spread, several bp | `time` endpoints, sampled over an hour |

Items 1–4 are unit errors: they do not degrade the answer, they make it nonsense,
and items 3 and 4 can pass the notional check on the way through. Items 5–6 are the
ones that produce a **confident, plausible, wrong ranking** — the failure mode that
actually loses money, because nothing looks broken.

---

## How this document gets retired

An item leaves this file **only when a measurement replaces it** — a recorded
response, with a timestamp, from our own keys. Not when it seems fine, not when the
code works, not when someone remembers more confidently.

**Do not hand-edit the `!unverified` header out of a fixture.** A verified fixture is
a *new file generated from a recorded live response*, sitting alongside the invented
one or replacing it wholesale. Deleting that first line from a file whose numbers
were still invented converts a labelled fiction into an unlabelled one, which is
worse than having no fixture at all — and it is unrecoverable, because nothing else
in the file records where the numbers came from.

Until then: **16 files, 8 venues, 0 verified numbers.**






