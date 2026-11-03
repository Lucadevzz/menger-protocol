// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;

interface IPriceFeed {

    // --- Events ---
    event LastGoodPriceUpdated(uint _lastGoodPrice);

    // --- Function ---

    // MENGER: returns the price of 1 ETH in Mengers (1 Menger = 3.545 g of fine gold), with 18 decimals.
    function fetchPrice() external returns (uint);

    // MENGER: false while the gold market is closed (weekend window) or while the gold or ETH price is not reliable.
    // TroveManager refuses redemptions when this returns false. Liquidations and borrowing are never blocked.
    function isRedemptionAllowed() external view returns (bool);
}
