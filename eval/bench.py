"""Latency and throughput of the exchange, layer by layer.

1. the matching engine in process, and through the shared-memory bus
   (engine-bench, three threads, open loop);
2. order entry over TCP through the Rust gateway (loadgen, OUCH and FIX, at
   several rates), next to the round-trip time of plain TCP on the same
   machine (tcprtt) and the gateway's own measurement from reading a request
   to writing its acknowledgment;
3. market data: the ITCH feed through the Go server to many MoldUDP64
   subscribers while orders flow, and recovery of dropped packets.

    python eval/bench.py [--quick] [--out results/bench_<platform>.json]
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools"))

from exchange import EXE, LOADGEN, Exchange, path  # noqa: E402

ENGINE_BENCH = path("engine", "build", "engine-bench" + EXE)
TCPRTT = path("gateway", "target", "release", "tcprtt" + EXE)
MDBENCH = path("marketdata", "bin", "mdbench" + EXE)
OUCH_USERS = "ALICE:alice,BOB:bob,CAROL:carol,DAVE:dave"
FIX_USERS = "CLIENT1:x,CLIENT2:x,CLIENT3:x,CLIENT4:x"


def run_json(args, timeout=1800):
    r = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    lines = [l for l in r.stdout.splitlines() if l.startswith("{")]
    if r.returncode or not lines:
        raise RuntimeError(f"{args[0]} failed: {r.stdout[-500:]} {r.stderr[-500:]}")
    return json.loads(lines[-1])


def loadgen(protocol, rate, orders, sessions=4):
    users = OUCH_USERS if protocol == "ouch" else FIX_USERS
    return run_json([LOADGEN, "--protocol", protocol, "--sessions", str(sessions), "--users", users, "--rate", str(rate),
                     "--orders", str(orders), "--symbols", "AAPL,MSFT,AMD,INTC"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--out", default=None)
    a = ap.parse_args()
    plat = f"{platform.system().lower()}"
    out = {"platform": platform.platform(), "processor": platform.processor(), "cpus": os.cpu_count()}
    out_path = a.out or path("results", f"bench_{plat}.json")

    # 1. the engine
    if os.path.exists(ENGINE_BENCH):
        tmp = path("results", f"engine_bench_{plat}.json")
        subprocess.run([ENGINE_BENCH, "--commands", "1000000" if a.quick else "4000000", "--json", tmp], check=True, timeout=3600)
        out["engine"] = json.load(open(tmp))
        os.remove(tmp)
        print("engine", out["engine"]["in_process"], flush=True)

    # 2. order entry over TCP
    out["tcp_baseline"] = run_json([TCPRTT, "--count", "20000" if a.quick else "100000"])
    print("tcp", out["tcp_baseline"], flush=True)
    out["order_entry"] = []
    rates = [1000, 10000] if a.quick else [1000, 10000, 50000, 100000]
    for protocol in ("ouch", "fix"):
        for rate in rates:
            if protocol == "fix" and rate > 50000:
                continue
            orders = min(rate * (5 if a.quick else 20), 2_000_000)
            with Exchange(md=False, risk=False) as x:
                res = loadgen(protocol, rate, orders)
            gw = json.load(open(os.path.join(x.run, "gateway_stats.json")))
            res["gateway_internal_us"] = gw["internal_ack_latency_us"]
            out["order_entry"].append(res)
            print(protocol, rate, res["latency_us"], "gateway", res["gateway_internal_us"], flush=True)

    # 3. market data fan-out
    out["market_data"] = []
    subs_list = [1, 10] if a.quick else [1, 10, 50, 100]
    for subs, drop in [(n, 0.0) for n in subs_list] + [(10, 0.01)]:
        seconds = 8 if a.quick else 20
        with Exchange(md=True, risk=False) as x:
            mdb = subprocess.Popen([MDBENCH, "-udp", f"127.0.0.1:{x.ports['udp']}", "-subscribers", str(subs), "-seconds", str(seconds),
                                    "-drop", str(drop)], stdout=subprocess.PIPE, text=True)
            time.sleep(1.0)
            order_rate = 20000
            loadgen("ouch", order_rate, order_rate * (seconds - 3), sessions=4)
            res = json.loads(mdb.communicate(timeout=120)[0].strip().splitlines()[-1])
        res["order_rate"] = order_rate
        out["market_data"].append(res)
        print("md", subs, drop, res, flush=True)

    json.dump(out, open(out_path, "w"), indent=1)
    print("wrote", out_path)


if __name__ == "__main__":
    main()
