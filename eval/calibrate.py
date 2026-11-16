"""Calibrate the agent-based market to the real data by random search.

For each candidate set of parameters, simulate half an hour (two seeds),
compute the stylized-fact statistics and their distance to the median over
the real stocks (AAPL, MSFT, AMD, INTC, QQQ on 2019-01-30). Keep the best.

    python eval/calibrate.py [--trials 300] [--out results/calibration.json]
"""

from __future__ import annotations

import argparse
import json
import math
import os
import random
import sys
from dataclasses import asdict
from multiprocessing import Pool

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "sim"))

from bourse_sim import facts as F  # noqa: E402
from bourse_sim import market as M  # noqa: E402

STOCKS = ["AAPL", "MSFT", "AMD", "INTC", "QQQ"]
# statistic -> (scale: "log" for ratios of positive quantities, "lin" for differences, weight)
TARGETS = {
    "trades_per_minute": ("log", 0.5),
    "volatility_10s_ticks": ("log", 2.0),
    "kurtosis_10s": ("log", 1.0),
    "hill_10s": ("log", 1.0),
    "abs_return_acf_mean_1_30": ("lin10", 5.0),
    "return_acf_mean_abs_1_30": ("lin10", 3.0),
    "sign_acf_exponent": ("lin", 1.0),
    "sign_acf_1": ("lin", 1.0),
    "spread_mean_ticks": ("log", 1.0),
    "interarrival_cv": ("log", 1.0),
    "add_at_best_or_inside": ("lin", 0.5),
}

SPACE = {
    "limit_rate": (20, 200, "log"),
    "market_rate": (1, 10, "log"),
    "cancel_rate": (0.05, 2.0, "log"),
    "mu": (0.4, 1.3, "lin"),
    "meta_rate": (0.005, 0.15, "log"),
    "meta_child_rate": (0.2, 3.0, "log"),
    "fund_rate": (0.2, 4.0, "log"),
    "fund_strength": (0.005, 0.3, "log"),
    "fund_vol": (0.00005, 0.0006, "log"),
    "vol_of_vol": (0.0, 1.5, "lin"),
    "vol_tau": (60, 1200, "log"),
    "chart_rate": (0.01, 2.0, "log"),
    "chart_threshold": (0.0003, 0.002, "log"),
    "lot_p": (0.4, 0.9, "lin"),
    "market_lot_p": (0.15, 0.6, "lin"),
    "hawkes_tau": (10, 900, "log"),
    "hawkes_branching": (0.0, 0.9, "lin"),
    "activity_vol": (0.0, 1.5, "lin"),
    "activity_tau": (60, 1800, "log"),
}


def real_targets():
    rs = []
    for s in STOCKS:
        t, q, a = F.load_real(os.path.join(ROOT, "data", "derived"), s)
        rs.append(F.facts(t, q, a))
    return {k: float(np.median([r[k] for r in rs if r.get(k) is not None and not (isinstance(r[k], float) and math.isnan(r[k]))])) for k in TARGETS}, rs


def sample(rng: random.Random) -> dict:
    p = {}
    for k, (lo, hi, sc) in SPACE.items():
        p[k] = math.exp(rng.uniform(math.log(lo), math.log(hi))) if sc == "log" else rng.uniform(lo, hi)
    return p


def to_params(x: dict) -> M.Params:
    x = dict(x)
    br = x.pop("hawkes_branching")
    p = M.Params(**{k: v for k, v in x.items() if k in M.Params.__dataclass_fields__})
    # branching ratio = jump * tau * (rate of market orders of all kinds)
    p.hawkes_jump = br / (p.hawkes_tau * (p.market_rate + p.fund_rate * 0.5 + p.meta_rate * 10))
    return p


def simulate(x: dict, seed: int, seconds: float = 1800.0, makers: bool = True):
    m = M.Market(to_params(x), seed=seed)
    if makers:
        m.add_maker(M.FixedMaker("mm1", 11))
        m.add_maker(M.AvellanedaStoikov("mm2", 12))
    m.run(seconds)
    return m


def distance(f: dict, target: dict) -> float:
    d = 0.0
    for k, (sc, w) in TARGETS.items():
        v, t = f.get(k), target[k]
        if v is None or (isinstance(v, float) and (math.isnan(v) or math.isinf(v))):
            d += w * 4
            continue
        if sc == "log":
            d += w * (math.log(max(v, 1e-6) / max(t, 1e-6))) ** 2
        elif sc == "lin10":
            d += w * (10 * (v - t)) ** 2
        else:
            d += w * (v - t) ** 2 * 4
    failed = sum(1 for ok in F.check(f).values() if not ok)
    return d + 2.0 * failed


def evaluate(args):
    x, target = args
    try:
        ds, fs = [], []
        for seed in (1, 2):
            m = simulate(x, seed)
            if len(m.trades) < 200:
                return x, 1e9, None
            f = F.facts(m.trades, m.quotes, m.adds)
            ds.append(distance(f, target))
            fs.append(f)
        return x, float(np.mean(ds)), F.nan_to_none({k: v for k, v in fs[0].items() if not isinstance(v, (list, dict))})
    except Exception as e:  # a bad corner of the space
        return x, 1e9, {"error": str(e)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trials", type=int, default=300)
    ap.add_argument("--out", default=os.path.join(ROOT, "results", "calibration.json"))
    a = ap.parse_args()
    target, real = real_targets()
    print("targets (median of the real stocks):", {k: round(v, 3) for k, v in target.items()}, flush=True)
    rng = random.Random(42)
    cands = [sample(rng) for _ in range(a.trials)]
    # the hand-set defaults are a candidate too
    d = asdict(M.Params())
    cands.append({k: d.get(k, 0.5) for k in SPACE if k != "hawkes_branching"} | {"hawkes_branching": 0.5})
    with Pool(max(1, os.cpu_count() - 2)) as pool:
        res = pool.map(evaluate, [(x, target) for x in cands], chunksize=1)
    res.sort(key=lambda r: r[1])
    # refine around the best: a second, local round
    best = res[0][0]
    local = []
    for _ in range(a.trials // 3):
        y = {}
        for k, (lo, hi, sc) in SPACE.items():
            if sc == "log":
                y[k] = min(hi, max(lo, best[k] * math.exp(rng.gauss(0, 0.25))))
            else:
                y[k] = min(hi, max(lo, best[k] + rng.gauss(0, 0.08 * (hi - lo))))
        local.append(y)
    with Pool(max(1, os.cpu_count() - 2)) as pool:
        res += pool.map(evaluate, [(x, target) for x in local], chunksize=1)
    res.sort(key=lambda r: r[1])
    for x, dist, f in res[:5]:
        print(round(dist, 3), {k: round(v, 4) for k, v in x.items()}, flush=True)
        print("   ", f, flush=True)
    p = to_params(res[0][0])
    out = {
        "targets": target,
        "real": {s: F.nan_to_none({k: v for k, v in r.items() if not isinstance(v, (list, dict))}) for s, r in zip(STOCKS, real)},
        "trials": len(res),
        "best_distance": res[0][1],
        "best": res[0][0],
        "params": asdict(p),
        "best_facts": res[0][2],
        "default_distance": next(d for x, d, _ in res if x is cands[-1] or x == cands[-1]),
    }
    os.makedirs(os.path.dirname(a.out), exist_ok=True)
    json.dump(out, open(a.out, "w"), indent=1)
    print("wrote", a.out)


if __name__ == "__main__":
    main()
