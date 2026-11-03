// SPDX-License-Identifier: MIT

pragma solidity 0.6.11;
pragma experimental ABIEncoderV2;

import "../MengerTestBase.sol";
import "./CrashData.sol";

/*
* Replays, with the real contracts, the largest historical falls of ETH measured in gold (selected and exported
* by sim/history.py into CrashData.sol). Results are printed as "SIM|window|pool share|metric" lines that
* sim/report.py turns into a table. Run with: forge test --match-contract HistoricalCrashesSim -vv
*
* At the first day of each window, a fresh system is deployed with:
* - 40 troves of equal debt (1,000 Mengers), with collateral ratios from 115 % to 400 % (average 200 %);
* - a Stability Pool holding a given share of all Mengers (50 % or 15 %).
* Each following day at 12:00 UTC, the ETH price is updated, and the gold price too except on Saturdays and
* Sundays (the gold market is closed: the oracle keeps Friday's price, and Monday brings the weekend's gap).
* Then a liquidation bot liquidates every trove it can.
*
* Worst-case assumptions: borrowers are passive (nobody adds collateral or repays during the crash), and prices
* move in daily steps, while real liquidation bots act within minutes of each price update, close to 110 %.
* Daily steps therefore exaggerate the number of troves liquidated below 100 % and the Stability Pool's losses.
*/
contract HistoricalCrashesSim is MengerTestBase {
    uint constant TROVES = 40;
    uint constant DEBT = 1000e18;

    address constant SP_DEPOSITOR = address(0x5D0);

    struct Result {
        uint days_;
        uint weekendDays;
        uint liquidated;
        uint debtLiquidated;
        uint liquidatedBelow100;
        uint debtLiquidatedBelow100;
        uint minTCR;
        uint recoveryModeDays;
        uint spDepositStart;
        uint spEthGainValueAtLiquidation; // ETH received by the pool, valued at the price of the day it was received
    }

    function setUp() public override {} // each window deploys its own system, at its own dates

    function test_replayCrashes_poolHolds50Percent() public {
        for (uint i = 0; i < CrashData.WINDOWS; i++) { _replay(i, 50); }
    }

    function test_replayCrashes_poolHolds15Percent() public {
        for (uint i = 0; i < CrashData.WINDOWS; i++) { _replay(i, 15); }
    }

    // Collateral ratio of trove k, in basis points. Opened from the safest to the riskiest, to keep the TCR high.
    function _targetICR(uint k) internal pure returns (uint) {
        if (k < 3) return 40000;   // 3 troves at 400 %
        if (k < 7) return 30000;   // 4 at 300 %
        if (k < 12) return 25000;  // 5 at 250 %
        if (k < 18) return 20000;  // 6 at 200 %
        if (k < 24) return 17500;  // 6 at 175 %
        if (k < 28) return 15000;  // 4 at 150 %
        if (k < 32) return 13500;  // 4 at 135 %
        if (k < 36) return 12500;  // 4 at 125 %
        return 11500;              // 4 at 115 %
    }

    function _owner(uint k) internal pure returns (address) {
        return address(uint160(0x100000 + k));
    }

    function _setUpPopulation(uint _spPercent) internal returns (uint spDeposit) {
        uint price = priceFeed.fetchPrice();
        uint entireDebt = DEBT * 1005 / 1000 + 0.4e18; // with the 0.5 % borrowing fee and the 0.4 Menger gas reserve
        for (uint k = 0; k < TROVES; k++) {
            uint coll = _targetICR(k) * entireDebt / 10000 * 1e18 / price + 1;
            vm.deal(_owner(k), coll);
            vm.prank(_owner(k));
            borrowerOperations.openTrove{value: coll}(1e18, DEBT, address(0), address(0));
        }
        // The owners of the safest troves hand their Mengers to the depositor, who puts them in the Stability Pool
        spDeposit = menger.totalSupply() * _spPercent / 100;
        uint gathered;
        for (uint k = 0; gathered < spDeposit; k++) {
            uint amount = spDeposit - gathered < DEBT ? spDeposit - gathered : DEBT;
            vm.prank(_owner(k));
            menger.transfer(SP_DEPOSITOR, amount);
            gathered += amount;
        }
        vm.prank(SP_DEPOSITOR);
        stabilityPool.provideToSP(spDeposit, address(0));
    }

    function _isGoldMarketOpenDay(uint _timestamp) internal pure returns (bool) {
        uint weekday = (_timestamp / 1 days + 4) % 7; // 0 = Sunday, 6 = Saturday
        return weekday != 0 && weekday != 6;
    }

    function _replay(uint _window, uint _spPercent) internal {
        (string memory name, uint[] memory day, uint[] memory ethUsd, uint[] memory xauUsd) = CrashData.window(_window);
        _deploySystem(day[0], ethUsd[0], xauUsd[0]);

        Result memory r;
        r.minTCR = uint(-1);
        r.spDepositStart = _setUpPopulation(_spPercent);
        uint ethMengerStart = priceFeed.fetchPrice();

        uint[] memory icrBefore = new uint[](TROVES);
        uint[] memory debtBefore = new uint[](TROVES);
        for (uint i = 1; i < day.length; i++) {
            vm.warp(day[i]);
            ethPrimary.setAnswer(int(ethUsd[i] * 1e6), block.timestamp);
            ethSecondary.setAnswer(int(ethUsd[i] * 1e6), block.timestamp);
            if (_isGoldMarketOpenDay(day[i])) {
                xauPrimary.setAnswer(int(xauUsd[i] * 1e6), block.timestamp);
                xauSecondary.setAnswer(int(xauUsd[i] * 1e6), block.timestamp);
            } else {
                r.weekendDays++;
            }
            r.days_++;

            uint price = priceFeed.fetchPrice();
            uint tcr = troveManager.getTCR(price);
            if (tcr < r.minTCR) { r.minTCR = tcr; }
            if (troveManager.checkRecoveryMode(price)) { r.recoveryModeDays++; }

            for (uint k = 0; k < TROVES; k++) {
                if (troveManager.getTroveStatus(_owner(k)) != 1) { continue; }
                icrBefore[k] = troveManager.getCurrentICR(_owner(k), price);
                (debtBefore[k], , , ) = troveManager.getEntireDebtAndColl(_owner(k));
            }

            // The bot liquidates until nothing is left to liquidate
            uint spEthGainBefore = stabilityPool.getDepositorETHGain(SP_DEPOSITOR);
            for (uint round = 0; round < 10; round++) {
                try troveManager.liquidateTroves(TROVES) {} catch { break; }
            }
            r.spEthGainValueAtLiquidation += (stabilityPool.getDepositorETHGain(SP_DEPOSITOR) - spEthGainBefore) * price / 1e18;

            for (uint k = 0; k < TROVES; k++) {
                if (debtBefore[k] == 0 || troveManager.getTroveStatus(_owner(k)) != 3) { continue; }
                r.liquidated++;
                r.debtLiquidated += debtBefore[k];
                if (icrBefore[k] < 1e18) {
                    r.liquidatedBelow100++;
                    r.debtLiquidatedBelow100 += debtBefore[k];
                }
                debtBefore[k] = 0; // counted once
            }
            for (uint k = 0; k < TROVES; k++) {
                if (troveManager.getTroveStatus(_owner(k)) != 1) { debtBefore[k] = 0; }
            }
            _assertSupplyEqualsDebt();
        }

        _report(_window, _spPercent, name, r, ethMengerStart);
    }

    function _report(uint _window, uint _spPercent, string memory _name, Result memory r, uint _ethMengerStart) internal {
        uint price = priceFeed.fetchPrice();
        uint minICR = uint(-1);
        uint remaining;
        uint extraDebtOnSurvivors; // debt of liquidated troves that the pool could not absorb, now carried by the others
        for (uint k = 0; k < TROVES; k++) {
            if (troveManager.getTroveStatus(_owner(k)) != 1) { continue; }
            remaining++;
            uint icr = troveManager.getCurrentICR(_owner(k), price);
            if (icr < minICR) { minICR = icr; }
            (uint debt, , , ) = troveManager.getEntireDebtAndColl(_owner(k));
            extraDebtOnSurvivors += debt - (DEBT * 1005 / 1000 + 0.4e18);
        }
        uint spDepositEnd = stabilityPool.getCompoundedLUSDDeposit(SP_DEPOSITOR);
        uint spEthGain = stabilityPool.getDepositorETHGain(SP_DEPOSITOR);

        emit log_named_string(_key(_window, _spPercent, "name"), _name);
        _log(_window, _spPercent, "days", r.days_);
        _log(_window, _spPercent, "weekendDays", r.weekendDays);
        _log(_window, _spPercent, "ethMengerEndOverStartBp", price * 10000 / _ethMengerStart);
        _log(_window, _spPercent, "trovesLiquidated", r.liquidated);
        _log(_window, _spPercent, "debtLiquidated", r.debtLiquidated);
        _log(_window, _spPercent, "trovesLiquidatedBelow100", r.liquidatedBelow100);
        _log(_window, _spPercent, "debtLiquidatedBelow100", r.debtLiquidatedBelow100);
        _log(_window, _spPercent, "totalDebtStart", TROVES * (DEBT * 1005 / 1000 + 0.4e18));
        _log(_window, _spPercent, "spDepositStart", r.spDepositStart);
        _log(_window, _spPercent, "spDepositEnd", spDepositEnd);
        _log(_window, _spPercent, "spEthGain", spEthGain);
        _log(_window, _spPercent, "spEthGainValueEnd", spEthGain * price / 1e18);
        _log(_window, _spPercent, "spEthGainValueAtLiquidation", r.spEthGainValueAtLiquidation);
        _log(_window, _spPercent, "minTCRBp", r.minTCR * 10000 / 1e18);
        _log(_window, _spPercent, "recoveryModeDays", r.recoveryModeDays);
        _log(_window, _spPercent, "trovesRemaining", remaining);
        _log(_window, _spPercent, "extraDebtOnSurvivors", extraDebtOnSurvivors);
        _log(_window, _spPercent, "minRemainingICRBp", remaining == 0 ? 0 : minICR * 10000 / 1e18);
        _log(_window, _spPercent, "finalTCRBp", troveManager.getTCR(price) * 10000 / 1e18);
    }

    function _key(uint _window, uint _spPercent, string memory _metric) internal pure returns (string memory) {
        return string(abi.encodePacked("SIM|", vm.toString(_window), "|", vm.toString(_spPercent), "|", _metric));
    }

    function _log(uint _window, uint _spPercent, string memory _metric, uint _value) internal {
        emit log_named_uint(_key(_window, _spPercent, _metric), _value);
    }
}
