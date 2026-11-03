// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;

import "./Interfaces/IPriceFeed.sol";
import "./Dependencies/AggregatorV3Interface.sol";
import "./Dependencies/SafeMath.sol";
import "./Dependencies/BaseMath.sol";

/*
* MENGER price feed. Replaces Liquity V1's PriceFeed (Chainlink + Tellor for ETH/USD).
*
* The system's debt unit is the Menger: 3.545 g of fine gold, the weight of the Venetian ducat (1284-1797).
* Every price used by TroveManager and BorrowerOperations is the price of 1 ETH in Mengers, with 18 decimals:
*
*     ETH/Menger = ETH/USD / ( XAU/USD x 3.545 / 31.1034768 )
*
* where XAU/USD is the price of one troy ounce (31.1034768 g) of gold.
*
* Sources: any Chainlink-compatible AggregatorV3Interface that supports getRoundData, fixed at deployment.
* No owner, no upgrade. For each of ETH/USD and XAU/USD, a primary and a secondary feed.
*
* Reading a feed. A feed's answer is valid when it is positive and in range (at most MAX_PRICE once scaled), with a
* sane timestamp. It is LIVE when recent enough: 4 hours for ETH, 4 days for gold (the gold market closes on
* weekends and holidays, and its last price stays the market price). As in Liquity V1, each answer is compared with
* the feed's previous round: a move of more than 50 % in one round means the feed is broken.
*
* Choosing the price (fetchPrice(), which never reverts, so that liquidations always remain possible):
*  - Each pair follows its primary, as Liquity V1 followed Chainlink. A disagreement with the secondary does not
*    change the price; it only suspends redemptions.
*  - A primary that jumped is distrusted, and stays distrusted until it agrees with the secondary again (Liquity V1's
*    rule). Meanwhile the secondary is used. This is the only state kept besides the last good prices.
*  - The secondary also takes over when the primary is frozen or broken, and, for gold, when the primary is lagging:
*    the two disagree, only the secondary was updated within GOLD_MAX_AGE, and the gold market is open.
*  - When neither feed can be used, the last good price is kept.
*
* isRedemptionAllowed() is stricter: the secondary feeds must confirm the primaries. TroveManager refuses
* redemptions unless:
*  - the gold market is open: no redemptions from Friday 21:00 UTC to Monday 00:00 UTC. The gold market closes on
*    Friday at 17:00 and reopens on Sunday at 18:00, New York time; the UTC window covers both summer and winter
*    time, plus at least one hour of margin after the reopening;
*  - both gold feeds can be used, agree within 1 %, and were both updated within GOLD_MAX_AGE;
*  - the ETH price comes from a feed, and the other ETH feed does not contradict it by more than 5 %.
*/
contract MengerPriceFeed is IPriceFeed, BaseMath {
    using SafeMath for uint;

    string constant public NAME = "MengerPriceFeed";

    // --- Gold weights, in units of 1e-7 gram ---

    uint constant public MENGER_WEIGHT = 35450000;       // 3.545 g of fine gold
    uint constant public TROY_OUNCE_WEIGHT = 311034768;  // 31.1034768 g

    // --- Parameters ---

    uint constant public TARGET_DIGITS = 18;

    // Prices above this (in USD, 18 decimals) are treated as a broken answer: 1e18 USD per unit
    uint constant public MAX_PRICE = 1e36;

    // Gas given to each call to a feed: far above what a price feed needs, low enough that a hostile feed cannot
    // make fetchPrice() run out of gas
    uint constant public FEED_CALL_GAS = 500000;

    // An ETH/USD feed older than this is frozen (same value as Liquity V1)
    uint constant public ETH_TIMEOUT = 4 hours;

    // A gold feed older than this is frozen: longer than any market closure (Good Friday to Monday is 75 hours)
    uint constant public GOLD_TIMEOUT = 4 days;

    // A feed whose answer moved by more than this since its previous round is broken (same value as Liquity V1)
    uint constant public MAX_ROUND_CHANGE = 5e17; // 50%

    // Maximum relative difference between the two ETH/USD feeds for redemptions (Liquity V1: Chainlink vs Tellor)
    uint constant public MAX_ETH_DEVIATION = 5e16; // 5%

    // Maximum relative difference between the two XAU/USD feeds for redemptions
    uint constant public MAX_GOLD_DEVIATION = 1e16; // 1%

    // Redemptions are suspended from Friday 21:00 UTC until Monday 00:00 UTC
    uint constant public WEEKEND_START_ON_FRIDAY = 75600; // 21 hours

    // Redemptions are suspended if a gold feed was not updated within this delay (set at deployment)
    uint public immutable GOLD_MAX_AGE;

    AggregatorV3Interface public immutable ethUsdPrimary;
    AggregatorV3Interface public immutable ethUsdSecondary;
    AggregatorV3Interface public immutable xauUsdPrimary;
    AggregatorV3Interface public immutable xauUsdSecondary;

    uint8 public immutable ethUsdPrimaryDecimals;
    uint8 public immutable ethUsdSecondaryDecimals;
    uint8 public immutable xauUsdPrimaryDecimals;
    uint8 public immutable xauUsdSecondaryDecimals;

    // Price of 1 ETH in Mengers, 18 decimals. Updated by every fetchPrice() call.
    uint public lastGoodPrice;

    // Last prices actually read from the feeds, 18 decimals
    uint public lastGoodEthUsdPrice;
    uint public lastGoodXauUsdPrice;

    // A primary that jumped stays distrusted until it agrees with its secondary again
    bool public ethPrimaryUntrusted;
    bool public xauPrimaryUntrusted;

    struct FeedResponse {
        bool success;      // a valid answer, in range
        uint price;        // 18 decimals
        uint updatedAt;
        bool jumped;       // moved by more than MAX_ROUND_CHANGE since its previous round
        bool prevMissing;  // the previous round is unavailable, so the move cannot be checked
    }

    struct Selection {
        uint price;
        bool fromFeed;          // false: the last good price is kept
        bool confirmed;         // ETH: the other feed does not contradict it. Gold: both usable and agreeing
        bool fresh;             // gold only: both feeds updated within GOLD_MAX_AGE
        bool primaryUntrusted;  // the primary's trust after this reading
    }

    // --- Events ---

    event LastGoodEthUsdPriceUpdated(uint _ethUsdPrice);
    event LastGoodXauUsdPriceUpdated(uint _xauUsdPrice);
    event EthPrimaryTrustChanged(bool _untrusted);
    event XauPrimaryTrustChanged(bool _untrusted);

    // --- Constructor ---

    constructor(
        address _ethUsdPrimary,
        address _ethUsdSecondary,
        address _xauUsdPrimary,
        address _xauUsdSecondary,
        uint _goldMaxAge
    )
        public
    {
        require(
            _ethUsdPrimary != address(0) && _ethUsdSecondary != address(0) &&
            _xauUsdPrimary != address(0) && _xauUsdSecondary != address(0),
            "MengerPriceFeed: zero address"
        );
        require(_ethUsdPrimary != _ethUsdSecondary, "MengerPriceFeed: ETH feeds must be different");
        require(_xauUsdPrimary != _xauUsdSecondary, "MengerPriceFeed: gold feeds must be different");
        require(_goldMaxAge > 0 && _goldMaxAge <= GOLD_TIMEOUT, "MengerPriceFeed: gold max age out of range");

        uint8 ethPrimaryDecimals = AggregatorV3Interface(_ethUsdPrimary).decimals();
        uint8 ethSecondaryDecimals = AggregatorV3Interface(_ethUsdSecondary).decimals();
        uint8 xauPrimaryDecimals = AggregatorV3Interface(_xauUsdPrimary).decimals();
        uint8 xauSecondaryDecimals = AggregatorV3Interface(_xauUsdSecondary).decimals();
        require(
            ethPrimaryDecimals <= 36 && ethSecondaryDecimals <= 36 && xauPrimaryDecimals <= 36 && xauSecondaryDecimals <= 36,
            "MengerPriceFeed: unsupported decimals"
        );

        ethUsdPrimary = AggregatorV3Interface(_ethUsdPrimary);
        ethUsdSecondary = AggregatorV3Interface(_ethUsdSecondary);
        xauUsdPrimary = AggregatorV3Interface(_xauUsdPrimary);
        xauUsdSecondary = AggregatorV3Interface(_xauUsdSecondary);

        ethUsdPrimaryDecimals = ethPrimaryDecimals;
        ethUsdSecondaryDecimals = ethSecondaryDecimals;
        xauUsdPrimaryDecimals = xauPrimaryDecimals;
        xauUsdSecondaryDecimals = xauSecondaryDecimals;

        GOLD_MAX_AGE = _goldMaxAge;

        // The system must start from four working feeds that agree, each with its round history (getRoundData).
        // (Immutables cannot be read during construction, hence the local variables.)
        FeedResponse memory ethA = _readFeed(_ethUsdPrimary, ethPrimaryDecimals);
        FeedResponse memory ethB = _readFeed(_ethUsdSecondary, ethSecondaryDecimals);
        require(
            _isUsable(ethA, ETH_TIMEOUT) && _isUsable(ethB, ETH_TIMEOUT) &&
            _withinDeviation(ethA.price, ethB.price, MAX_ETH_DEVIATION),
            "MengerPriceFeed: ETH feeds not working or not agreeing"
        );
        FeedResponse memory xauA = _readFeed(_xauUsdPrimary, xauPrimaryDecimals);
        FeedResponse memory xauB = _readFeed(_xauUsdSecondary, xauSecondaryDecimals);
        require(
            _isUsable(xauA, GOLD_TIMEOUT) && _isUsable(xauB, GOLD_TIMEOUT) &&
            _withinDeviation(xauA.price, xauB.price, MAX_GOLD_DEVIATION),
            "MengerPriceFeed: gold feeds not working or not agreeing"
        );

        _storeEthUsdPrice(ethA.price);
        _storeXauUsdPrice(xauA.price);
        _storePrice(_computeEthMengerPrice(ethA.price, xauA.price));
    }

    // --- Functions ---

    /*
    * Returns the price of 1 ETH in Mengers (18 decimals) and stores it as lastGoodPrice. Never reverts.
    */
    function fetchPrice() external override returns (uint) {
        Selection memory eth = _selectEth();
        Selection memory xau = _selectXau();

        if (eth.primaryUntrusted != ethPrimaryUntrusted) {
            ethPrimaryUntrusted = eth.primaryUntrusted;
            emit EthPrimaryTrustChanged(eth.primaryUntrusted);
        }
        if (xau.primaryUntrusted != xauPrimaryUntrusted) {
            xauPrimaryUntrusted = xau.primaryUntrusted;
            emit XauPrimaryTrustChanged(xau.primaryUntrusted);
        }
        if (eth.fromFeed) { _storeEthUsdPrice(eth.price); }
        if (xau.fromFeed) { _storeXauUsdPrice(xau.price); }

        uint price = _computeEthMengerPrice(eth.price, xau.price);
        if (price == 0) { return lastGoodPrice; } // only reachable with absurd but in-range answers
        _storePrice(price);

        return price;
    }

    function isRedemptionAllowed() external view override returns (bool) {
        if (isGoldMarketClosedAt(block.timestamp)) { return false; }

        Selection memory eth = _selectEth();
        if (!eth.fromFeed || !eth.confirmed) { return false; }

        Selection memory xau = _selectXau();
        return xau.confirmed && xau.fresh;
    }

    /*
    * True from Friday 21:00 UTC to Monday 00:00 UTC. The Unix epoch (1 January 1970) was a Thursday,
    * hence the +4: day 0 = Sunday, 1 = Monday, ..., 5 = Friday, 6 = Saturday.
    */
    function isGoldMarketClosedAt(uint _timestamp) public pure returns (bool) {
        uint dayOfWeek = (_timestamp / 1 days + 4) % 7;
        uint secondOfDay = _timestamp % 1 days;

        if (dayOfWeek == 6 || dayOfWeek == 0) { return true; }                    // Saturday, Sunday
        if (dayOfWeek == 5 && secondOfDay >= WEEKEND_START_ON_FRIDAY) { return true; } // Friday evening
        return false;
    }

    // --- Views for front ends and tests (what fetchPrice() would use now) ---

    // reliable: good enough for redemptions
    function getEthUsdPrice() external view returns (uint price, bool reliable) {
        Selection memory eth = _selectEth();
        return (eth.price, eth.fromFeed && eth.confirmed);
    }

    // agreed: both gold feeds usable and within 1 %; fresh: both updated within GOLD_MAX_AGE
    function getXauUsdPrice() external view returns (uint price, bool agreed, bool fresh) {
        Selection memory xau = _selectXau();
        return (xau.price, xau.confirmed, xau.fresh);
    }

    // --- Internal price selection ---

    function _selectEth() internal view returns (Selection memory sel) {
        FeedResponse memory primary = _readFeed(address(ethUsdPrimary), ethUsdPrimaryDecimals);
        FeedResponse memory secondary = _readFeed(address(ethUsdSecondary), ethUsdSecondaryDecimals);

        bool primaryLive = _isLive(primary, ETH_TIMEOUT);
        bool secondaryUsable = _isUsable(secondary, ETH_TIMEOUT);
        bool agree = primaryLive && secondaryUsable && _withinDeviation(primary.price, secondary.price, MAX_ETH_DEVIATION);

        // A primary that jumped is distrusted until it agrees with the secondary again
        sel.primaryUntrusted = agree ? false : (ethPrimaryUntrusted || (primaryLive && primary.jumped));
        bool primaryUsable = primaryLive && !primary.jumped && !primary.prevMissing && !sel.primaryUntrusted;

        if (primaryUsable) {
            sel.price = primary.price;
            sel.fromFeed = true;
            sel.confirmed = !secondaryUsable || agree;
        } else if (secondaryUsable) {
            // Redemptions stay allowed if the primary is silent, not if it still answers another price
            sel.price = secondary.price;
            sel.fromFeed = true;
            sel.confirmed = !primaryLive || agree;
        } else {
            sel.price = lastGoodEthUsdPrice;
        }
    }

    function _selectXau() internal view returns (Selection memory sel) {
        FeedResponse memory primary = _readFeed(address(xauUsdPrimary), xauUsdPrimaryDecimals);
        FeedResponse memory secondary = _readFeed(address(xauUsdSecondary), xauUsdSecondaryDecimals);

        bool primaryLive = _isLive(primary, GOLD_TIMEOUT);
        bool secondaryUsable = _isUsable(secondary, GOLD_TIMEOUT);
        bool agree = primaryLive && secondaryUsable && _withinDeviation(primary.price, secondary.price, MAX_GOLD_DEVIATION);

        sel.primaryUntrusted = agree ? false : (xauPrimaryUntrusted || (primaryLive && primary.jumped));
        bool primaryUsable = primaryLive && !primary.jumped && !primary.prevMissing && !sel.primaryUntrusted;

        bool secondaryFresh = secondaryUsable && _age(secondary) <= GOLD_MAX_AGE;
        sel.confirmed = primaryUsable && agree;
        sel.fresh = primaryUsable && _age(primary) <= GOLD_MAX_AGE && secondaryFresh;

        if (primaryUsable) {
            // The primary is lagging: they disagree, only the secondary is recent, and the market is open. On a
            // weekend a quiet primary is normal, and the secondary alone must not move the gold price.
            bool lagging = secondaryUsable && !agree && secondaryFresh && _age(primary) > GOLD_MAX_AGE &&
                !isGoldMarketClosedAt(block.timestamp);
            sel.price = lagging ? secondary.price : primary.price;
            sel.fromFeed = true;
        } else if (secondaryUsable) {
            sel.price = secondary.price;
            sel.fromFeed = true;
        } else {
            sel.price = lastGoodXauUsdPrice;
        }
    }

    // --- Feed reading ---

    function _readFeed(address _feed, uint8 _decimals) internal view returns (FeedResponse memory response) {
        (bool ok, uint roundId, int256 answer, uint updatedAt) =
            _callFeed(_feed, abi.encodeWithSelector(AggregatorV3Interface.latestRoundData.selector));
        if (!ok || roundId == 0 || updatedAt == 0 || updatedAt > block.timestamp || answer <= 0) {
            return response;
        }
        uint price = _scalePrice(uint(answer), _decimals);
        if (price == 0) { return response; }

        response.success = true;
        response.price = price;
        response.updatedAt = updatedAt;

        // Compare with the previous round, as Liquity V1 did for Chainlink
        (bool prevOk, uint prevRoundId, int256 prevAnswer, uint prevUpdatedAt) =
            _callFeed(_feed, abi.encodeWithSelector(AggregatorV3Interface.getRoundData.selector, uint80(roundId - 1)));
        uint prevPrice = prevAnswer > 0 ? _scalePrice(uint(prevAnswer), _decimals) : 0;
        if (!prevOk || prevRoundId == 0 || prevUpdatedAt == 0 || prevUpdatedAt > block.timestamp || prevPrice == 0) {
            response.prevMissing = true;
        } else {
            response.jumped = !_withinRoundChange(price, prevPrice);
        }
    }

    /*
    * Low-level call with a gas cap that copies at most the 160 bytes expected: a revert, short or malformed data,
    * a feed without code, a gas-burning feed or a huge return buffer all count as a failed answer, and cannot make
    * fetchPrice() revert. Every value is read as a full word.
    */
    function _callFeed(address _feed, bytes memory _calldata)
        internal
        view
        returns (bool ok, uint roundId, int256 answer, uint updatedAt)
    {
        uint[5] memory out;
        bool success;
        uint size;
        uint gasCap = FEED_CALL_GAS;
        assembly {
            success := staticcall(gasCap, _feed, add(_calldata, 0x20), mload(_calldata), out, 160)
            size := returndatasize()
        }
        if (!success || size < 160 || out[0] > uint80(-1)) { return (false, 0, 0, 0); }
        return (true, out[0], int256(out[1]), out[3]);
    }

    function _isLive(FeedResponse memory _response, uint _timeout) internal view returns (bool) {
        return _response.success && _age(_response) <= _timeout;
    }

    function _isUsable(FeedResponse memory _response, uint _timeout) internal view returns (bool) {
        return _isLive(_response, _timeout) && !_response.jumped && !_response.prevMissing;
    }

    function _age(FeedResponse memory _response) internal view returns (uint) {
        return block.timestamp - _response.updatedAt; // updatedAt <= block.timestamp is checked when reading
    }

    // Scales an answer to 18 decimals. Returns 0 (a failed answer) if the result would exceed MAX_PRICE.
    function _scalePrice(uint _answer, uint8 _decimals) internal pure returns (uint) {
        if (_decimals >= TARGET_DIGITS) {
            uint scaled = _answer / (10 ** (uint(_decimals) - TARGET_DIGITS));
            return scaled > MAX_PRICE ? 0 : scaled;
        }
        uint factor = 10 ** (TARGET_DIGITS - uint(_decimals));
        if (_answer > MAX_PRICE / factor) { return 0; }
        return _answer * factor;
    }

    // Relative difference measured against the lower price, as in Liquity V1
    function _withinDeviation(uint _a, uint _b, uint _maxDeviation) internal pure returns (bool) {
        uint minPrice = _a < _b ? _a : _b;
        uint maxPrice = _a < _b ? _b : _a;
        return maxPrice.sub(minPrice).mul(DECIMAL_PRECISION) <= minPrice.mul(_maxDeviation);
    }

    // Relative change measured against the higher price, as Liquity V1 did between two Chainlink rounds
    function _withinRoundChange(uint _current, uint _previous) internal pure returns (bool) {
        uint minPrice = _current < _previous ? _current : _previous;
        uint maxPrice = _current < _previous ? _previous : _current;
        return maxPrice.sub(minPrice).mul(DECIMAL_PRECISION) <= maxPrice.mul(MAX_ROUND_CHANGE);
    }

    function _computeEthMengerPrice(uint _ethUsd, uint _xauUsd) internal pure returns (uint) {
        // ETH/Menger = ETH/USD x TROY_OUNCE_WEIGHT / (XAU/USD x MENGER_WEIGHT). Both prices are at most MAX_PRICE.
        return _ethUsd.mul(DECIMAL_PRECISION).mul(TROY_OUNCE_WEIGHT).div(_xauUsd.mul(MENGER_WEIGHT));
    }

    function _storePrice(uint _price) internal {
        lastGoodPrice = _price;
        emit LastGoodPriceUpdated(_price);
    }

    function _storeEthUsdPrice(uint _price) internal {
        lastGoodEthUsdPrice = _price;
        emit LastGoodEthUsdPriceUpdated(_price);
    }

    function _storeXauUsdPrice(uint _price) internal {
        lastGoodXauUsdPrice = _price;
        emit LastGoodXauUsdPriceUpdated(_price);
    }
}
