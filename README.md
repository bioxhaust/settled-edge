# gettin_rich_babe — cross-venue funding & basis scanner

A scanner whose only claim is that **the number it displays is the money that
would actually arrive**. Everything else about it is secondary.

Every other scanner in this space prints a top-of-book spread, charges one fee leg
or none, ignores whether the book can absorb your size, and never says that a
quote is stale. Each of those omissions is worth more than the edge being hunted.
This one refuses to print a number it cannot stand behind.

---

## Run it

Requires only a Rust toolchain. **Zero external dependencies** — nothing to
install, no network needed.

Build:

```bash
cargo build --offline --workspace
```

Test:

```bash
cargo test --offline --workspace
```

Run the replay over the fixtures in this repository:

```bash
cargo run --offline -p arb-engine -- replay fixtures/
```

Help:

```bash
cargo run --offline -p arb-engine -- --help
```

Verified on this machine: `rustc 1.97.1 (8bab26f4f 2026-07-14)`,
`cargo 1.97.1`, `aarch64-apple-darwin`. [fact, verified]

`--offline` is used everywhere because this project has no dependencies to fetch;
it also proves that. The commands work without it too.

### What you will see

A ranked table — instrument, both venues, the size the number is true for,
net-of-costs APR, gross APR, the fee and slippage totals it was built from,
required margin, distance to liquidation in bps, periods to break even, and the
data-quality marks. Then a per-leg breakdown, and then this:

```
NOTHING HERE IS TRADEABLE.
```

That is the scanner working, not failing. See *Honesty* below.

---

## Honesty

**The fixtures in `fixtures/` were invented from memory of exchange API shapes and
have never been compared against a live response.** crates.io and every exchange
API are blocked in the environment this was built in, so no measurement was
possible. Consequently:

- Every fixture file carries an unreliability marker, and
  [`FIXTURES-UNVERIFIED.md`](FIXTURES-UNVERIFIED.md) states field by field what
  is recalled, what is guessed, and what is unknown.
- Every row the replay prints carries `Provenance::SynthesizedUnverified`, which
  makes it **unrankable by construction**. No configuration of invented data can
  present itself as tradeable. `arb-engine`'s test suite asserts this.
- The arithmetic *is* real and *is* tested — including against numeric vectors
  computed by hand as exact rationals, independently of the implementation.

So: the engine works, the numbers do not mean anything yet. Those are two
different statements and this repository keeps them apart.

Every claim in the documents here is tagged `[fact, verified]`, `[assumption]`, or
`[do not know]`. "Verified" means verified against the code or the toolchain in
front of us — never against an exchange.

---

## What the number contains

For a **given size**, not for the touch:

| Component | How it is obtained |
|---|---|
| Gross funding | Each leg annualised **separately** after the sign law, then summed. Never `rate_a - rate_b` — that ranks backwards whenever the two venues' funding intervals differ. |
| Fees | **Four** legs: enter both, exit both. A "spread minus fees" display that charges one understates by 4×. |
| Slippage | VWAP from walking the real ladder for the real size, both legs, entry **and** exit. |
| Amortisation | The round-trip cost is spread over a configured holding horizon (7 days in the replay). Amortising over one 8h settlement flatters a carry threefold. |
| Size | The largest size whose **average** fill price stays inside the slippage budget, capped so the realised notional never exceeds the budget. |
| Margin, liquidation | From the realised notional at the configured leverage; `None` rather than an invented number where the inputs do not determine it. |

### And what it does not contain

Read [`OPEN.md`](OPEN.md) before trusting any figure. The short version:

- **Exit slippage is assumed equal to entry slippage.** The book at exit time is
  unknowable now. This is the largest known unknown in `net_apr`.
- **Borrow cost on a short-spot leg is not modelled.** It can exceed the entire
  edge on its own.
- Funding is a snapshot, often a *predicted* one. `periods_to_breakeven` exists
  so you can see how long the rate must persist to pay off the round trip.

---

## Layout

Cargo workspace, one crate per owner, path dependencies only.

| Crate | Contents |
|---|---|
| `arb-num` | Fixed-point money newtypes. No `f64` touches a price, size, rate or fee anywhere in this repository. |
| `arb-types` | Venues, instruments, snapshots, clocks, quality marks. |
| `arb-math` | Every formula: annualisation, VWAP, capacity, fees, margin, liquidation, break-even. |
| `arb-book` | Order-book state and ladder walking. |
| `arb-detect` | Staleness, clock skew, crossed books, painted depth. Every threshold justified in a comment beside the constant. |
| `arb-venue` | The only crate permitted to do I/O. Adapter traits plus a fixture-backed implementation. |
| `arb-scan` | **Public — this is the product.** Pairing rules, sizing, the net-of-costs row. |
| `arb-private` | **Never shipped to clients.** Thin-capacity strategies, stubs only. The split is physical at crate level, not a feature flag. |
| `arb-engine` | Wiring, the deterministic replay, the report, the binary. |
| `arb-adversary` | Tests only, no product code. Its single question is where the displayed number diverges from the money in the account. |

### Why there are no dependencies

Not minimalism. The environment blocks crates.io, so the core is pure `std` and
the network layer exists as **traits** plus a fixture implementation behind them.
When the network opens, `tokio` and a WebSocket client appear behind the same
traits and the core does not change by one line.

---

## Tests

661 tests, all green. [fact, verified]

| Crate | Tests |
|---|---|
| arb-math | 141 |
| arb-adversary | 107 |
| arb-detect | 97 |
| arb-venue | 94 |
| arb-types | 74 |
| arb-book | 71 |
| arb-num | 41 |
| arb-scan | 15 |
| arb-private | 12 |
| arb-engine | 9 |

`arb-adversary` writes no product code and cannot edit anyone else's. It is
allowed exactly one action: add a failing test that proves a defect. Round one
produced ten; eight named real defects and are fixed, two were arithmetic errors
in the adversary's own expectations. The adjudication, re-derived as exact
rationals rather than by reading the code's output, is
[`decisions/018`](decisions/018-adversary-round-one-adjudication.md).

### A note on build warnings

`cargo build --offline --workspace` produces **zero warnings about this code**.
On this machine one environmental warning appears on the crate that links a
binary:

```
warning: linker stderr: cc: error: couldn't create cache file
'/var/folders/.../T/xcrun_db-…' (errno=Operation not permitted)
```

That is `rustc`'s `linker_messages` lint relaying that Apple's `cc` could not
write its `xcrun` cache, because the sandbox this was built in denies writes to
that directory. It is not a warning about the source and it does not appear
outside such a sandbox. It is documented rather than suppressed. [fact, verified]

---

## Decisions

`decisions/` holds one file per resolved dispute, each a paragraph on what was
decided and why. A decision taken in conversation that never reached that
directory counts as not taken.

The one most worth reading is
[`019 — book units are not contracts`](decisions/019-book-units-are-not-contracts.md):
an order book is denominated in base for linear markets and in contracts for
inverse ones, while every `ContractSpec` function speaks contracts. Comparing the
two is wrong by a factor of the contract size — 100× on OKX's BTC swap, 10 000× on
Gate's — and on Binance, where one contract is one coin, it is wrong in a way that
looks exactly right. It reached the report as a margin column ranging from 4.69 to
261 447.99 against a single 100 000 target before it was caught.

---

## Scope of this round

- **Stage 1 — implemented.** Funding and basis across eight venues (Binance,
  Bybit, OKX, Bitget, MEXC, Gate, KuCoin, HTX), net of costs, ranked by expected
  return, with distance to liquidation and explicit data-quality marks.
- **Stage 2 — types and seams only.** Executable spread over depth, real fee
  tiers, three leg configurations, venue trust scoring.
- **Stage 5 — traits only, no implementation.** Execution, order lifecycle,
  idempotency, reconciliation, unwind, limits, kill switch. Nothing here can place
  an order, and nothing here has ever held a key.
