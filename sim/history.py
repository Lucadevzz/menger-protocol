#!/usr/bin/env python3
"""
Menger v0: ETH price history measured in gold, and selection of the worst crashes.

Data (downloaded, not redistributed):
  - ETH/USD daily close: Coin Metrics community data, github.com/coinmetrics/data (csv/eth.csv, column PriceUSD).
  - Gold/USD daily, from 2020: the gold tokens PAXG and XAUT (1 token = 1 troy ounce), same source (csv/paxg.csv,
    csv/xaut.csv). Chainlink's round-the-clock gold feeds rely on the same tokens.
  - Gold/USD monthly average, before 2020: github.com/datasets/gold-prices (data/monthly.csv), interpolated daily.

Outputs:
  - sim/out/history_stats.md : volatility, worst days, weekend gold gaps, the selected crash windows
  - test/sim/CrashData.sol   : the daily prices of each crash window, replayed with the real contracts by
                               test/sim/HistoricalCrashes.t.sol

Usage: python3 sim/history.py   (from the project root; standard library only)
"""
import csv
import datetime as dt
import math
import os
import statistics
import sys
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.path.join(ROOT, "sim", "data")
OUT = os.path.join(ROOT, "sim", "out")

SOURCES = {
    "eth.csv": "https://raw.githubusercontent.com/coinmetrics/data/master/csv/eth.csv",
    "paxg.csv": "https://raw.githubusercontent.com/coinmetrics/data/master/csv/paxg.csv",
    "xaut.csv": "https://raw.githubusercontent.com/coinmetrics/data/master/csv/xaut.csv",
    "gold_monthly.csv": "https://raw.githubusercontent.com/datasets/gold-prices/main/data/monthly.csv",
}

MENGER_GRAMS = 3.545
TROY_OUNCE_GRAMS = 31.1034768
WINDOWS = 12            # number of crash windows to replay
WINDOW_DAYS = 14        # a crash is the largest fall within this many days
TOKENS_MAX_GAP = 0.02   # PAXG and XAUT are averaged only if they agree within 2% (like the oracle's two gold feeds)


def download():
    os.makedirs(DATA, exist_ok=True)
    for name, url in SOURCES.items():
        path = os.path.join(DATA, name)
        if not os.path.exists(path):
            print("downloading", url, file=sys.stderr)
            urllib.request.urlretrieve(url, path)


def load_cm(name):
    out = {}
    with open(os.path.join(DATA, name)) as f:
        for row in csv.DictReader(f):
            if row.get("PriceUSD"):
                out[dt.date.fromisoformat(row["time"])] = float(row["PriceUSD"])
    return out


def load_monthly_gold():
    points = []
    with open(os.path.join(DATA, "gold_monthly.csv")) as f:
        for row in csv.DictReader(f):
            y, m = map(int, row["Date"].split("-"))
            points.append((dt.date(y, m, 15), float(row["Price"])))  # a monthly average, placed mid-month
    return points


def interpolate(points, day):
    for (d0, v0), (d1, v1) in zip(points, points[1:]):
        if d0 <= day <= d1:
            return v0 + (v1 - v0) * (day - d0).days / (d1 - d0).days
    raise ValueError(day)


def build_series():
    eth, paxg, xaut = load_cm("eth.csv"), load_cm("paxg.csv"), load_cm("xaut.csv")
    monthly = load_monthly_gold()
    days = sorted(d for d in eth if d >= dt.date(2016, 1, 1))
    series = []
    for d in days:
        ref = interpolate(monthly, d)
        p, x = paxg.get(d), xaut.get(d)
        if p and x and abs(p / x - 1) <= TOKENS_MAX_GAP:
            gold, src = (p + x) / 2, "tokens"
        elif p or x:
            # only one token, or the two disagree: keep the one closest to the monthly reference
            cands = [v for v in (p, x) if v]
            best = min(cands, key=lambda v: abs(v / ref - 1))
            gold, src = (best, "token") if abs(best / ref - 1) < 0.05 else (ref, "monthly")
        else:
            gold, src = ref, "monthly"
        menger_usd = gold * MENGER_GRAMS / TROY_OUNCE_GRAMS
        series.append({"day": d, "eth_usd": eth[d], "gold_usd": gold, "eth_menger": eth[d] / menger_usd, "src": src})
    return series


def pct(x):
    return "%+.1f %%" % (100 * x)


def stats(series):
    lines = []
    r_menger = [math.log(b["eth_menger"] / a["eth_menger"]) for a, b in zip(series, series[1:])]
    r_usd = [math.log(b["eth_usd"] / a["eth_usd"]) for a, b in zip(series, series[1:])]
    first, last = series[0]["day"], series[-1]["day"]
    lines.append("# ETH measured in gold: history %s to %s\n" % (first, last))
    lines.append("%d days. Gold source: %d days from the gold tokens, %d days interpolated from monthly averages.\n" % (
        len(series), sum(1 for s in series if s["src"] != "monthly"), sum(1 for s in series if s["src"] == "monthly")))
    lines.append("## Volatility (annualised, daily closes)\n")
    lines.append("| | ETH/USD | ETH/Menger |\n|---|---|---|")
    lines.append("| Volatility | %.0f %% | %.0f %% |" % (statistics.pstdev(r_usd) * math.sqrt(365) * 100,
                                                     statistics.pstdev(r_menger) * math.sqrt(365) * 100))
    for label, threshold in [("Daily falls of more than 9.1 % (a trove at 110 % drops below 100 %)", 1 / 1.1),
                             ("Daily falls of more than 26.7 % (a trove at 150 % drops below 110 %)", 1.1 / 1.5)]:
        n_d = sum(1 for r in r_menger if math.exp(r) < threshold)
        n_u = sum(1 for r in r_usd if math.exp(r) < threshold)
        lines.append("| %s | %d days | %d days |" % (label, n_u, n_d))
    lines.append("")

    lines.append("## The 10 worst days for ETH measured in gold\n")
    lines.append("| Day | ETH/Menger | ETH/USD | Gold/USD |\n|---|---|---|---|")
    moves = []
    for a, b in zip(series, series[1:]):
        moves.append((b["eth_menger"] / a["eth_menger"] - 1, b["day"], b["eth_usd"] / a["eth_usd"] - 1, b["gold_usd"] / a["gold_usd"] - 1))
    for m, d, mu, mg in sorted(moves)[:10]:
        lines.append("| %s | %s | %s | %s |" % (d, pct(m), pct(mu), pct(mg)))
    lines.append("")

    # Weekend gold gaps: the oracle keeps Friday's gold price until the market reopens on Monday
    gaps = []
    by_day = {s["day"]: s for s in series if s["src"] != "monthly"}
    for d, s in by_day.items():
        if d.weekday() == 4 and (d + dt.timedelta(days=3)) in by_day:
            gaps.append((by_day[d + dt.timedelta(days=3)]["gold_usd"] / s["gold_usd"] - 1, d))
    gaps.sort()
    if gaps:
        lines.append("## Gold, Friday close to Monday close (%d weekends, %s to %s)\n" % (len(gaps), min(g[1] for g in gaps), max(g[1] for g in gaps)))
        lines.append("Upper bound of the weekend gap: Monday's close also includes Monday's own move.\n")
        absg = sorted(abs(g[0]) for g in gaps)
        lines.append("| | |\n|---|---|")
        lines.append("| Median move | %.2f %% |" % (100 * statistics.median(absg)))
        lines.append("| 99th percentile | %.2f %% |" % (100 * absg[int(len(absg) * 0.99)]))
        lines.append("| Largest rise (troves lose this much ratio) | %s (weekend of %s) |" % (pct(gaps[-1][0]), gaps[-1][1]))
        lines.append("| Largest fall | %s (weekend of %s) |" % (pct(gaps[0][0]), gaps[0][1]))
        lines.append("")
    return lines


def select_windows(series):
    """Largest falls of ETH/Menger within WINDOW_DAYS days, non-overlapping."""
    p = [s["eth_menger"] for s in series]
    candidates = []
    for t in range(len(p) - 1):
        horizon = range(t, min(t + WINDOW_DAYS + 1, len(p)))
        trough = min(horizon, key=lambda i: p[i])
        candidates.append((p[trough] / p[t] - 1, t, trough))
    candidates.sort()
    chosen = []
    for fall, peak, trough in candidates:
        if all(abs(peak - c[1]) > 3 * WINDOW_DAYS for c in chosen):
            chosen.append((fall, peak, trough))
        if len(chosen) == WINDOWS:
            break
    windows = []
    for fall, peak, trough in sorted(chosen, key=lambda c: c[1]):
        start, end = max(0, peak - 2), min(len(series) - 1, trough + 4)
        windows.append({"fall": fall, "peak": series[peak]["day"], "trough": series[trough]["day"], "days": series[start:end + 1]})
    return windows


def cents(x):
    return int(round(x * 100))


def write_solidity(windows):
    path = os.path.join(ROOT, "test", "sim", "CrashData.sol")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    out = []
    out.append("// SPDX-License-Identifier: MIT")
    out.append("// GENERATED by sim/history.py - do not edit by hand.")
    out.append("// Daily closes. ETH/USD: Coin Metrics community data. Gold/USD: PAXG and XAUT (Coin Metrics), or monthly")
    out.append("// averages (github.com/datasets/gold-prices) interpolated before 2020.")
    out.append("pragma solidity 0.6.11;")
    out.append("")
    out.append("library CrashData {")
    out.append("    uint constant WINDOWS = %d;" % len(windows))
    out.append("")
    out.append("    // Unix timestamp of each day at 12:00 UTC, ETH/USD and gold/USD in US cents")
    out.append("    function window(uint _i) internal pure returns (string memory name, uint[] memory day, uint[] memory ethUsd, uint[] memory xauUsd) {")
    for i, w in enumerate(windows):
        days = w["days"]
        n = len(days)
        out.append("        %sif (_i == %d) {" % ("" if i == 0 else "} else ", i))
        out.append("            name = \"%s to %s (ETH/Menger %s)\";" % (w["peak"], w["trough"], pct(w["fall"]).replace(" ", "")))
        out.append("            day = new uint[](%d); ethUsd = new uint[](%d); xauUsd = new uint[](%d);" % (n, n, n))
        for j, s in enumerate(days):
            ts = int(dt.datetime(s["day"].year, s["day"].month, s["day"].day, 12, tzinfo=dt.timezone.utc).timestamp())
            out.append("            day[%d] = %d; ethUsd[%d] = %d; xauUsd[%d] = %d; // %s" % (
                j, ts, j, cents(s["eth_usd"]), j, cents(s["gold_usd"]), s["day"].strftime("%a %Y-%m-%d")))
    out.append("        } else { revert(\"CrashData: no such window\"); }")
    out.append("    }")
    out.append("}")
    with open(path, "w") as f:
        f.write("\n".join(out) + "\n")
    return path


def main():
    download()
    series = build_series()
    lines = stats(series)
    windows = select_windows(series)
    lines.append("## The %d largest falls of ETH measured in gold within %d days (replayed with the contracts)\n" % (len(windows), WINDOW_DAYS))
    lines.append("| Peak | Trough | ETH/Menger | ETH/USD over the same days |\n|---|---|---|---|")
    for w in windows:
        a = next(s for s in w["days"] if s["day"] == w["peak"])
        b = next(s for s in w["days"] if s["day"] == w["trough"])
        lines.append("| %s | %s | %s | %s |" % (w["peak"], w["trough"], pct(w["fall"]), pct(b["eth_usd"] / a["eth_usd"] - 1)))
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "history_stats.md"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))
    print("wrote", write_solidity(windows), file=sys.stderr)


if __name__ == "__main__":
    main()
