// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;

import "../src/Dependencies/AggregatorV3Interface.sol";

/*
* TEST NETWORKS ONLY. A Chainlink-style price feed whose price is set by its owner (the deployer), to demonstrate
* the system on a test network: change the gold price, make ETH crash, watch liquidations happen.
* Never use it with real money: whoever owns these feeds controls every price in the system.
*/
contract TestnetAggregator is AggregatorV3Interface {
    struct Round {
        int256 answer;
        uint256 updatedAt;
    }

    uint8 public override decimals;
    string public override description;
    address public immutable owner;

    uint80 public roundId;
    mapping(uint80 => Round) public rounds;

    event AnswerUpdated(int256 indexed current, uint256 indexed roundId, uint256 updatedAt);

    constructor(uint8 _decimals, string memory _description, int256 _answer) public {
        owner = msg.sender;
        decimals = _decimals;
        description = _description;
        // Two identical rounds: the price feed compares each round with the previous one
        _set(_answer);
        _set(_answer);
    }

    // Publishes a new price, timestamped now
    function setAnswer(int256 _answer) external {
        require(msg.sender == owner, "TestnetAggregator: caller is not the owner");
        _set(_answer);
    }

    // Publishes the same price again with a fresh timestamp, like a Chainlink heartbeat
    function refresh() external {
        require(msg.sender == owner, "TestnetAggregator: caller is not the owner");
        _set(rounds[roundId].answer);
    }

    function answer() external view returns (int256) {
        return rounds[roundId].answer;
    }

    function _set(int256 _answer) internal {
        require(_answer > 0, "TestnetAggregator: price must be positive");
        roundId++;
        rounds[roundId] = Round(_answer, block.timestamp);
        emit AnswerUpdated(_answer, roundId, block.timestamp);
    }

    function version() external view override returns (uint256) {
        return 1;
    }

    function getRoundData(uint80 _roundId) external view override returns (uint80, int256, uint256, uint256, uint80) {
        Round memory r = rounds[_roundId];
        require(r.updatedAt > 0, "No data present");
        return (_roundId, r.answer, r.updatedAt, r.updatedAt, _roundId);
    }

    function latestRoundData() external view override returns (uint80, int256, uint256, uint256, uint80) {
        Round memory r = rounds[roundId];
        return (roundId, r.answer, r.updatedAt, r.updatedAt, roundId);
    }
}
