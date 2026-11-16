"""An agent-based market on the exchange's own matching engine.

Time is continuous: every kind of agent acts at random times (a Poisson
clock per kind, simulated with Gillespie's method), and market makers act on
a fixed schedule. The agents:

- zero-intelligence liquidity providers: limit orders a power-law number of
  ticks away from the opposite best price (Bouchaud et al. 2002), cancelled
  at random (each order at a constant rate);
- zero-intelligence liquidity takers: small market orders of random side;
- institutions that split large meta-orders (Pareto-distributed sizes) into
  small market orders over time: the source of the long memory of order
  signs (Lillo, Mike and Farmer 2005);
- fundamentalists, informed traders whose orders lean to buying when the
  price is below a fundamental value (a random walk with stochastic
  volatility) and to selling above it: the price follows the value;
- chartists, who follow the recent trend: volatility feeds on itself;
- market makers (market.py: fixed spread, Avellaneda-Stoikov; mm.py: a
  policy learned with reinforcement learning), quoting both sides.

Prices are integers in 1/10000 dollar; the tick is one cent.
"""

from __future__ import annotations

import math
import random
from dataclasses import dataclass, field

from . import engine as E

TICK = 100


def calibrated_params(path: str | None = None) -> "Params":
    """The parameters found by eval/calibrate.py (results/calibration.json),
    or the defaults when there is no calibration."""
    import json
    import os

    path = path or os.path.join(E.ROOT, "results", "calibration.json")
    if not os.path.exists(path):
        return Params()
    d = json.load(open(path))["params"]
    return Params(**{k: v for k, v in d.items() if k in Params.__dataclass_fields__})


@dataclass
class Params:
    p0: int = 100_0000            # start price, $100
    # zero intelligence
    limit_rate: float = 15.0      # limit orders per second
    market_rate: float = 3.0      # market orders per second
    cancel_rate: float = 0.2     # per resting ZI order per second
    mu: float = 0.7               # tail exponent of the distance of limit orders (ticks)
    max_distance: int = 200
    # meta-orders
    meta_rate: float = 0.03       # new meta-orders per second
    meta_alpha: float = 1.5       # Pareto exponent of their length (child orders)
    meta_child_rate: float = 1.0  # child orders per second per active meta-order
    # fundamentalists
    fund_rate: float = 2.0
    fund_vol: float = 0.0002      # per sqrt(second), relative
    vol_of_vol: float = 0.8       # the log of that volatility is an Ornstein-Uhlenbeck
    vol_tau: float = 300.0        # process with this standard deviation and time constant (s)
    fund_strength: float = 0.05   # P(buy) = 1/2 + strength * (value - mid) in ticks, clipped
    # chartists
    chart_rate: float = 0.3
    chart_window: float = 30.0    # seconds
    chart_threshold: float = 0.0005
    # sizes: round lots, a geometric number of them (market orders larger)
    lot_p: float = 0.8
    market_lot_p: float = 0.3
    # self-excitation (a Hawkes process): every market order raises the rate
    # of all zero-intelligence activity by hawkes_jump (relative), decaying
    # with time constant hawkes_tau
    hawkes_jump: float = 0.002
    hawkes_tau: float = 60.0
    # trading activity varies slowly ("trading time", Clark 1973): every
    # zero-intelligence and institutional rate is multiplied by
    # exp(activity_vol * z - activity_vol^2 / 2), z an Ornstein-Uhlenbeck
    # process of unit variance and time constant activity_tau (s)
    activity_vol: float = 0.0
    activity_tau: float = 600.0


@dataclass
class Agent:
    name: str
    account: int
    cash: int = 0       # 1/10000 dollar
    inventory: int = 0  # shares
    orders: set = field(default_factory=set)

    def on_fill(self, market: "Market", order_id: int, side: str, price: int, qty: int):
        pass

    def act(self, market: "Market"):
        pass

    def wealth(self, mid: int) -> int:
        return self.cash + self.inventory * mid


class Market:
    def __init__(self, params: Params | None = None, seed: int = 1, record: bool = True, sample_dt: float = 0.1, record_commands: bool = False):
        self.p = params or Params()
        self.rng = random.Random(seed)
        self.eng = E.Engine()
        self.sym = self.eng.add_symbol("SIM", TICK)
        self.t = 0.0
        self.next_id = 1
        self.owner: dict[int, Agent] = {}      # live order id -> agent
        self.zi = Agent("zi", 20)
        self.zi_orders: list[int] = []         # resting ZI orders (for random cancels)
        self.zi_pos: dict[int, int] = {}
        self.inst = Agent("institutions", 21)
        self.fund = Agent("fundamentalists", 22)
        self.chart = Agent("chartists", 23)
        self.makers: list[Agent] = []
        self.meta: list[list] = []             # active meta-orders: [side, children left]
        self.fundamental = float(self.p.p0)
        self.log_vol = 0.0
        self.activity_z = 0.0
        self.activity = 1.0
        self.history: list[tuple[float, int]] = []  # (t, mid) every second, for chartists
        self.record = record
        self.trades: list[tuple[float, int, int, str]] = []    # (t, price, qty, aggressor)
        self.quotes: list[tuple[float, int, int, int, int]] = []  # (t, bid, bid size, ask, ask size)
        self.adds: list[tuple[float, str, int, int, int]] = []    # (t, side, price, bid, ask)
        self.cancels = 0
        self.commands: list[dict] | None = [] if record_commands else None
        self.sample_dt = sample_dt
        self._next_sample = 0.0
        self._next_second = 0.0
        self.last_price = self.p.p0
        self.volume = 0
        self.excitation = 0.0
        self._seed_book()

    # ---- the book -------------------------------------------------------------

    def bbo(self):
        (b, bq), (a, aq) = self.eng.best(self.sym)
        return b, bq, a, aq

    def mid(self) -> int:
        b, _, a, _ = self.bbo()
        if b and a:
            return (b + a) // 2
        return b or a or self.last_price

    def _seed_book(self):
        for k in range(1, 30):
            for side, px in (("B", self.p.p0 - k * TICK), ("S", self.p.p0 + k * TICK)):
                self.submit(self.zi, side, px, 100 * (1 + self.rng.randrange(5)), zi=True)

    def submit(self, agent: Agent, side: str, price: int, qty: int, tif: int = E.DAY, flags: int = 0, zi: bool = False) -> int:
        oid = self.next_id
        self.next_id += 1
        if self.record and not (flags & E.MARKET) and tif == E.DAY:
            b, _, a, _ = self.bbo()
            self.adds.append((self.t, side, price, b, a))
        if self.commands is not None:
            self.commands.append({"type": E.NEW, "account": agent.account, "order_id": oid, "side": side, "price": price, "qty": qty, "tif": tif, "flags": flags})
        ev = self.eng.submit(E.NEW, side, self.sym, agent.account, oid, price, qty, tif, flags, ts=int(self.t * 1e9))
        self._events(ev, agent, oid, zi)
        return oid

    def cancel(self, oid: int):
        ag = self.owner.get(oid)
        if ag is None:
            return
        if self.commands is not None:
            self.commands.append({"type": E.CANCEL, "account": ag.account, "order_id": oid})
        ev = self.eng.submit(E.CANCEL, "B", self.sym, ag.account, oid, 0, 0, ts=int(self.t * 1e9))
        self._events(ev, ag, oid, False)

    def _drop(self, oid: int):
        ag = self.owner.pop(oid, None)
        if ag is not None:
            ag.orders.discard(oid)
        i = self.zi_pos.pop(oid, None)
        if i is not None:
            last = self.zi_orders.pop()
            if last != oid:
                self.zi_orders[i] = last
                self.zi_pos[last] = i

    def _events(self, ev, agent: Agent, oid: int, zi: bool):
        for e in ev:
            t = e["type"]
            if t == E.BOOKED:
                self.owner[oid] = agent
                agent.orders.add(oid)
                if zi:
                    self.zi_pos[oid] = len(self.zi_orders)
                    self.zi_orders.append(oid)
            elif t == E.EXECUTED:
                rid, qty, px = int(e["order_id"]), int(e["qty"]), int(e["price"])
                rest = self.owner.get(rid)
                rside = chr(e["side"])
                aggressor = "B" if rside == "S" else "S"
                for ag, sd in ((rest, rside), (agent, aggressor)):
                    if ag is None:
                        continue
                    sign = 1 if sd == "B" else -1
                    ag.inventory += sign * qty
                    ag.cash -= sign * qty * px
                    ag.on_fill(self, rid if ag is rest else oid, sd, px, qty)
                if e["flags"] & 1:
                    self._drop(rid)
                self.last_price = px
                self.volume += qty
                if self.record:
                    self.trades.append((self.t, px, qty, aggressor))
            elif t == E.CANCELLED:
                if e["qty2"] == 0:
                    self._drop(int(e["order_id"]))
                self.cancels += 1

    # ---- agents -------------------------------------------------------------------

    def lots(self, p: float | None = None) -> int:
        p = self.p.lot_p if p is None else p
        n = 1
        while self.rng.random() > p and n < 50:
            n += 1
        return 100 * n

    def zi_limit(self):
        side = "B" if self.rng.random() < 0.5 else "S"
        b, _, a, _ = self.bbo()
        ref = a if side == "B" else b
        if not ref:
            ref = self.mid() + (TICK if side == "B" else -TICK)
        # P(d >= k) ~ k^-mu for k >= 1 ticks
        d = min(self.p.max_distance, int(self.rng.random() ** (-1.0 / self.p.mu)))
        px = ref - d * TICK if side == "B" else ref + d * TICK
        if px <= 0:
            return
        self.submit(self.zi, side, px, self.lots(), zi=True)

    def market_order(self, agent: Agent, side: str, qty: int):
        """A protected market order: immediate-or-cancel with a limit ten
        ticks past the best opposite price (exchanges put such collars on
        market orders so that one order cannot empty the book)."""
        self.excitation = min(self.excitation + self.p.hawkes_jump, 20.0)
        b, _, a, _ = self.bbo()
        ref = a if side == "B" else b
        if not ref:
            return
        limit = ref + 10 * TICK if side == "B" else ref - 10 * TICK
        if limit > 0:
            self.submit(agent, side, limit, qty, E.IOC)

    def zi_cancel(self):
        if self.zi_orders:
            self.cancel(self.zi_orders[self.rng.randrange(len(self.zi_orders))])

    def new_meta(self):
        # length in child orders: Pareto with exponent alpha, at least 1
        n = int(self.rng.random() ** (-1.0 / self.p.meta_alpha))
        self.meta.append(["B" if self.rng.random() < 0.5 else "S", min(n, 5000)])

    def meta_child(self):
        m = self.meta[self.rng.randrange(len(self.meta))]
        self.market_order(self.inst, m[0], 100)
        m[1] -= 1
        if m[1] <= 0:
            self.meta.remove(m)

    def fundamentalist(self):
        # informed flow: the further the price is from the value, the more
        # one-sided the orders (a continuous drift of the order imbalance)
        gap = (self.fundamental - self.mid()) / TICK
        p_buy = min(1.0, max(0.0, 0.5 + self.p.fund_strength * gap))
        self.market_order(self.fund, "B" if self.rng.random() < p_buy else "S", self.lots(self.p.market_lot_p))

    def chartist(self):
        if len(self.history) < 2:
            return
        back = max(0, len(self.history) - 1 - int(self.p.chart_window))
        r = (self.history[-1][1] - self.history[back][1]) / self.history[back][1]
        if abs(r) > self.p.chart_threshold:
            self.market_order(self.chart, "B" if r > 0 else "S", self.lots(self.p.market_lot_p))

    # ---- the clock ----------------------------------------------------------------

    def add_maker(self, m: Agent):
        self.makers.append(m)
        m.next_time = 0.0

    def run(self, seconds: float, on_second=None):
        """Advance the market by `seconds` of simulated time."""
        end = self.t + seconds
        p = self.p
        rng = self.rng
        while self.t < end:
            x = (1.0 + self.excitation) * self.activity
            rates = (
                p.limit_rate * x,
                p.market_rate * x,
                p.cancel_rate * len(self.zi_orders) * x,
                p.meta_rate * self.activity,
                p.meta_child_rate * len(self.meta) * self.activity,
                p.fund_rate,
                p.chart_rate,
            )
            total = sum(rates)
            dt = -math.log(1.0 - rng.random()) / total
            # scheduled events come first if they are due before the next random one
            t_next = self.t + dt
            due = min([m.next_time for m in self.makers] + [self._next_sample, self._next_second, end])
            if due <= t_next:
                self.excitation *= math.exp(-(due - self.t) / p.hawkes_tau)
                self.t = due
                if self.t >= end:
                    break
                self._scheduled(on_second)
                continue
            self.excitation *= math.exp(-dt / p.hawkes_tau)
            self.t = t_next
            u = rng.random() * total
            k = 0
            while u > rates[k]:
                u -= rates[k]
                k += 1
            if k == 0:
                self.zi_limit()
            elif k == 1:
                self.market_order(self.zi, "B" if rng.random() < 0.5 else "S", self.lots(p.market_lot_p))
            elif k == 2:
                self.zi_cancel()
            elif k == 3:
                self.new_meta()
            elif k == 4:
                self.meta_child()
            elif k == 5:
                self.fundamentalist()
            else:
                self.chartist()
        self.t = end

    def _scheduled(self, on_second):
        t = self.t
        if t >= self._next_sample:
            if self.record:
                b, bq, a, aq = self.bbo()
                self.quotes.append((t, b, bq, a, aq))
            self._next_sample += self.sample_dt
        if t >= self._next_second:
            self.history.append((t, self.mid()))
            if len(self.history) > 600:
                del self.history[:300]
            # the fundamental value: a random walk with stochastic volatility
            tau = self.p.vol_tau
            self.log_vol += -self.log_vol / tau + math.sqrt(2.0 / tau) * self.rng.gauss(0, 1)
            vv = self.p.vol_of_vol
            vol = self.p.fund_vol * math.exp(vv * self.log_vol - vv * vv / 2)
            self.fundamental *= math.exp(vol * self.rng.gauss(0, 1))
            at = self.p.activity_tau
            self.activity_z += -self.activity_z / at + math.sqrt(2.0 / at) * self.rng.gauss(0, 1)
            av = self.p.activity_vol
            self.activity = math.exp(av * self.activity_z - av * av / 2)
            self._next_second += 1.0
            if on_second:
                on_second(self)
        for m in self.makers:
            if t >= m.next_time:
                m.act(self)
                m.next_time = t + m.period


# ---- market makers with fixed rules ------------------------------------------------


class Maker(Agent):
    """Quotes a bid and an ask around the mid every `period` seconds
    (post-only orders), cancelling its previous quotes first."""

    period = 1.0

    def __init__(self, name: str, account: int, size: int = 500, max_inventory: int = 5000):
        super().__init__(name, account)
        self.size = size
        self.max_inventory = max_inventory
        self.next_time = 0.0
        self.quotes_sent = 0
        self.live: dict[str, tuple[int, int]] = {}  # side -> (order id, price)

    def quote(self, market: Market, half_spread_ticks: float, skew_ticks: float):
        """Bid at center - h, ask at center + h (rounded away from the center
        to whole ticks; h = 0.5 with a one-tick spread is the best bid and
        ask). A quote whose price does not change keeps its place in the queue."""
        half_spread_ticks = min(max(half_spread_ticks, 0.5), 20.0)
        skew_ticks = min(max(skew_ticks, -20.0), 20.0)
        center = market.mid() - skew_ticks * TICK
        bid = math.floor((center - half_spread_ticks * TICK) / TICK) * TICK
        ask = math.ceil((center + half_spread_ticks * TICK) / TICK) * TICK
        if ask <= bid:
            ask = bid + TICK
        want = {"B": bid if self.inventory < self.max_inventory else None, "S": ask if self.inventory > -self.max_inventory else None}
        for side in ("B", "S"):
            oid, px = self.live.get(side, (None, None))
            if oid is not None and oid not in self.orders:
                oid = None  # filled
            if oid is not None and px == want[side]:
                continue
            if oid is not None:
                market.cancel(oid)
            self.live.pop(side, None)
            if want[side] is not None and want[side] > 0:
                n = market.submit(self, side, want[side], self.size, flags=E.POST_ONLY)
                if n in self.orders:
                    self.live[side] = (n, want[side])
                self.quotes_sent += 1


class FixedMaker(Maker):
    def __init__(self, name, account, half_spread=0.5, **kw):
        super().__init__(name, account, **kw)
        self.h = half_spread

    def act(self, market: Market):
        self.quote(market, self.h, 0.0)


class AvellanedaStoikov(Maker):
    """Avellaneda and Stoikov (2008): reservation price s - q*gamma*sigma^2*(T - t)
    and spread gamma*sigma^2*(T - t) + (2/gamma) ln(1 + gamma/k), with the
    horizon rolled forward (a stationary version: T - t fixed)."""

    def __init__(self, name, account, gamma=0.1, horizon=60.0, k=1.5, **kw):
        super().__init__(name, account, **kw)
        self.gamma, self.horizon, self.k = gamma, horizon, k

    def act(self, market: Market):
        h = market.history
        if len(h) > 10:
            rets = [(h[i][1] - h[i - 1][1]) / TICK for i in range(len(h) - 30, len(h)) if i > 0]
            sigma2 = max(sum(r * r for r in rets) / len(rets), 0.01)  # ticks^2 per second
        else:
            sigma2 = 1.0
        q = self.inventory / self.size
        skew = q * self.gamma * sigma2 * self.horizon
        spread = self.gamma * sigma2 * self.horizon + (2 / self.gamma) * math.log(1 + self.gamma / self.k)
        self.quote(market, max(0.5, spread / 2), skew)
