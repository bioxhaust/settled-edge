# 006 — The funding sign law, stated once

**Decided:** A positive funding rate means **longs pay shorts**. The single
authority is `PositionSide::funding_receipt_sign`, which returns `-1` for `Long`
and `+1` for `Short`. No other crate may write a literal minus sign next to a
funding rate.

**Alternatives considered.** (a) Store the rate already signed for the position we
intend to hold — rejected: the snapshot is a fact about the market and must not
bake in a strategy assumption; the same snapshot serves a perp-perp trade where the
two legs take opposite signs. (b) Let each consumer apply the sign inline —
rejected: this is the single highest-consequence sign in the system. Inverting it
turns a losing carry into the top-ranked row of the table, and it is invisible in
review because both spellings look plausible.

**Why this convention.** It is what all eight venues publish, so an adapter can
pass the venue's number through unchanged and any deviation is a transcription bug
rather than a convention mismatch. The consequence for the classic cash-and-carry
follows mechanically: long spot plus short perp **receives** funding when the rate
is positive.

**What this breaks.** Nothing in-tree, but it makes `receipt_rate` the only
sanctioned path, so a crate that needs the raw unsigned rate (display, or a
detector comparing venue-to-venue rates) must reach for `snapshot.rate` and is
then responsible for not attributing a direction to it. Reviewers should treat any
appearance of `rate` outside a display context as suspect. Related: hard rule 8 —
the *claim* that all eight venues use this convention is `[assumption]`, not
`[fact, verified]`, because no live API was reachable this round. It is listed in
`FIXTURES-UNVERIFIED.md` as a first-order item to confirm.
