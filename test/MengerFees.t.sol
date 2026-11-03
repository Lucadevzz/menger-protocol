// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

// Fees replace LQTY: who receives them, in what proportion, and who may move them.
contract MengerFeesTest is MengerTestBase {

    function setUp() public override {
        super.setUp();
        _openTrove(alice, 10 ether, 40e18);   // fee 0.2
        _openTrove(bob, 60 ether, 200e18);    // fee 1.0
        _openTrove(carol, 40 ether, 120e18);  // fee 0.6
        vm.prank(carol);
        menger.transfer(dave, 60e18);
    }

    function test_borrowingFees_splitProRata() public {
        _deposit(bob, 100e18);   // first pool operation: the 1.8 Mengers of fees paid so far become pending
        _deposit(carol, 50e18);
        _openTrove(erin, 30 ether, 100e18); // fee 0.5, pending from the next pool operation

        uint bobBefore = menger.balanceOf(bob);
        uint carolBefore = menger.balanceOf(carol);

        // First period: the 1.8 are released. Second period: Erin's 0.5.
        for (uint i = 0; i < 2; i++) {
            _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
            _claim(bob);
            _claim(carol);
        }

        // Bob holds 2/3 of the deposits, Carol 1/3
        assertApproxEqAbs(menger.balanceOf(bob) - bobBefore, uint(2.3e18) * 2 / 3, 1e9);
        assertApproxEqAbs(menger.balanceOf(carol) - carolBefore, uint(2.3e18) / 3, 1e9);
        assertLt(menger.balanceOf(address(feeIssuance)), 1e9);
        _assertSupplyEqualsDebt();
    }

    function test_redemptionFee_splitProRata() public {
        _deposit(bob, 100e18);
        _deposit(carol, 50e18);
        _warpAndRefresh(AFTER_BOOTSTRAP);

        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 30e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;
        assertGt(fee, 0);
        assertEq(stabilityPool.pendingRedemptionFeeETH(), fee);

        uint bobEthBefore = bob.balance;
        uint carolEthBefore = carol.balance;
        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        _claim(bob);
        _claim(carol);

        assertApproxEqAbs(bob.balance - bobEthBefore, fee * 2 / 3, 1e6);
        assertApproxEqAbs(carol.balance - carolEthBefore, fee / 3, 1e6);
        assertLt(stabilityPool.getETH(), 1e6); // only rounding dust is left
    }

    function test_feesAfterLiquidation_followCompoundedDeposits() public {
        _deposit(bob, 50e18);
        _deposit(carol, 50e18);

        // ETH falls to 2,100 USD: Alice's ratio is 42.7 / 40.6 = 105%. The pool absorbs her debt, shared equally.
        _setEthPrice(210000);
        uint price = priceFeed.fetchPrice();
        assertLt(troveManager.getCurrentICR(alice, price), 1.1e18);
        troveManager.liquidate(alice);

        uint bobDeposit = stabilityPool.getCompoundedLUSDDeposit(bob);
        uint carolDeposit = stabilityPool.getCompoundedLUSDDeposit(carol);
        assertApproxEqAbs(bobDeposit, carolDeposit, 1e9);
        assertLt(bobDeposit, 50e18);

        // A redemption fee paid after the liquidation is shared on the compounded deposits
        _warpAndRefresh(AFTER_BOOTSTRAP);
        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 20e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;
        uint ethGainBobBefore = stabilityPool.getDepositorETHGain(bob);
        uint ethGainCarolBefore = stabilityPool.getDepositorETHGain(carol);

        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        _deposit(alice, 1e18); // any pool operation releases the pending fee, before Alice's deposit counts

        assertApproxEqAbs(stabilityPool.getDepositorETHGain(bob) - ethGainBobBefore, fee / 2, 1e6);
        assertApproxEqAbs(stabilityPool.getDepositorETHGain(carol) - ethGainCarolBefore, fee / 2, 1e6);
        assertEq(stabilityPool.getDepositorETHGain(alice), 0);
        _assertSupplyEqualsDebt();
    }

    function test_frontEnd_takesItsShareOfBorrowingFeesOnly() public {
        address frontEnd = address(0xF407);
        vm.prank(frontEnd);
        stabilityPool.registerFrontEnd(0.9e18); // depositors keep 90%

        vm.prank(bob);
        stabilityPool.provideToSP(100e18, frontEnd); // the 1.8 paid so far become pending
        _openTrove(erin, 30 ether, 100e18);          // fee 0.5

        _warpAndRefresh(AFTER_BOOTSTRAP);
        uint poolEthBefore = stabilityPool.getETH();
        _redeem(dave, 20e18);
        uint fee = stabilityPool.getETH() - poolEthBefore;

        uint bobMengersBefore = menger.balanceOf(bob);
        uint bobEthBefore = bob.balance;
        _claim(bob);                      // releases the 1.8; Erin's fee becomes pending
        _waitReleasePeriodAndClaim(bob);  // releases Erin's fee and the redemption fee

        // Bob is the only depositor: the 2.3 Mengers of fees are shared 90% / 10% with his front end
        assertApproxEqAbs(menger.balanceOf(bob) - bobMengersBefore, 2.07e18, 1e9);
        assertApproxEqAbs(menger.balanceOf(frontEnd), 0.23e18, 1e9);
        // The redemption fee, in ETH, goes entirely to the depositor
        assertApproxEqAbs(bob.balance - bobEthBefore, fee, 1e6);
        assertEq(frontEnd.balance, 0);
    }

    function test_feeIssuance_onlyStabilityPool() public {
        vm.expectRevert(bytes("StabilityPoolFeeIssuance: caller is not SP"));
        feeIssuance.issueLQTY();
        vm.expectRevert(bytes("StabilityPoolFeeIssuance: caller is not SP"));
        feeIssuance.sendLQTY(alice, 1);
    }

    function test_distributeRedemptionFee_onlyTroveManager() public {
        vm.expectRevert(bytes("StabilityPool: Caller is not TroveManager"));
        stabilityPool.distributeRedemptionFee(1 ether);
    }

    function test_noAdminKeys() public {
        assertEq(troveManager.owner(), address(0));
        assertEq(borrowerOperations.owner(), address(0));
        assertEq(stabilityPool.owner(), address(0));
        assertEq(activePool.owner(), address(0));
        assertEq(defaultPool.owner(), address(0));
        assertEq(collSurplusPool.owner(), address(0));
        assertEq(sortedTroves.owner(), address(0));
        assertEq(hintHelpers.owner(), address(0));
        assertEq(feeIssuance.owner(), address(0));
    }

    function testFuzz_redemption_conservesEth(uint _mengers) public {
        _mengers = bound(_mengers, 1e18, 60e18);
        _deposit(bob, 100e18);
        _warpAndRefresh(AFTER_BOOTSTRAP);

        uint price = priceFeed.fetchPrice();
        uint daveMengersBefore = menger.balanceOf(dave);
        uint daveEthBefore = dave.balance;
        uint poolEthBefore = stabilityPool.getETH();

        // Amounts that would leave a trove below the minimum debt are truncated, possibly to zero
        if (_redeem(dave, _mengers) == 0) { return; }

        uint redeemed = daveMengersBefore - menger.balanceOf(dave);
        assertGt(redeemed, 0);
        uint toDave = dave.balance - daveEthBefore;
        uint fee = stabilityPool.getETH() - poolEthBefore;
        // ETH paid out (to Dave + to the pool) is worth exactly the Mengers burned, at the gold price
        assertApproxEqAbs(toDave + fee, redeemed * 1e18 / price, 3);
        _assertSupplyEqualsDebt();
    }
}
