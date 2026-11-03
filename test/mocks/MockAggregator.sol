// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;

import "../../src/Dependencies/AggregatorV3Interface.sol";

// Chainlink-style price feed whose answer, timestamp and failure modes are set by the tests.
// It keeps the previous round, which the price feed compares with the latest one.
contract MockAggregator is AggregatorV3Interface {
    uint8 public override decimals;
    int256 public answer;
    uint256 public updatedAt;
    uint80 public roundId = 2;     // round 1 holds the same answer, so the first round has a valid predecessor
    int256 public previousAnswer;
    uint256 public previousUpdatedAt;
    bool public reverts;
    bool public malformed;         // returns 64 bytes instead of 160
    bool public returnBomb;        // returns a buffer as large as the gas it receives allows
    bool public gasBurner;         // uses all the gas it receives

    constructor(uint8 _decimals, int256 _answer, uint256 _updatedAt) public {
        decimals = _decimals;
        answer = _answer;
        updatedAt = _updatedAt;
        previousAnswer = _answer;
        previousUpdatedAt = _updatedAt;
    }

    // Publishes a new round
    function setAnswer(int256 _answer, uint256 _updatedAt) external {
        previousAnswer = answer;
        previousUpdatedAt = updatedAt;
        answer = _answer;
        updatedAt = _updatedAt;
        roundId++;
    }

    // Changes the timestamp of the latest round, without a new round
    function setUpdatedAt(uint256 _updatedAt) external {
        updatedAt = _updatedAt;
    }

    function setReverts(bool _reverts) external {
        reverts = _reverts;
    }

    function setMalformed(bool _malformed) external {
        malformed = _malformed;
    }

    function setReturnBomb(bool _returnBomb) external {
        returnBomb = _returnBomb;
    }

    function setGasBurner(bool _gasBurner) external {
        gasBurner = _gasBurner;
    }

    function description() external view override returns (string memory) {
        return "mock";
    }

    function version() external view override returns (uint256) {
        return 1;
    }

    function getRoundData(uint80 _roundId) external view override returns (uint80, int256, uint256, uint256, uint80) {
        _failIfAsked();
        if (_roundId == roundId) { return (roundId, answer, updatedAt, updatedAt, roundId); }
        if (_roundId == roundId - 1) { return (_roundId, previousAnswer, previousUpdatedAt, previousUpdatedAt, _roundId); }
        revert("No data present");
    }

    function latestRoundData() external view override returns (uint80, int256, uint256, uint256, uint80) {
        _failIfAsked();
        return (roundId, answer, updatedAt, updatedAt, roundId);
    }

    function _failIfAsked() internal view {
        require(!reverts, "MockAggregator: reverted");
        if (gasBurner) {
            while (gasleft() > 2000) {}
        }
        if (returnBomb) {
            uint words = _sqrt(gasleft() * 6 / 10 * 512); // memory costs about words^2 / 512 gas
            assembly { return(0, mul(words, 32)) }
        }
        if (malformed) {
            assembly {
                let ptr := mload(0x40)
                mstore(ptr, 1)
                mstore(add(ptr, 0x20), 1)
                return(ptr, 0x40)
            }
        }
    }

    function _sqrt(uint _x) internal pure returns (uint y) {
        y = _x;
        uint z = (_x + 1) / 2;
        while (z < y) { y = z; z = (_x / z + z) / 2; }
    }
}
