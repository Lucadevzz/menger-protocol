# Menger on Lightning: the rules and the messages (version 3)

This document describes what two parties must do to keep a balance stable in gold inside Lightning
channels. It is written for reviewers and for anyone who wants to write another program that
speaks to this one. The reference implementation is in `core/` (the rules, no I/O) and `node/`
(a Lightning node built on LDK Node). Where this text and the code disagree, that is a bug: please
report it.

Nothing here is enforced on the chain or by a third party. Each party values the position with its
own prices and pays what it owes; the only sanction is the right to leave with what one holds.
What that means for each party's risk is in [SECURITY.md](SECURITY.md).

## 1. Roles

- The **saver** (épargnant) wants a balance that keeps its value in gold.
- The **provider** (fournisseur) takes the other side: it gains when bitcoin outpaces gold and
  loses when it falls behind. A provider serves any number of savers; a saver has one provider at a
  time.

Both hold their own keys and their own bitcoin, in ordinary Lightning channels between the two of
them. The saver's channels with its provider are unannounced. To pay the saver when bitcoin falls,
the provider needs bitcoin on its side of those channels: it opens a channel of its own towards
the saver (or the saver receives payments through it).

## 2. Units

- 1 Menger (MNGR) = 3.545 g of fine gold. Amounts are counted in micro-Mengers (µMNGR,
  1 MNGR = 1,000,000 µMNGR) and in satoshis, as integers.
- A price is a whole number of satoshis per Menger, `P`. From a dollar price of bitcoin `B` and of
  a troy ounce of gold `G`: `P = round(100,000,000 × G × 3.545 / 31.1034768 / B)`.
- `sats_for(m) = round(m × P / 1,000,000)` (half up); `micro_floor(s) = floor(s × 1,000,000 / P)`;
  `micro_ceil(s) = ceil(s × 1,000,000 / P)`.

## 3. Prices

Each party reads its own sources; there is no common oracle. A party's price is computed as
follows, separately for bitcoin and for gold.

1. Keep, for each source, its most recent quote, if it is positive, finite, not older than the
   maximum age (bitcoin 120 s, gold 900 s) and not more than 60 s in the future.
2. Group sources into families: the family of `paxg:kraken` is `paxg`; a name without a colon is
   its own family. A family is worth the median of its sources. (The same gold token quoted on
   three markets is one opinion, not three.)
3. At least `min_sources` families (default 2) must be present; take the median of the families;
   keep those within the maximum spread of that median (bitcoin 2 %, gold 1 %); at least
   `min_sources` must remain; the price is the median of those.

Gold does not trade from Friday 21:00 to Monday 00:00 UTC. In that window the last gold price
accepted while the market was open is used, for at most four days; bitcoin keeps moving.

**Without a price a party does nothing**: it does not pay, does not judge the other party, and its
clock for the other party's delays starts again when prices are back.

The default sources are three bitcoin markets (Kraken, Bitstamp, Coinbase) and three gold families:
the spot price (Swissquote), the PAXG token (Kraken, Coinbase, Bitstamp) and the XAUT token
(Kraken). For Kraken and Bitstamp the price is the middle of the best bid and ask, refused when
they are more than 1 % apart. Any party can replace the list (`--prices sources:FILE`).

## 4. The position

Both parties record the same four numbers:

| Field | Meaning |
|---|---|
| `micro_at_start` | µMNGR promised when the position was last changed |
| `started_at` | Unix time of that change |
| `rate_ppm_per_year` | price of stability, in millionths per year; positive: paid by the saver |
| `backing_sats` | satoshis the saver holds for the position; moves only with stability payments and accepted changes |

The value promised at time `t` is
`micro(t) = micro_at_start − micro_at_start × rate × (t − started_at) / (1,000,000 × 31,557,600)`,
with integer division truncating toward zero, `t − started_at` kept between 0 and 100 years, the
rate between −1,000,000 and +1,000,000, and the result not below zero.

The position is bookkeeping. The satoshis are simply in the saver's side of the channels; whatever
the saver holds beyond `backing_sats` is ordinary, "free" bitcoin.

## 5. The stability rule

Each party runs this at every tick (30 s by default) with its own price `P` and clock `now`.

```
target    = sats_for(micro(now))
drift     = backing_sats − target         (positive: the saver owes; negative: the provider owes)
owed      = |drift|
tolerance = max(target × threshold, min_payment)
```

1. **Coverage (saver only).** `reserve` = what the provider could still send over all open
   channels (a payment from it still on its way counts as its own), less what it owes now. `coverage = reserve / (reserve + target)`: the largest fall of
   bitcoin against gold the provider can still pay for. Below `close_coverage`, the saver leaves.
   Below `warn_coverage` it warns its owner. This is read from the channels' state and does not
   depend on the provider being connected.
2. **Backing (provider only).** If the saver holds, in the channels, less than `backing_sats` by
   more than `min_payment` (measured when no payment is in flight between them, or when payments
   have been in flight without a pause for 10 minutes: a payment held open on purpose must not
   hide that the bitcoin left), the provider pays nothing and treats the missing amount as owed to it: it waits, then leaves (rule 5).
3. If `owed < tolerance`: do nothing.
4. **The party that owes pays**, unless one of its own stability payments is still in flight, or
   it paid less than `cooldown` ago, or the other party cannot be reached. It pays
   `owed − tolerance / 2`, at most what it can send, and nothing if that is under `min_payment`.
   Paying down to half the tolerance, not to zero, keeps two parties whose prices differ slightly
   from sending the difference back and forth.
5. **The party that is owed waits**, from the first tick at which it saw the debt (`since`), until
   a deadline:
   - other party unreachable: `since + offline_grace`;
   - other party reachable since `back`: `max(since, back) + grace`, and never later than
     `since + offline_grace + grace`.

   At the deadline, if it is still owed at least `max(target × close_owed, tolerance)` (or, for
   rule 2, if the backing is still missing), it leaves.

   **A party that slept** (two of its ticks more than 5 minutes apart: a computer asleep, a
   program stopped) forgets `since` and `back` for every counterparty: the delays start again from
   its return, since the other party could not have paid it, nor be heard, meanwhile.

| Parameter | Default | Meaning |
|---|---|---|
| `threshold` | 0.1 % | drift nobody pays for |
| `min_payment` | 100 sats | smallest stability payment |
| `cooldown` | 60 s | pause after one's own payment |
| `grace` | 10 min | time to pay, when reachable |
| `offline_grace` | 6 h (saver), 72 h (provider) | time to pay, when unreachable |
| `close_owed` | 2 % | unpaid debt that justifies leaving |
| `warn_coverage`, `close_coverage` | 30 %, 10 % | provider's remaining cover |

Every parameter is a party's own choice. They do not have to match.

## 6. Changing a position

Only the saver asks; the provider prices. A request adds satoshis to the position (the first one
opens it) or takes satoshis out.

```
saver    → provider   Adjust { at, delta_backing, limit_micro, rate_ppm_per_year }
provider → saver      Reply  { at, accepted, delta_micro, reason, owed_sats }
```

- `at`: Unix time chosen by the saver; identifies the request, and is the time from which the
  changed position accrues. Each request of a saver has a strictly larger `at` than the one before.
- `delta_backing`: satoshis added (positive) or removed (negative); never zero.
- `limit_micro`: growing, the fewest µMNGR the saver accepts for these satoshis; shrinking, the
  most µMNGR it gives up for them. The reference saver computes it from its own price with a
  slack of 0.3 %: `floor(micro_floor(s) × 0.997)` or `ceil(micro_ceil(s) × 1.003)`.
- `rate_ppm_per_year`: the rate the saver proposes when opening; afterwards, the position's rate.

**The provider** refuses (`accepted: false`, with a `reason` in words) when any of this holds, and
otherwise answers with `delta_micro`, computed at its own price `P`, with `spread` its margin
(default 0):

1. `|at − now| > 600 s`; `|rate| > 1,000,000`; the rate differs from the open position's; no
   position and the rate is below the provider's minimum; `delta_backing` is zero or above
   2.1 × 10^15.
2. Growing by `s` satoshis: `s` under its minimum (1,000 sats);
   `granted = floor(micro_floor(s) × (1 − spread))` is zero or below `limit_micro`; the saver would
   not hold `backing_sats + s` in the channels; the provider's own bitcoin in the channels would
   cover a smaller fall than it requires (30 %). Otherwise `delta_micro = granted`.
3. Shrinking by `s` satoshis, with `M = micro(at)`:
   - no position, or `s > backing_sats`;
   - `s = backing_sats` (everything leaves; the position ends): with
     `worth = floor(sats_floor(M) / (1 + spread))`, refused if `backing_sats > worth`, with
     `owed_sats = backing_sats − worth` (the saver must first pay what it owes, however little), or
     if `M > limit_micro`; otherwise `delta_micro = −M`;
   - `s < backing_sats`: `taken = ceil(micro_ceil(s) × (1 + spread))`; refused if `taken ≥ M` or
     `taken > limit_micro`; otherwise `delta_micro = −taken`.

The provider also refuses when it has no price of its own, or while its channels with the saver
are closing.

At the provider's own price, an accepted change never lowers what the saver owes it (growing is
rounded down, shrinking up), and a position ends only once the saver owes it nothing. Forgiving
small debts at the end would let a saver end its position at each small rise of bitcoin, reopen
it, and never pay for one.

**Ending with a debt.** A refusal with `owed_sats > 0` tells the saver what to pay. The reference
saver, when it asked to end its position and `owed_sats` is no more than what it owes at its own
price plus its slack and `min_payment`, sends a stability payment of `owed_sats`, then asks again
once it has arrived; twice at most.

**Applying a change**, on both sides and with the same numbers:
`micro_at_start ← micro(at) + delta_micro`, `started_at ← at`, `backing_sats ← backing_sats +
delta_backing` (the rate is set when the position opens and never changes). Each side adds the
change to what it has recorded, so a stability payment that crosses the request is not lost. A
position with zero Mengers no longer exists, whatever backing the books still show (a stability
payment that crossed the request can leave a few satoshis on one side).

**The saver** checks an accepting reply: growing, `delta_micro > 0` and
`delta_micro ≥ limit_micro`; shrinking, `delta_micro < 0` and `|delta_micro| ≤ limit_micro`. A reply outside those bounds means the provider did something else
than what was asked: the saver leaves.

**Delivery.** The saver writes a request down before sending it and sends it again, unchanged,
until it is answered: after 60 s, then twice as long each time, up to an hour. It does not ask at
all while the provider has nothing on its side of the channels (its reply travels with a payment).
The provider stores its answer before sending it; when the same `at` comes again it sends the same
answer; an older `at` gets a refusal. A request is therefore applied once on each side or on
neither.

## 7. Messages

Messages travel in the custom records of Lightning spontaneous payments (keysend) between the two
parties: the channel is the only link, there is no server.

- TLV record type `0x4D4E4701` (odd, above 65,536). Value: a JSON object, UTF-8.
- A message that is not a stability payment carries 1,000 msat.

```json
{"v":3,"from":"<sender node id, hex>","to":"<recipient node id, hex>","sig":"<signature>",
 "kind":"adjust","at":1793793600,"delta_backing":-246128,"limit_micro":501500,"rate_ppm_per_year":0}
```

| `kind` | Other fields |
|---|---|
| `adjust` | `at`, `delta_backing`, `limit_micro`, `rate_ppm_per_year` |
| `reply` | `at`, `accepted`, `delta_micro` (0 when refused), `reason` (text or null), `owed_sats` (only when not zero) |
| `stability` | `sats_per_menger` (the payer's price), `backing_after` (the saver's backing after this payment, as the payer computes it); both are informative |

**Signature.** `sig` is the sender's signature, with its node key, in the format of Lightning's
`signmessage` (zbase32, recoverable), of these bytes: the message without `sig`, as compact JSON
(no spaces), fields in this order: `v`, `from`, `to`, `kind`, then the other fields in the order of
the table above. A keysend does not say who sent it; the signature does. `to` keeps a message that
one party received from being passed on to someone else as if it were meant for them.

**A recipient ignores** a message whose `v` is not 3 or that does not parse (its satoshis stay
plain bitcoin), whose `to` is not its own node id, whose signature does not verify for `from`,
whose kind does not fit its role (only a provider answers `adjust`; only a saver reads `reply`, and
only from its provider), or whose sender has no channel with it.

## 8. Stability payments

A stability payment is a keysend of the amount due carrying a `stability` message. The recipient
adds the amount received to `backing_sats` (provider pays saver) or subtracts it (saver pays
provider) when the payment arrives; the payer does the same when it learns the payment has
succeeded. The payer chooses the payment's preimage itself and writes the payment down before
sending it, so that after a crash it can look up what became of it. An incoming payment is counted
once, by its hash.

A stability payment that reaches a saver before the `reply` that opens its position is counted
when the reply arrives.

## 9. Paying and receiving

A saver can pay any Lightning invoice from its balance; the payment is an ordinary one, routed
through its provider.

1. `need = amount + fee margin`, where the fee margin is `amount / 100 + 50` sats. That margin is
   also the routing-fee limit given to the payment.
2. Free satoshis are used first. For what is missing, the saver first asks the provider to shrink
   the position (section 6); the payment leaves only once the reply is in.
3. One thing at a time: no payment while a request to the provider is waiting for its answer,
   and no request that grows the position while a payment is in progress (both could count the
   same free satoshis). An invoice whose hash the node already knows is refused.
4. A payment that has not left after 5 minutes (no reply), or whose invoice expires, is given up.
   Satoshis released for a payment that failed or was given up, and the part of the margin that
   was not spent, go back into the position automatically.

What a saver receives (any payment that is not a Menger message) is added to its position
automatically, by an ordinary request, once no payment is in flight in its channels. A refused
automatic request is tried again 5 minutes later.

An invoice "for 0.3 MNGR" is an ordinary invoice in satoshis, priced by the payee at its own price
when it is created.

## 10. Leaving

Leaving means closing every channel with the other party; the position ends, and each keeps what
it holds.

- If the other party can be reached, the close is cooperative: the bitcoin is back on the chain at
  the next confirmation. A cooperative close that has not completed after 15 minutes is forced.
- Otherwise the close is forced: the bitcoin comes back after the channel's waiting period (144
  blocks by default).
- When no channel is left with a party, any position with it is over. New channels with the same
  party start from nothing.

A saver can leave at any time; it can also take its whole position back to plain bitcoin without
leaving (section 6, shrinking by everything).

## 11. What is written down, and when

- A request, before it is sent; an answer, before it is sent.
- A stability payment (its identity and amount), before it is sent; a payment to someone else,
  under the hash of its invoice, before it is sent.
- Every change caused by an event of the Lightning node, before the event is acknowledged to the
  node (an event not acknowledged is delivered again; incoming payments are remembered by hash so
  that a second delivery changes nothing).
- The state file is written to a temporary file, flushed to disk and renamed; the previous
  version is kept as a copy and used if the file is missing or damaged.
