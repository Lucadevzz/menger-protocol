// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

// Selective imports: Liquity's own console.sol would clash with forge-std's console library.
import {Test} from "forge-std/Test.sol";

import {ActivePool} from "../src/ActivePool.sol";
import {BorrowerOperations} from "../src/BorrowerOperations.sol";
import {CollSurplusPool} from "../src/CollSurplusPool.sol";
import {DefaultPool} from "../src/DefaultPool.sol";
import {GasPool} from "../src/GasPool.sol";
import {HintHelpers} from "../src/HintHelpers.sol";
import {LUSDToken} from "../src/LUSDToken.sol";
import {SortedTroves} from "../src/SortedTroves.sol";
import {StabilityPool} from "../src/StabilityPool.sol";
import {TroveManager} from "../src/TroveManager.sol";
import {MengerPriceFeed} from "../src/MengerPriceFeed.sol";
import {StabilityPoolFeeIssuance} from "../src/StabilityPoolFeeIssuance.sol";
import {MockAggregator} from "./mocks/MockAggregator.sol";

// Deploys and wires the whole Menger system on a simulated chain, with mock price feeds.
contract MengerTestBase is Test {
    // Wednesday 7 October 2026, 12:00 UTC
    uint constant WEDNESDAY_NOON = 1791374400;
    // Wednesday 21 October 2026, 12:00 UTC: after the 14-day redemption bootstrap period
    uint constant AFTER_BOOTSTRAP = 1792584000;
    uint constant GOLD_MAX_AGE = 1 hours;
    // Fees are released to Stability Pool depositors over this period (StabilityPool and StabilityPoolFeeIssuance)
    uint constant FEE_RELEASE_PERIOD = 7 days;

    uint constant ETH_USD = 4000;   // USD per ETH
    uint constant XAU_USD = 4319;   // USD per troy ounce of gold
    // ETH/USD / (XAU/USD x 3.545 / 31.1034768), computed exactly: 8.1258628... Mengers per ETH
    uint constant EXPECTED_PRICE = 8125862807792249355;

    string constant REDEMPTION_SUSPENDED = "TroveManager: Redemptions suspended (gold market closed or price unreliable)";

    MockAggregator ethPrimary;
    MockAggregator ethSecondary;
    MockAggregator xauPrimary;
    MockAggregator xauSecondary;

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
    LUSDToken menger;
    StabilityPoolFeeIssuance feeIssuance;

    address alice = address(0xA11CE);
    address bob = address(0xB0B);
    address carol = address(0xCA201);
    address dave = address(0xDA7E);
    address erin = address(0xE1219);

    function setUp() public virtual {
        _deploySystem(WEDNESDAY_NOON, ETH_USD * 100, XAU_USD * 100);
    }

    // Deploys and wires the whole system at a given time, with given ETH/USD and gold/USD prices (in US cents)
    function _deploySystem(uint _timestamp, uint _ethUsdCents, uint _xauUsdCents) internal {
        _deploySystemWithGoldMaxAge(_timestamp, _ethUsdCents, _xauUsdCents, GOLD_MAX_AGE);
    }

    function _deploySystemWithGoldMaxAge(uint _timestamp, uint _ethUsdCents, uint _xauUsdCents, uint _goldMaxAge) internal {
        vm.warp(_timestamp);

        ethPrimary = new MockAggregator(8, int(_ethUsdCents * 1e6), block.timestamp);
        ethSecondary = new MockAggregator(8, int(_ethUsdCents * 1e6), block.timestamp);
        xauPrimary = new MockAggregator(8, int(_xauUsdCents * 1e6), block.timestamp);
        xauSecondary = new MockAggregator(8, int(_xauUsdCents * 1e6), block.timestamp);

        priceFeed = new MengerPriceFeed(
            address(ethPrimary), address(ethSecondary), address(xauPrimary), address(xauSecondary), _goldMaxAge
        );

        sortedTroves = new SortedTroves();
        troveManager = new TroveManager();
        activePool = new ActivePool();
        stabilityPool = new StabilityPool();
        gasPool = new GasPool();
        defaultPool = new DefaultPool();
        collSurplusPool = new CollSurplusPool();
        borrowerOperations = new BorrowerOperations();
        hintHelpers = new HintHelpers();
        feeIssuance = new StabilityPoolFeeIssuance();
        menger = new LUSDToken(address(troveManager), address(stabilityPool), address(borrowerOperations));

        sortedTroves.setParams(uint(-1), address(troveManager), address(borrowerOperations));
        troveManager.setAddresses(
            address(borrowerOperations), address(activePool), address(defaultPool), address(stabilityPool),
            address(gasPool), address(collSurplusPool), address(priceFeed), address(menger), address(sortedTroves)
        );
        borrowerOperations.setAddresses(
            address(troveManager), address(activePool), address(defaultPool), address(stabilityPool),
            address(gasPool), address(collSurplusPool), address(priceFeed), address(sortedTroves), address(menger),
            address(feeIssuance)
        );
        stabilityPool.setAddresses(
            address(borrowerOperations), address(troveManager), address(activePool), address(menger),
            address(sortedTroves), address(priceFeed), address(feeIssuance)
        );
        activePool.setAddresses(address(borrowerOperations), address(troveManager), address(stabilityPool), address(defaultPool));
        defaultPool.setAddresses(address(troveManager), address(activePool));
        collSurplusPool.setAddresses(address(borrowerOperations), address(troveManager), address(activePool));
        hintHelpers.setAddresses(address(sortedTroves), address(troveManager));
        feeIssuance.setAddresses(address(menger), address(stabilityPool));

        vm.deal(alice, 1000 ether);
        vm.deal(bob, 1000 ether);
        vm.deal(carol, 1000 ether);
        vm.deal(dave, 1000 ether);
        vm.deal(erin, 1000 ether);
    }

    // --- Helpers ---

    function _openTrove(address _user, uint _collETH, uint _mengers) internal {
        vm.prank(_user);
        borrowerOperations.openTrove{value: _collETH}(1e18, _mengers, address(0), address(0));
    }

    function _deposit(address _user, uint _mengers) internal {
        vm.prank(_user);
        stabilityPool.provideToSP(_mengers, address(0));
    }

    // Claims a depositor's gains (fees in Mengers, ETH) without withdrawing any of the deposit
    function _claim(address _user) internal {
        vm.prank(_user);
        stabilityPool.withdrawFromSP(0);
    }

    // Lets a full fee release period pass, then has _user claim: every fee pending before is released
    function _waitReleasePeriodAndClaim(address _user) internal {
        _warpAndRefresh(block.timestamp + FEE_RELEASE_PERIOD);
        _claim(_user);
    }

    function _setEthPrice(uint _usdCents) internal {
        // _usdCents: price in US cents, e.g. 400000 = 4,000.00 USD
        ethPrimary.setAnswer(int(_usdCents * 1e6), block.timestamp);
        ethSecondary.setAnswer(int(_usdCents * 1e6), block.timestamp);
    }

    function _setGoldPrice(uint _usdCents) internal {
        xauPrimary.setAnswer(int(_usdCents * 1e6), block.timestamp);
        xauSecondary.setAnswer(int(_usdCents * 1e6), block.timestamp);
    }

    // Publish the new price in two identical rounds. The price feed treats a move of more than 50 % in one round as
    // a broken feed (as Liquity V1 did); two rounds make the move acceptable, as if the feed had got there gradually.
    function _settleEthPrice(uint _usdCents) internal {
        _setEthPrice(_usdCents);
        _setEthPrice(_usdCents);
    }

    function _settleGoldPrice(uint _usdCents) internal {
        _setGoldPrice(_usdCents);
        _setGoldPrice(_usdCents);
    }

    function _refreshFeeds() internal {
        ethPrimary.setUpdatedAt(block.timestamp);
        ethSecondary.setUpdatedAt(block.timestamp);
        xauPrimary.setUpdatedAt(block.timestamp);
        xauSecondary.setUpdatedAt(block.timestamp);
    }

    function _warpAndRefresh(uint _timestamp) internal {
        vm.warp(_timestamp);
        _refreshFeeds();
    }

    // Redeems like a front end would: HintHelpers may truncate the amount when a partial redemption would leave
    // a trove below the minimum debt. Returns the amount actually requested from the TroveManager.
    function _redeem(address _user, uint _mengers) internal returns (uint) {
        uint price = priceFeed.fetchPrice();
        (address firstHint, uint partialNICR, uint truncated) = hintHelpers.getRedemptionHints(_mengers, price, 0);
        if (truncated == 0) { return 0; }
        (address upperHint, address lowerHint) = sortedTroves.findInsertPosition(partialNICR, address(0), address(0));
        vm.prank(_user);
        troveManager.redeemCollateral(truncated, firstHint, upperHint, lowerHint, partialNICR, 0, 1e18);
        return truncated;
    }

    function _redeemExpectRevert(address _user, uint _mengers, string memory _reason) internal {
        uint price = priceFeed.fetchPrice();
        (address firstHint, uint partialNICR, ) = hintHelpers.getRedemptionHints(_mengers, price, 0);
        (address upperHint, address lowerHint) = sortedTroves.findInsertPosition(partialNICR, address(0), address(0));
        vm.expectRevert(bytes(_reason));
        vm.prank(_user);
        troveManager.redeemCollateral(_mengers, firstHint, upperHint, lowerHint, partialNICR, 0, 1e18);
    }

    function _assertSupplyEqualsDebt() internal {
        assertEq(menger.totalSupply(), troveManager.getEntireSystemDebt(), "Menger supply must equal system debt");
    }

    // Price of 1 ETH in Mengers for given USD prices, computed independently of the contract
    function _expectedPrice(uint _ethUsd, uint _xauUsd) internal pure returns (uint) {
        return _ethUsd * 1e18 * 1e18 * 311034768 / (_xauUsd * 1e18 * 35450000);
    }

    // The test contract liquidates troves and receives the liquidation reward in ETH
    receive() external payable {}
}
