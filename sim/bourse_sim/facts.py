"""Stylized facts of markets, computed the same way on real data (exported
from ITCH by itch-replay) and on simulated markets:

1. returns are heavy-tailed (excess kurtosis, Hill tail exponent);
2. returns are not autocorrelated;
3. volatility clusters (|returns| are autocorrelated, slowly);
4. the signs of trades have long memory (autocorrelation decaying as a
   power law with exponent below 1);
5. the spread is mostly one or two ticks;
6. price impact grows concavely with trade size;
7. trades come in bursts (inter-arrival times more dispersed than Poisson);
8. limit orders are placed at distances from the best price with a fat tail.

Input: trades (time s, price, shares, aggressor 'B'/'S'), quotes sampled on
a fixed grid (time, bid, bid size, ask, ask size) and limit-order additions
(time, side, price, bid, ask). Prices in 1/10000 dollar.
"""

from __future__ import annotations

import csv
import math
import os

import numpy as np

TICK = 100


def load_real(directory: str, symbol: str, start_s: float = 34200.0 + 600, end_s: float = 57600.0 - 600, max_adds: int = 400_000):
    """Read the CSVs of one stock exported by itch-replay; keep regular
    hours without the first and last ten minutes (the auctions)."""

    def rows(name):
        with open(os.path.join(directory, f"{symbol}_{name}.csv"), newline="") as f:
            r = csv.reader(f)
            next(r)
            yield from r

    trades = []
    for ts, price, shares, agg, kind in rows("trades"):
        t = int(ts) / 1e9
        if start_s <= t < end_s and agg in "BS" and kind in "ECP":
            trades.append((t, int(price), int(shares), agg))
    quotes = []
    for ts, bid, bq, ask, aq in rows("quotes"):
        t = int(ts) / 1e9
        if start_s <= t < end_s:
            quotes.append((t, int(bid), int(bq), int(ask), int(aq)))
    adds = []
    for ts, typ, side, price, shares, bid, ask, age in rows("events"):
        if typ != "A":
            continue
        t = int(ts) / 1e9
        if start_s <= t < end_s:
            adds.append((t, side, int(price), int(bid), int(ask)))
            if len(adds) >= max_adds:
                break
    return trades, quotes, adds


def acf(x: np.ndarray, lags) -> np.ndarray:
    x = np.asarray(x, float) - np.mean(x)
    v = np.dot(x, x)
    return np.array([np.dot(x[:-k], x[k:]) / v if 0 < k < len(x) else np.nan for k in lags])


def hill(x: np.ndarray, frac: float = 0.05) -> float:
    """Hill estimator of the tail exponent of |x| from the largest frac."""
    a = np.sort(np.abs(x[x != 0]))[::-1]
    k = max(5, int(frac * len(a)))
    if len(a) <= k or a[k] <= 0 or a[0] == a[k]:
        return float("nan")
    return float(1.0 / np.mean(np.log(a[:k] / a[k])))


def power_fit(lags, values) -> float:
    """Exponent g of values ~ lags^-g (least squares in log-log, positive values only)."""
    lags, values = np.asarray(lags, float), np.asarray(values, float)
    ok = values > 0
    if ok.sum() < 3:
        return float("nan")
    return float(-np.polyfit(np.log(lags[ok]), np.log(values[ok]), 1)[0])


def mid_returns(quotes, dt: float) -> np.ndarray:
    q = np.array([(t, (b + a) / 2) for t, b, _, a, _ in quotes if b > 0 and a > b])
    if len(q) < 3:
        return np.empty(0)
    grid = np.arange(q[0, 0], q[-1, 0], dt)
    idx = np.searchsorted(q[:, 0], grid, side="right") - 1
    m = q[np.clip(idx, 0, len(q) - 1), 1]
    return np.diff(np.log(m))


def facts(trades, quotes, adds) -> dict:
    out = {}
    trades = sorted(trades)
    out["trades"] = len(trades)
    span = (trades[-1][0] - trades[0][0]) if len(trades) > 1 else 1.0
    out["trades_per_minute"] = len(trades) / span * 60
    # 1. 2. 3. returns of the mid price over 10 s and 60 s
    for dt, name in ((10.0, "10s"), (60.0, "60s")):
        r = mid_returns(quotes, dt)
        r = r[np.isfinite(r)]
        if len(r) < 20:
            continue
        z = (r - r.mean()) / (r.std() or 1)
        out[f"kurtosis_{name}"] = float(np.mean(z**4) - 3)
        out[f"hill_{name}"] = hill(r)
        out[f"return_acf1_{name}"] = float(acf(r, [1])[0])
        if name == "10s":
            out["volatility_10s_ticks"] = float(r.std() * np.mean([(b + a_) / 2 for _, b, _, a_, _ in quotes if b > 0 and a_ > b]) / TICK)
            lags = np.arange(1, 31)
            a = acf(np.abs(r), lags)
            out["abs_return_acf_10s"] = [float(x) for x in a]
            out["abs_return_acf_mean_1_30"] = float(np.nanmean(a))
            out["return_acf_mean_abs_1_30"] = float(np.nanmean(np.abs(acf(r, lags))))
    # 4. trade signs
    s = np.array([1 if x[3] == "B" else -1 for x in trades], float)
    lags = np.unique(np.logspace(0, 3, 25).astype(int))
    a = acf(s, lags)
    out["sign_acf"] = {int(k): float(v) for k, v in zip(lags, a)}
    out["sign_acf_exponent"] = power_fit(lags[(lags >= 5) & (lags <= 500)], a[(lags >= 5) & (lags <= 500)])
    out["sign_acf_1"] = float(a[0])
    # 5. spread
    sp = np.array([(a_ - b) / TICK for _, b, _, a_, _ in quotes if b > 0 and a_ > b])
    out["spread_mean_ticks"] = float(sp.mean())
    out["spread_one_tick"] = float(np.mean(sp <= 1.0001))
    out["spread_at_most_two_ticks"] = float(np.mean(sp <= 2.0001))
    # 6. impact: mid change from just before a trade to 10 s after, signed by aggressor, by size bucket
    q = np.array([(t, (b + a_) / 2) for t, b, _, a_, _ in quotes if b > 0 and a_ > b])
    if len(q) > 10 and len(trades) > 100:
        tt = np.array([x[0] for x in trades])
        sz = np.array([x[2] for x in trades], float)
        i0 = np.clip(np.searchsorted(q[:, 0], tt, side="right") - 1, 0, len(q) - 1)
        i1 = np.clip(np.searchsorted(q[:, 0], tt + 10.0, side="right") - 1, 0, len(q) - 1)
        imp = s * (q[i1, 1] - q[i0, 1]) / TICK
        rel = sz / np.mean(sz)
        edges = np.quantile(rel, np.linspace(0, 1, 7))
        xs, ys = [], []
        for lo, hi in zip(edges[:-1], edges[1:]):
            m = (rel >= lo) & (rel <= hi)
            if m.sum() > 20 and hi > lo:
                xs.append(float(np.mean(rel[m])))
                ys.append(float(np.mean(imp[m])))
        out["impact_by_size"] = list(zip(xs, ys))
        out["impact_mean_ticks"] = float(np.mean(imp))
        ok = [(x, y) for x, y in zip(xs, ys) if y > 0]
        out["impact_exponent"] = float(np.polyfit(np.log([x for x, _ in ok]), np.log([y for _, y in ok]), 1)[0]) if len(ok) >= 3 else float("nan")
    # 7. bursts
    gaps = np.diff([x[0] for x in trades])
    gaps = gaps[gaps >= 0]
    out["interarrival_cv"] = float(gaps.std() / gaps.mean()) if len(gaps) > 1 and gaps.mean() > 0 else float("nan")
    # 8. distance of new limit orders from the best price on their own side (ticks; negative = inside the spread)
    d = []
    for _, side, price, b, a_ in adds:
        if b <= 0 or a_ <= 0:
            continue
        d.append((b - price) / TICK if side == "B" else (price - a_) / TICK)
    d = np.array(d)
    if len(d):
        out["add_at_best_or_inside"] = float(np.mean(d <= 0))
        far = d[d >= 1]
        out["add_distance_tail_exponent"] = hill(far, 0.1) if len(far) > 100 else float("nan")
        out["add_distance_median_ticks"] = float(np.median(d))
    return out


# which facts count, and the condition that says a market shows them
def _num(f: dict, key: str, default: float) -> float:
    v = f.get(key)
    if v is None or (isinstance(v, float) and math.isnan(v)):
        return default
    return v


CHECKS = {
    "heavy tails (10 s kurtosis > 1)": lambda f: _num(f, "kurtosis_10s", 0) > 1,
    "no return autocorrelation (|acf| < 0.1)": lambda f: _num(f, "return_acf_mean_abs_1_30", 1) < 0.1,
    "volatility clustering (|r| acf > 0.05)": lambda f: _num(f, "abs_return_acf_mean_1_30", 0) > 0.05,
    "long memory of trade signs (exponent < 1)": lambda f: 0 < _num(f, "sign_acf_exponent", 9) < 1,
    "tight spread (mostly <= 2 ticks)": lambda f: _num(f, "spread_at_most_two_ticks", 0) > 0.5,
    "positive, concave impact (exponent < 1)": lambda f: _num(f, "impact_mean_ticks", 0) > 0 and _num(f, "impact_exponent", 0) < 1,
    "bursty trading (inter-arrival CV > 1)": lambda f: _num(f, "interarrival_cv", 0) > 1,
    "fat-tailed order placement (tail exponent < 3)": lambda f: 0 < _num(f, "add_distance_tail_exponent", 9) < 3,
}


def check(f: dict) -> dict:
    return {name: bool(test(f)) for name, test in CHECKS.items()}


def nan_to_none(x):
    if isinstance(x, float) and math.isnan(x):
        return None
    if isinstance(x, dict):
        return {k: nan_to_none(v) for k, v in x.items()}
    if isinstance(x, (list, tuple)):
        return [nan_to_none(v) for v in x]
    return x
