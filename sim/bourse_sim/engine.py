"""The exchange's matching engine, in process, through its C interface
(engine/src/capi.cpp): the same code that runs behind the gateways."""

from __future__ import annotations

import ctypes
import os
import sys

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

NEW, CANCEL, REPLACE, MASS_CANCEL, BLOCK, UNBLOCK = 1, 2, 3, 4, 5, 6
DAY, IOC, FOK = 0, 1, 2
POST_ONLY, MARKET, STP = 1, 2, 4
ACCEPTED, REJECTED, EXECUTED, CANCELLED, REPLACED, BOOKED = 1, 2, 3, 4, 5, 6


class Command(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_uint8),
        ("side", ctypes.c_uint8),
        ("tif", ctypes.c_uint8),
        ("flags", ctypes.c_uint8),
        ("session", ctypes.c_uint16),
        ("reserved0", ctypes.c_uint16),
        ("symbol", ctypes.c_uint32),
        ("account", ctypes.c_uint32),
        ("order_id", ctypes.c_uint64),
        ("new_order_id", ctypes.c_uint64),
        ("price", ctypes.c_int64),
        ("qty", ctypes.c_uint32),
        ("reserved1", ctypes.c_uint32),
        ("reserved2", ctypes.c_uint64),
        ("ts", ctypes.c_uint64),
    ]


EVENT_DTYPE = np.dtype(
    [
        ("type", "u1"),
        ("side", "u1"),
        ("reason", "u1"),
        ("flags", "u1"),
        ("session", "<u2"),
        ("session2", "<u2"),
        ("symbol", "<u4"),
        ("account", "<u4"),
        ("order_id", "<u8"),
        ("order_id2", "<u8"),
        ("price", "<i8"),
        ("qty", "<u4"),
        ("qty2", "<u4"),
        ("account2", "<u4"),
        ("cmd_ref", "<u4"),
        ("ts", "<u8"),
    ]
)
assert ctypes.sizeof(Command) == 64 and EVENT_DTYPE.itemsize == 64


def library_path() -> str:
    names = ["bourse.dll", "libbourse.dll"] if sys.platform == "win32" else ["libbourse.so", "libbourse.dylib"]
    for d in (os.environ.get("BOURSE_LIB_DIR", ""), os.path.join(ROOT, "engine", "build")):
        for n in names:
            p = os.path.join(d, n)
            if d and os.path.exists(p):
                return p
    raise FileNotFoundError("build the engine first (engine/build): no " + " or ".join(names))


_lib = None


def lib():
    global _lib
    if _lib is None:
        L = ctypes.CDLL(library_path())
        L.bourse_new.restype = ctypes.c_void_p
        L.bourse_free.argtypes = [ctypes.c_void_p]
        L.bourse_add_symbol.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
        L.bourse_add_symbol.restype = ctypes.c_uint32
        L.bourse_submit.argtypes = [ctypes.c_void_p, ctypes.POINTER(Command), ctypes.c_uint64]
        L.bourse_submit.restype = ctypes.c_int
        L.bourse_events.argtypes = [ctypes.c_void_p]
        L.bourse_events.restype = ctypes.c_void_p
        L.bourse_depth.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_int, ctypes.c_int, ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(ctypes.c_uint64)]
        L.bourse_depth.restype = ctypes.c_int
        L.bourse_live_orders.argtypes = [ctypes.c_void_p]
        L.bourse_live_orders.restype = ctypes.c_uint64
        _lib = L
    return _lib


class Engine:
    """One matching engine. Prices are integers in 1/10000 dollar."""

    def __init__(self):
        self.h = lib().bourse_new()
        self._cmd = Command()
        self._px = (ctypes.c_int64 * 64)()
        self._qty = (ctypes.c_uint64 * 64)()

    def __del__(self):
        if getattr(self, "h", None):
            lib().bourse_free(self.h)
            self.h = None

    def add_symbol(self, name: str, tick: int = 100) -> int:
        return lib().bourse_add_symbol(self.h, name.encode(), tick)

    def submit(self, type: int, side: str = "B", symbol: int = 0, account: int = 0, order_id: int = 0, price: int = 0, qty: int = 0,
               tif: int = DAY, flags: int = 0, new_order_id: int = 0, ts: int = 0, session: int = 0) -> np.ndarray:
        """Process one command; returns its events (a structured array)."""
        c = self._cmd
        c.type, c.side, c.tif, c.flags, c.session = type, ord(side), tif, flags, session
        c.symbol, c.account, c.order_id, c.new_order_id = symbol, account, order_id, new_order_id
        c.price, c.qty, c.ts = price, qty, ts
        n = lib().bourse_submit(self.h, ctypes.byref(c), ts)
        if n == 0:
            return np.empty(0, EVENT_DTYPE)
        buf = (ctypes.c_char * (64 * n)).from_address(lib().bourse_events(self.h))
        return np.frombuffer(bytes(buf), EVENT_DTYPE)

    def submit_command(self, c: Command) -> np.ndarray:
        n = lib().bourse_submit(self.h, ctypes.byref(c), c.ts)
        if n == 0:
            return np.empty(0, EVENT_DTYPE)
        buf = (ctypes.c_char * (64 * n)).from_address(lib().bourse_events(self.h))
        return np.frombuffer(bytes(buf), EVENT_DTYPE)

    def depth(self, symbol: int, side: int, levels: int = 10) -> list[tuple[int, int]]:
        """Best levels of one side (0 bids, 1 asks): [(price, shares)], best first."""
        n = lib().bourse_depth(self.h, symbol, side, min(levels, 64), self._px, self._qty)
        return [(self._px[i], self._qty[i]) for i in range(n)]

    def best(self, symbol: int = 0):
        b = self.depth(symbol, 0, 1)
        a = self.depth(symbol, 1, 1)
        return (b[0] if b else (0, 0)), (a[0] if a else (0, 0))

    def live_orders(self) -> int:
        return lib().bourse_live_orders(self.h)
