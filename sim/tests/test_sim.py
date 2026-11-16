import os
import struct
import sys

import numpy as np
import pytest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from bourse_sim import engine as E  # noqa: E402
from bourse_sim import facts as F  # noqa: E402
from bourse_sim import market as M  # noqa: E402


def test_engine_binding_matches():
    e = E.Engine()
    s = e.add_symbol("TEST", 100)
    ev = e.submit(E.NEW, "S", s, 1, 1, 100_0000, 300)
    assert list(ev["type"]) == [E.ACCEPTED, E.BOOKED]
    e.submit(E.NEW, "S", s, 1, 2, 100_0100, 200)
    ev = e.submit(E.NEW, "B", s, 2, 3, 100_0100, 400)
    ex = ev[ev["type"] == E.EXECUTED]
    assert list(ex["order_id"]) == [1, 2] and list(ex["qty"]) == [300, 100] and list(ex["price"]) == [100_0000, 100_0100]
    (b, bq), (a, aq) = e.best(s)
    assert (b, bq, a, aq) == (0, 0, 100_0100, 100)
    ev = e.submit(E.NEW, "B", s, 2, 4, 100_0200, 10, flags=E.POST_ONLY)
    assert ev["type"][0] == E.REJECTED
    assert e.live_orders() == 1


def test_market_runs_and_records():
    m = M.Market(seed=3, record_commands=True)
    m.add_maker(M.FixedMaker("mm", 11))
    m.run(120)
    assert len(m.trades) > 50 and len(m.quotes) >= 1190
    assert m.commands and m.commands[0]["type"] == E.NEW
    b, _, a, _ = m.bbo()
    assert 0 < b < a
    # money and shares are conserved among the agents
    agents = [m.zi, m.inst, m.fund, m.chart] + m.makers
    assert sum(x.inventory for x in agents) == 0
    assert sum(x.cash for x in agents) == 0


def test_facts_on_known_series():
    rng = np.random.default_rng(1)
    x = rng.pareto(3.0, 200_000) + 1
    assert abs(F.hill(x, 0.02) - 3.0) < 0.4
    w = rng.standard_normal(50_000)
    assert abs(F.acf(w, [1])[0]) < 0.02
    lags = np.arange(1, 100)
    assert abs(F.power_fit(lags, lags ** -0.6) - 0.6) < 1e-9


def test_facts_on_a_simulated_market():
    m = M.Market(seed=5)
    m.run(900)
    f = F.facts(m.trades, m.quotes, m.adds)
    for k in ("kurtosis_10s", "sign_acf_exponent", "spread_mean_ticks", "interarrival_cv"):
        assert k in f
    assert f["spread_mean_ticks"] >= 1


def test_maker_env_steps():
    pytest.importorskip("torch")
    from bourse_sim import mm

    env = mm.MakerEnv(M.Params(), seed=1, seconds=30)
    o = env.reset()
    assert o.shape == (mm.OBS,)
    done, n = False, 0
    while not done:
        o, r, done, pnl = env.step(n % len(mm.ACTIONS))
        n += 1
    assert n == 30 and np.all(np.isfinite(o))


def test_ouch_enter_order_layout():
    from bourse_sim import ouch

    class Fake(ouch.Session):
        def __init__(self):
            self.sent = []
            self.user_ref = 6

            class S:
                def sendall(s2, b):
                    self.sent.append(b)

            self.s = S()

    f = Fake()
    f.enter("B", 300, "AAPL", 1_543_200, b"0", post_only=True)
    pkt = f.sent[0]
    n = struct.unpack(">H", pkt[:2])[0]
    assert pkt[2:3] == b"U" and n == len(pkt) - 2
    msg = pkt[3:]
    # offsets of the OUCH 5.0 Enter Order message
    assert msg[0:1] == b"O" and struct.unpack(">I", msg[1:5])[0] == 7 and msg[5:6] == b"B"
    assert struct.unpack(">I", msg[6:10])[0] == 300 and msg[10:18] == b"AAPL    "
    assert struct.unpack(">Q", msg[18:26])[0] == 1_543_200 and msg[26:27] == b"0"
    assert struct.unpack(">H", msg[45:47])[0] == 3 and msg[47:50] == bytes([2, 12]) + b"P"
