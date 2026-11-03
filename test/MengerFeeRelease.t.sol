// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

// Fees are released to Stability Pool depositors gradually, so that a deposit made just before a fee and
// withdrawn just after captures almost nothing of it.
contract MengerFeeReleaseTest is MengerTestBase {

    function setUp() public override {
        super.setUp();
        _openTrove(alice, 10 ether, 40e18);
        _openTrove(bob, 60 ether, 200e18);
        _openTrove(carol, 40 ether, 120e18);
        _openTrove(erin, 300 ether, 1000e18);   // Erin holds enough Mengers to try a just-in-time deposit
        vm.prank(carol);
        menger.transfer(dave, 60e18);

        _deposit(bob, 100e18);                  // Bob stays in the pool
        _waitReleasePeriodAndClaim(bob);        // the fees paid so far are released and paid out
        assertEq(feeIssuance.pendingFees(), 0);
    }

    function test_releasePeriodIsTheSameForBothFees() public {
        assertEq(stabilityPool.FEE_RELEASE_PERIOD(), FEE_RELEASE_PERIOD);
        assertEq(feeIssuance.FEE_RELEASE_PERIOD(), FEE_RELEASE_PERIOD);
    }

    function test_justInTimeDeposit_capturesNoRedemptionFee() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);

        // In one block: Erin deposits ten times the pool, Dave redeems, Erin withdraws everything.
        // Paid out at once, the fee would have gone 1,000 / 1,100 = 91% to Erin.
        _deposit(erin, 1000e18);
        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 30e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;
        assertGt(fee, 0);

        uint erinEthBefore = erin.balance;
        uint erinMengersBefore = menger.balanceOf(erin);
        vm.prank(erin);
        stabilityPool.withdrawFromSP(1000e18);
        assertEq(erin.balance, erinEthBefore);                          // no share of the fee
        assertEq(menger.balanceOf(erin) - erinMengersBefore, 1000e18);   // just her deposit back

        // Bob, who stays, receives the fee over the release period
        uint bobEthBefore = bob.balance;
        _waitReleasePeriodAndClaim(bob);
        assertApproxEqAbs(bob.balance - bobEthBefore, fee, 1e6);
    }

    function test_justInTimeDeposit_capturesNoBorrowingFee() public {
        // In one block: Erin deposits, borrows 500 more Mengers (fee 2.5) and withdraws everything
        _deposit(erin, 1000e18);
        vm.prank(erin);
        borrowerOperations.withdrawLUSD(1e18, 500e18, address(0), address(0));
        uint erinMengersBefore = menger.balanceOf(erin);
        vm.prank(erin);
        stabilityPool.withdrawFromSP(1000e18);
        assertEq(menger.balanceOf(erin) - erinMengersBefore, 1000e18); // her deposit back, no fee

        // Bob, who stays, receives Erin's fee over the release period
        uint bobBefore = menger.balanceOf(bob);
        _waitReleasePeriodAndClaim(bob);
        assertApproxEqAbs(menger.balanceOf(bob) - bobBefore, 2.5e18, 1e9);
    }

    function test_feesReleasedInProportionToTime() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);
        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 30e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;

        // One day out of seven: one seventh of the fee
        uint bobEthBefore = bob.balance;
        _warpAndRefresh(block.timestamp + 1 days);
        _claim(bob);
        assertApproxEqAbs(bob.balance - bobEthBefore, fee / 7, 1e6);
        assertEq(stabilityPool.pendingRedemptionFeeETH(), fee - fee / 7);

        // After a full period without any pool operation, everything left is released
        _waitReleasePeriodAndClaim(bob);
        assertApproxEqAbs(bob.balance - bobEthBefore, fee, 1e6);
        assertEq(stabilityPool.pendingRedemptionFeeETH(), 0);
    }

    // What "released gradually" means in practice (pointed out by the independent review): each pool operation
    // releases elapsed / 7 days of what is still pending, so with daily activity about two thirds of a fee are paid
    // after one week and 96 % after three weeks. Without any activity, everything is paid at the first operation
    // after 7 days (test above).
    function test_releaseWithDailyActivity_twoThirdsAfterOneWeek() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);
        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 30e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;

        for (uint i = 0; i < 7; i++) {
            _warpAndRefresh(block.timestamp + 1 days);
            _claim(bob);
        }
        uint paid = fee - stabilityPool.pendingRedemptionFeeETH();
        assertApproxEqRel(paid, fee * 66 / 100, 0.01e18);   // 1 - (6/7)^7 = 66 %

        for (uint i = 0; i < 14; i++) {
            _warpAndRefresh(block.timestamp + 1 days);
            _claim(bob);
        }
        paid = fee - stabilityPool.pendingRedemptionFeeETH();
        assertApproxEqRel(paid, fee * 96 / 100, 0.01e18);   // 1 - (6/7)^21 = 96 %
    }

    function test_newFeeDoesNotInheritTimeAlreadyElapsed() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);
        _claim(bob);                                  // last pool operation
        _warpAndRefresh(block.timestamp + 6 days);    // a long quiet period, then a redemption
        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 30e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;

        // One more day: only one seventh is released, not 7/7 as if the fee had waited the whole week
        uint bobEthBefore = bob.balance;
        _warpAndRefresh(block.timestamp + 1 days);
        _claim(bob);
        assertApproxEqAbs(bob.balance - bobEthBefore, fee / 7, 1e6);
    }

    function test_liquidation_releasesPendingFeesBeforeChangingDeposits() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);
        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 30e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;
        _openTrove(dave, 10 ether, 65e18);    // a risky trove, ratio about 122%

        // A week later ETH falls 12% and Dave's trove is liquidated. The liquidation first releases the whole
        // pending fee, on Bob's deposit as it was before the liquidation reduces it.
        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        _setEthPrice(352000);
        uint price = priceFeed.fetchPrice();
        assertLt(troveManager.getCurrentICR(dave, price), 1.1e18);
        assertGt(troveManager.getCurrentICR(alice, price), 1.1e18);
        uint gainBefore = stabilityPool.getDepositorETHGain(bob);
        troveManager.liquidate(dave);

        assertEq(stabilityPool.pendingRedemptionFeeETH(), 0);
        // Bob, the only depositor, receives the fee plus Dave's 10 ETH minus the 0.5% liquidation reward
        uint gain = stabilityPool.getDepositorETHGain(bob) - gainBefore;
        assertApproxEqAbs(gain, fee + 9.95 ether, 1e9);
        _assertSupplyEqualsDebt();
    }

    function test_ethGainToTrove_includesReleasedFees() public {
        _warpAndRefresh(AFTER_BOOTSTRAP);
        _redeem(dave, 30e18);
        uint fee = stabilityPool.pendingRedemptionFeeETH();
        assertEq(stabilityPool.getDepositorETHGain(bob), 0);

        // Bob's gain is still pending, but moving it to his trove releases it first
        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        (, uint collBefore, , ) = troveManager.getEntireDebtAndColl(bob);
        vm.prank(bob);
        stabilityPool.withdrawETHGainToTrove(address(0), address(0));
        (, uint collAfter, , ) = troveManager.getEntireDebtAndColl(bob);
        assertApproxEqAbs(collAfter - collBefore, fee, 1e6);
    }
}

// Fees paid while nobody is in the pool wait, and the empty period does not count toward their release.
contract MengerFeeReleaseEmptyPoolTest is MengerTestBase {

    function test_feesWaitWhilePoolIsEmpty() public {
        _openTrove(alice, 10 ether, 40e18);   // fee 0.2
        _openTrove(bob, 60 ether, 200e18);    // fee 1.0

        _warpAndRefresh(block.timestamp + 30 days);
        _deposit(bob, 100e18);                // the first deposit: nothing released, the fees become pending
        assertEq(feeIssuance.pendingFees(), 1.2e18);
        assertEq(feeIssuance.totalIssuedNotPaid(), 0);

        // One day later, one seventh: the 30 empty days did not count
        uint bobBefore = menger.balanceOf(bob);
        _warpAndRefresh(block.timestamp + 1 days);
        _claim(bob);
        assertApproxEqAbs(menger.balanceOf(bob) - bobBefore, uint(1.2e18) / 7, 1e9);

        _waitReleasePeriodAndClaim(bob);
        assertApproxEqAbs(menger.balanceOf(bob) - bobBefore, 1.2e18, 1e9);
    }
}
