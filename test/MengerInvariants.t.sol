// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

// Random sequences of user actions, price moves and time. After every call, the accounting invariants must hold.
contract MengerHandler is Test {
    BorrowerOperations borrowerOperations;
    TroveManager troveManager;
    StabilityPool stabilityPool;
    LUSDToken menger;
    MengerPriceFeed priceFeed;
    HintHelpers hintHelpers;
    SortedTroves sortedTroves;
    MockAggregator ethPrimary;
    MockAggregator ethSecondary;
    MockAggregator xauPrimary;
    MockAggregator xauSecondary;

    address[] public actors;
    address public constant FRONT_END = address(0xF407);

    // Market prices, and how the feeds deviate from them
    uint public ethPriceCents = 400000;
    uint public goldPriceCents = 431900;
    int public ethSecondaryOffsetBp;   // the secondary ETH feed is off by this many basis points
    bool public ethPrimaryFrozen;      // the primary ETH feed has stopped updating
    uint public ethPrimaryBrokenPct;   // when not 0, the primary ETH feed answers this % of the market price
    int public goldSecondaryOffsetBp;  // the secondary gold feed is off by this many basis points

    // Ghost counters, to check that the fuzzer really exercised each path
    uint public troveOpenings;
    uint public liquidationCalls;
    uint public redemptions;
    uint public deposits;
    uint public taggedDeposits;
    uint public claims;
    uint public gainsToTrove;
    uint public feedIncidents;

    constructor(
        BorrowerOperations _bo, TroveManager _tm, StabilityPool _sp, LUSDToken _menger, MengerPriceFeed _pf,
        HintHelpers _hh, SortedTroves _st, MockAggregator[4] memory _feeds, address[] memory _actors
    ) public {
        borrowerOperations = _bo;
        troveManager = _tm;
        stabilityPool = _sp;
        menger = _menger;
        priceFeed = _pf;
        hintHelpers = _hh;
        sortedTroves = _st;
        ethPrimary = _feeds[0];
        ethSecondary = _feeds[1];
        xauPrimary = _feeds[2];
        xauSecondary = _feeds[3];
        actors = _actors;
    }

    function actorsCount() external view returns (uint) {
        return actors.length;
    }

    function _actor(uint _seed) internal view returns (address) {
        return actors[_seed % actors.length];
    }

    function openTrove(uint _actorSeed, uint _collEth, uint _debt) external {
        address actor = _actor(_actorSeed);
        if (troveManager.getTroveStatus(actor) == 1) { return; }
        _collEth = bound(_collEth, 1 ether, 100 ether);
        uint price = priceFeed.fetchPrice();
        uint maxDebt = _collEth * price / 1e18 * 100 / 170; // aim for a ratio of about 170%
        if (maxDebt < 5e18) { return; }
        _debt = bound(_debt, 4.1e18, maxDebt);
        vm.deal(actor, actor.balance + _collEth);
        vm.prank(actor);
        try borrowerOperations.openTrove{value: _collEth}(1e18, _debt, address(0), address(0)) {
            troveOpenings++;
        } catch {}
    }

    function closeTrove(uint _actorSeed) external {
        address actor = _actor(_actorSeed);
        vm.prank(actor);
        try borrowerOperations.closeTrove() {} catch {}
    }

    function provideToSP(uint _actorSeed, uint _amount) external {
        address actor = _actor(_actorSeed);
        uint balance = menger.balanceOf(actor);
        if (balance == 0) { return; }
        _amount = bound(_amount, 1, balance);
        vm.prank(actor);
        try stabilityPool.provideToSP(_amount, address(0)) {
            deposits++;
        } catch {}
    }

    // A deposit through the registered front end (the tag only applies to a new deposit)
    function provideToSPWithFrontEnd(uint _actorSeed, uint _amount) external {
        address actor = _actor(_actorSeed);
        uint balance = menger.balanceOf(actor);
        if (balance == 0) { return; }
        _amount = bound(_amount, 1, balance);
        vm.prank(actor);
        try stabilityPool.provideToSP(_amount, FRONT_END) {
            taggedDeposits++;
        } catch {}
    }

    function withdrawFromSP(uint _actorSeed, uint _amount) external {
        address actor = _actor(_actorSeed);
        _amount = bound(_amount, 0, 1000e18);
        vm.prank(actor);
        try stabilityPool.withdrawFromSP(_amount) {} catch {}
    }

    function claimGains(uint _actorSeed) external {
        address actor = _actor(_actorSeed);
        vm.prank(actor);
        try stabilityPool.withdrawFromSP(0) {
            claims++;
        } catch {}
    }

    function withdrawETHGainToTrove(uint _actorSeed) external {
        address actor = _actor(_actorSeed);
        vm.prank(actor);
        try stabilityPool.withdrawETHGainToTrove(address(0), address(0)) {
            gainsToTrove++;
        } catch {}
    }

    function moveEthPrice(uint _seed) external {
        int change = int(_seed % 41) - 20; // from -20% to +20%
        uint newPrice = uint(int(ethPriceCents) * (100 + change) / 100);
        if (newPrice < 50000) { newPrice = 50000; }       // 500 USD
        if (newPrice > 2000000) { newPrice = 2000000; }   // 20,000 USD
        ethPriceCents = newPrice;
        _publishEth();
    }

    function moveGoldPrice(uint _seed) external {
        int change = int(_seed % 61) - 30; // from -3% to +3%
        goldPriceCents = uint(int(goldPriceCents) * (1000 + change) / 1000);
        _publishGold();
    }

    // The secondary gold feed drifts up to 3 % away from the market, or back to it
    function desyncGoldSecondary(uint _seed) external {
        goldSecondaryOffsetBp = int(_seed % 601) - 300;
        feedIncidents++;
        _publishGold();
    }

    function _publishGold() internal {
        xauPrimary.setAnswer(int(goldPriceCents * 1e6), block.timestamp);
        uint secondary = uint(int(goldPriceCents) * (10000 + goldSecondaryOffsetBp) / 10000);
        xauSecondary.setAnswer(int(secondary * 1e6), block.timestamp);
    }

    // The primary ETH feed breaks: it answers 10 % or 1,000 % of the market price, and keeps doing so
    function breakEthPrimary(uint _seed) external {
        ethPrimaryBrokenPct = _seed % 2 == 0 ? 10 : 1000;
        feedIncidents++;
        _publishEth();
    }

    function repairEthPrimary() external {
        ethPrimaryBrokenPct = 0;
        _publishEth();
    }

    // The secondary ETH feed drifts up to 10 % away from the market, or back to it
    function desyncEthSecondary(uint _seed) external {
        ethSecondaryOffsetBp = int(_seed % 2001) - 1000;
        feedIncidents++;
        _publishEth();
    }

    // The primary ETH feed stops updating, or resumes
    function toggleEthPrimaryFrozen() external {
        ethPrimaryFrozen = !ethPrimaryFrozen;
        feedIncidents++;
        _publishEth();
    }

    function _publishEth() internal {
        if (!ethPrimaryFrozen) {
            uint primary = ethPrimaryBrokenPct == 0 ? ethPriceCents : ethPriceCents * ethPrimaryBrokenPct / 100;
            ethPrimary.setAnswer(int(primary * 1e6), block.timestamp);
        }
        uint secondary = uint(int(ethPriceCents) * (10000 + ethSecondaryOffsetBp) / 10000);
        ethSecondary.setAnswer(int(secondary * 1e6), block.timestamp);
        // The price is read after every new round, as by an active system or a keeper. A jump that happens between
        // two readings and is then repeated goes unnoticed: a limit shared with Liquity V1.
        priceFeed.fetchPrice();
    }

    function liquidateTroves() external {
        liquidationCalls++;
        try troveManager.liquidateTroves(5) {} catch {}
    }

    function redeem(uint _actorSeed, uint _amount) external {
        address actor = _actor(_actorSeed);
        uint balance = menger.balanceOf(actor);
        if (balance < 1e18) { return; }
        _amount = bound(_amount, 1e18, balance);
        uint price = priceFeed.fetchPrice();
        (address firstHint, uint partialNICR, uint truncated) = hintHelpers.getRedemptionHints(_amount, price, 0);
        if (truncated == 0) { return; }
        (address upperHint, address lowerHint) = sortedTroves.findInsertPosition(partialNICR, address(0), address(0));
        vm.prank(actor);
        try troveManager.redeemCollateral(truncated, firstHint, upperHint, lowerHint, partialNICR, 0, 1e18) {
            redemptions++;
        } catch {}
    }

    function passTime(uint _seconds) external {
        _seconds = bound(_seconds, 1 minutes, 8 days);
        vm.warp(block.timestamp + _seconds);
        if (!ethPrimaryFrozen) { ethPrimary.setUpdatedAt(block.timestamp); }
        ethSecondary.setUpdatedAt(block.timestamp);
        xauPrimary.setUpdatedAt(block.timestamp);
        xauSecondary.setUpdatedAt(block.timestamp);
    }

    function sumDepositorETHGains() external view returns (uint total) {
        for (uint i = 0; i < actors.length; i++) {
            total += stabilityPool.getDepositorETHGain(actors[i]);
        }
    }

    // Fee gains of all depositors, plus the front end's share
    function sumDepositorFeeGains() external view returns (uint total) {
        for (uint i = 0; i < actors.length; i++) {
            total += stabilityPool.getDepositorLQTYGain(actors[i]);
        }
        total += stabilityPool.getFrontEndLQTYGain(FRONT_END);
    }

    receive() external payable {}
}

// The starting state of the random runs, also used to replay sequences found by the fuzzer
contract MengerInvariantBase is MengerTestBase {
    MengerHandler handler;

    function setUp() public virtual override {
        super.setUp();

        // Start with a few troves and deposits, after the bootstrap period, on a weekday
        _openTrove(alice, 20 ether, 60e18);
        _openTrove(bob, 30 ether, 80e18);
        _openTrove(carol, 15 ether, 40e18);
        _deposit(bob, 50e18);
        _warpAndRefresh(AFTER_BOOTSTRAP);

        address[] memory actors = new address[](5);
        actors[0] = alice;
        actors[1] = bob;
        actors[2] = carol;
        actors[3] = dave;
        actors[4] = erin;
        MockAggregator[4] memory feeds = [ethPrimary, ethSecondary, xauPrimary, xauSecondary];

        handler = new MengerHandler(
            borrowerOperations, troveManager, stabilityPool, menger, priceFeed, hintHelpers, sortedTroves, feeds, actors
        );
        vm.prank(handler.FRONT_END());
        stabilityPool.registerFrontEnd(0.8e18);
    }
}

contract MengerInvariantTest is MengerInvariantBase {

    // Rounding in the pool's sums S, G and P can make the gains add up to slightly more than what the pool holds
    // for them; payouts are capped, so the last claimant receives that much less. 1e9 wei = 0.000000001 ETH or Menger.
    uint constant DUST = 1e9;

    function setUp() public override {
        super.setUp();
        targetContract(address(handler));
    }

    function invariant_mengerSupplyEqualsSystemDebt() public {
        assertEq(menger.totalSupply(), troveManager.getEntireSystemDebt());
    }

    function invariant_ethTrackersMatchBalances() public {
        assertEq(address(activePool).balance, activePool.getETH());
        assertEq(address(defaultPool).balance, defaultPool.getETH());
        assertEq(address(stabilityPool).balance, stabilityPool.getETH());
        assertEq(address(collSurplusPool).balance, collSurplusPool.getETH());
    }

    function invariant_poolMengersMatchDeposits() public {
        assertEq(menger.balanceOf(address(stabilityPool)), stabilityPool.getTotalLUSDDeposits());
    }

    // Pending redemption fees are always fully backed; depositors' ETH gains never exceed the rest by more than dust
    function invariant_poolEthCoversPendingFeesAndGains() public {
        uint poolEth = stabilityPool.getETH();
        uint pending = stabilityPool.pendingRedemptionFeeETH();
        assertGe(poolEth, pending);
        assertLe(handler.sumDepositorETHGains(), poolEth - pending + DUST);
    }

    // The fee contract always holds the pending and the released fees; released fees cover the gains (up to dust)
    function invariant_feeIssuanceCoversPendingFeesAndGains() public {
        uint balance = menger.balanceOf(address(feeIssuance));
        uint released = feeIssuance.totalIssuedNotPaid();
        assertGe(balance, released + feeIssuance.pendingFees());
        assertLe(handler.sumDepositorFeeGains(), released + DUST);
    }

    // Liveness: at any moment every depositor can claim their gains, and, when no trove is below the minimum
    // ratio, everyone can withdraw, down to the 1 Menger the pool always keeps. Checked on a copy of the state.
    function invariant_depositorsCanAlwaysWithdraw() public {
        uint snapshot = vm.snapshotState();
        address[5] memory all = [alice, bob, carol, dave, erin];

        for (uint i = 0; i < all.length; i++) {
            if (stabilityPool.getCompoundedLUSDDeposit(all[i]) == 0) { continue; }
            vm.prank(all[i]);
            stabilityPool.withdrawFromSP(0); // must not revert
        }

        uint price = priceFeed.fetchPrice();
        address lowest = sortedTroves.getLast();
        if (lowest == address(0) || troveManager.getCurrentICR(lowest, price) >= 1.1e18) {
            for (uint i = 0; i < all.length; i++) {
                uint deposit = stabilityPool.getCompoundedLUSDDeposit(all[i]);
                uint total = stabilityPool.getTotalLUSDDeposits();
                uint room = total > 1e18 ? total - 1e18 : 0;
                uint amount = deposit < room ? deposit : room;
                if (amount == 0) { continue; }
                vm.prank(all[i]);
                stabilityPool.withdrawFromSP(amount); // must not revert
            }
        }

        vm.revertToState(snapshot);
    }

    // Whatever the feeds do, the price can always be read: liquidations are never blocked by the oracle
    function invariant_priceCanAlwaysBeRead() public {
        uint snapshot = vm.snapshotState();
        uint price = priceFeed.fetchPrice();
        assertGt(price, 0);
        vm.revertToState(snapshot);
    }

    // A broken primary ETH feed is never followed: while it answers a price more than 50 % away from the market,
    // the system's price stays within the secondary's drift (10 %) of the market. (A primary that merely stopped
    // updating is followed until it counts as frozen, after 4 hours: the primary is authoritative.)
    function invariant_brokenEthPrimaryNotFollowed() public {
        uint market = handler.ethPriceCents() * 1e16;
        uint primary = uint(ethPrimary.answer()) * 1e10;
        if (primary * 2 > market && primary < market * 2) { return; }
        (uint ethUsd, ) = priceFeed.getEthUsdPrice();
        assertGe(ethUsd, market * 89 / 100);
        assertLe(ethUsd, market * 111 / 100);
    }

    // On a weekend the gold price is the primary's, whatever the secondary says
    function invariant_weekendGoldFollowsPrimary() public {
        if (!priceFeed.isGoldMarketClosedAt(block.timestamp)) { return; }
        (uint xauUsd, , ) = priceFeed.getXauUsdPrice();
        assertEq(xauUsd, uint(xauPrimary.answer()) * 1e10);
    }

    // Logs how many actions really succeeded during the random runs (visible with forge test -vv)
    function afterInvariant() external {
        emit log_named_uint("troves opened", handler.troveOpenings());
        emit log_named_uint("SP deposits", handler.deposits());
        emit log_named_uint("redemptions", handler.redemptions());
        emit log_named_uint("liquidation calls", handler.liquidationCalls());
        emit log_named_uint("claims", handler.claims());
        emit log_named_uint("ETH gains moved to troves", handler.gainsToTrove());
        emit log_named_uint("deposits through the front end", handler.taggedDeposits());
        emit log_named_uint("feed incidents", handler.feedIncidents());
    }
}

// Checks that the handler's actions really execute (they swallow reverts during random runs)
contract MengerHandlerSanityTest is MengerTestBase {
    function test_handlerActionsExecute() public {
        _openTrove(alice, 20 ether, 60e18);
        _openTrove(bob, 30 ether, 80e18);
        _warpAndRefresh(AFTER_BOOTSTRAP);

        address[] memory actors = new address[](2);
        actors[0] = alice;
        actors[1] = dave;
        MockAggregator[4] memory feeds = [ethPrimary, ethSecondary, xauPrimary, xauSecondary];
        MengerHandler h = new MengerHandler(
            borrowerOperations, troveManager, stabilityPool, menger, priceFeed, hintHelpers, sortedTroves, feeds, actors
        );

        h.openTrove(1, 10 ether, 20e18);      // dave opens a trove
        assertEq(h.troveOpenings(), 1);
        h.provideToSP(0, 10e18);              // alice deposits
        assertEq(h.deposits(), 1);
        h.redeem(1, 5e18);                    // dave redeems
        assertEq(h.redemptions(), 1);
        h.passTime(1 days);
        h.claimGains(0);                      // alice claims her share of the released fees
        assertEq(h.claims(), 1);
        _assertSupplyEqualsDebt();
    }
}
