"""Start and stop the whole exchange: engine, gateways, market data, risk.

    python tools/exchange.py run [--seconds N]     # start everything, wait, stop
    python tools/exchange.py start                 # start and leave running
    python tools/exchange.py stop

As a library: `with Exchange() as x: ...` starts everything and stops it
cleanly (the engine first, which the other components notice).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = ".exe" if os.name == "nt" else ""


def path(*p):
    return os.path.join(ROOT, *p)


ENGINE = path("engine", "build", "bourse-engine" + EXE)
GATEWAY = path("gateway", "target", "release", "bourse-gateway" + EXE)
BUSCTL = path("gateway", "target", "release", "busctl" + EXE)
LOADGEN = path("gateway", "target", "release", "loadgen" + EXE)
MDSERVER = path("marketdata", "bin", "mdserver" + EXE)
RISK_JAR = path("risk", "build", "libs", "bourse-risk.jar")


def java():
    home = os.environ.get("JAVA_HOME")
    if home and os.path.exists(os.path.join(home, "bin", "java" + EXE)):
        return os.path.join(home, "bin", "java" + EXE)
    return shutil.which("java") or "java"


def wait_http(url, timeout=30.0):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            with urllib.request.urlopen(url, timeout=1) as r:
                return json.loads(r.read())
        except Exception:
            time.sleep(0.1)
    raise TimeoutError(url)


def get_json(url):
    with urllib.request.urlopen(url, timeout=5) as r:
        return json.loads(r.read())


class Exchange:
    def __init__(self, run_dir=None, ouch=9001, fix=9002, itch=9003, udp=9010, http=8080, risk_http=8081,
                 md=True, risk=True, idle=False, engine_cpu=None, stats=True):
        self.run = run_dir or path("run")
        self.ports = dict(ouch=ouch, fix=fix, itch=itch, udp=udp, http=http, risk_http=risk_http)
        self.md, self.risk, self.idle, self.engine_cpu, self.stats = md, risk, idle, engine_cpu, stats
        self.procs: list[tuple[str, subprocess.Popen]] = []
        self.bus = os.path.join(self.run, "bus.dat")

    def _spawn(self, name, args):
        log = open(os.path.join(self.run, name + ".log"), "w")
        p = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT, cwd=ROOT)
        self.procs.append((name, p))
        return p

    def start(self):
        os.makedirs(self.run, exist_ok=True)
        if os.path.exists(self.bus):
            os.remove(self.bus)
        engine = [ENGINE, "--bus", self.bus, "--symbols", path("config", "symbols.txt")]
        if self.engine_cpu is not None:
            engine += ["--cpu", str(self.engine_cpu)]
        self._spawn("engine", engine)
        t0 = time.time()
        while not os.path.exists(self.bus) and time.time() - t0 < 10:
            time.sleep(0.02)
        limits = os.path.join(self.run, "limits.json")
        if self.risk:
            self._spawn("risk", [java(), "-jar", RISK_JAR, "--bus", self.bus, "--config", path("config", "risk.properties"),
                                 "--http", f"127.0.0.1:{self.ports['risk_http']}", "--limits", limits,
                                 "--report", os.path.join(self.run, "settlement.json")])
            wait_http(f"http://127.0.0.1:{self.ports['risk_http']}/api/limits")
        gw = [GATEWAY, "--bus", self.bus, "--accounts", path("config", "accounts.json"),
              "--ouch", f"127.0.0.1:{self.ports['ouch']}", "--fix", f"127.0.0.1:{self.ports['fix']}",
              "--itch", f"127.0.0.1:{self.ports['itch']}", "--limits", limits]
        if self.stats:
            gw += ["--stats", os.path.join(self.run, "gateway_stats.json")]
        if self.idle:
            gw.append("--idle")
        self._spawn("gateway", gw)
        if self.md:
            self._spawn("mdserver", [MDSERVER, "-itch", f"127.0.0.1:{self.ports['itch']}", "-udp", f"127.0.0.1:{self.ports['udp']}",
                                     "-http", f"127.0.0.1:{self.ports['http']}", "-static", path("ui")])
            wait_http(f"http://127.0.0.1:{self.ports['http']}/api/stats")
        else:
            time.sleep(0.5)
        return self

    def stop(self):
        if os.path.exists(BUSCTL) and os.path.exists(self.bus):
            subprocess.run([BUSCTL, "--bus", self.bus, "stop"], capture_output=True, timeout=30)
        for name, p in self.procs:
            if name == "mdserver":
                p.terminate()
            try:
                p.wait(timeout=20)
            except subprocess.TimeoutExpired:
                p.kill()
        self.procs.clear()

    def __enter__(self):
        return self.start()

    def __exit__(self, *exc):
        self.stop()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("action", choices=["run", "start", "stop"])
    ap.add_argument("--seconds", type=float, default=0)
    a = ap.parse_args()
    if a.action == "stop":
        subprocess.run([BUSCTL, "--bus", path("run", "bus.dat"), "stop"])
        return
    x = Exchange(idle=True).start()
    print("exchange running: OUCH 9001, FIX 9002, ITCH 9003, MoldUDP64 9010, web http://127.0.0.1:8080, risk http://127.0.0.1:8081")
    if a.action == "start":
        return
    try:
        if a.seconds:
            time.sleep(a.seconds)
        else:
            while True:
                time.sleep(1)
    except KeyboardInterrupt:
        pass
    x.stop()


if __name__ == "__main__":
    sys.exit(main())
