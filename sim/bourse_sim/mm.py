"""Market makers trained with reinforcement learning (PPO).

The environment is the agent-based market (market.py) with the usual
background traders and two rule-based market makers. The learning maker
decides once a second where to quote: a half-spread (0.5 to 3.5 ticks
around the mid) and a skew (-2 to +2 ticks, to lean against its inventory).
It sees its inventory, the spread, the recent return and volatility, the
imbalance of recent trades and of the best queues, and the time left. Its
reward is the change of its wealth marked to the mid, minus a penalty on the
square of its inventory (the price risk it carries).

PPO (Schulman et al. 2017) with a clipped objective, generalized advantage
estimation, and a small actor-critic network.
"""

from __future__ import annotations

import math
import time
from dataclasses import dataclass

import numpy as np
import torch
import torch.nn as nn

from . import market as M

HALF_SPREADS = [0.5, 1.5, 2.5, 3.5]
SKEWS = [-2, -1, 0, 1, 2]
ACTIONS = [(h, s) for h in HALF_SPREADS for s in SKEWS]
OBS = 7


class LearningMaker(M.Maker):
    period = 1e18  # driven by the environment, not by the market clock

    def act(self, market):
        pass


class MakerEnv:
    def __init__(self, params: M.Params, seed: int = 0, seconds: float = 1800.0, step: float = 1.0, size: int = 500,
                 max_inventory: int = 5000, inventory_penalty: float = 2e-3, record: bool = False):
        self.params, self.seed, self.seconds, self.dt = params, seed, seconds, step
        self.size, self.max_inventory, self.penalty, self.record = size, max_inventory, inventory_penalty, record

    def reset(self, seed: int | None = None):
        if seed is not None:
            self.seed = seed
        self.m = M.Market(self.params, seed=self.seed, record=True)
        self.m.add_maker(M.FixedMaker("fixed", 11))
        self.m.add_maker(M.AvellanedaStoikov("as", 12))
        self.agent = LearningMaker("learner", 13, size=self.size, max_inventory=self.max_inventory)
        self.m.run(60.0)  # warm up
        self.t0 = self.m.t
        self.wealth = self.agent.wealth(self.m.mid())
        self.trade_i = len(self.m.trades)
        self.seed += 1
        return self.obs()

    def obs(self) -> np.ndarray:
        m = self.m
        b, bq, a, aq = m.bbo()
        spread = (a - b) / M.TICK if b and a else 5.0
        h = m.history
        r10 = (h[-1][1] - h[-11][1]) / M.TICK if len(h) > 11 else 0.0
        rets = [(h[i][1] - h[i - 1][1]) / M.TICK for i in range(max(1, len(h) - 30), len(h))]
        vol = math.sqrt(sum(x * x for x in rets) / len(rets)) if rets else 0.0
        buy = sell = 0
        for t, _, q, agg in m.trades[-200:]:
            if t >= m.t - 10:
                if agg == "B":
                    buy += q
                else:
                    sell += q
        qi = (bq - aq) / (bq + aq) if bq + aq else 0.0
        left = 1 - (m.t - self.t0) / self.seconds
        return np.array([
            self.agent.inventory / self.max_inventory,
            min(spread, 10) / 5,
            max(-5, min(5, r10)) / 5,
            min(vol, 10) / 3,
            (buy - sell) / (buy + sell + 100),
            qi,
            left,
        ], np.float32)

    def step(self, action: int):
        h, s = ACTIONS[action]
        self.agent.quote(self.m, h, s)
        self.m.run(self.dt)
        w = self.agent.wealth(self.m.mid())
        pnl = (w - self.wealth) / 1e4  # dollars
        self.wealth = w
        inv = self.agent.inventory / 100
        reward = pnl / 10 - self.penalty * inv * inv
        done = self.m.t - self.t0 >= self.seconds
        return self.obs(), float(reward), done, pnl


class ActorCritic(nn.Module):
    def __init__(self, hidden: int = 64):
        super().__init__()
        self.body = nn.Sequential(nn.Linear(OBS, hidden), nn.Tanh(), nn.Linear(hidden, hidden), nn.Tanh())
        self.pi = nn.Linear(hidden, len(ACTIONS))
        self.v = nn.Linear(hidden, 1)

    def forward(self, x):
        z = self.body(x)
        return self.pi(z), self.v(z).squeeze(-1)


@dataclass
class PPOConfig:
    steps: int = 200_000
    rollout: int = 2048
    epochs: int = 8
    minibatch: int = 256
    lr: float = 3e-4
    gamma: float = 0.99
    lam: float = 0.95
    clip: float = 0.2
    entropy: float = 0.02
    value_coef: float = 0.5
    normalize_rewards: bool = True


def train(params: M.Params, cfg: PPOConfig = PPOConfig(), seed: int = 0, log=print) -> tuple[ActorCritic, list[dict]]:
    torch.manual_seed(seed)
    np.random.seed(seed)
    env = MakerEnv(params, seed=10_000 + seed)
    net = ActorCritic()
    opt = torch.optim.Adam(net.parameters(), lr=cfg.lr)
    obs = env.reset()
    history = []
    # running scale of the discounted return, to normalize rewards
    ret_run, r_count, r_mean, r_m2 = 0.0, 0, 0.0, 0.0
    ep_pnl, ep_ret, done_eps = 0.0, 0.0, []
    t_start = time.time()
    for it in range(cfg.steps // cfg.rollout):
        O = np.zeros((cfg.rollout, OBS), np.float32)
        A = np.zeros(cfg.rollout, np.int64)
        R = np.zeros(cfg.rollout, np.float32)
        D = np.zeros(cfg.rollout, np.float32)
        V = np.zeros(cfg.rollout, np.float32)
        LP = np.zeros(cfg.rollout, np.float32)
        for k in range(cfg.rollout):
            with torch.no_grad():
                logits, v = net(torch.from_numpy(obs))
                dist = torch.distributions.Categorical(logits=logits)
                a = dist.sample()
            O[k], A[k], V[k], LP[k] = obs, a.item(), v.item(), dist.log_prob(a).item()
            obs, r, done, pnl = env.step(a.item())
            ep_ret += r
            if cfg.normalize_rewards:
                ret_run = ret_run * cfg.gamma + r
                r_count += 1
                d = ret_run - r_mean
                r_mean += d / r_count
                r_m2 += d * (ret_run - r_mean)
                r = r / (math.sqrt(r_m2 / max(1, r_count - 1)) + 1e-8) if r_count > 10 else r
                if done:
                    ret_run = 0.0
            R[k], D[k] = r, done
            ep_pnl += pnl
            if done:
                done_eps.append((ep_pnl, ep_ret, env.agent.inventory))
                ep_pnl, ep_ret = 0.0, 0.0
                obs = env.reset()
        with torch.no_grad():
            _, last_v = net(torch.from_numpy(obs))
        adv = np.zeros_like(R)
        gae = 0.0
        for k in reversed(range(cfg.rollout)):
            nv = last_v.item() if k == cfg.rollout - 1 else V[k + 1]
            nonterm = 1.0 - D[k]
            delta = R[k] + cfg.gamma * nv * nonterm - V[k]
            gae = delta + cfg.gamma * cfg.lam * nonterm * gae
            adv[k] = gae
        ret = adv + V
        Ot, At, LPt = torch.from_numpy(O), torch.from_numpy(A), torch.from_numpy(LP)
        ADVt = torch.from_numpy((adv - adv.mean()) / (adv.std() + 1e-8))
        RETt = torch.from_numpy(ret)
        for _ in range(cfg.epochs):
            perm = torch.randperm(cfg.rollout)
            for s in range(0, cfg.rollout, cfg.minibatch):
                idx = perm[s:s + cfg.minibatch]
                logits, v = net(Ot[idx])
                dist = torch.distributions.Categorical(logits=logits)
                ratio = torch.exp(dist.log_prob(At[idx]) - LPt[idx])
                surr = torch.min(ratio * ADVt[idx], torch.clamp(ratio, 1 - cfg.clip, 1 + cfg.clip) * ADVt[idx])
                loss = -surr.mean() + cfg.value_coef * ((v - RETt[idx]) ** 2).mean() - cfg.entropy * dist.entropy().mean()
                opt.zero_grad()
                loss.backward()
                nn.utils.clip_grad_norm_(net.parameters(), 0.5)
                opt.step()
        recent = done_eps[-5:]
        row = {
            "iteration": it + 1,
            "steps": (it + 1) * cfg.rollout,
            "seconds": round(time.time() - t_start, 1),
            "episodes": len(done_eps),
            "mean_episode_pnl": float(np.mean([e[0] for e in recent])) if recent else None,
            "mean_episode_return": float(np.mean([e[1] for e in recent])) if recent else None,
            "rollout_reward": float(R.sum()),
        }
        history.append(row)
        log(row)
    return net, history


def policy(net: ActorCritic, greedy: bool = True):
    def choose(obs: np.ndarray) -> int:
        with torch.no_grad():
            logits, _ = net(torch.from_numpy(obs))
        if greedy:
            return int(torch.argmax(logits).item())
        return int(torch.distributions.Categorical(logits=logits).sample().item())

    return choose


def evaluate(params: M.Params, choose, seeds, seconds: float = 1800.0) -> dict:
    """Run the learning maker's seat with a policy (a function obs -> action),
    or with a rule ("fixed", "as") in the same seat, on the given seeds."""
    pnls, invs, fills, spreads = [], [], [], []
    for sd in seeds:
        env = MakerEnv(params, seed=sd, seconds=seconds)
        obs = env.reset()
        rule = None
        if choose == "fixed":
            rule = M.FixedMaker("x", 13)
        elif choose == "as":
            rule = M.AvellanedaStoikov("x", 13)
        pnl, inv_abs, n, done = 0.0, 0.0, 0, False
        filled_before = env.m.volume
        while not done:
            if rule is not None:
                # the rule quotes through the learner's seat
                if isinstance(rule, M.AvellanedaStoikov):
                    q = env.agent.inventory / env.agent.size
                    hh = env.m.history
                    if len(hh) > 10:
                        rets = [(hh[i][1] - hh[i - 1][1]) / M.TICK for i in range(max(1, len(hh) - 30), len(hh))]
                        s2 = max(sum(r * r for r in rets) / len(rets), 0.01)
                    else:
                        s2 = 1.0
                    skew = q * rule.gamma * s2 * rule.horizon
                    spread = rule.gamma * s2 * rule.horizon + (2 / rule.gamma) * math.log(1 + rule.gamma / rule.k)
                    env.agent.quote(env.m, max(0.5, spread / 2), skew)
                else:
                    env.agent.quote(env.m, rule.h, 0.0)
                env.m.run(env.dt)
                w = env.agent.wealth(env.m.mid())
                p = (w - env.wealth) / 1e4
                env.wealth = w
                done = env.m.t - env.t0 >= env.seconds
                obs = None
            else:
                obs, _, done, p = env.step(choose(obs))
            pnl += p
            inv_abs += abs(env.agent.inventory)
            n += 1
        pnls.append(pnl)
        invs.append(inv_abs / n)
        fills.append(env.m.volume - filled_before)
        sp = [(a - b) / M.TICK for _, b, _, a, _ in env.m.quotes if b and a]
        spreads.append(float(np.mean(sp)) if sp else float("nan"))
    pnls = np.array(pnls)
    return {
        "episodes": len(seeds),
        "pnl_mean": float(pnls.mean()),
        "pnl_std": float(pnls.std()),
        "pnl_sharpe": float(pnls.mean() / pnls.std()) if pnls.std() > 0 else float("nan"),
        "share_of_episodes_profitable": float(np.mean(pnls > 0)),
        "mean_abs_inventory": float(np.mean(invs)),
        "market_spread_ticks": float(np.nanmean(spreads)),
    }
