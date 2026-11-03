// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

// Problems found by fuzzing, replayed as fixed tests so that they stay fixed.
contract MengerRegressionTest is MengerTestBase {
    address whale = address(0x3A1E);
    address victim1 = address(0x71C1);
    address victim2 = address(0x71C2);

    /*
    * Found by the invariant fuzzer. Two separate liquidations: the first leaves a rounding error that the second
    * carries over, and the second drains the pool to its 1-Menger minimum. A large fee is then shared over that
    * single Menger, which multiplies the rounding error: the depositors' gains add up to a few hundred units of
    * 1e-18 more than what the pool holds for them. Before the fix, the last depositor's withdrawal reverted
    * ("SafeMath: subtraction overflow" for ETH, "ERC20: transfer amount exceeds balance" for Mengers) and their
    * deposit was locked. Now every gain is paid through a cap: the last depositor receives that dust less.
    * Amounts come from the fuzzer's counterexample.
    */
    function _drainPoolAfterRoundingError() internal {
        vm.deal(whale, 10000 ether);
        vm.deal(victim1, 10000 ether);
        vm.deal(victim2, 10000 ether);
        _openTrove(whale, 3000 ether, 6000e18);
        vm.prank(whale);
        menger.transfer(bob, 1043922778927921278352);
        vm.prank(whale);
        menger.transfer(carol, 237488278835993116345);
        _deposit(bob, 1043922778927921278352);
        _deposit(carol, 237488278835993116345);
        _waitReleasePeriodAndClaim(bob); // pays out the fees of the troves opened so far
        _claim(carol);

        // Two troves at about 115%, then ETH falls 10%
        uint price = priceFeed.fetchPrice();
        uint d1 = 1171406030829861343361;
        uint d2 = 241667890159564546045;
        _openTrove(victim1, (d1 * 1005 / 1000 + 0.4e18) * 115 / 100 * 1e18 / price + 1e15, d1);
        _openTrove(victim2, (d2 * 1005 / 1000 + 0.4e18) * 116 / 100 * 1e18 / price + 1e15, d2);
        _setEthPrice(360000);
        troveManager.liquidate(victim1);
        troveManager.liquidate(victim2);
        assertEq(stabilityPool.getTotalLUSDDeposits(), 1e18); // only the 1-Menger minimum is left
    }

    function test_lastDepositorCanWithdraw_whenEthGainsExceedPoolEth() public {
        _drainPoolAfterRoundingError();

        // A large redemption: its fee, several ETH, is shared over that single Menger once released
        _warpAndRefresh(AFTER_BOOTSTRAP);
        assertGt(_redeem(whale, menger.balanceOf(whale) * 45 / 100), 0);
        assertGt(stabilityPool.pendingRedemptionFeeETH(), 10 ether);
        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        vm.prank(whale);
        menger.transfer(dave, 1);
        _deposit(dave, 1); // any pool operation releases the fee

        uint gains = stabilityPool.getDepositorETHGain(bob) + stabilityPool.getDepositorETHGain(carol);
        uint poolEth = stabilityPool.getETH();
        assertGt(gains, poolEth);            // rounding: more gains than ETH...
        assertLe(gains - poolEth, 1e9);      // ...by dust only

        // Both depositors can still claim; the last one receives the dust less
        uint carolBefore = carol.balance;
        uint carolGain = stabilityPool.getDepositorETHGain(carol);
        _claim(bob);
        _claim(carol);
        assertEq(carol.balance - carolBefore, carolGain - (gains - poolEth));
        assertEq(stabilityPool.getETH(), 0);
    }

    function test_lastDepositorCanWithdraw_whenFeeGainsExceedReleasedFees() public {
        _drainPoolAfterRoundingError();

        // A large loan: its borrowing fee, 15 Mengers, is shared over that single Menger once released
        vm.prank(whale);
        borrowerOperations.withdrawLUSD(1e18, 3000e18, address(0), address(0));
        _claim(bob);                                   // the fee becomes pending
        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        vm.prank(whale);
        stabilityPool.provideToSP(1, address(0));      // any pool operation releases it

        uint gains = stabilityPool.getDepositorLQTYGain(bob) + stabilityPool.getDepositorLQTYGain(carol);
        uint released = feeIssuance.totalIssuedNotPaid();
        assertGt(gains, released);           // rounding: more gains than released fees...
        assertLe(gains - released, 1e9);     // ...by dust only

        // Both depositors can still claim; the last one receives the dust less
        uint carolBefore = menger.balanceOf(carol);
        uint carolGain = stabilityPool.getDepositorLQTYGain(carol);
        _claim(bob);
        _claim(carol);
        assertEq(menger.balanceOf(carol) - carolBefore, carolGain - (gains - released));
        assertEq(feeIssuance.totalIssuedNotPaid(), 0);
    }

    // Found by the independent review: when rounding leaves nothing to pay a depositor whose gain shows a few wei,
    // withdrawETHGainToTrove() reverted deep inside BorrowerOperations. It now stops with the pool's own message,
    // and claiming with withdrawFromSP(0) still works.
    function test_ethGainToTrove_whenNothingLeftToPay() public {
        vm.deal(alice, 1000 ether);
        _openTrove(alice, 900 ether, 10e18);
        _drainPoolAfterRoundingError();
        _deposit(alice, 1);
        _warpAndRefresh(AFTER_BOOTSTRAP);
        assertGt(_redeem(whale, menger.balanceOf(whale) * 45 / 100), 0);
        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        vm.prank(whale);
        menger.transfer(dave, 1);
        _deposit(dave, 1);                    // releases the fee
        _claim(bob);
        _claim(carol);

        if (stabilityPool.getETH() == 0 && stabilityPool.getDepositorETHGain(alice) > 0) {
            vm.expectRevert(bytes("StabilityPool: caller must have non-zero ETH Gain"));
            vm.prank(alice);
            stabilityPool.withdrawETHGainToTrove(address(0), address(0));
        }
        _claim(alice);
        assertEq(stabilityPool.getDepositorETHGain(alice), 0);
    }
}
