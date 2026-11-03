// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;

import "./Interfaces/ICommunityIssuance.sol";
import "./Interfaces/ILUSDToken.sol";
import "./Interfaces/IStabilityPool.sol";
import "./Dependencies/Ownable.sol";
import "./Dependencies/CheckContract.sol";
import "./Dependencies/SafeMath.sol";
import "./Dependencies/LiquityMath.sol";

/*
* MENGER: replaces Liquity V1's CommunityIssuance. There is no LQTY token and no pre-mine.
*
* BorrowerOperations mints every borrowing fee (in Mengers) to this contract. Liquity V1's CommunityIssuance
* released LQTY to the Stability Pool little by little, at each pool operation, according to the time elapsed.
* This contract does the same with the fees: at each pool operation, issueLQTY() releases the part of the pending
* fees that matches the time elapsed since the previous operation (all of them once FEE_RELEASE_PERIOD has passed).
* The pool distributes what is released to depositors pro rata through its sum G, exactly as it distributed LQTY.
* The interface and its function names are unchanged, so the Stability Pool code that uses them is unchanged.
*
* Why gradually: handed over at once, a large fee could be captured by someone who deposits just before it is
* paid and withdraws just after, without having protected the system. Released over time, fees go to the
* depositors who stay in the pool.
*
* Fees received between two pool operations start being released at the next operation.
* No fee is ever lost: while the Stability Pool is empty nothing is released, and the fees wait here.
* Anyone may send Mengers to this contract; they are then distributed to depositors like fees.
*/
contract StabilityPoolFeeIssuance is ICommunityIssuance, Ownable, CheckContract {
    using SafeMath for uint;

    // --- Data ---

    string constant public NAME = "StabilityPoolFeeIssuance";

    // Each release hands over (time elapsed / FEE_RELEASE_PERIOD) of the pending fees, or all of them
    uint constant public FEE_RELEASE_PERIOD = 7 days;

    ILUSDToken public lusdToken;

    address public stabilityPoolAddress;

    // Fees received and not yet released to the Stability Pool
    uint public pendingFees;

    // Fees released to the Stability Pool (accounted in its sum G) and not yet paid out to depositors
    uint public totalIssuedNotPaid;

    // Cumulative fees released to the Stability Pool (name kept from Liquity V1)
    uint public totalLQTYIssued;

    uint public lastReleaseTime;

    event PendingFeesUpdated(uint _pendingFees);

    // --- Functions ---

    function setAddresses
    (
        address _lusdTokenAddress,
        address _stabilityPoolAddress
    )
        external
        onlyOwner
        override
    {
        checkContract(_lusdTokenAddress);
        checkContract(_stabilityPoolAddress);

        lusdToken = ILUSDToken(_lusdTokenAddress);
        stabilityPoolAddress = _stabilityPoolAddress;
        lastReleaseTime = block.timestamp;

        emit LQTYTokenAddressSet(_lusdTokenAddress);
        emit StabilityPoolAddressSet(_stabilityPoolAddress);

        _renounceOwnership();
    }

    /*
    * Called by the Stability Pool at each operation (deposit, withdrawal, liquidation). Returns the fees released.
    * 1) Releases the part of the pending fees that matches the time elapsed since the last call.
    *    While the pool holds no deposits nothing is released: nobody could receive it.
    * 2) Fees received since the last call become pending: their release starts now.
    */
    function issueLQTY() external override returns (uint) {
        _requireCallerIsStabilityPool();

        uint elapsed = block.timestamp.sub(lastReleaseTime);
        lastReleaseTime = block.timestamp;

        uint pending = pendingFees;
        uint issuance;
        if (IStabilityPool(stabilityPoolAddress).getTotalLUSDDeposits() > 0) {
            issuance = elapsed >= FEE_RELEASE_PERIOD ? pending : pending.mul(elapsed).div(FEE_RELEASE_PERIOD);
        }

        // Fees received since the last call: whatever is held here beyond the pending and the released fees
        uint balance = lusdToken.balanceOf(address(this));
        uint accounted = totalIssuedNotPaid.add(pending);
        uint newFees = balance > accounted ? balance - accounted : 0;

        pendingFees = pending.sub(issuance).add(newFees);
        totalIssuedNotPaid = totalIssuedNotPaid.add(issuance);
        totalLQTYIssued = totalLQTYIssued.add(issuance);

        emit PendingFeesUpdated(pendingFees);
        emit TotalLQTYIssuedUpdated(totalLQTYIssued);

        return issuance;
    }

    /*
    * Pays a depositor's (or front end's) share of the released fees.
    * Rounding in the Stability Pool's sums can make the shares add up to a few units of 1e-18 Menger more than
    * what was released. Capping each payment by the released fees not yet paid out means the last claimant
    * receives that dust less, instead of their withdrawal reverting. Pending fees are never used.
    */
    function sendLQTY(address _account, uint _amount) external override {
        _requireCallerIsStabilityPool();

        uint amountToSend = LiquityMath._min(_amount, totalIssuedNotPaid);
        if (amountToSend == 0) { return; }

        totalIssuedNotPaid = totalIssuedNotPaid.sub(amountToSend);
        lusdToken.transfer(_account, amountToSend);
    }

    // --- 'require' functions ---

    function _requireCallerIsStabilityPool() internal view {
        require(msg.sender == stabilityPoolAddress, "StabilityPoolFeeIssuance: caller is not SP");
    }
}
