// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

contract MengerPriceFeedTest is MengerTestBase {

    function test_price_isEthInMengers() public {
        // 1 Menger = 4,319 x 3.545 / 31.1034768 = 492.26 USD ; 1 ETH = 4,000 USD = 8.1259 Mengers
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);
        assertEq(priceFeed.lastGoodPrice(), EXPECTED_PRICE);
    }

    function testFuzz_price_conversionIsExact(uint _ethUsdCents, uint _xauUsdCents) public {
        _ethUsdCents = bound(_ethUsdCents, 1000, 100000000);   // 10 USD to 1,000,000 USD per ETH
        _xauUsdCents = bound(_xauUsdCents, 10000, 10000000);   // 100 USD to 100,000 USD per ounce
        _settleEthPrice(_ethUsdCents);
        _settleGoldPrice(_xauUsdCents);

        uint price = priceFeed.fetchPrice();

        uint ethUsd = _ethUsdCents * 1e16;
        uint mengerUsd = _xauUsdCents * 1e16 * 35450000 / 311034768;
        // Mengers per ETH x USD per Menger = USD per ETH
        assertApproxEqRel(price * mengerUsd / 1e18, ethUsd, 1e8); // relative error below 1e-10
    }

    function test_goldMarketClosedWindow() public {
        assertFalse(priceFeed.isGoldMarketClosedAt(1792670400)); // Thursday 22 Oct 2026, 12:00 UTC
        assertFalse(priceFeed.isGoldMarketClosedAt(1792789199)); // Friday 23 Oct, 20:59:59 UTC
        assertTrue(priceFeed.isGoldMarketClosedAt(1792789200));  // Friday 23 Oct, 21:00:00 UTC
        assertTrue(priceFeed.isGoldMarketClosedAt(1792843200));  // Saturday 24 Oct, 12:00 UTC
        assertTrue(priceFeed.isGoldMarketClosedAt(1792972799));  // Sunday 25 Oct, 23:59:59 UTC
        assertFalse(priceFeed.isGoldMarketClosedAt(1792972800)); // Monday 26 Oct, 00:00:00 UTC
    }

    // --- ETH ---

    function test_ethFeeds_fallbackToSecondaryThenLastGoodPrice() public {
        // Primary frozen for 5 hours: the secondary feed is used
        ethPrimary.setUpdatedAt(block.timestamp - 5 hours);
        ethSecondary.setAnswer(int(3900e8), block.timestamp);
        uint expected = _expectedPrice(3900, XAU_USD);
        assertEq(priceFeed.fetchPrice(), expected);
        (, bool reliable) = priceFeed.getEthUsdPrice();
        assertTrue(reliable);

        // Both frozen: last good ETH price, redemptions suspended
        ethSecondary.setUpdatedAt(block.timestamp - 5 hours);
        assertEq(priceFeed.fetchPrice(), expected);
        assertFalse(priceFeed.isRedemptionAllowed());

        // A feed that reverts is treated as broken, never blocks the system
        ethPrimary.setReverts(true);
        ethSecondary.setUpdatedAt(block.timestamp);
        assertEq(priceFeed.fetchPrice(), expected);
        assertTrue(priceFeed.isRedemptionAllowed());
    }

    function test_ethFeeds_disagree_followPrimaryAndNoRedemption() public {
        ethSecondary.setAnswer(int(4400e8), block.timestamp); // 10% above the primary
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);     // the primary's 4,000
        assertFalse(priceFeed.isRedemptionAllowed());

        ethSecondary.setAnswer(int(4100e8), block.timestamp); // 2.5% apart: confirmed again
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);
        assertTrue(priceFeed.isRedemptionAllowed());
    }

    // Found by the independent review: with a median of (primary, secondary, last good price), a secondary stuck at
    // the old price froze the system's price while the market fell. The price now follows the primary.
    function test_ethSecondaryStuck_doesNotFreezePrice() public {
        _openTrove(bob, 100 ether, 200e18);
        _openTrove(alice, 10 ether, 70e18);   // 114.9 %
        _deposit(bob, 150e18);

        ethPrimary.setAnswer(int(2800e8), block.timestamp); // the market falls 30 %
        ethSecondary.setUpdatedAt(block.timestamp);         // stuck at 4,000, but with a fresh timestamp
        uint price = priceFeed.fetchPrice();
        assertEq(price, _expectedPrice(2800, XAU_USD));
        assertFalse(priceFeed.isRedemptionAllowed());

        troveManager.liquidate(alice);
        assertEq(troveManager.getTroveStatus(alice), 3);
    }

    // A move of more than 50 % in one round means a broken feed (Liquity V1's rule): the secondary takes over
    function test_ethPrimaryJump_secondaryTakesOver() public {
        ethSecondary.setAnswer(int(2500e8), block.timestamp);  // the secondary falls in two rounds
        ethSecondary.setAnswer(int(1500e8), block.timestamp);
        ethPrimary.setAnswer(int(1500e8), block.timestamp);    // the primary in one round: -62.5 %
        assertEq(priceFeed.fetchPrice(), _expectedPrice(1500, XAU_USD));
        (, bool reliable) = priceFeed.getEthUsdPrice();
        assertTrue(reliable);                                 // both say 1,500

        ethPrimary.setAnswer(int(1510e8), block.timestamp);    // its next round is normal: the primary is back
        assertEq(priceFeed.fetchPrice(), _expectedPrice(1510, XAU_USD));
    }

    function test_ethPrimaryWrongJump_ignored() public {
        ethPrimary.setAnswer(int(100e8), block.timestamp);     // a wrong answer: -97.5 % in one round
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);      // the secondary's 4,000
        assertFalse(priceFeed.isRedemptionAllowed());          // the primary still answers something else
    }

    // --- Gold ---

    function test_goldFeeds_disagree_followPrimaryAndNoRedemption() public {
        xauSecondary.setAnswer(int(4406e8), block.timestamp); // 2% above the primary
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);     // the primary's 4,319
        (, bool agreed, ) = priceFeed.getXauUsdPrice();
        assertFalse(agreed);
        assertFalse(priceFeed.isRedemptionAllowed());

        xauSecondary.setAnswer(int(4340e8), block.timestamp); // back within 1%
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);
        assertTrue(priceFeed.isRedemptionAllowed());
    }

    function test_goldPrimaryLagging_secondaryTakesOver() public {
        _warpAndRefresh(block.timestamp + 2 days);            // Friday 9 October, 12:00 UTC
        xauPrimary.setUpdatedAt(block.timestamp - 30 hours);  // the primary stopped updating
        xauSecondary.setAnswer(int(4450e8), block.timestamp); // gold rises 3 %
        assertEq(priceFeed.fetchPrice(), _expectedPrice(ETH_USD, 4450));
        assertFalse(priceFeed.isRedemptionAllowed());
    }

    // Found by the independent review: a gold feed that stops updating but keeps answering froze the gold price for
    // good once gold had moved more than 1 % away from it. The gold price now follows the feed that is still alive.
    function test_deadGoldFeed_doesNotFreezeGoldPrice() public {
        uint start = block.timestamp;
        for (uint day = 1; day <= 30; day++) {
            vm.warp(start + day * 1 days);
            ethPrimary.setUpdatedAt(block.timestamp);
            ethSecondary.setUpdatedAt(block.timestamp);
            xauPrimary.setAnswer(int((431900 + day * 1440) * 1e6), block.timestamp); // +10 % over 30 days
            // xauSecondary is never updated again
        }
        uint price = priceFeed.fetchPrice();
        assertEq(price, _expectedPrice(ETH_USD, 4751));
        assertFalse(priceFeed.isRedemptionAllowed());         // a single usable gold feed
    }

    function test_goldFeed_broken_usesOtherAndNoRedemption() public {
        xauSecondary.setReverts(true);
        xauPrimary.setAnswer(int(4400e8), block.timestamp);
        assertEq(priceFeed.fetchPrice(), _expectedPrice(ETH_USD, 4400));
        assertFalse(priceFeed.isRedemptionAllowed());
    }

    // --- Feeds that misbehave never block liquidations ---

    // Found by the independent review: an absurdly large answer overflowed in the scaling, so fetchPrice() reverted
    function test_extremeAnswers_doNotBlockLiquidations() public {
        _openTrove(bob, 100 ether, 200e18);
        _openTrove(alice, 10 ether, 70e18);
        _deposit(bob, 150e18);
        _setEthPrice(350000);
        assertLt(troveManager.getCurrentICR(alice, priceFeed.fetchPrice()), 1.1e18);

        ethSecondary.setAnswer(int(2**176 - 1), block.timestamp);  // Chainlink's maximum answer
        xauSecondary.setAnswer(int(2**176 - 1), block.timestamp);
        assertEq(priceFeed.fetchPrice(), _expectedPrice(3500, XAU_USD));
        troveManager.liquidate(alice);
        assertEq(troveManager.getTroveStatus(alice), 3);
    }

    function test_malformedAnswer_treatedAsBroken() public {
        ethPrimary.setMalformed(true);                        // returns 64 bytes instead of 160
        ethSecondary.setAnswer(int(3900e8), block.timestamp);
        assertEq(priceFeed.fetchPrice(), _expectedPrice(3900, XAU_USD));
    }

    function test_constructor_revertsIfGoldFeedsDisagree() public {
        MockAggregator otherGold = new MockAggregator(8, int(4500e8), block.timestamp);
        vm.expectRevert(bytes("MengerPriceFeed: gold feeds not working or not agreeing"));
        new MengerPriceFeed(address(ethPrimary), address(ethSecondary), address(xauPrimary), address(otherGold), GOLD_MAX_AGE);
    }
}

contract MengerBorrowingTest is MengerTestBase {

    function test_openTrove_debtIncludesFeeAndGasReserve() public {
        _openTrove(alice, 10 ether, 40e18);

        // Fee: 0.5% of 40 = 0.2 Menger. Gas reserve: 0.4 Menger.
        assertEq(troveManager.getTroveDebt(alice), 40.6e18);
        assertEq(menger.balanceOf(alice), 40e18);
        assertEq(menger.balanceOf(address(feeIssuance)), 0.2e18);
        assertEq(menger.balanceOf(address(gasPool)), 0.4e18);
        _assertSupplyEqualsDebt();
    }

    function test_openTrove_revertsBelowMinimumDebt() public {
        // Net debt 3 + 0.015 = 3.015 Mengers < 4 Mengers
        vm.expectRevert(bytes("BorrowerOps: Trove's net debt must be greater than minimum"));
        vm.prank(alice);
        borrowerOperations.openTrove{value: 10 ether}(1e18, 3e18, address(0), address(0));
    }

    function test_openTrove_revertsBelowMinimumCollateralRatio() public {
        // 10 ETH = 81.26 Mengers ; debt 73.5 + 0.3675 + 0.4 = 74.27 Mengers ; ratio 109.4% < 110%
        vm.expectRevert(bytes("BorrowerOps: An operation that would result in ICR < MCR is not permitted"));
        vm.prank(alice);
        borrowerOperations.openTrove{value: 10 ether}(1e18, 73.5e18, address(0), address(0));
    }

    function test_borrowingFees_goToStabilityPoolDepositors() public {
        _openTrove(alice, 10 ether, 40e18);  // fee 0.2, kept while the pool is empty
        _openTrove(bob, 30 ether, 100e18);   // fee 0.5
        _deposit(bob, 100e18);               // these fees become pending: nothing released yet
        assertEq(feeIssuance.totalIssuedNotPaid(), 0);
        assertEq(feeIssuance.pendingFees(), 0.7e18);
        assertEq(menger.balanceOf(address(feeIssuance)), 0.7e18);

        _openTrove(carol, 20 ether, 50e18);  // fee 0.25, pending from the next pool operation

        _waitReleasePeriodAndClaim(bob);     // releases the first 0.7; Carol's fee starts its release
        assertApproxEqAbs(menger.balanceOf(bob), 0.7e18, 1e9);
        assertEq(feeIssuance.pendingFees(), 0.25e18);

        _waitReleasePeriodAndClaim(bob);     // releases Carol's fee
        // Bob, the only depositor, receives every fee: 0.2 + 0.5 + 0.25
        assertApproxEqAbs(menger.balanceOf(bob), 0.95e18, 1e9);
        assertLt(menger.balanceOf(address(feeIssuance)), 1e9);
        _assertSupplyEqualsDebt();
    }

    function test_recoveryMode_blocksRiskyBorrowing() public {
        _openTrove(bob, 30 ether, 100e18);
        _openTrove(alice, 10 ether, 45e18);

        _setEthPrice(260000); // ETH falls to 2,600 USD
        uint price = priceFeed.fetchPrice();
        assertTrue(troveManager.checkRecoveryMode(price));

        // In Recovery Mode a new trove needs at least 150% (no borrowing fee is charged)
        vm.expectRevert(bytes("BorrowerOps: Operation must leave trove with ICR >= CCR"));
        vm.prank(carol);
        borrowerOperations.openTrove{value: 10 ether}(1e18, 36e18, address(0), address(0));

        _openTrove(carol, 10 ether, 30e18);
        assertGe(troveManager.getCurrentICR(carol, price), 1.5e18);
        _assertSupplyEqualsDebt();
    }
}

contract MengerRedemptionTest is MengerTestBase {

    function setUp() public override {
        super.setUp();
        _openTrove(alice, 10 ether, 40e18);   // lowest ratio (200%): redeemed first
        _openTrove(bob, 30 ether, 100e18);
        _openTrove(carol, 20 ether, 50e18);
        vm.prank(carol);
        menger.transfer(dave, 40e18);
    }

    function test_redemption_paysGoldValueInEth_feeToStabilityPool() public {
        _deposit(bob, 100e18);
        _warpAndRefresh(AFTER_BOOTSTRAP);

        uint price = priceFeed.fetchPrice();
        uint daveEthBefore = dave.balance;
        uint poolEthBefore = stabilityPool.getETH();

        _redeem(dave, 20e18);

        uint ethDrawn = 20e18 * 1e18 / price; // ETH worth 20 Mengers, i.e. 70.9 g of gold
        uint toDave = dave.balance - daveEthBefore;
        uint fee = stabilityPool.getETH() - poolEthBefore;

        assertEq(toDave + fee, ethDrawn);
        assertGt(fee, 0);
        // Fee rate = 0.5% floor + base rate (half the redeemed share of the supply)
        assertApproxEqAbs(fee * 1e18 / ethDrawn, troveManager.baseRate() + 5e15, 1e9);
        assertEq(menger.balanceOf(dave), 20e18);

        // The pool holds the fee and releases it to depositors over FEE_RELEASE_PERIOD
        assertEq(stabilityPool.pendingRedemptionFeeETH(), fee);
        assertEq(stabilityPool.getDepositorETHGain(bob), 0);

        // Bob, the only depositor, receives the whole fee once the period has passed
        uint bobEthBefore = bob.balance;
        _waitReleasePeriodAndClaim(bob);
        assertApproxEqAbs(bob.balance - bobEthBefore, fee, 1e6);
        assertEq(stabilityPool.pendingRedemptionFeeETH(), 0);
        _assertSupplyEqualsDebt();
    }

    function test_redemption_noFeeWhenStabilityPoolEmpty() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);
        uint price = priceFeed.fetchPrice();
        uint before = dave.balance;

        _redeem(dave, 20e18);

        assertEq(dave.balance - before, 20e18 * 1e18 / price);
        assertEq(stabilityPool.getETH(), 0);
        _assertSupplyEqualsDebt();
    }

    function test_redemption_revertsDuringBootstrap() public {
        _warpAndRefresh(WEDNESDAY_NOON + 13 days); // Tuesday 20 October
        _redeemExpectRevert(dave, 10e18, "TroveManager: Redemptions are not allowed during bootstrap phase");
    }

    function test_redemption_suspendedOnWeekend() public {
        _warpAndRefresh(1792789199);                       // Friday 20:59:59 UTC: allowed
        _redeem(dave, 10e18);

        _warpAndRefresh(1792789200);                       // Friday 21:00 UTC: suspended
        _redeemExpectRevert(dave, 10e18, REDEMPTION_SUSPENDED);

        _warpAndRefresh(1792843200);                       // Saturday: suspended
        _redeemExpectRevert(dave, 10e18, REDEMPTION_SUSPENDED);

        _warpAndRefresh(1792972800);                       // Monday 00:00 UTC: allowed again
        _redeem(dave, 10e18);
        _assertSupplyEqualsDebt();
    }

    function test_redemption_suspendedWhenGoldPriceStale() public {
        vm.warp(AFTER_BOOTSTRAP);
        ethPrimary.setUpdatedAt(block.timestamp);
        ethSecondary.setUpdatedAt(block.timestamp);
        xauPrimary.setUpdatedAt(block.timestamp - 2 hours);
        xauSecondary.setUpdatedAt(block.timestamp - 2 hours);
        _redeemExpectRevert(dave, 10e18, REDEMPTION_SUSPENDED);

        xauPrimary.setUpdatedAt(block.timestamp);           // one gold feed still stale
        _redeemExpectRevert(dave, 10e18, REDEMPTION_SUSPENDED);

        xauSecondary.setUpdatedAt(block.timestamp);         // both fresh
        _redeem(dave, 10e18);
    }

    function test_redemption_suspendedWhenGoldFeedsDisagree() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);
        xauSecondary.setAnswer(int(4406e8), block.timestamp); // 2% apart
        _redeemExpectRevert(dave, 10e18, REDEMPTION_SUSPENDED);

        xauSecondary.setAnswer(int(4340e8), block.timestamp); // 0.5% apart
        _redeem(dave, 10e18);
    }
}

contract MengerLiquidationTest is MengerTestBase {

    function test_liquidation_worksOnWeekend_withLastGoldPrice() public {
        _openTrove(bob, 100 ether, 200e18);
        _openTrove(alice, 10 ether, 70e18);  // ratio 81.26 / 70.75 = 114.9%
        _deposit(bob, 150e18);

        // Saturday 10 October 2026, 12:00 UTC. Gold feeds last updated Friday 21:00 UTC.
        vm.warp(1791633600);
        xauPrimary.setUpdatedAt(1791579600);
        xauSecondary.setUpdatedAt(1791579600);
        _setEthPrice(370000); // ETH falls to 3,700 USD on Saturday

        uint price = priceFeed.fetchPrice();
        assertEq(price, _expectedPrice(3700, XAU_USD));
        assertLt(troveManager.getCurrentICR(alice, price), 1.1e18);
        assertFalse(priceFeed.isRedemptionAllowed());

        troveManager.liquidate(alice);

        assertEq(troveManager.getTroveStatus(alice), 3); // closed by liquidation
        // The pool absorbed the debt and received 10 ETH minus the 0.5% liquidation reward
        assertApproxEqAbs(stabilityPool.getDepositorETHGain(bob), 9.95 ether, 1e9);
        _assertSupplyEqualsDebt();
    }

    function test_ethCrash40Percent_systemStaysSolvent() public {
        _openTrove(bob, 100 ether, 300e18);
        _openTrove(alice, 10 ether, 60e18);
        _openTrove(carol, 10 ether, 55e18);
        _openTrove(dave, 10 ether, 50e18);
        _openTrove(erin, 20 ether, 50e18);
        _deposit(bob, 250e18);

        _setEthPrice(240000); // ETH falls 40%, to 2,400 USD
        uint price = priceFeed.fetchPrice();
        // Alice, Carol and Dave are now under water (ratio below 100%)
        assertLt(troveManager.getCurrentICR(alice, price), 1e18);
        assertLt(troveManager.getCurrentICR(carol, price), 1e18);
        assertLt(troveManager.getCurrentICR(dave, price), 1e18);

        troveManager.liquidateTroves(10);

        assertEq(troveManager.getTroveStatus(alice), 3);
        assertEq(troveManager.getTroveStatus(carol), 3);
        assertEq(troveManager.getTroveStatus(dave), 3);

        // Every remaining trove is still backed by more than its debt
        address current = sortedTroves.getFirst();
        uint remaining;
        while (current != address(0)) {
            assertGt(troveManager.getCurrentICR(current, price), 1e18);
            current = sortedTroves.getNext(current);
            remaining++;
        }
        assertGt(remaining, 0);
        assertGt(troveManager.getTCR(price), 1e18);
        assertGe(stabilityPool.getTotalLUSDDeposits(), 1e18); // the pool keeps at least 1 Menger
        _assertSupplyEqualsDebt();
    }
}
