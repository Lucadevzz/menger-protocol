#!/usr/bin/env python3
"""
Runs the replay of historical crashes with the real contracts (test/sim/HistoricalCrashes.t.sol) and turns its
"SIM|window|pool share|metric" lines into sim/out/crash_replay.md.

Usage: python3 sim/report.py            (needs forge in the PATH; run sim/history.py first)
       python3 sim/report.py saved.log  (parses the output of: forge test --match-contract HistoricalCrashesSim -vv)
"""
import os
import re
import subprocess
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "sim", "out")
LINE = re.compile(r"SIM\|(\d+)\|(\d+)\|(\w+): (.*)$")


def run_forge():
    cmd = ["forge", "test", "--match-contract", "HistoricalCrashesSim", "-vv"]
    res = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if res.returncode != 0:
        sys.stderr.write(res.stdout + res.stderr)
        raise SystemExit("forge test failed")
    return res.stdout


def parse(text):
    data = defaultdict(dict)
    for line in text.splitlines():
        m = LINE.search(line.strip())
        if m:
            window, pool, key, value = int(m.group(1)), int(m.group(2)), m.group(3), m.group(4).strip()
            data[(pool, window)][key] = value if key == "name" else int(value)
    return data


def pct(x):
    return "%+.1f %%" % x


def table(data, pool):
    rows = []
    rows.append("| Crash (peak to trough, ETH in gold) | Troves liquidated | of which below 100 % | Pool deposits used | "
                "Liquidated debt now carried by the remaining troves | Lowest TCR | Days in Recovery Mode | Pool result, at the day's price | "
                "Pool result, at the end of the window | TCR at the end |")
    rows.append("|---|---|---|---|---|---|---|---|---|---|")
    totals = defaultdict(int)
    for (p, w), d in sorted(data.items()):
        if p != pool:
            continue
        absorbed = d["spDepositStart"] - d["spDepositEnd"]
        redistributed = d["extraDebtOnSurvivors"]
        result_liq = (d["spEthGainValueAtLiquidation"] - absorbed) / d["spDepositStart"] * 100
        result_end = (d["spEthGainValueEnd"] - absorbed) / d["spDepositStart"] * 100
        rows.append("| %s | %d / 40 | %d | %.0f %% | %.0f %% of the initial debt | %.0f %% | %d | %s | %s | %.0f %% |" % (
            d["name"], d["trovesLiquidated"], d["trovesLiquidatedBelow100"],
            absorbed / d["spDepositStart"] * 100, redistributed / d["totalDebtStart"] * 100,
            d["minTCRBp"] / 100, d["recoveryModeDays"], pct(result_liq), pct(result_end), d["finalTCRBp"] / 100))
        totals["below100"] += d["trovesLiquidatedBelow100"]
        totals["liquidated"] += d["trovesLiquidated"]
        totals["insolvent"] += 1 if d["finalTCRBp"] < 10000 or d["minTCRBp"] < 10000 else 0
        totals["windows"] += 1
    return rows, totals


def main():
    text = open(sys.argv[1]).read() if len(sys.argv) > 1 else run_forge()
    data = parse(text)
    if not data:
        raise SystemExit("no SIM lines found")
    lines = ["# Historical crashes replayed with the Menger contracts", ""]
    lines.append("Method: test/sim/HistoricalCrashes.t.sol. 40 troves of 1,000 Mengers each, collateral ratios from 115 % "
                 "to 400 % (average 200 %). Daily ETH and gold prices; on Saturdays and Sundays the gold price stays at "
                 "Friday's. A bot liquidates every trove it can, once a day. Borrowers are passive (nobody adds "
                 "collateral). Worst case on two counts: real borrowers react, and real bots liquidate within minutes "
                 "of each price update, close to 110 %, while daily steps let ratios fall far below before liquidation.")
    lines.append("")
    lines.append("Pool result: ETH received by the Stability Pool minus the Mengers it used, as a share of its deposits. "
                 "At the day's price: what liquidations themselves earned. At the end of the window: after the ETH "
                 "received kept falling (depositors can sell the ETH at once; the table assumes they did not).")
    lines.append("")
    for pool in sorted({p for p, _ in data}, reverse=True):
        rows, totals = table(data, pool)
        lines.append("## Stability Pool holding %d %% of all Mengers" % pool)
        lines.append("")
        lines.extend(rows)
        lines.append("")
        lines.append("%d crashes: %d troves liquidated in total, %d of them below 100 %% (daily steps); system "
                     "insolvent (TCR below 100 %%) in %d crashes." % (totals["windows"], totals["liquidated"], totals["below100"], totals["insolvent"]))
        lines.append("")
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "crash_replay.md"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
