// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {TestnetAggregator} from "./TestnetAggregator.sol";
import {MengerPriceFeed} from "../src/MengerPriceFeed.sol";

/*
* TEST NETWORKS ONLY. Sets the prices of the four test feeds deployed by DeployMenger.s.sol (both ETH feeds and both
* gold feeds at once), reading their addresses from deployments/<chain id>.json. See DEPLOIEMENT.md.
*
*   ETH_USD_CENTS   new ETH price in US cents (e.g. 250000 for 2,500 USD). Unchanged if not set (but republished).
*   XAU_USD_CENTS   new gold price per troy ounce in US cents. Unchanged if not set (but republished).
*
* The price feed treats a feed whose price moves by more than 50 % in one round as broken, as Liquity V1 did. Large
* moves are therefore published in several rounds of at most -40 % or +60 % each, as a real feed would.
*/
contract SetTestnetPrices is Script {
    function run() external {
        string memory json = vm.readFile(string(abi.encodePacked("deployments/", vm.toString(_chainId()), ".json")));
        require(vm.parseJsonBool(json, ".testnetFeeds"), "SetTestnetPrices: this deployment does not use test feeds");

        TestnetAggregator ethA = TestnetAggregator(vm.parseJsonAddress(json, ".ethUsdPrimary"));
        TestnetAggregator ethB = TestnetAggregator(vm.parseJsonAddress(json, ".ethUsdSecondary"));
        TestnetAggregator xauA = TestnetAggregator(vm.parseJsonAddress(json, ".xauUsdPrimary"));
        TestnetAggregator xauB = TestnetAggregator(vm.parseJsonAddress(json, ".xauUsdSecondary"));
        MengerPriceFeed priceFeed = MengerPriceFeed(vm.parseJsonAddress(json, ".priceFeed"));

        uint ethCents = vm.envOr("ETH_USD_CENTS", uint(0));
        uint xauCents = vm.envOr("XAU_USD_CENTS", uint(0));

        vm.startBroadcast();
        _publish(ethA, ethB, ethCents);
        _publish(xauA, xauB, xauCents);
        uint price = priceFeed.fetchPrice();
        vm.stopBroadcast();

        console.log("ETH/USD (8 decimals)    ", uint(ethA.answer()));
        console.log("XAU/USD (8 decimals)    ", uint(xauA.answer()));
        console.log("Price of 1 ETH in Mengers", price);
        console.log("Redemptions allowed now ", priceFeed.isRedemptionAllowed());
    }

    // Publishes the new price on both feeds of a pair, in steps if needed; republishes the current one if 0
    function _publish(TestnetAggregator _a, TestnetAggregator _b, uint _cents) internal {
        if (_cents == 0) {
            _a.refresh();
            _b.refresh();
            return;
        }
        int target = int(_cents * 1e6); // 8 decimals, like Chainlink
        int current = _a.answer();
        while (current != target) {
            int next = target;
            if (target * 10 < current * 6) { next = current * 6 / 10; }       // at most -40 % per round
            else if (target * 10 > current * 16) { next = current * 16 / 10; } // at most +60 % per round
            _a.setAnswer(next);
            _b.setAnswer(next);
            current = next;
        }
    }

    // block.chainid only exists from Solidity 0.8
    function _chainId() internal pure returns (uint id) {
        assembly { id := chainid() }
    }
}
