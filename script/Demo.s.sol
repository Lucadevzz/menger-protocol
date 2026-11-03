// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {BorrowerOperations} from "../src/BorrowerOperations.sol";
import {TroveManager} from "../src/TroveManager.sol";
import {StabilityPool} from "../src/StabilityPool.sol";
import {HintHelpers} from "../src/HintHelpers.sol";
import {SortedTroves} from "../src/SortedTroves.sol";
import {LUSDToken} from "../src/LUSDToken.sol";
import {MengerPriceFeed} from "../src/MengerPriceFeed.sol";

/*
* Everyday actions on a deployed system, for demonstrations, without a website. Addresses are read from
* deployments/<chain id>.json. Amounts are in wei (1 ETH or 1 Menger = 1000000000000000000). See DEPLOIEMENT.md.
*
*   forge script script/Demo.s.sol --sig "status()" --rpc-url $RPC_URL
*   forge script script/Demo.s.sol --sig "openTrove(uint256,uint256)" <ETH collateral> <Mengers> --rpc-url $RPC_URL --account <wallet> --broadcast
*   ... and likewise deposit, withdraw, liquidate, redeem, closeTrove.
*/
contract Demo is Script {
    BorrowerOperations borrowerOperations;
    TroveManager troveManager;
    StabilityPool stabilityPool;
    HintHelpers hintHelpers;
    SortedTroves sortedTroves;
    LUSDToken menger;
    MengerPriceFeed priceFeed;

    // The wallet whose position status() shows: the broadcasting wallet after an action, otherwise --sender
    address user;

    function _load() internal {
        string memory json = vm.readFile(string(abi.encodePacked("deployments/", vm.toString(_chainId()), ".json")));
        borrowerOperations = BorrowerOperations(vm.parseJsonAddress(json, ".borrowerOperations"));
        troveManager = TroveManager(vm.parseJsonAddress(json, ".troveManager"));
        stabilityPool = StabilityPool(payable(vm.parseJsonAddress(json, ".stabilityPool")));
        hintHelpers = HintHelpers(vm.parseJsonAddress(json, ".hintHelpers"));
        sortedTroves = SortedTroves(vm.parseJsonAddress(json, ".sortedTroves"));
        menger = LUSDToken(vm.parseJsonAddress(json, ".mengerToken"));
        priceFeed = MengerPriceFeed(vm.parseJsonAddress(json, ".priceFeed"));
    }

    // Prints the state of the system and of a position: the wallet that just acted, or the address given with --sender
    function status() public {
        _load();
        if (user == address(0)) { user = msg.sender; }
        uint price = priceFeed.lastGoodPrice();
        (uint xauUsd, bool agreed, bool fresh) = priceFeed.getXauUsdPrice();
        (uint ethUsd, bool ethReliable) = priceFeed.getEthUsdPrice();
        console.log("--- System ---");
        console.log("ETH/USD (18 decimals)            ", ethUsd, ethReliable ? "(reliable)" : "(NOT reliable)");
        console.log("Gold/USD per ounce (18 decimals) ", xauUsd);
        console.log("Gold feeds agree / fresh         ", agreed, fresh);
        console.log("Price of 1 ETH in Mengers         ", price);
        console.log("Redemptions allowed now          ", priceFeed.isRedemptionAllowed());
        console.log("Troves open                      ", troveManager.getTroveOwnersCount());
        console.log("Mengers in circulation            ", menger.totalSupply());
        console.log("Total collateral ratio (1e18=100%)", troveManager.getTCR(price));
        console.log("Recovery Mode                    ", troveManager.checkRecoveryMode(price));
        console.log("Stability Pool deposits          ", stabilityPool.getTotalLUSDDeposits());
        console.log("--- You:", user, "---");
        console.log("ETH balance                      ", user.balance);
        console.log("Menger balance                    ", menger.balanceOf(user));
        if (troveManager.getTroveStatus(user) == 1) {
            (uint debt, uint coll, , ) = troveManager.getEntireDebtAndColl(user);
            console.log("Trove debt (Mengers)              ", debt);
            console.log("Trove collateral (ETH)           ", coll);
            console.log("Trove collateral ratio           ", troveManager.getCurrentICR(user, price));
        } else {
            console.log("No open trove");
        }
        console.log("Stability Pool deposit           ", stabilityPool.getCompoundedLUSDDeposit(user));
        console.log("  ETH gains to claim             ", stabilityPool.getDepositorETHGain(user));
        console.log("  Menger fee gains to claim       ", stabilityPool.getDepositorLQTYGain(user));
    }

    // Opens a trove: locks _collateral wei of ETH and borrows _mengers (plus a 0.5% fee and the 0.4 Menger gas reserve)
    function openTrove(uint _collateral, uint _mengers) external {
        _load();
        vm.startBroadcast();
        (, user, ) = vm.readCallers();
        borrowerOperations.openTrove{value: _collateral}(1e18, _mengers, address(0), address(0));
        vm.stopBroadcast();
        status();
    }

    // Repays the whole debt (you need that many Mengers, minus the 0.4 gas reserve) and gets the ETH back
    function closeTrove() external {
        _load();
        vm.startBroadcast();
        (, user, ) = vm.readCallers();
        borrowerOperations.closeTrove();
        vm.stopBroadcast();
        status();
    }

    // Deposits Mengers into the Stability Pool
    function deposit(uint _mengers) external {
        _load();
        vm.startBroadcast();
        (, user, ) = vm.readCallers();
        stabilityPool.provideToSP(_mengers, address(0));
        vm.stopBroadcast();
        status();
    }

    // Withdraws Mengers from the Stability Pool and claims the gains. 0 claims the gains only.
    function withdraw(uint _mengers) external {
        _load();
        vm.startBroadcast();
        (, user, ) = vm.readCallers();
        stabilityPool.withdrawFromSP(_mengers);
        vm.stopBroadcast();
        status();
    }

    // Liquidates up to 10 troves below 110 % (anyone may do it, and earns the gas reserve plus 0.5 % of the collateral)
    function liquidate() external {
        _load();
        vm.startBroadcast();
        (, user, ) = vm.readCallers();
        troveManager.liquidateTroves(10);
        vm.stopBroadcast();
        status();
    }

    // Exchanges _mengers for their value in ETH, at the gold price, minus the redemption fee
    function redeem(uint _mengers) external {
        _load();
        uint price = priceFeed.fetchPrice();
        (address firstHint, uint partialNICR, uint truncated) = hintHelpers.getRedemptionHints(_mengers, price, 0);
        require(truncated > 0, "Demo: nothing can be redeemed for this amount");
        (address upperHint, address lowerHint) = sortedTroves.findInsertPosition(partialNICR, address(0), address(0));
        vm.startBroadcast();
        (, user, ) = vm.readCallers();
        troveManager.redeemCollateral(truncated, firstHint, upperHint, lowerHint, partialNICR, 0, 1e18);
        vm.stopBroadcast();
        console.log("Redeemed Mengers:", truncated);
        status();
    }

    // block.chainid only exists from Solidity 0.8
    function _chainId() internal pure returns (uint id) {
        assembly { id := chainid() }
    }
}
