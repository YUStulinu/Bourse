"""Statistical realism of the simulated market against the real one.

The same statistics on five real stocks (NASDAQ ITCH, 2019-01-30, regular
hours) and on simulated trading days (6.5 hours each) of the calibrated
market, with rule-based market makers and with the learned one.

    python eval/realism.py [--days 4]
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sys

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "sim"))
sys.path.insert(0, os.path.join(ROOT, "eval"))

from bourse_sim import facts as F  # noqa: E402
from bourse_sim import market as M  # noqa: E402

STOCKS = ["AAPL", "MSFT", "AMD", "INTC", "QQQ"]
DAY = 6.5 * 3600


def sim_day(params, seed, learned=None):
    m = M.Market(params, seed=seed)
    m.add_maker(M.FixedMaker("fixed", 11))
    m.add_maker(M.AvellanedaStoikov("as", 12))
    if learned is not None:
        from bourse_sim import mm

        agent = mm.LearningMaker("learner", 13)
        choose = mm.policy(learned)
        env = mm.MakerEnv(params, seconds=DAY)
        env.m, env.agent, env.t0 = m, agent, 0.0
        env.wealth = 0

        class Driver(M.Agent):
            period = 1.0

            def act(self, market):
                agent.quote(market, *mm.ACTIONS[choose(env.obs())])

        d = Driver("driver", 0)
        d.next_time = 0.0
        m.makers.append(d)
    m.run(DAY)
    # keep the same window as the real data: skip the first and last ten minutes
    lo, hi = 600, DAY - 600
    trades = [t for t in m.trades if lo <= t[0] < hi]
    quotes = [q for q in m.quotes if lo <= q[0] < hi]
    adds = [a for a in m.adds if lo <= a[0] < hi]
    return trades, quotes, adds


def normalized_returns(quotes, dt=10.0):
    r = F.mid_returns(quotes, dt)
    r = r[np.isfinite(r)]
    return (r - r.mean()) / (r.std() or 1)


def ks(a, b):
    a, b = np.sort(a), np.sort(b)
    grid = np.concatenate([a, b])
    return float(np.max(np.abs(np.searchsorted(a, grid, side="right") / len(a) - np.searchsorted(b, grid, side="right") / len(b))))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=4)
    a = ap.parse_args()
    params = M.calibrated_params()
    out = {"real": {}, "sim": {}, "checks": {}}
    real_returns, sign_acfs, abs_acfs = [], {}, {}
    for s in STOCKS:
        t, q, ad = F.load_real(os.path.join(ROOT, "data", "derived"), s)
        f = F.facts(t, q, ad)
        out["real"][s] = F.nan_to_none(f)
        out["checks"][s] = F.check(f)
        real_returns.append(normalized_returns(q))
        sign_acfs[s] = f["sign_acf"]
        abs_acfs[s] = f.get("abs_return_acf_10s")
        print(s, sum(out["checks"][s].values()), "of", len(F.CHECKS), flush=True)
    real_r = np.concatenate(real_returns)
    learned = None
    pol = os.path.join(ROOT, "results", "mm_policy.pt")
    if os.path.exists(pol):
        import torch

        from bourse_sim import mm

        learned = mm.ActorCritic()
        learned.load_state_dict(torch.load(pol))
    variants = [("simulated, rule-based makers", None)] + ([("simulated, with the learned maker", learned)] if learned is not None else [])
    for name, net in variants:
        fs, rs = [], []
        for d in range(a.days):
            t, q, ad = sim_day(params, 5000 + d, net)
            f = F.facts(t, q, ad)
            fs.append(f)
            rs.append(normalized_returns(q))
            print(name, "day", d, sum(F.check(f).values()), "of", len(F.CHECKS), flush=True)
        keys = [k for k, v in fs[0].items() if isinstance(v, (int, float))]
        mean = {k: float(np.nanmean([f.get(k, np.nan) if f.get(k) is not None else np.nan for f in fs])) for k in keys}
        out["sim"][name] = F.nan_to_none(mean)
        out["checks"][name] = F.check(mean)
        out["sim"][name]["ks_10s_returns_vs_real"] = ks(np.concatenate(rs), real_r)
        sign_acfs[name] = fs[0]["sign_acf"]
        abs_acfs[name] = fs[0].get("abs_return_acf_10s")
    # the real stocks against each other, for scale
    out["ks_10s_returns_real_stock_vs_rest"] = {
        s: ks(real_returns[i], np.concatenate([r for j, r in enumerate(real_returns) if j != i])) for i, s in enumerate(STOCKS)
    }
    json.dump(out, open(os.path.join(ROOT, "results", "realism.json"), "w"), indent=1)
    try:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        os.makedirs(os.path.join(ROOT, "docs"), exist_ok=True)
        fig, ax = plt.subplots(1, 3, figsize=(15, 4.2))
        for name, acfd in sign_acfs.items():
            lags = np.array(sorted(acfd, key=int), float)
            vals = np.array([acfd[k] for k in sorted(acfd, key=int)])
            ok = vals > 0
            sim = name.startswith("sim")
            ax[0].loglog(lags[ok], vals[ok], "-" if sim else ":", lw=2.2 if sim else 1.2, label=name)
        ax[0].set_title("autocorrelation of trade signs")
        ax[0].set_xlabel("lag (trades)")
        for name, vals in abs_acfs.items():
            if vals:
                sim = name.startswith("sim")
                ax[1].plot(range(1, len(vals) + 1), vals, "-" if sim else ":", lw=2.2 if sim else 1.2, label=name)
        ax[1].axhline(0, color="gray", lw=0.5)
        ax[1].set_title("autocorrelation of |10 s returns|")
        ax[1].set_xlabel("lag (10 s)")
        bins = np.linspace(-8, 8, 81)
        ax[2].hist(real_r, bins=bins, density=True, histtype="step", lw=1.2, label="real (5 stocks)", log=True)
        t, q, ad = sim_day(params, 5000)
        ax[2].hist(normalized_returns(q), bins=bins, density=True, histtype="step", lw=2.2, label="simulated", log=True)
        x = np.linspace(-8, 8, 200)
        ax[2].plot(x, np.exp(-x * x / 2) / math.sqrt(2 * math.pi), color="gray", lw=0.8, label="Gaussian")
        ax[2].set_ylim(1e-5, 1)
        ax[2].set_title("10 s returns, normalized")
        for x_ in ax:
            x_.legend(fontsize=7)
        fig.tight_layout()
        fig.savefig(os.path.join(ROOT, "docs", "realism.png"), dpi=110)
    except ImportError:
        pass
    print(json.dumps(out["checks"], indent=1))


if __name__ == "__main__":
    main()
