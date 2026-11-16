"""End-to-end check of the whole exchange: simulate a market with the engine
in process, then send the same commands through the gateways (OUCH over
TCP, one session per agent group) to the running exchange (engine, Rust
gateway, Go market data, Kotlin risk and clearing), and compare:

- the trades the clients see (price and quantity, in match order) with the
  trades of the simulation;
- the order books the market-data server rebuilt from the ITCH feed with
  the final book of the simulation;
- the settlement of the clearing service (balanced, and the net position of
  each account equal to its position in the simulation).

    python eval/replay_check.py [--seconds 600]
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "sim"))
sys.path.insert(0, os.path.join(ROOT, "tools"))
sys.path.insert(0, os.path.join(ROOT, "eval"))

from bourse_sim import market as M  # noqa: E402
from bourse_sim import ouch  # noqa: E402
from exchange import Exchange, get_json  # noqa: E402

USERS = {20: ("SIM", "sim"), 21: ("INST", "inst"), 22: ("FUND", "fund"), 23: ("CHART", "chart"), 11: ("MM1", "mm1"), 12: ("MM2", "mm2")}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seconds", type=float, default=600)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    m = M.Market(M.calibrated_params(), seed=a.seed, record_commands=True)
    m.add_maker(M.FixedMaker("mm1", 11))
    m.add_maker(M.AvellanedaStoikov("mm2", 12))
    m.run(a.seconds)
    sim_trades = [(px, q) for _, px, q, _ in m.trades]
    depth = (m.eng.depth(0, 0, 10), m.eng.depth(0, 1, 10))
    print(f"simulated {a.seconds:.0f} s: {len(m.commands)} commands, {len(sim_trades)} trades", flush=True)
    out = {"seconds": a.seconds, "commands": len(m.commands), "sim_trades": len(sim_trades)}
    with Exchange(idle=True) as x:
        t0 = time.time()
        trades = ouch.replay(m.commands, USERS, port=x.ports["ouch"], symbol="AAPL")
        out["replay_seconds"] = time.time() - t0
        out["commands_per_second"] = len(m.commands) / out["replay_seconds"]
        time.sleep(0.5)
        book = get_json(f"http://127.0.0.1:{x.ports['http']}/api/book?symbol=AAPL&levels=10")
        stats = get_json(f"http://127.0.0.1:{x.ports['http']}/api/stats")
        accounts = get_json(f"http://127.0.0.1:{x.ports['risk_http']}/api/accounts")
        settlement = get_json(f"http://127.0.0.1:{x.ports['risk_http']}/api/settlement")
    got = [(px, q) for _, px, q in trades]
    out["exchange_trades"] = len(got)
    out["trades_identical"] = got == sim_trades
    if not out["trades_identical"]:
        first = next((i for i, (g, s) in enumerate(zip(got, sim_trades)) if g != s), min(len(got), len(sim_trades)))
        out["first_difference"] = first
    md_book = ([(l["price"], l["shares"]) for l in book["bids"] or []], [(l["price"], l["shares"]) for l in book["asks"] or []])
    out["market_data_book_identical"] = md_book == (depth[0], depth[1])
    out["market_data_unknown_refs"] = stats["unknown_refs"]
    out["market_data_crossed"] = stats["crossed"]
    agents = {11: m.makers[0], 12: m.makers[1], 20: m.zi, 21: m.inst, 22: m.fund, 23: m.chart}
    pos = {r["account"]: r["positions"].get("AAPL", 0) for r in accounts}
    out["clearing_positions_identical"] = all(pos.get(acc, 0) == ag.inventory for acc, ag in agents.items())
    out["settlement_balanced"] = settlement["shares_balance"] and settlement["cash_balances"]
    out["clearing_trades"] = settlement["trades"]
    print(json.dumps(out, indent=1))
    json.dump(out, open(os.path.join(ROOT, "results", "replay_check.json"), "w"), indent=1)
    ok = out["trades_identical"] and out["market_data_book_identical"] and out["clearing_positions_identical"] and out["settlement_balanced"]
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
