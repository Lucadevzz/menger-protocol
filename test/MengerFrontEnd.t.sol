// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

/*
* Front ends (kept from Liquity V1): a registered front end receives a share of the fee gains of the deposits tagged
* with it. Found by the independent review, in code unchanged from Liquity V1: rounding could leave the front end's
* compounded stake 1 wei below the deposits tagged with it, and the last tagged depositor's full withdrawal then
* reverted ("SafeMath: subtraction overflow"). In 26 of 200 random configurations.
*/
contract MengerFrontEndTest is MengerTestBase {
    address frontEnd = address(0xF407);

    function _liquidateRiskyTrove(address _user, uint _debt) internal {
        uint price = priceFeed.fetchPrice();
        uint coll = (_debt * 1005 / 1000 + 0.4e18) * 111 / 100 * 1e18 / price + 1; // about 111 %
        vm.deal(_user, coll + 1 ether);
        _openTrove(_user, coll, _debt);
        _setEthPrice(380000);                 // -5 %: the risky trove falls below 110 %
        troveManager.liquidate(_user);
        _setEthPrice(400000);
    }

    // Two depositors tagged with the same front end, one joining between two liquidations: both can withdraw everything
    function testFuzz_taggedDepositorsCanWithdrawEverything(uint _a, uint _b, uint _d1, uint _d2) public {
        _a = bound(_a, 1e18, 300e18);
        _b = bound(_b, 1e18, 300e18);
        _d1 = bound(_d1, 5e18, 40e18);
        _d2 = bound(_d2, 5e18, 40e18);

        vm.prank(frontEnd);
        stabilityPool.registerFrontEnd(0.5e18);
        vm.deal(bob, 1000 ether);
        _openTrove(bob, 900 ether, 2000e18);
        _deposit(bob, 100e18);                // untagged, keeps the pool above its 1-Menger minimum
        vm.prank(bob);
        menger.transfer(alice, _a);
        vm.prank(bob);
        menger.transfer(carol, _b);

        vm.prank(alice);
        stabilityPool.provideToSP(_a, frontEnd);
        _liquidateRiskyTrove(dave, _d1);
        vm.prank(carol);
        stabilityPool.provideToSP(_b, frontEnd);
        _liquidateRiskyTrove(erin, _d2);

        vm.prank(alice);
        stabilityPool.withdrawFromSP(uint(-1));
        vm.prank(carol);
        stabilityPool.withdrawFromSP(uint(-1));
        assertEq(stabilityPool.getCompoundedLUSDDeposit(alice), 0);
        assertEq(stabilityPool.getCompoundedLUSDDeposit(carol), 0);
        // Rounding can also go the other way and leave the front end a stake of 1 wei: harmless dust
        assertLe(stabilityPool.getCompoundedFrontEndStake(frontEnd), 1);
        _assertSupplyEqualsDebt();
    }

    // The review's counterexample: before the fix, Carol's withdrawal reverted
    function test_lastTaggedDepositorCanWithdrawEverything() public {
        testFuzz_taggedDepositorsCanWithdrawEverything(
            291868011005464671503, 1000000000000000003, 14991827743784806257, 27426066737768197438
        );
    }
}
