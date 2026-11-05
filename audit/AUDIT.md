# Menger v0: notes for auditors

Prepared on 10 October 2026, after two independent reviews (section 3.6). This document describes what was changed in Liquity V1, why, what was tested, and what we already know to be weak. It is not an audit.

## 1. Summary

Menger v0 is a fork of Liquity V1 in which the unit of debt is the **Menger, 3.545 g of fine gold** (the weight of the Venetian ducat), instead of the US dollar. Collateral is ETH only. There is no LQTY token: borrowing fees and redemption fees go to Stability Pool depositors. Redemptions are suspended when the gold market is closed or the price sources do not confirm each other; liquidations never stop.

| | |
|---|---|
| Baseline | [liquity/dev](https://github.com/liquity/dev), commit `3e64ee1b52c50d51587c64c1cf75e0ba82934979` (25 November 2025), `packages/contracts/contracts` |
| Compiler | solc 0.6.11, optimizer 100 runs, EVM `istanbul` (as Liquity V1) |
| Tooling | Foundry 1.5.1, forge-std 1.9.7 |
| Admin keys | None. Every contract renounces ownership in `setAddresses`; the price feed has no owner. |
| Upgradability | None |

**The baseline is not the code deployed on mainnet in April 2021.** The `main` branch of the dev repository contains later changes to `StabilityPool` (no epochs, scale-only accounting, `MIN_LUSD_IN_SP = 1e18` kept in the pool at all times, revised rounding). The 2021 audits of Liquity V1 do not cover those changes, so the Stability Pool should be reviewed in full, not only the lines marked `MENGER`.

## 2. Scope

| File | Status | Lines |
|---|---|---|
| `src/MengerPriceFeed.sol` | New, replaces `PriceFeed.sol` (Chainlink + Tellor) | 420 |
| `src/StabilityPoolFeeIssuance.sol` | New, replaces `LQTY/CommunityIssuance.sol` | 136 |
| `src/StabilityPool.sol` | Modified: redemption fees released over time, capped ETH payouts, front-end stake fix | +104 / -6 |
| `src/TroveManager.sol` | Modified: LQTY removed, redemption gating, fee routing, bootstrap start | +23 / -23 |
| `src/BorrowerOperations.sol` | Modified: borrowing fee minted to the fee contract | +10 / -12 |
| `src/LUSDToken.sol` | Modified: name and symbol only | +4 / -2 |
| `src/Dependencies/LiquityBase.sol` | Modified: gas compensation and minimum debt | +7 / -5 |
| `src/Interfaces/` | `IStabilityPool`, `ITroveManager`, `IBorrowerOperations`, `IPriceFeed` adjusted | +20 / -12 |
| `ActivePool`, `DefaultPool`, `CollSurplusPool`, `GasPool`, `SortedTroves`, `HintHelpers`, `MultiTroveGetter`, all other dependencies | Unchanged (byte-identical) | |
| `LQTY/*`, `LPRewards/*`, `Proxy/*`, `Integrations/*`, `TestContracts/*`, `PriceFeed.sol`, Tellor files, `Migrations.sol` | Removed | |

Every change in existing files is marked with a `MENGER:` comment. The complete diff of modified and new contracts is in `audit/liquity-v1-to-menger.diff`. Internal identifiers (`LUSD`, `LQTY`, `G`, `communityIssuance`) are deliberately unchanged so that the code can be compared line by line with Liquity V1: in this codebase "LUSD" means Menger, and "LQTY gains" means borrowing-fee gains paid in Mengers.

Out of scope, provided for context: `test/`, `script/` (including `script/TestnetAggregator.sol`, an owner-controlled feed for test networks only), `sim/`, and `app/`, a static demo interface that sends ordinary transactions from the visitor's wallet.

## 3. Changes and rationale

### 3.1 Unit of account

The system's price is the price of 1 ETH in Mengers:

```
ETH/Menger = ETH/USD x 31.1034768 / (XAU/USD x 3.545)
```

computed in `MengerPriceFeed._computeEthMengerPrice` with integer gram weights (`TROY_OUNCE_WEIGHT = 311034768`, `MENGER_WEIGHT = 35450000`, in units of 1e-7 g). All core arithmetic is unchanged: it operates in "debt units", which are now Mengers.

`LUSD_GAS_COMPENSATION` is 0.4 Menger and `MIN_NET_DEBT` 4 Mengers (Liquity V1: 200 and 1,800 LUSD), about 200 and 2,000 USD at 4,319 USD per ounce. MCR (110 %), CCR (150 %), fee floors (0.5 %), maximum borrowing fee (5 %), BETA, the base rate decay and the 14-day redemption bootstrap are unchanged.

### 3.2 Oracle: `MengerPriceFeed`

Four Chainlink-compatible feeds, fixed at deployment: ETH/USD primary and secondary, XAU/USD primary and secondary. Every feed must support `getRoundData`. No owner, no setter. The only state besides the last good prices is one flag per pair: whether its primary is distrusted.

**Reading a feed.** Each read is a low-level `staticcall` with a 500,000 gas cap that copies at most the 160 bytes expected. A revert, short or malformed data, an address without code, a gas-burning feed or a huge return buffer therefore count as a failed answer and cannot make `fetchPrice()` revert or run out of gas. All words are read as full 256-bit values. An answer is valid if `roundId != 0`, `0 < updatedAt <= now`, `answer > 0`, and the answer scaled to 18 decimals is in `(0, MAX_PRICE = 1e36]`, which also rules out overflow later. Feed decimals above 36 are refused at deployment.

**Live, jumped, usable.** A valid answer is live if recent enough (`ETH_TIMEOUT = 4 hours`; `GOLD_TIMEOUT = 4 days`, longer than any market closure). It has jumped if it moved by more than 50 % since the feed's previous round (`getRoundData(roundId - 1)`), Liquity V1's test for a broken Chainlink feed. If the previous round is unavailable (a phase change), the move cannot be checked and the feed is not used for that round. A usable feed is live, not jumped, and checked.

**Trust (Liquity V1's rule).** A primary that jumped becomes distrusted, and stays so until it is live and agrees with a usable secondary again (5 % for ETH, 1 % for gold). The flag is written by `fetchPrice()`; the views compute what `fetchPrice()` would do now.

**Price selection (`fetchPrice()`, never reverts).** For each pair: the primary if usable and trusted; otherwise the secondary if usable; otherwise the last good price. For gold only, the secondary is also followed when the primary is lagging: both usable, more than 1 % apart, only the secondary updated within `GOLD_MAX_AGE`, and the gold market open (on weekends a quiet primary is normal, and the secondary alone must not move the price). If the resulting ETH/Menger price is 0, which only absurd in-range answers can produce, the last good price is returned.

**Redemption gate (`isRedemptionAllowed()`, view).** False:

- from Friday 21:00 UTC to Monday 00:00 UTC (`isGoldMarketClosedAt`; the gold market closes Friday 17:00 and reopens Sunday 18:00 New York time, so the window covers both summer and winter time with at least one hour of margin after the reopening);
- unless both gold feeds are usable, the primary trusted, the two within 1 %, and both updated within `GOLD_MAX_AGE` (immutable, at most `GOLD_TIMEOUT`);
- unless the ETH price comes from a feed and the other one does not contradict it: with the primary in use, the secondary is unusable or within 5 %; with the secondary in use, the primary is silent (stale or failed) or within 5 %.

The primary is authoritative, as Chainlink was in Liquity V1: a disagreement without a jump suspends redemptions but does not change the price. At deployment, all four feeds must be usable, and each pair must agree.

### 3.3 Redemption gating

`TroveManager.redeemCollateral` calls `_requireRedemptionsAllowedByPriceFeed()` right after `fetchPrice()`. Reason: while the gold market is closed, the oracle keeps Friday's gold price; a redemption at that stale price would let anyone exchange Mengers for ETH at a rate that may be wrong by the weekend's move. Liquidations are not gated: they keep using the last gold price and the live ETH price, because ETH can crash on a Sunday.

### 3.4 No LQTY: where fees go

- **Bootstrap.** The 14-day redemption bootstrap starts at `TroveManager.setAddresses` (`systemDeploymentTime`) instead of the LQTY token deployment.
- **Borrowing fee (Mengers).** `BorrowerOperations._triggerBorrowingFee` mints the fee to `borrowingFeeRecipient`, the `StabilityPoolFeeIssuance` contract. That contract implements Liquity's `ICommunityIssuance`: the Stability Pool calls `issueLQTY()` at every deposit, withdrawal and liquidation, and distributes what is returned through its sum `G`, as it distributed LQTY. Depositors claim through `sendLQTY`.
- **Redemption fee (ETH).** `TroveManager` sends the fee from the ActivePool to the Stability Pool and calls `StabilityPool.distributeRedemptionFee`. If the pool holds no deposits, no fee is charged.
- **Gradual release (`FEE_RELEASE_PERIOD = 7 days`, both fees).** At each pool operation, `pending x min(elapsed, 7 days) / 7 days` is released: borrowing fees into `G`, redemption fees into `S`. This is a geometric release with a mean life of about 7 days: with daily pool activity, about 66 % of a fee is paid after one week and 96 % after three weeks; after 7 days without any operation, everything left is paid at once. The clock restarts at every release, including when rounding makes it zero, so a new fee never inherits time elapsed before it arrived. While the pool is empty, nothing is released and the fees wait. Borrowing fees received between two pool operations start their release at the next operation. Reason: paid at once, a fee could be captured by a deposit made just before it and withdrawn just after (about 90 % of the fee in our test where the depositor brought ten times the pool). It does not stop a large depositor who stays for weeks from recapturing a share of their own fees equal to their share of the pool (section 6).
- **Front ends.** Liquity's front-end kickback is kept: a registered front end receives its share of the `G` gains (borrowing fees) of the deposits tagged with it. It takes no share of ETH gains.
- `withdrawETHGainToTrove` now checks the gain after `_triggerLQTYIssuance`, since the trigger may release redemption fees into the caller's ETH gain, and checks the gain that can actually be paid (after the cap of section 3.5).

### 3.5 Payout caps

Fuzzing found a liveness bug in the first version of the fee routing (see `test/MengerRegression.t.sol`). After a liquidation leaves a rounding error in `P` and a second liquidation drains the pool to its 1-Menger minimum, the sum of the true compounded deposits can exceed `totalLUSDDeposits` by a few wei. Liquidation gains are roughly proportional to the debt absorbed, so the error stays negligible; but a fee shared over a 1-Menger pool multiplies it by the size of the fee. The depositors' gains then exceeded the pool's assets by a few hundred wei, and the last depositor's withdrawal reverted (`SafeMath: subtraction overflow`), locking their deposit.

Fix: ETH gains are paid through `_cappedETHGain` (at most the pool's ETH minus pending redemption fees), and `sendLQTY` pays at most the released fees not yet paid out. The last claimant receives the dust less. Both regression tests reproduce the counterexample found by the fuzzer, assert that the excess exists, and pass only with the caps. The review estimated the possible excess at about (largest past pool size / 1e18) wei of deposit, scaled by fee / pool size; the largest excess observed was 572 wei. The invariant suite bounds it at 1e9 wei.

### 3.6 Independent review

A separate Claude instance, which had not seen the code being written, reviewed the modifications and proved its findings with tests or on a local chain. First review, findings and resolution:

| | Finding | Resolution |
|---|---|---|
| R-1 (high) | With the median rule, an ETH feed stuck at an old price but still timestamped froze the system's price: no liquidations during a 30 % fall, and borrowing beyond the collateral's market value | Primary authoritative (section 3.2). `test_ethSecondaryStuck_doesNotFreezePrice` |
| R-2 (medium) | A gold feed that stopped updating but kept answering froze the gold price for good once gold moved more than 1 % | `GOLD_TIMEOUT` and the lagging-primary rule. `test_deadGoldFeed_doesNotFreezeGoldPrice`, `test_goldPrimaryLagging_secondaryTakesOver` |
| R-3 (medium) | A large depositor recaptures most of their own redemption or borrowing fee by staying about three weeks | Not changed: a property of paying fees to depositors pro rata. Section 6, item 3 |
| R-4 (low) | An answer above about 1.16e49 (8 decimals) overflowed and made `fetchPrice()` revert, blocking liquidations | Range check and low-level calls. `test_extremeAnswers_doNotBlockLiquidations`, `test_malformedAnswer_treatedAsBroken`, invariant 7 |
| R-5 (low) | The release is geometric, not "over 7 days" as documented | Documentation corrected. `test_releaseWithDailyActivity_twoThirdsAfterOneWeek` |
| R-6 (info) | `withdrawETHGainToTrove` reverted inside BorrowerOperations when the capped gain was 0 | Checks the payable gain. `test_ethGainToTrove_whenNothingLeftToPay` |
| R-7 (low, inherited) | Unchanged Liquity code: the last depositor tagged with a front end could not withdraw everything (1 wei short; 26 of 200 random cases) | Saturating subtraction. `MengerFrontEndTest` (fuzzed) |
| R-8 (low, tooling) | Forge estimates gas with istanbul's rules; on current networks some script transactions ran out of gas | `--gas-estimate-multiplier 200` in the deployment guide (raising `evm_version` broke constructor decoding in `forge script`). Rehearsed on anvil with the guide's commands, after both reviews: 51 transactions, none failed, the tightest using 70 % of its gas limit. |

A second review, by the same instance, checked those fixes and found four new problems in the rewritten oracle:

| | Finding | Resolution |
|---|---|---|
| N-1 (high) | The 50 % jump check lasted one round: a broken primary repeating its wrong value was trusted from its second round (a 200 % trove liquidated at 2.5 % of its value; 700 Mengers borrowed against 81 Mengers of ETH) | Persistent distrust until the primary agrees with the secondary again, as in Liquity V1. `test_primaryThatJumped_staysDistrustedUntilItAgreesAgain`, `test_primaryStuckTooHigh_cannotBeBorrowedAgainst`, invariant 8 |
| N-2 (medium) | On a weekend, the lagging-primary rule let the gold secondary alone move the gold price | The rule applies only while the gold market is open. `test_weekend_goldSecondaryAloneCannotMoveThePrice`, invariant 9 |
| N-3 (low) | An ETH secondary without round history was accepted, then silently never used | All four feeds must be usable at deployment. `test_constructor_rejectsFeedWithoutRoundHistory` |
| N-5 (info) | A feed returning a huge buffer made `fetchPrice()` run out of gas | Capped low-level call copying 160 bytes. `test_hostileFeeds_cannotBlockFetchPrice` |

Its phase-change test and its fuzz test of everything a feed can answer are part of the suite (`test/MengerOracle.t.sol`). The reviewer's numbering has no N-4. It also measured the gas cost of the new oracle (section 6, item 10).

The reviews also confirmed as sound: the diff matches the code and the unchanged files are byte-identical; fee attribution and accounting; the caps cannot be used to extract value; fee release across a scale change of `P` (`test/MengerScale.t.sol`, written by the review); the weekend arithmetic; the TroveManager changes; the deployment wiring.

## 4. Tests

71 Foundry tests, all passing (`forge test`):

| Suite | What it covers |
|---|---|
| `MengerPriceFeedTest` (15), `MengerOracleFailureTest` (7), `MengerOracleFuzzTest` (1) | Conversion exactness (fuzzed), the weekend window to the second, ETH fallback, disagreement, stuck secondary, jumps (genuine and wrong) and persistent distrust, gold disagreement, lagging and dead gold feeds, the weekend rule, phase changes, broken, extreme, malformed and hostile feeds, constructor checks; every combination of feed answers (fuzzed): `fetchPrice()` never reverts |
| `MengerBorrowingTest` (5), `MengerRedemptionTest` (6), `MengerLiquidationTest` (2) | Debt composition, minimums, borrowing fee routing, Recovery Mode, redemption at the gold value, fee rate, bootstrap, weekend and stale or disputed gold, weekend liquidation, a 40 % ETH crash |
| `MengerFeesTest` (8) | Pro rata split of both fees, fees after a liquidation, front-end share, access control, no admin keys, ETH conservation in redemptions (fuzzed) |
| `MengerFeeReleaseTest` (8), `MengerFeeReleaseEmptyPoolTest` (1) | Just-in-time deposits capture no fee, release proportional to time and its pace with daily activity, no inherited elapsed time, release before a liquidation changes deposits, gains moved to a trove, empty-pool waiting |
| `MengerRegressionTest` (3), `MengerFrontEndTest` (2), `MengerScaleTest` (1) | The rounding counterexample for ETH and fee gains, the capped-gain check, front-end withdrawals (fuzzed), a scale change of `P` |
| `MengerInvariantTest` (9), `MengerHandlerSanityTest` (1) | Invariants below, under random sequences of 17 actions: open and close troves, deposit (directly or through a front end), withdraw, claim, move ETH gains to a trove, move the ETH price by up to 20 % and the gold price by up to 3 %, drift the secondary ETH feed by up to 10 % and the secondary gold feed by up to 3 %, freeze or unfreeze the primary ETH feed, break or repair it (10 % or 1,000 % of the market), liquidate, redeem, let up to 8 days pass. The price is read after every published ETH round, as an active system or a keeper would. |
| `HistoricalCrashesSim` (2) | The 12 worst historical crashes replayed with the contracts (section 5) |

Invariants, checked after every call. Last campaign: each invariant over 1,000 random sequences of 200 calls (200,000 calls per invariant), no failure. Fuzz tests: 5,000 runs each.

1. Menger supply equals the system's total debt.
2. The ETH trackers of ActivePool, DefaultPool, StabilityPool and CollSurplusPool equal their balances.
3. The Stability Pool's Menger balance equals `totalLUSDDeposits`.
4. The Stability Pool holds at least its pending redemption fees, and depositors' ETH gains do not exceed the rest by more than 1e9 wei.
5. The fee contract holds at least its pending and released fees, and the fee gains of depositors and front end do not exceed the released fees by more than 1e9 wei.
6. Liveness: on a copy of the state, every depositor can claim their gains, and, when no trove is below MCR, every depositor can withdraw down to the 1 Menger the pool keeps.
7. `fetchPrice()` never reverts (and, in these runs, never returns 0).
8. While the primary ETH feed answers a price more than 50 % away from the market, the system's ETH price stays within 11 % of the market (the secondary drifts by at most 10 %).
9. On a weekend, the system's gold price is the primary's.

## 5. Historical stress test

`sim/history.py` builds a daily series of ETH priced in gold since 2016 (ETH: Coin Metrics; gold: the PAXG and XAUT tokens from 2020, World Bank monthly averages interpolated before) and selects the 12 largest falls within 14 days, all between 2016 and 2022, from -37 % to -53 %. `test/sim/HistoricalCrashes.t.sol` replays each one day by day with the contracts: 40 troves of equal debt with ratios from 115 % to 400 % (average 200 %), a Stability Pool holding 50 % or 15 % of all Mengers, passive borrowers, gold frozen on weekends, and a bot that liquidates once a day.

The system stayed solvent (TCR above 100 %) in all 24 runs. Lowest TCR: 111 % with the 50 % pool, 106 % with the 15 % pool. With daily steps, 68 of 276 liquidations (50 % pool) happened below 100 %, which real-time bots would avoid; even so, liquidations were profitable for the pool at the day's price in all 12 crashes with the 50 % pool, and in 9 of 12 with the 15 % pool (worst -3.3 %). With the small pool, redistribution moved up to 83 % of the initial debt onto the surviving troves. Full tables: `sim/out/crash_replay.md`, `sim/out/history_stats.md`.

Weekend gold moves (Friday close to Monday close, 2020-2026, an upper bound of the weekend gap): median 0.56 %, 99th percentile 3.4 %, maximum 7.6 % (March 2020).

## 6. Known issues and open questions

1. **Stability Pool baseline.** `MIN_LUSD_IN_SP = 1e18` is 1 Menger, about 490 USD, which the last depositor can never withdraw. While total deposits are below 1 Menger, no non-zero withdrawal is possible at all. Should the minimum be scaled down for a gold-denominated unit, and does that affect the precision analysis behind it?
2. **Trust in the primary feeds.** A primary that is wrong but plausible (less than 50 % away from its previous round), or stuck while still timestamped, is followed; the secondary only suspends redemptions. A primary that stops updating altogether is followed for up to `ETH_TIMEOUT` (4 hours) whatever the market does meanwhile, even a 50 % fall (a sequence found by the invariant fuzzer in November 2026); redemptions stay suspended while the secondary disagrees. A wrong-but-fresh secondary can no longer freeze the price, but a wrong-but-fresh primary moves it. As in Liquity V1, a jump is detected only if `fetchPrice()` runs during the round that contains it: a primary that jumps and repeats the value before any reading is trusted. A keeper calling `fetchPrice()` after each feed update closes this gap. After a detected jump, if the secondary is down, the price stays at the last good value until both feeds agree again. Which sources (operators, aggregation) should be primary?
3. **Self-recapture of fees (R-3).** Fees go to whoever holds the pool over time. A redeemer or borrower who is most of the pool recovers most of their own fee after a few weeks, which weakens the deterrent of the 0.5 % redemption floor and of the base rate for large actors. Liquity V2 pays redemption fees to the redeemed borrowers instead. Keep, or change before mainnet?
4. **Holidays.** Chainlink-style feeds republish on a heartbeat even when the market is closed. On a weekday holiday the gold feeds look fresh, so redemptions are allowed at a frozen price. The 0.5 % redemption fee floor limits the gain, less so for a large depositor (item 3). `GOLD_MAX_AGE` (25 hours in the deployment script) must be chosen against the heartbeat of the feeds actually used. The review also noted, unproven, that redemptions could reopen at Monday 00:00 UTC on a heartbeat-refreshed Friday price if gold gapped at the Sunday reopening.
5. **Round-the-clock gold feeds.** Some providers' 24/7 gold feeds are derived from tokenized gold (PAXG, XAUT) outside market hours. With such feeds the weekend price is a token price, not a market price, and the redemption window and freshness rules should be revisited.
6. **Two gold sources, no tie-breaker.** When both gold feeds are usable and disagree, the price follows the primary unless it is lagging on a market day; redemptions stay suspended until they agree. With one feed failed or dead, the other is used alone. On a weekday holiday the lagging rule can still apply (the window does not know holidays): a quiet primary and a drifting secondary then move the gold price.
7. **Fee release edge cases.** Pending amounts too small to produce one wei per release are not released while pool operations are frequent. Fees wait while the pool is empty. A fee can be released to a pool that has shrunk to its 1-Menger minimum, which concentrates it on few depositors (this was the trigger of the rounding bug). Redemptions are free while the pool is empty (they are blocked anyway during the 14-day bootstrap); afterwards, the first deposit, however small, restores the fee and receives all of it.
8. **Stability Pool yield without LQTY.** Depositors earn liquidation gains and fees only. A smaller pool means more redistribution to borrowers in crashes (section 5).
9. **Front ends.** The kickback is retained; rounding can leave a front end a 1-wei stake after all its depositors left (harmless). Its use needs legal review.
10. **Gas.** Every price read makes eight feed calls (latest and previous round of four feeds). In the tests (istanbul's rules, mock feeds), `fetchPrice()` cost 86,000 to 127,000 gas; the cost with real feeds on a current chain remains to be measured. Redemptions read the feeds twice (`fetchPrice` and `isRedemptionAllowed`). Scripts must be broadcast with `--gas-estimate-multiplier 200` (R-8). `TroveManager` is 23,215 bytes, 1,361 bytes under the EIP-170 limit.
11. **Name**: Menger, symbol MNGR (the project was called Ducat, DCT, until November 2026). No trademark search has been made. The Scala cryptocurrency gave the name Menger to one of its releases in 2021, and a little-known token, Mongraal, uses the symbol MNGR.
12. **License**: the upstream contracts carry MIT SPDX headers, but the upstream repository's LICENSE file and README declare GPL-3.0. This fork is distributed under GPL-3.0.

## 7. Reproducing

```bash
forge test                                                   # all tests
FOUNDRY_INVARIANT_RUNS=1000 FOUNDRY_INVARIANT_DEPTH=200 forge test --match-contract MengerInvariantTest
FOUNDRY_FUZZ_RUNS=5000 forge test --match-test testFuzz
python3 sim/history.py && python3 sim/report.py              # historical stress test
forge build --sizes                                          # bytecode sizes
```
