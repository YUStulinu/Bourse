//! The shared-memory bus of the engine (docs/bus.md), mapped from Rust:
//! a producer side for one inbound ring and a reader of the event log.

use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAGIC: u64 = 0x3142_4553_5255_4F42; // "BOURSEB1"
const SYMBOLS_OFF: usize = 4096;
const CONSUMERS_OFF: usize = 8192;
const RINGS_OFF: usize = 12288;
const RING_HEADER: usize = 256;
const SLOT: usize = 64;
const CONSUMER_SIZE: usize = 128;
pub const MAX_CONSUMERS: usize = 16;

pub const CMD_NEW: u8 = 1;
pub const CMD_CANCEL: u8 = 2;
pub const CMD_REPLACE: u8 = 3;
pub const CMD_MASS_CANCEL: u8 = 4;
pub const CMD_BLOCK: u8 = 5;
pub const CMD_UNBLOCK: u8 = 6;

pub const TIF_DAY: u8 = 0;
pub const TIF_IOC: u8 = 1;
pub const TIF_FOK: u8 = 2;
pub const F_POST_ONLY: u8 = 1;
pub const F_MARKET: u8 = 2;
pub const F_STP: u8 = 4;

pub const EV_ACCEPTED: u8 = 1;
pub const EV_REJECTED: u8 = 2;
pub const EV_EXECUTED: u8 = 3;
pub const EV_CANCELLED: u8 = 4;
pub const EV_REPLACED: u8 = 5;
pub const EV_BOOKED: u8 = 6;
pub const EV_BLOCKED: u8 = 7;
pub const EV_UNBLOCKED: u8 = 8;

pub const REJ_SYMBOL: u8 = 1;
pub const REJ_QTY: u8 = 2;
pub const REJ_PRICE: u8 = 3;
pub const REJ_DUPLICATE: u8 = 4;
pub const REJ_UNKNOWN_ORDER: u8 = 5;
pub const REJ_BLOCKED: u8 = 6;
pub const REJ_POST_ONLY: u8 = 7;
pub const REJ_HALTED: u8 = 8;

pub const CXL_USER: u8 = 1;
pub const CXL_IOC: u8 = 2;
pub const CXL_STP: u8 = 3;
pub const CXL_MASS: u8 = 4;
pub const CXL_FOK: u8 = 5;
pub const CXL_BLOCKED: u8 = 6;

pub const BUS_RUNNING: u32 = 1;

/// A command into the engine (64 bytes, the layout of the C++ struct).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Command {
    pub kind: u8,
    pub side: u8,
    pub tif: u8,
    pub flags: u8,
    pub session: u16,
    pub reserved0: u16,
    pub symbol: u32,
    pub account: u32,
    pub order_id: u64,
    pub new_order_id: u64,
    pub price: i64,
    pub qty: u32,
    pub reserved1: u32,
    pub reserved2: u64,
    pub ts: u64,
}

/// An event from the engine (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Event {
    pub kind: u8,
    pub side: u8,
    pub reason: u8,
    pub flags: u8,
    pub session: u16,
    pub session2: u16,
    pub symbol: u32,
    pub account: u32,
    pub order_id: u64,
    pub order_id2: u64,
    pub price: i64,
    pub qty: u32,
    pub qty2: u32,
    pub account2: u32,
    pub cmd_ref: u32,
    pub ts: u64,
}

const _: () = assert!(std::mem::size_of::<Command>() == 64);
const _: () = assert!(std::mem::size_of::<Event>() == 64);

pub fn wall_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub round_lot: u32,
    pub tick: u32,
}

/// The mapped bus file.
pub struct Bus {
    map: MmapMut,
    ptr: *mut u8,
    pub n_in: u32,
    pub in_cap: u32,
    pub out_cap: u32,
    out_off: usize,
}

impl Bus {
    pub fn open(path: &str) -> std::io::Result<Bus> {
        let f = OpenOptions::new().read(true).write(true).open(path)?;
        let mut map = unsafe { MmapMut::map_mut(&f)? };
        if map.len() < RINGS_OFF || u64::from_le_bytes(map[0..8].try_into().unwrap()) != MAGIC {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a bus file"));
        }
        let rd = |o: usize| u32::from_le_bytes(map[o..o + 4].try_into().unwrap());
        let (n_in, in_cap, out_cap) = (rd(12), rd(16), rd(20));
        let out_off = RINGS_OFF + n_in as usize * (RING_HEADER + in_cap as usize * SLOT);
        let ptr = map.as_mut_ptr();
        Ok(Bus { map, ptr, n_in, in_cap, out_cap, out_off })
    }

    fn base(&self) -> *mut u8 {
        self.ptr
    }
    fn a64(&self, off: usize) -> &AtomicU64 {
        unsafe { &*(self.base().add(off) as *const AtomicU64) }
    }
    fn a32(&self, off: usize) -> &AtomicU32 {
        unsafe { &*(self.base().add(off) as *const AtomicU32) }
    }

    pub fn state(&self) -> u32 {
        self.a32(28).load(Ordering::Acquire)
    }
    /// Ask the engine to stop (it finishes its loop and marks the bus stopped).
    pub fn request_stop(&self) {
        self.a32(28).store(2, Ordering::Release);
    }
    /// Read cursors of the registered consumers of the event log.
    pub fn consumers(&self) -> Vec<(u32, u64)> {
        (0..MAX_CONSUMERS)
            .filter(|&i| self.a32(self.consumer(i)).load(Ordering::Acquire) == 1)
            .map(|i| (self.a32(self.consumer(i) + 4).load(Ordering::Relaxed), self.a64(self.consumer(i) + 64).load(Ordering::Acquire)))
            .collect()
    }
    pub fn heartbeat(&self) -> u64 {
        self.a64(64).load(Ordering::Relaxed)
    }

    pub fn symbols(&self) -> Vec<Symbol> {
        let n = u32::from_le_bytes(self.map[24..28].try_into().unwrap()) as usize;
        (0..n)
            .map(|i| {
                let o = SYMBOLS_OFF + i * 16;
                let e = &self.map[o..o + 16];
                Symbol {
                    name: String::from_utf8_lossy(&e[0..8]).trim_end().to_string(),
                    round_lot: u32::from_le_bytes(e[8..12].try_into().unwrap()),
                    tick: u32::from_le_bytes(e[12..16].try_into().unwrap()),
                }
            })
            .collect()
    }

    fn ring(&self, i: u32) -> usize {
        RINGS_OFF + i as usize * (RING_HEADER + self.in_cap as usize * SLOT)
    }
    fn consumer(&self, i: usize) -> usize {
        CONSUMERS_OFF + i * CONSUMER_SIZE
    }
    pub fn log_cursor(&self) -> u64 {
        self.a64(self.out_off).load(Ordering::Acquire)
    }
}

// The mapping is shared memory written through atomics and volatile copies.
unsafe impl Send for Bus {}
unsafe impl Sync for Bus {}

/// The producer side of one inbound ring.
pub struct Producer<'a> {
    bus: &'a Bus,
    ring: u32,
    head: u64,
    tail_cache: u64,
}

impl<'a> Producer<'a> {
    /// Claim a free inbound ring.
    pub fn attach(bus: &'a Bus) -> Option<Producer<'a>> {
        let pid = std::process::id();
        for i in 0..bus.n_in {
            let off = bus.ring(i);
            if bus.a32(off + 8).compare_exchange(0, pid, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                let head = bus.a64(off).load(Ordering::Acquire);
                let tail_cache = bus.a64(off + 128).load(Ordering::Acquire);
                return Some(Producer { bus, ring: i, head, tail_cache });
            }
        }
        None
    }

    pub fn ring(&self) -> u32 {
        self.ring
    }

    /// Publish one command, spinning while the ring is full.
    pub fn push(&mut self, c: &Command) {
        let off = self.bus.ring(self.ring);
        let cap = self.bus.in_cap as u64;
        while self.head - self.tail_cache >= cap {
            self.tail_cache = self.bus.a64(off + 128).load(Ordering::Acquire);
            std::hint::spin_loop();
        }
        let slot = off + RING_HEADER + ((self.head & (cap - 1)) as usize) * SLOT;
        unsafe { std::ptr::write_volatile(self.bus.base().add(slot) as *mut Command, *c) };
        self.head += 1;
        self.bus.a64(off).store(self.head, Ordering::Release);
    }

    /// The ring position the next command will take.
    pub fn position(&self) -> u64 {
        self.head
    }
}

impl Drop for Producer<'_> {
    fn drop(&mut self) {
        self.bus.a32(self.bus.ring(self.ring) + 8).store(0, Ordering::Release);
    }
}

/// A registered reader of the event log.
pub struct Consumer<'a> {
    bus: &'a Bus,
    slot: usize,
    cursor: u64,
}

impl<'a> Consumer<'a> {
    pub fn attach(bus: &'a Bus) -> Option<Consumer<'a>> {
        for i in 0..MAX_CONSUMERS {
            let off = bus.consumer(i);
            if bus.a32(off).compare_exchange(0, 2, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                let cursor = bus.log_cursor();
                bus.a64(off + 64).store(cursor, Ordering::Release);
                bus.a64(off + 8).store(wall_ns(), Ordering::Relaxed);
                bus.a32(off + 4).store(std::process::id(), Ordering::Relaxed);
                bus.a32(off).store(1, Ordering::Release);
                return Some(Consumer { bus, slot: i, cursor });
            }
        }
        None
    }

    /// The sequence number of the next event to be read.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Call f(sequence, event) for each new event (at most max); returns how many.
    pub fn poll<F: FnMut(u64, &Event)>(&mut self, max: usize, mut f: F) -> usize {
        let avail = self.bus.log_cursor();
        let cap = self.bus.out_cap as u64;
        let mut n = 0;
        while self.cursor < avail && n < max {
            let slot = self.bus.out_off + RING_HEADER + ((self.cursor & (cap - 1)) as usize) * SLOT;
            let e = unsafe { std::ptr::read_volatile(self.bus.base().add(slot) as *const Event) };
            f(self.cursor, &e);
            self.cursor += 1;
            n += 1;
        }
        if n > 0 {
            self.bus.a64(self.bus.consumer(self.slot) + 64).store(self.cursor, Ordering::Release);
        }
        n
    }

    pub fn heartbeat(&self) {
        self.bus.a64(self.bus.consumer(self.slot) + 8).store(wall_ns(), Ordering::Relaxed);
    }

    /// Whether the engine still counts this reader (it drops stale ones).
    pub fn active(&self) -> bool {
        self.bus.a32(self.bus.consumer(self.slot)).load(Ordering::Acquire) == 1
    }
}

impl Drop for Consumer<'_> {
    fn drop(&mut self) {
        self.bus.a32(self.bus.consumer(self.slot)).store(0, Ordering::Release);
    }
}

/// Price of 1/10000 dollar as a decimal string ("123.45").
pub fn price_str(p: i64) -> String {
    let neg = p < 0;
    let a = p.unsigned_abs();
    let (whole, frac) = (a / 10000, a % 10000);
    let mut s = format!("{}{}", if neg { "-" } else { "" }, whole);
    if frac != 0 {
        let f = format!("{:04}", frac);
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
    s
}

/// Parse a decimal price into 1/10000 dollar (at most 4 decimals).
pub fn parse_price(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (w, f) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if f.len() > 4 || (w.is_empty() && f.is_empty()) {
        return None;
    }
    let whole: i64 = if w.is_empty() { 0 } else { w.parse().ok()? };
    let mut frac: i64 = if f.is_empty() { 0 } else { f.parse().ok()? };
    for _ in f.len()..4 {
        frac *= 10;
    }
    let v = whole.checked_mul(10000)?.checked_add(frac)?;
    Some(if neg { -v } else { v })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices() {
        assert_eq!(price_str(1234500), "123.45");
        assert_eq!(price_str(1000000), "100");
        assert_eq!(price_str(1), "0.0001");
        assert_eq!(parse_price("123.45"), Some(1234500));
        assert_eq!(parse_price("0.0001"), Some(1));
        assert_eq!(parse_price("7"), Some(70000));
        assert_eq!(parse_price("1.23456"), None);
        assert_eq!(parse_price("abc"), None);
    }
}
