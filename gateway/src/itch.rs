//! ITCH 5.0 market data: the encoder of the messages Bourse publishes, and
//! the translation of the engine's event log into a public order-by-order
//! feed (what NASDAQ TotalView-ITCH shows: orders as they rest on the book,
//! executions against them, cancels, deletes and replaces).

use crate::bus::{self, Event};
use crate::soup::alpha;
use std::collections::HashMap;

pub const NS_PER_DAY: u64 = 86_400_000_000_000;

fn header(v: &mut Vec<u8>, kind: u8, locate: u16, ts: u64) {
    v.push(kind);
    v.extend(locate.to_be_bytes());
    v.extend(0u16.to_be_bytes()); // tracking number
    let t = ts % NS_PER_DAY;
    v.extend(&t.to_be_bytes()[2..8]); // 6 bytes
}

pub fn system_event(ts: u64, code: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(12);
    header(&mut v, b'S', 0, ts);
    v.push(code);
    v
}

pub fn stock_directory(ts: u64, locate: u16, symbol: &str, round_lot: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(39);
    header(&mut v, b'R', locate, ts);
    v.extend(alpha(symbol, 8));
    v.extend([b'Q', b'N']);
    v.extend(round_lot.to_be_bytes());
    v.extend([b'N', b'C', b' ', b' ', b'P', b'N', b' ', b'1', b'N']);
    v.extend(0u32.to_be_bytes());
    v.push(b'N');
    v
}

pub fn trading_action(ts: u64, locate: u16, symbol: &str, state: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(25);
    header(&mut v, b'H', locate, ts);
    v.extend(alpha(symbol, 8));
    v.extend([state, b' ']);
    v.extend(*b"    ");
    v
}

pub fn add_order(ts: u64, locate: u16, order_ref: u64, side: u8, shares: u32, symbol: &str, price: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(36);
    header(&mut v, b'A', locate, ts);
    v.extend(order_ref.to_be_bytes());
    v.push(side);
    v.extend(shares.to_be_bytes());
    v.extend(alpha(symbol, 8));
    v.extend(price.to_be_bytes());
    v
}

pub fn executed(ts: u64, locate: u16, order_ref: u64, shares: u32, match_number: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(31);
    header(&mut v, b'E', locate, ts);
    v.extend(order_ref.to_be_bytes());
    v.extend(shares.to_be_bytes());
    v.extend(match_number.to_be_bytes());
    v
}

pub fn cancel(ts: u64, locate: u16, order_ref: u64, shares: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(23);
    header(&mut v, b'X', locate, ts);
    v.extend(order_ref.to_be_bytes());
    v.extend(shares.to_be_bytes());
    v
}

pub fn delete(ts: u64, locate: u16, order_ref: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(19);
    header(&mut v, b'D', locate, ts);
    v.extend(order_ref.to_be_bytes());
    v
}

pub fn replace(ts: u64, locate: u16, orig: u64, new: u64, shares: u32, price: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(35);
    header(&mut v, b'U', locate, ts);
    v.extend(orig.to_be_bytes());
    v.extend(new.to_be_bytes());
    v.extend(shares.to_be_bytes());
    v.extend(price.to_be_bytes());
    v
}

#[derive(Clone, Copy)]
struct Resting {
    locate: u16,
    open: u32,
}

/// Turns engine events into ITCH messages. Only orders that rest on the book
/// are public: an incoming order shows up as executions against resting
/// orders, then (if anything is left) as an add.
pub struct Feed {
    symbols: Vec<bus::Symbol>,
    resting: HashMap<u64, Resting>,
    pending_replace: Option<Event>,
}

impl Feed {
    pub fn new(symbols: Vec<bus::Symbol>) -> Feed {
        Feed { symbols, resting: HashMap::new(), pending_replace: None }
    }

    /// The messages that open a day: start of messages, the stock directory,
    /// every stock trading, start of market hours.
    pub fn start_of_day(&self, ts: u64) -> Vec<Vec<u8>> {
        let mut out = vec![system_event(ts, b'O'), system_event(ts, b'S')];
        for (i, s) in self.symbols.iter().enumerate() {
            out.push(stock_directory(ts, i as u16 + 1, &s.name, s.round_lot));
            out.push(trading_action(ts, i as u16 + 1, &s.name, b'T'));
        }
        out.push(system_event(ts, b'Q'));
        out
    }

    fn sym(&self, s: u32) -> &str {
        self.symbols.get(s as usize).map(|x| x.name.as_str()).unwrap_or("")
    }

    /// Feed one event (with its sequence number in the log, used as the match
    /// number); appends the resulting messages to out.
    pub fn on_event(&mut self, seq: u64, e: &Event, out: &mut Vec<Vec<u8>>) {
        // a replace is public as one 'U' when the new order goes straight to
        // the book with its full size; otherwise as a delete then whatever
        // the new order does
        if let Some(r) = self.pending_replace.take() {
            if e.kind == bus::EV_BOOKED && e.order_id == r.order_id2 && e.qty == r.qty {
                let locate = r.symbol as u16 + 1;
                if self.resting.remove(&r.order_id).is_some() {
                    out.push(replace(e.ts, locate, r.order_id, e.order_id, e.qty, e.price as u32));
                } else {
                    out.push(add_order(e.ts, locate, e.order_id, e.side, e.qty, self.sym(e.symbol), e.price as u32));
                }
                self.resting.insert(e.order_id, Resting { locate, open: e.qty });
                return;
            }
            if self.resting.remove(&r.order_id).is_some() {
                out.push(delete(r.ts, r.symbol as u16 + 1, r.order_id));
            }
        }
        let locate = e.symbol as u16 + 1;
        match e.kind {
            bus::EV_BOOKED => {
                out.push(add_order(e.ts, locate, e.order_id, e.side, e.qty, self.sym(e.symbol), e.price as u32));
                self.resting.insert(e.order_id, Resting { locate, open: e.qty });
            }
            bus::EV_EXECUTED => {
                if let Some(r) = self.resting.get_mut(&e.order_id) {
                    out.push(executed(e.ts, r.locate, e.order_id, e.qty, seq));
                    r.open -= e.qty.min(r.open);
                    if r.open == 0 {
                        self.resting.remove(&e.order_id);
                    }
                }
            }
            bus::EV_CANCELLED => {
                if let Some(r) = self.resting.get_mut(&e.order_id) {
                    if e.qty2 == 0 {
                        out.push(delete(e.ts, r.locate, e.order_id));
                        self.resting.remove(&e.order_id);
                    } else if e.qty > 0 {
                        out.push(cancel(e.ts, r.locate, e.order_id, e.qty));
                        r.open = e.qty2;
                    }
                }
            }
            bus::EV_REPLACED => self.pending_replace = Some(*e),
            _ => {}
        }
    }

    pub fn resting_orders(&self) -> usize {
        self.resting.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert_eq!(system_event(1, b'O').len(), 12);
        assert_eq!(stock_directory(1, 1, "AAPL", 100).len(), 39);
        assert_eq!(trading_action(1, 1, "AAPL", b'T').len(), 25);
        assert_eq!(add_order(1, 1, 5, b'B', 100, "AAPL", 1500000).len(), 36);
        assert_eq!(executed(1, 1, 5, 100, 9).len(), 31);
        assert_eq!(cancel(1, 1, 5, 10).len(), 23);
        assert_eq!(delete(1, 1, 5).len(), 19);
        assert_eq!(replace(1, 1, 5, 6, 100, 1).len(), 35);
        // timestamp: 6 bytes of nanoseconds since midnight
        let a = add_order(NS_PER_DAY * 3 + 0x0102_0304_0506, 7, 5, b'S', 1, "X", 2);
        assert_eq!(&a[1..3], &[0, 7]);
        assert_eq!(&a[5..11], &[1, 2, 3, 4, 5, 6]);
    }

    fn ev(kind: u8, id: u64, qty: u32, qty2: u32) -> Event {
        Event { kind, side: b'B', symbol: 0, order_id: id, qty, qty2, price: 100, ..Default::default() }
    }

    #[test]
    fn feed() {
        let mut f = Feed::new(vec![bus::Symbol { name: "AAPL".into(), round_lot: 100, tick: 100 }]);
        let mut out = Vec::new();
        f.on_event(0, &ev(bus::EV_ACCEPTED, 1, 100, 0), &mut out);
        assert!(out.is_empty());
        f.on_event(1, &ev(bus::EV_BOOKED, 1, 100, 0), &mut out);
        f.on_event(2, &ev(bus::EV_EXECUTED, 1, 30, 0), &mut out);
        f.on_event(3, &ev(bus::EV_CANCELLED, 1, 20, 50), &mut out);
        let kinds: Vec<u8> = out.iter().map(|m| m[0]).collect();
        assert_eq!(kinds, b"AEX");
        // replace that goes to the book in full: one 'U'
        out.clear();
        let mut r = ev(bus::EV_REPLACED, 1, 70, 50);
        r.order_id2 = 2;
        f.on_event(4, &r, &mut out);
        f.on_event(5, &ev(bus::EV_BOOKED, 2, 70, 0), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0], b'U');
        // replace that executes first: delete, then add of the rest
        out.clear();
        let mut r = ev(bus::EV_REPLACED, 2, 70, 70);
        r.order_id2 = 3;
        f.on_event(6, &r, &mut out);
        f.on_event(7, &ev(bus::EV_EXECUTED, 9, 10, 60), &mut out); // against some other (unknown) order
        f.on_event(8, &ev(bus::EV_BOOKED, 3, 60, 0), &mut out);
        let kinds: Vec<u8> = out.iter().map(|m| m[0]).collect();
        assert_eq!(kinds, b"DA");
        assert_eq!(f.resting_orders(), 1);
        // full cancel deletes
        out.clear();
        f.on_event(9, &ev(bus::EV_CANCELLED, 3, 60, 0), &mut out);
        assert_eq!(out[0][0], b'D');
        assert_eq!(f.resting_orders(), 0);
    }
}
