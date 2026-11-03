// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "./MengerTestBase.sol";

// A feed without round history: getRoundData is not supported, as in some non-Chainlink adapters
contract NoHistoryAggregator {
    uint8 public decimals = 8;
    int256 public answer;
    uint256 public updatedAt;

    constructor(int256 _answer, uint256 _updatedAt) public { answer = _answer; updatedAt = _updatedAt; }

    function latestRoundData() external view returns (uint80, int256, uint256, uint256, uint80) {
        return (7, answer, updatedAt, updatedAt, 7);
    }

    function getRoundData(uint80) external pure returns (uint80, int256, uint256, uint256, uint80) {
        revert("GetRoundDataNotSupported");
    }
}

// Behaves like a Chainlink proxy across a phase change: the first round of a new phase has no predecessor in that
// phase (the aggregator returns zeros, as OCR aggregators do). Written by the independent review.
contract PhasedAggregator {
    uint8 public decimals = 8;
    uint16 public phase = 1;
    uint64 public aggRound = 2;
    mapping(uint16 => mapping(uint64 => int256)) public answers;
    mapping(uint16 => mapping(uint64 => uint256)) public times;

    constructor(int256 _answer, uint256 _t) public {
        answers[1][1] = _answer; times[1][1] = _t;
        answers[1][2] = _answer; times[1][2] = _t;
    }

    function publish(int256 _answer, uint256 _t) external {
        aggRound++;
        answers[phase][aggRound] = _answer;
        times[phase][aggRound] = _t;
    }

    function startNewPhase(int256 _answer, uint256 _t) external {
        phase++;
        aggRound = 1;
        answers[phase][1] = _answer;
        times[phase][1] = _t;
    }

    function latestRoundData() external view returns (uint80, int256, uint256, uint256, uint80) {
        uint80 id = uint80((uint(phase) << 64) | aggRound);
        return (id, answers[phase][aggRound], times[phase][aggRound], times[phase][aggRound], id);
    }

    function getRoundData(uint80 _id) external view returns (uint80, int256, uint256, uint256, uint80) {
        uint16 p = uint16(_id >> 64);
        uint64 r = uint64(_id);
        return (_id, answers[p][r], times[p][r], times[p][r], _id);
    }
}

/*
* Oracle failures found by the second independent review, now handled:
* - N-1: a primary that jumped to a wrong value was trusted again from its next round, once it repeated the value;
* - N-2: on a weekend, the gold secondary alone could move the gold price;
* - N-3: an ETH secondary without round history was silently ignored;
* - N-5: a hostile feed could make fetchPrice() run out of gas.
*/
contract MengerOracleFailureTest is MengerTestBase {
    address eve = address(0xE7E);

    function _refreshAll() internal {
        ethPrimary.setUpdatedAt(block.timestamp);
        ethSecondary.setUpdatedAt(block.timestamp);
        xauPrimary.setUpdatedAt(block.timestamp);
        xauSecondary.setUpdatedAt(block.timestamp);
    }

    // N-1: the primary breaks and repeats its wrong value. It stays distrusted until it agrees with the secondary again.
    function test_primaryThatJumped_staysDistrustedUntilItAgreesAgain() public {
        _openTrove(bob, 100 ether, 200e18);
        _openTrove(alice, 10 ether, 40e18);                   // 200 %
        _deposit(bob, 150e18);

        ethPrimary.setAnswer(int(100e8), block.timestamp);    // the primary breaks: 100 USD, -97.5 % in one round
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);     // the secondary's 4,000
        assertTrue(priceFeed.ethPrimaryUntrusted());

        vm.warp(block.timestamp + 1 hours);                   // next round: the same wrong value
        ethPrimary.setAnswer(int(100e8), block.timestamp);
        ethSecondary.setUpdatedAt(block.timestamp);
        xauPrimary.setUpdatedAt(block.timestamp);
        xauSecondary.setUpdatedAt(block.timestamp);
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);     // still the secondary
        (, bool reliable) = priceFeed.getEthUsdPrice();
        assertFalse(reliable);                                // and no redemptions
        vm.expectRevert(bytes("TroveManager: nothing to liquidate"));
        troveManager.liquidateTroves(10);                     // Alice's 200 % trove is safe

        ethPrimary.setAnswer(int(4000e8), block.timestamp);   // repaired: agrees with the secondary again
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);
        assertFalse(priceFeed.ethPrimaryUntrusted());
        ethPrimary.setAnswer(int(4010e8), block.timestamp);   // from its next round, the primary is followed
        assertEq(priceFeed.fetchPrice(), _expectedPrice(4010, XAU_USD));
    }

    // N-1, other direction: a primary stuck 10x too high cannot be used to borrow against overvalued collateral
    function test_primaryStuckTooHigh_cannotBeBorrowedAgainst() public {
        _openTrove(bob, 100 ether, 200e18);
        ethPrimary.setAnswer(int(40000e8), block.timestamp);
        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);
        vm.warp(block.timestamp + 1 hours);
        ethPrimary.setAnswer(int(40000e8), block.timestamp);
        ethSecondary.setUpdatedAt(block.timestamp);
        xauPrimary.setUpdatedAt(block.timestamp);
        xauSecondary.setUpdatedAt(block.timestamp);

        vm.deal(eve, 10 ether);
        vm.expectRevert(bytes("BorrowerOps: An operation that would result in ICR < MCR is not permitted"));
        vm.prank(eve);
        borrowerOperations.openTrove{value: 10 ether}(1e18, 700e18, address(0), address(0));
    }

    // N-2: on a weekend a quiet gold primary is normal; the secondary alone does not move the gold price.
    // Once the market has reopened, a primary that still has not updated is lagging, and the secondary is followed.
    function test_weekend_goldSecondaryAloneCannotMoveThePrice() public {
        _deploySystemWithGoldMaxAge(WEDNESDAY_NOON, ETH_USD * 100, XAU_USD * 100, 25 hours); // as the deployment script
        _openTrove(bob, 100 ether, 200e18);
        _openTrove(alice, 10 ether, 64e18);                   // 125.6 %
        _deposit(bob, 150e18);

        vm.warp(WEDNESDAY_NOON + 2 days + 8 hours);           // Friday 9 October, 20:00 UTC: last gold update
        _refreshAll();
        vm.warp(block.timestamp + 26 hours);                  // Saturday 22:00 UTC
        assertTrue(priceFeed.isGoldMarketClosedAt(block.timestamp));
        ethPrimary.setUpdatedAt(block.timestamp);
        ethSecondary.setUpdatedAt(block.timestamp);
        xauSecondary.setAnswer(int(4967e8), block.timestamp); // the secondary alone: +15 %

        assertEq(priceFeed.fetchPrice(), EXPECTED_PRICE);     // gold stays at Friday's price
        vm.expectRevert(bytes("TroveManager: nothing to liquidate"));
        troveManager.liquidateTroves(10);

        vm.warp(block.timestamp + 27 hours);                  // Monday 01:00 UTC, the primary still quiet
        ethPrimary.setUpdatedAt(block.timestamp);
        ethSecondary.setUpdatedAt(block.timestamp);
        xauSecondary.setUpdatedAt(block.timestamp);
        assertEq(priceFeed.fetchPrice(), _expectedPrice(ETH_USD, 4967));
        assertFalse(priceFeed.isRedemptionAllowed());
    }

    // N-3: every feed must support getRoundData; one that does not is refused at deployment
    function test_constructor_rejectsFeedWithoutRoundHistory() public {
        NoHistoryAggregator noHistory = new NoHistoryAggregator(int(4000e8), block.timestamp);
        vm.expectRevert(bytes("MengerPriceFeed: ETH feeds not working or not agreeing"));
        new MengerPriceFeed(address(ethPrimary), address(noHistory), address(xauPrimary), address(xauSecondary), GOLD_MAX_AGE);
    }

    function test_constructor_rejectsEthFeedsThatDisagree() public {
        MockAggregator otherEth = new MockAggregator(8, int(3200e8), block.timestamp);
        vm.expectRevert(bytes("MengerPriceFeed: ETH feeds not working or not agreeing"));
        new MengerPriceFeed(address(ethPrimary), address(otherEth), address(xauPrimary), address(xauSecondary), GOLD_MAX_AGE);
    }

    // Written by the review: at a Chainlink phase change the first round of the new phase cannot be checked, so the
    // other feed is used for that round only; nothing reverts and nothing stays distrusted.
    function test_phaseBoundary_oneRoundOnTheOtherFeed() public {
        PhasedAggregator phased = new PhasedAggregator(int(4319e8), block.timestamp);
        MengerPriceFeed pf = new MengerPriceFeed(
            address(ethPrimary), address(ethSecondary), address(phased), address(xauSecondary), GOLD_MAX_AGE
        );
        assertTrue(pf.isRedemptionAllowed());
        phased.startNewPhase(int(4320e8), block.timestamp);
        xauSecondary.setAnswer(int(4330e8), block.timestamp);
        assertEq(pf.fetchPrice(), _expectedPrice(ETH_USD, 4330));
        assertFalse(pf.isRedemptionAllowed());
        phased.publish(int(4321e8), block.timestamp);
        assertEq(pf.fetchPrice(), _expectedPrice(ETH_USD, 4321));
        assertTrue(pf.isRedemptionAllowed());
        assertFalse(pf.xauPrimaryUntrusted());
    }

    // N-5: a feed returning a huge buffer, or burning all the gas it gets, cannot block the price
    function test_hostileFeeds_cannotBlockFetchPrice() public {
        ethSecondary.setReturnBomb(true);
        (bool ok, bytes memory data) = address(priceFeed).call{gas: 10000000}(abi.encodeWithSignature("fetchPrice()"));
        assertTrue(ok);
        assertEq(abi.decode(data, (uint)), EXPECTED_PRICE);

        ethSecondary.setReturnBomb(false);
        ethPrimary.setReturnBomb(true);
        (ok, data) = address(priceFeed).call{gas: 10000000}(abi.encodeWithSignature("fetchPrice()"));
        assertTrue(ok);
        assertEq(abi.decode(data, (uint)), EXPECTED_PRICE);   // the secondary's identical 4,000

        ethPrimary.setReturnBomb(false);
        xauPrimary.setGasBurner(true);
        (ok, ) = address(priceFeed).call{gas: 3000000}(abi.encodeWithSignature("fetchPrice()"));
        assertTrue(ok);
    }
}

// Everything a feed can answer after a valid deployment: fetchPrice() and isRedemptionAllowed() never revert.
// Written by the independent review.
contract MengerOracleFuzzTest is MengerTestBase {
    function _feed(uint8 _decimals, uint _usd) internal returns (MockAggregator) {
        return new MockAggregator(_decimals, int(_usd * 10 ** uint(_decimals)), block.timestamp);
    }

    function _round(MockAggregator _f, int _previous, int _latest, uint _seed) internal {
        _f.setAnswer(_previous, block.timestamp - (_seed % 10 days));
        _f.setAnswer(_latest, block.timestamp - ((_seed >> 32) % 10 days));
        if (_seed % 7 == 0) { _f.setReverts(true); }
        if (_seed % 11 == 0) { _f.setMalformed(true); }
        if (_seed % 13 == 0) { _f.setUpdatedAt(block.timestamp + 1); }   // in the future
    }

    function testFuzz_fetchPriceNeverReverts(uint _decimals, int _a, int _b, int _c, int _d, uint _seed) public {
        vm.warp(1800000000);
        MockAggregator e1 = _feed(uint8(_decimals % 37), 4000);
        MockAggregator e2 = _feed(uint8((_decimals >> 8) % 37), 4000);
        MockAggregator x1 = _feed(uint8((_decimals >> 16) % 37), 4319);
        MockAggregator x2 = _feed(uint8((_decimals >> 24) % 37), 4319);
        MengerPriceFeed pf = new MengerPriceFeed(address(e1), address(e2), address(x1), address(x2), 25 hours);
        _round(e1, _a, _b, _seed);
        _round(e2, _b, _c, _seed >> 64);
        _round(x1, _c, _d, _seed >> 128);
        _round(x2, _d, _a, _seed >> 192);
        pf.fetchPrice();
        pf.isRedemptionAllowed();
        vm.warp(block.timestamp + (_seed % 30 days));
        pf.fetchPrice();
        pf.isRedemptionAllowed();
        pf.getEthUsdPrice();
        pf.getXauUsdPrice();
    }
}
