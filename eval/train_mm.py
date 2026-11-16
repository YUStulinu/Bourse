"""Train the market maker with PPO on the calibrated market, then compare
it with the rule-based makers in the same seat on unseen days.

    python eval/train_mm.py [--steps 200000] [--episodes 20]
"""

from __future__ import annotations

import argparse
import json
import os
import sys

import torch

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "sim"))

from bourse_sim import market as M  # noqa: E402
from bourse_sim import mm  # noqa: E402


calibrated_params = M.calibrated_params


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--steps", type=int, default=200_000)
    ap.add_argument("--episodes", type=int, default=20)
    a = ap.parse_args()
    torch.set_num_threads(2)
    params = calibrated_params()
    net, history = mm.train(params, mm.PPOConfig(steps=a.steps), seed=0, log=lambda r: print(json.dumps(r), flush=True))
    torch.save(net.state_dict(), os.path.join(ROOT, "results", "mm_policy.pt"))
    seeds = list(range(900_000, 900_000 + a.episodes))  # days never seen in training
    out = {"steps": a.steps, "training": history}
    for name, choose in (("learned (PPO)", mm.policy(net)), ("Avellaneda-Stoikov", "as"), ("fixed, at the touch", "fixed")):
        out[name] = mm.evaluate(params, choose, seeds)
        print(name, out[name], flush=True)
    # what the learned policy does: its action frequencies on the test days
    counts = [0] * len(mm.ACTIONS)
    env = mm.MakerEnv(params, seed=seeds[0])
    o = env.reset()
    choose = mm.policy(net)
    done = False
    while not done:
        k = choose(o)
        counts[k] += 1
        o, _, done, _ = env.step(k)
    out["learned_action_share"] = {f"h={h} skew={s}": c / sum(counts) for (h, s), c in zip(mm.ACTIONS, counts) if c}
    json.dump(out, open(os.path.join(ROOT, "results", "mm.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
