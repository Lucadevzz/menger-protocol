// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import {Script} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {console} from "forge-std/console.sol";

import {ActivePool} from "../src/ActivePool.sol";
import {BorrowerOperations} from "../src/BorrowerOperations.sol";
import {CollSurplusPool} from "../src/CollSurplusPool.sol";
import {DefaultPool} from "../src/DefaultPool.sol";
import {GasPool} from "../src/GasPool.sol";
import {HintHelpers} from "../src/HintHelpers.sol";
import {LUSDToken} from "../src/LUSDToken.sol";
import {MultiTroveGetter} from "../src/MultiTroveGetter.sol";
import {SortedTroves} from "../src/SortedTroves.sol";
import {StabilityPool} from "../src/StabilityPool.sol";
import {TroveManager} from "../src/TroveManager.sol";
import {MengerPriceFeed} from "../src/MengerPriceFeed.sol";
import {StabilityPoolFeeIssuance} from "../src/StabilityPoolFeeIssuance.sol";
import {TestnetAggregator} from "./TestnetAggregator.sol";

/*
* Deploys and wires the whole Menger system, then writes the addresses to deployments/<chain id>.json.
* Every contract renounces its owner once wired: nobody, including the deployer, can change anything afterwards.
* See DEPLOIEMENT.md.
*
* Environment variables (all optional):
*   USE_TESTNET_FEEDS   true (default): deploys four price feeds controlled by the deployer (test networks only).
*                       false: uses the feeds given below.
*   ETH_USD_CENTS       starting ETH price of the test feeds, in US cents (default 400000 = 4,000 USD)
*   XAU_USD_CENTS       starting gold price of the test feeds, per troy ounce, in US cents (default 431900)
*   ETH_USD_PRIMARY, ETH_USD_SECONDARY, XAU_USD_PRIMARY, XAU_USD_SECONDARY   feed addresses, if USE_TESTNET_FEEDS=false
*   GOLD_MAX_AGE        seconds after which a gold feed counts as stale for redemptions (default 90000 = 25 hours)
*/
contract DeployMenger is Script {
    struct Core {
        MengerPriceFeed priceFeed;
        SortedTroves sortedTroves;
        TroveManager troveManager;
        ActivePool activePool;
        StabilityPool stabilityPool;
        GasPool gasPool;
        DefaultPool defaultPool;
        CollSurplusPool collSurplusPool;
        BorrowerOperations borrowerOperations;
        HintHelpers hintHelpers;
        StabilityPoolFeeIssuance feeIssuance;
        LUSDToken menger;
        MultiTroveGetter multiTroveGetter;
    }

    address[4] feeds; // ETH primary, ETH secondary, gold primary, gold secondary

    function run() external {
        bool testnetFeeds = vm.envOr("USE_TESTNET_FEEDS", true);
        uint goldMaxAge = vm.envOr("GOLD_MAX_AGE", uint(25 hours));

        vm.startBroadcast();

        if (testnetFeeds) {
            require(_chainId() != 1, "DeployMenger: test feeds on Ethereum mainnet");
            int ethAnswer = int(vm.envOr("ETH_USD_CENTS", uint(400000)) * 1e6);  // 8 decimals, like Chainlink
            int xauAnswer = int(vm.envOr("XAU_USD_CENTS", uint(431900)) * 1e6);
            feeds[0] = address(new TestnetAggregator(8, "ETH / USD (test, primary)", ethAnswer));
            feeds[1] = address(new TestnetAggregator(8, "ETH / USD (test, secondary)", ethAnswer));
            feeds[2] = address(new TestnetAggregator(8, "XAU / USD (test, primary)", xauAnswer));
            feeds[3] = address(new TestnetAggregator(8, "XAU / USD (test, secondary)", xauAnswer));
        } else {
            feeds[0] = vm.envAddress("ETH_USD_PRIMARY");
            feeds[1] = vm.envAddress("ETH_USD_SECONDARY");
            feeds[2] = vm.envAddress("XAU_USD_PRIMARY");
            feeds[3] = vm.envAddress("XAU_USD_SECONDARY");
        }

        Core memory c = _deployCore(goldMaxAge);
        _wire(c);

        vm.stopBroadcast();

        _report(c, testnetFeeds, goldMaxAge);
    }

    function _deployCore(uint _goldMaxAge) internal returns (Core memory c) {
        c.priceFeed = new MengerPriceFeed(feeds[0], feeds[1], feeds[2], feeds[3], _goldMaxAge);
        c.sortedTroves = new SortedTroves();
        c.troveManager = new TroveManager();
        c.activePool = new ActivePool();
        c.stabilityPool = new StabilityPool();
        c.gasPool = new GasPool();
        c.defaultPool = new DefaultPool();
        c.collSurplusPool = new CollSurplusPool();
        c.borrowerOperations = new BorrowerOperations();
        c.hintHelpers = new HintHelpers();
        c.feeIssuance = new StabilityPoolFeeIssuance();
        c.menger = new LUSDToken(address(c.troveManager), address(c.stabilityPool), address(c.borrowerOperations));
        c.multiTroveGetter = new MultiTroveGetter(c.troveManager, c.sortedTroves);
    }

    // Same wiring as test/MengerTestBase.sol. Each setAddresses call renounces that contract's owner.
    function _wire(Core memory c) internal {
        c.sortedTroves.setParams(uint(-1), address(c.troveManager), address(c.borrowerOperations));
        c.troveManager.setAddresses(
            address(c.borrowerOperations), address(c.activePool), address(c.defaultPool), address(c.stabilityPool),
            address(c.gasPool), address(c.collSurplusPool), address(c.priceFeed), address(c.menger), address(c.sortedTroves)
        );
        c.borrowerOperations.setAddresses(
            address(c.troveManager), address(c.activePool), address(c.defaultPool), address(c.stabilityPool),
            address(c.gasPool), address(c.collSurplusPool), address(c.priceFeed), address(c.sortedTroves), address(c.menger),
            address(c.feeIssuance)
        );
        c.stabilityPool.setAddresses(
            address(c.borrowerOperations), address(c.troveManager), address(c.activePool), address(c.menger),
            address(c.sortedTroves), address(c.priceFeed), address(c.feeIssuance)
        );
        c.activePool.setAddresses(address(c.borrowerOperations), address(c.troveManager), address(c.stabilityPool), address(c.defaultPool));
        c.defaultPool.setAddresses(address(c.troveManager), address(c.activePool));
        c.collSurplusPool.setAddresses(address(c.borrowerOperations), address(c.troveManager), address(c.activePool));
        c.hintHelpers.setAddresses(address(c.sortedTroves), address(c.troveManager));
        c.feeIssuance.setAddresses(address(c.menger), address(c.stabilityPool));
    }

    function _report(Core memory c, bool _testnetFeeds, uint _goldMaxAge) internal {
        string memory k = "menger";
        vm.serializeUint(k, "chainId", _chainId());
        vm.serializeBool(k, "testnetFeeds", _testnetFeeds);
        vm.serializeUint(k, "goldMaxAge", _goldMaxAge);
        vm.serializeAddress(k, "ethUsdPrimary", feeds[0]);
        vm.serializeAddress(k, "ethUsdSecondary", feeds[1]);
        vm.serializeAddress(k, "xauUsdPrimary", feeds[2]);
        vm.serializeAddress(k, "xauUsdSecondary", feeds[3]);
        vm.serializeAddress(k, "priceFeed", address(c.priceFeed));
        vm.serializeAddress(k, "mengerToken", address(c.menger));
        vm.serializeAddress(k, "borrowerOperations", address(c.borrowerOperations));
        vm.serializeAddress(k, "troveManager", address(c.troveManager));
        vm.serializeAddress(k, "stabilityPool", address(c.stabilityPool));
        vm.serializeAddress(k, "stabilityPoolFeeIssuance", address(c.feeIssuance));
        vm.serializeAddress(k, "activePool", address(c.activePool));
        vm.serializeAddress(k, "defaultPool", address(c.defaultPool));
        vm.serializeAddress(k, "collSurplusPool", address(c.collSurplusPool));
        vm.serializeAddress(k, "gasPool", address(c.gasPool));
        vm.serializeAddress(k, "sortedTroves", address(c.sortedTroves));
        vm.serializeAddress(k, "hintHelpers", address(c.hintHelpers));
        string memory json = vm.serializeAddress(k, "multiTroveGetter", address(c.multiTroveGetter));

        // Only a real broadcast records addresses: a dry run (no --broadcast, or no --rpc-url) deploys nothing
        if (!vm.isContext(VmSafe.ForgeContext.ScriptBroadcast) && !vm.isContext(VmSafe.ForgeContext.ScriptResume)) {
            console.log("DRY RUN: nothing was deployed. Add --rpc-url <network> and --broadcast to deploy.");
            return;
        }
        string memory path = string(abi.encodePacked("deployments/", vm.toString(_chainId()), ".json"));
        vm.writeJson(json, path);

        console.log("Menger deployed. Addresses written to", path);
        console.log("  Menger token (MNGR)       ", address(c.menger));
        console.log("  BorrowerOperations      ", address(c.borrowerOperations));
        console.log("  TroveManager            ", address(c.troveManager));
        console.log("  StabilityPool           ", address(c.stabilityPool));
        console.log("  MengerPriceFeed          ", address(c.priceFeed));
        console.log("  HintHelpers             ", address(c.hintHelpers));
        console.log("  Price of 1 ETH in Mengers", c.priceFeed.lastGoodPrice());
    }

    // block.chainid only exists from Solidity 0.8
    function _chainId() internal pure returns (uint id) {
        assembly { id := chainid() }
    }
}
