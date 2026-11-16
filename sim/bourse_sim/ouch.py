"""An OUCH 5.0 client over SoupBinTCP, and the replay of a simulated market
through the running exchange.

A simulation records every command its agents send. Replayed in the same
order through the gateways (each agent group on its own OUCH session, each
command sent after the previous one was acknowledged), the exchange must
produce exactly the trades the simulation produced with the engine in
process: the same prices, the same quantities, in the same order.
"""

from __future__ import annotations

import socket
import struct
import time

MARKET_PRICE = 0x7FFFFFFF


def _packet(kind: bytes, payload: bytes) -> bytes:
    return struct.pack(">H", len(payload) + 1) + kind + payload


class Session:
    def __init__(self, host: str, port: int, user: str, password: str):
        self.s = socket.create_connection((host, port))
        self.s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.buf = b""
        login = user.ljust(6)[:6].encode() + password.ljust(10)[:10].encode() + b" " * 10 + b"0".rjust(20)
        self.s.sendall(_packet(b"L", login))
        kind, _ = self._next()
        if kind != b"A":
            raise ConnectionError(f"login refused for {user}")
        self.user_ref = int(time.time()) % 100_000 * 10_000  # above any earlier run of this session
        self.executions: list[tuple[int, int, int, bytes]] = []  # (match number, price, qty, liquidity)

    def _next(self):
        while True:
            if len(self.buf) >= 2:
                n = struct.unpack(">H", self.buf[:2])[0]
                if len(self.buf) >= 2 + n:
                    kind, payload = self.buf[2:3], self.buf[3:2 + n]
                    self.buf = self.buf[2 + n:]
                    if kind == b"H":
                        continue
                    return kind, payload
            chunk = self.s.recv(65536)
            if not chunk:
                raise ConnectionError("connection closed")
            self.buf += chunk

    def wait(self, ref: int):
        """Read messages until the first answer about user reference `ref`."""
        while True:
            kind, p = self._next()
            if kind != b"S":
                continue
            t = p[:1]
            if t == b"E":
                _, uref, qty, price, liq, match = struct.unpack(">QIIQcQ", p[1:34])
                self.executions.append((match, price, qty, liq))
                continue
            if t in (b"A", b"C", b"J"):
                if struct.unpack(">I", p[9:13])[0] == ref:
                    return t

    def enter(self, side: str, qty: int, symbol: str, price: int, tif: bytes = b"0", post_only: bool = False) -> int:
        self.user_ref += 1
        app = struct.pack(">HBBc", 3, 2, 12, b"P") if post_only else struct.pack(">H", 0)
        msg = b"O" + struct.pack(">IcI", self.user_ref, side.encode(), qty) + symbol.ljust(8).encode() + struct.pack(">Q", price) + tif + b"YANN" + b"sim".ljust(14) + app
        self.s.sendall(_packet(b"U", msg))
        return self.user_ref

    def heartbeat(self):
        self.s.sendall(_packet(b"R", b""))

    def cancel(self, ref: int):
        self.s.sendall(_packet(b"U", b"X" + struct.pack(">II", ref, 0) + struct.pack(">H", 0)))

    def close(self):
        self.s.close()


def replay(commands, users: dict[int, tuple[str, str]], host="127.0.0.1", port=9001, symbol="AAPL") -> list[tuple[int, int, int]]:
    """Send recorded commands (dicts from Market.commands) through OUCH, one
    session per account. Returns the trades seen by the incoming side:
    (match number, price, qty), in match order."""
    sessions = {a: Session(host, port, u, p) for a, (u, p) in users.items()}
    refs: dict[int, tuple[int, int]] = {}  # sim order id -> (account, user ref)
    from . import engine as E

    last_beat = time.monotonic()
    for c in commands:
        # SoupBinTCP: a client silent for 15 s is logged out; sessions that
        # have nothing to send must still send heartbeats
        if time.monotonic() - last_beat > 1.0:
            for x in sessions.values():
                x.heartbeat()
            last_beat = time.monotonic()
        s = sessions[c["account"]]
        if c["type"] == E.NEW:
            market = bool(c["flags"] & E.MARKET)
            ref = s.enter(c["side"], c["qty"], symbol, MARKET_PRICE if market else c["price"], b"3" if c["tif"] == E.IOC else b"0",
                          post_only=bool(c["flags"] & E.POST_ONLY))
            refs[c["order_id"]] = (c["account"], ref)
            s.wait(ref)
        elif c["type"] == E.CANCEL:
            acct, ref = refs[c["order_id"]]
            s = sessions[acct]
            s.cancel(ref)
            s.wait(ref)
    # collect executions still in flight
    time.sleep(0.3)
    for s in sessions.values():
        s.s.settimeout(0.2)
        try:
            while True:
                kind, p = s._next()
                if kind == b"S" and p[:1] == b"E":
                    _, uref, qty, price, liq, match = struct.unpack(">QIIQcQ", p[1:34])
                    s.executions.append((match, price, qty, liq))
        except (socket.timeout, TimeoutError, ConnectionError):
            pass
    trades = sorted((m, px, q) for s in sessions.values() for m, px, q, liq in s.executions if liq == b"R")
    for s in sessions.values():
        s.close()
    return trades
