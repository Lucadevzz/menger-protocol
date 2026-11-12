# Menger on Lightning: what can go wrong, and for whom

**Status: unaudited research prototype. Test networks only.** It has been tested end to end on a
private Bitcoin network with real Lightning nodes; it has not been reviewed by a Lightning
developer, and it runs on a development version of LDK Node. Do not put real bitcoin in it.

This document says what the design promises, what it does not, and which attackers the program
defends against. The rules themselves are in [PROTOCOL.md](PROTOCOL.md).

## The promise, and its limit

Nobody ever holds anybody else's bitcoin: each party's funds stay on its own side of ordinary
Lightning channels, under its own keys. There is no token, no bridge, no common oracle, no
administrator.

The stable value is kept by payments that each party owes the other as prices move, and **nothing
forces those payments** except the other party's right to leave. So the design is trust-minimised,
not trustless:

> **Each party can lose what the other owes it and has not yet paid, and nothing else.**

| | Can lose | Bounded by |
|---|---|---|
| Saver | The fall of bitcoin against gold since the provider's last payment, if the provider stops paying. | The time before the saver leaves: 10 minutes if the provider is reachable and silent, 6 hours if it has vanished (both are the saver's own settings), plus price moves while a forced close confirms. |
| Saver | Up to its own stated slack (0.3 % by default) on each change of position, to a provider that always prices at the saver's limit. A payment to someone else that the provider shrinks the position for, then lets fail, costs that slack twice (out, then back in). | The slack, chosen by the saver. Every fill is logged next to the saver's own price. |
| Saver | Routing fees on its payments: the provider is their first hop and may charge up to the limit. | 1 % of the amount plus 50 sats per payment, set aside before it leaves. |
| Provider | The rise of bitcoin against gold that a saver owes and never pays (a saver that defaults or never comes back). The provider loses nothing it holds; it forgoes a gain while having stayed exposed to the loss. | The time before the provider leaves: 10 minutes for a reachable saver, 72 hours for an absent one (the provider's own settings). |
| Provider | With `--canal-retour`: on-chain fees and locked liquidity for channels opened in return for savers who then do nothing. | A minimum size for the saver's channel; off by default. |
| Either | On-chain fees of closing channels; a day's delay (144 blocks) after a forced close. | |

An absent saver therefore holds a free option against its provider for as long as the provider
waits. That is what the price of stability (`rate_ppm_per_year`), the provider's margin and its
patience are for: they are market prices that each provider sets and each saver compares.

## Attackers considered

**A saver with a modified program.**
- *Taking satoshis back without giving Mengers back* (the flaw of protocol version 2): the saver no
  longer names the Mengers of a change. It names satoshis and a limit; the provider computes the
  Mengers at its own price, rounded in its own favour. Property-tested: at the provider's price no
  accepted change lowers what the saver owes it, and a position ends only once the saver owes it
  nothing (an earlier version forgave debts within the tolerance at the end: a saver could have
  ended and reopened its position at each small rise of bitcoin, and never paid).
- *Moving its bitcoin out of the channels while keeping the position*, to be paid when bitcoin
  falls without holding what it would owe when it rises: the provider compares, at every tick,
  what the saver holds in the channels with what the position counts. If satoshis are missing it
  pays nothing and, after the usual delay, leaves. Payments in flight are not counted as the
  saver's, and do not hide its balance for more than 10 minutes (a payment held open on purpose).
- *Claiming a position it cannot back, lying about its price, replaying requests*: a growing
  position must be held in the channels; the saver's price is never used by the provider; a request
  is identified by its time, answered once, and the stored answer is repeated.
- *Never paying, or going offline when it owes*: see the table above.

**A provider with a modified program.**
- *Pricing changes against the saver*: bounded by the saver's limit; an answer outside the limit
  makes the saver leave.
- *Not paying*: the saver leaves after its delay. *Running out of bitcoin to pay with*: the saver
  watches the provider's remaining cover in the channels and leaves below 10 %, while it still
  holds its value.
- *Refusing to forward the saver's payments*: they fail; when the position had to shrink for one,
  the saver pays its slack again to put the satoshis back. The saver can leave.
- *Never answering requests*: the saver keeps asking, less and less often (up to once an hour,
  each time with 1 sat), and can leave.
- *Publishing an old channel state*: the saver's node punishes it, if it comes online within the
  channel's delay (144 blocks). The saver must connect about once a day; no watchtower is used.

**Someone else on the Lightning network.**
- Messages are signed with the sender's node key and name their recipient: a stranger cannot
  speak for a party, and a saver cannot pass a provider's message on to another saver.
- A payment carrying something that looks like a Menger message but cannot be read changes
  nothing and is never added to a position.
- The provider is an LSPS2 service only because LDK Node needs that to forward to private
  channels. Its just-in-time channels require a secret that nobody is given.
- A saver does not listen for connections; it calls its provider.

**A web page open in the same browser as the local app.**
The app is served on a loopback address only, answers only to that address by name (against DNS
rebinding), requires on every call a secret drawn at start-up, refuses to be framed, and inserts
everything that comes from the network as text. Anyone with access to the computer's user account
can open the app: the computer itself is trusted.

**A crash or a power cut at any moment.**
Requests, answers and outgoing payments are written down before they are sent; node events are
acknowledged only after their effect is saved; incoming payments are counted once by hash; on
restart, the program looks up what became of the payments it was waiting for (a payment the node
does not know is taken as never sent only two minutes after it was written down). The state file
is flushed to disk before it replaces the previous one, which is kept as a copy; reading the copy
is written in the journal, since its last operation may be missing.

**A computer that sleeps.** Delays run on the parties' clocks. A party whose ticks are more than
5 minutes apart restarts the delays it gives the others, rather than leaving at once on its
return.

**Price sources that fail or lie.**
A price needs at least two independent families of sources that agree (within 2 % for bitcoin,
1 % for gold). Sources that read the same token on several markets count once. Stale or missing
prices stop all payments and all judgments; they never move money. Each party reads its own
sources and can replace the list. Two honest parties whose prices differ by more than their
tolerances will disagree about who owes what; a persistent difference above 2 % ends in a close,
with each keeping what it holds.

## Known weaknesses

1. **Unaudited, and built on a development version of LDK Node** (0.8, as of 10 November 2026, on
   the third release candidate of LDK 0.3). Later fixes in LDK are not all included yet.
2. **Only the default delay of 144 blocks** to punish an old channel state: LDK Node does not yet
   let an application lengthen it.
3. **The provider sees and can censor its savers' payments**, like any routing node; it cannot
   redirect them.
4. **One spot source for gold.** The two other families are tokens that promise an ounce of gold
   and can lose that link; the three must agree, and two suffice.
5. **The provider's own risk is not managed by this program.** It is leveraged long bitcoin
   against gold; it should hold more bitcoin than it promises and may hedge elsewhere.
6. **Fees.** Channel opens, closes and forced closes cost on-chain fees that fall on whoever opened
   the channel; in a fee spike a small position may not be worth closing.
7. **Privacy.** A saver's position, payments and online hours are known to its provider.

## Reporting a problem

Please use GitHub's private vulnerability reporting on this repository if it is enabled
(Security → Report a vulnerability). Otherwise open an issue that says you have a security report,
without the details, and you will be contacted.
