// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

/*
* Written by the independent review. When repeated liquidations shrink the product P below 1e-9 of its start, the
* Stability Pool changes scale. The random runs never get that far; this test does, and checks that a redemption fee
* released afterwards is still shared pro rata and that every depositor can still claim and withdraw.
*/
contract MengerScaleTest is MengerTestBase {
    address whale = address(0x3A1E);
    uint victimCount;

    // Deposits _amount for _depositor, then liquidates a trove larger than the pool: the pool falls to 1 Menger
    function _drainPoolWith(address _depositor, uint _amount) internal {
        vm.prank(whale);
        menger.transfer(_depositor, _amount);
        _deposit(_depositor, _amount);
        uint total = stabilityPool.getTotalLUSDDeposits();
        address victim = address(uint(0xBAD000) + victimCount++);
        uint debt = total + 2e18;
        uint price = priceFeed.fetchPrice();
        uint coll = (debt * 1005 / 1000 + 0.4e18) * 111 / 100 * 1e18 / price + 1; // about 111 %
        vm.deal(victim, coll);
        _openTrove(victim, coll, debt);
        _setEthPrice(380000);
        troveManager.liquidate(victim);
        _setEthPrice(400000);
        assertEq(stabilityPool.getTotalLUSDDeposits(), 1e18);
    }

    function test_feeReleaseAcrossScaleChange() public {
        vm.deal(whale, 200000 ether);
        _openTrove(whale, 150000 ether, 400000e18);

        _drainPoolWith(bob, 10000e18);
        _drainPoolWith(carol, 10000e18);
        assertEq(stabilityPool.currentScale(), 0);
        _drainPoolWith(dave, 10000e18);
        assertEq(stabilityPool.currentScale(), 1, "P crossed the scale boundary");

        vm.prank(whale);
        menger.transfer(erin, 1000e18);
        _deposit(erin, 1000e18);

        _warpAndRefresh(AFTER_BOOTSTRAP);
        _waitReleasePeriodAndClaim(erin);      // pays out the liquidation gains and borrowing fees so far
        uint poolEthBefore = stabilityPool.getETH();
        assertGt(_redeem(whale, 50000e18), 0);
        uint fee = stabilityPool.getETH() - poolEthBefore;
        assertGt(fee, 1 ether);

        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        uint erinGainBefore = stabilityPool.getDepositorETHGain(erin);
        uint daveGainBefore = stabilityPool.getDepositorETHGain(dave);
        _deposit(whale, 1);                    // any pool operation releases the fee
        assertEq(stabilityPool.pendingRedemptionFeeETH(), 0);

        uint total = stabilityPool.getTotalLUSDDeposits() - 1;   // without the whale's 1 wei
        uint erinShare = stabilityPool.getCompoundedLUSDDeposit(erin);
        uint daveShare = stabilityPool.getCompoundedLUSDDeposit(dave);
        assertApproxEqRel(stabilityPool.getDepositorETHGain(erin) - erinGainBefore, fee * erinShare / total, 1e12);
        assertApproxEqRel(stabilityPool.getDepositorETHGain(dave) - daveGainBefore, fee * daveShare / total, 1e12);

        address[5] memory all = [bob, carol, dave, erin, whale];
        uint gains;
        for (uint i = 0; i < 5; i++) { gains += stabilityPool.getDepositorETHGain(all[i]); }
        assertLe(gains, stabilityPool.getETH() + 1e9);

        for (uint i = 0; i < 5; i++) { _claim(all[i]); }
        vm.prank(erin);
        stabilityPool.withdrawFromSP(uint(-1));
        _assertSupplyEqualsDebt();
    }
}
