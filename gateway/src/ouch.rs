//! OUCH 5.0 messages (Nasdaq OUCH 5.0 specification): binary, big-endian,
//! prices with 4 implied decimals. The core order messages are implemented:
//! Enter Order, Replace Order and Cancel Order in; System Event, Order
//! Accepted, Order Replaced, Order Canceled, Order Executed and Rejected out.
//! Of the optional appendage fields, PostOnly (tag 12) is understood.

use crate::soup::alpha;

#[derive(Clone, Debug, PartialEq)]
pub struct EnterOrder {
    pub user_ref: u32,
    pub side: u8,
    pub qty: u32,
    pub symbol: String,
    pub price: u64,
    pub tif: u8,
    pub display: u8,
    pub capacity: u8,
    pub iso: u8,
    pub cross: u8,
    pub cl_ord_id: [u8; 14],
    pub post_only: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplaceOrder {
    pub orig_user_ref: u32,
    pub user_ref: u32,
    pub qty: u32,
    pub price: u64,
    pub tif: u8,
    pub display: u8,
    pub iso: u8,
    pub cl_ord_id: [u8; 14],
}

#[derive(Clone, Debug, PartialEq)]
pub struct CancelOrder {
    pub user_ref: u32,
    pub qty: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Inbound {
    Enter(EnterOrder),
    Replace(ReplaceOrder),
    Cancel(CancelOrder),
}

fn u32be(p: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(p[o..o + 4].try_into().unwrap())
}
fn u64be(p: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(p[o..o + 8].try_into().unwrap())
}
fn u16be(p: &[u8], o: usize) -> u16 {
    u16::from_be_bytes(p[o..o + 2].try_into().unwrap())
}

/// The value of an option in an appendage of TagValue elements.
fn option(app: &[u8], tag: u8) -> Option<&[u8]> {
    let mut i = 0;
    while i < app.len() {
        let len = app[i] as usize;
        if len == 0 || i + 1 + len > app.len() {
            return None;
        }
        if app[i + 1] == tag {
            return Some(&app[i + 2..i + 1 + len]);
        }
        i += 1 + len;
    }
    None
}

pub fn parse_inbound(p: &[u8]) -> Option<Inbound> {
    match *p.first()? {
        b'O' if p.len() >= 47 => {
            let app_len = u16be(p, 45) as usize;
            let app = p.get(47..47 + app_len).unwrap_or(&[]);
            Some(Inbound::Enter(EnterOrder {
                user_ref: u32be(p, 1),
                side: p[5],
                qty: u32be(p, 6),
                symbol: String::from_utf8_lossy(&p[10..18]).trim_end().to_string(),
                price: u64be(p, 18),
                tif: p[26],
                display: p[27],
                capacity: p[28],
                iso: p[29],
                cross: p[30],
                cl_ord_id: p[31..45].try_into().unwrap(),
                post_only: option(app, 12) == Some(b"P"),
            }))
        }
        b'U' if p.len() >= 40 => Some(Inbound::Replace(ReplaceOrder {
            orig_user_ref: u32be(p, 1),
            user_ref: u32be(p, 5),
            qty: u32be(p, 9),
            price: u64be(p, 13),
            tif: p[21],
            display: p[22],
            iso: p[23],
            cl_ord_id: p[24..38].try_into().unwrap(),
        })),
        b'X' if p.len() >= 9 => Some(Inbound::Cancel(CancelOrder { user_ref: u32be(p, 1), qty: u32be(p, 5) })),
        _ => None,
    }
}

pub fn enter_order(o: &EnterOrder) -> Vec<u8> {
    let mut v = vec![b'O'];
    v.extend(o.user_ref.to_be_bytes());
    v.push(o.side);
    v.extend(o.qty.to_be_bytes());
    v.extend(alpha(&o.symbol, 8));
    v.extend(o.price.to_be_bytes());
    v.extend([o.tif, o.display, o.capacity, o.iso, o.cross]);
    v.extend(o.cl_ord_id);
    if o.post_only {
        v.extend(3u16.to_be_bytes());
        v.extend([2, 12, b'P']);
    } else {
        v.extend(0u16.to_be_bytes());
    }
    v
}

pub fn replace_order(r: &ReplaceOrder) -> Vec<u8> {
    let mut v = vec![b'U'];
    v.extend(r.orig_user_ref.to_be_bytes());
    v.extend(r.user_ref.to_be_bytes());
    v.extend(r.qty.to_be_bytes());
    v.extend(r.price.to_be_bytes());
    v.extend([r.tif, r.display, r.iso]);
    v.extend(r.cl_ord_id);
    v.extend(0u16.to_be_bytes());
    v
}

pub fn cancel_order(c: &CancelOrder) -> Vec<u8> {
    let mut v = vec![b'X'];
    v.extend(c.user_ref.to_be_bytes());
    v.extend(c.qty.to_be_bytes());
    v.extend(0u16.to_be_bytes());
    v
}

// ---- outbound ----------------------------------------------------------------

pub fn system_event(ts: u64, code: u8) -> Vec<u8> {
    let mut v = vec![b'S'];
    v.extend(ts.to_be_bytes());
    v.push(code);
    v
}

pub struct Accepted<'a> {
    pub ts: u64,
    pub user_ref: u32,
    pub side: u8,
    pub qty: u32,
    pub symbol: &'a str,
    pub price: u64,
    pub tif: u8,
    pub order_ref: u64,
    pub cl_ord_id: &'a [u8; 14],
    pub live: bool,
}

pub fn accepted(a: &Accepted) -> Vec<u8> {
    let mut v = Vec::with_capacity(64);
    v.push(b'A');
    v.extend(a.ts.to_be_bytes());
    v.extend(a.user_ref.to_be_bytes());
    v.push(a.side);
    v.extend(a.qty.to_be_bytes());
    v.extend(alpha(a.symbol, 8));
    v.extend(a.price.to_be_bytes());
    v.push(a.tif);
    v.push(b'Y');
    v.extend(a.order_ref.to_be_bytes());
    v.extend([b'A', b'N', b'N', if a.live { b'L' } else { b'D' }]);
    v.extend(a.cl_ord_id);
    v.extend(0u16.to_be_bytes());
    v
}

pub struct Replaced<'a> {
    pub ts: u64,
    pub orig_user_ref: u32,
    pub user_ref: u32,
    pub side: u8,
    pub qty: u32,
    pub symbol: &'a str,
    pub price: u64,
    pub tif: u8,
    pub order_ref: u64,
    pub cl_ord_id: &'a [u8; 14],
}

pub fn replaced(r: &Replaced) -> Vec<u8> {
    let mut v = Vec::with_capacity(68);
    v.push(b'U');
    v.extend(r.ts.to_be_bytes());
    v.extend(r.orig_user_ref.to_be_bytes());
    v.extend(r.user_ref.to_be_bytes());
    v.push(r.side);
    v.extend(r.qty.to_be_bytes());
    v.extend(alpha(r.symbol, 8));
    v.extend(r.price.to_be_bytes());
    v.push(r.tif);
    v.push(b'Y');
    v.extend(r.order_ref.to_be_bytes());
    v.extend([b'A', b'N', b'N', b'L']);
    v.extend(r.cl_ord_id);
    v.extend(0u16.to_be_bytes());
    v
}

pub fn canceled(ts: u64, user_ref: u32, qty: u32, reason: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(20);
    v.push(b'C');
    v.extend(ts.to_be_bytes());
    v.extend(user_ref.to_be_bytes());
    v.extend(qty.to_be_bytes());
    v.push(reason);
    v.extend(0u16.to_be_bytes());
    v
}

pub fn executed(ts: u64, user_ref: u32, qty: u32, price: u64, liquidity: u8, match_number: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(36);
    v.push(b'E');
    v.extend(ts.to_be_bytes());
    v.extend(user_ref.to_be_bytes());
    v.extend(qty.to_be_bytes());
    v.extend(price.to_be_bytes());
    v.push(liquidity);
    v.extend(match_number.to_be_bytes());
    v.extend(0u16.to_be_bytes());
    v
}

pub fn rejected(ts: u64, user_ref: u32, reason: u16, cl_ord_id: &[u8; 14]) -> Vec<u8> {
    let mut v = Vec::with_capacity(31);
    v.push(b'J');
    v.extend(ts.to_be_bytes());
    v.extend(user_ref.to_be_bytes());
    v.extend(reason.to_be_bytes());
    v.extend(cl_ord_id);
    v.extend(0u16.to_be_bytes());
    v
}

/// Reject reasons (Appendix C of the specification).
pub mod reject {
    pub const HALTED: u16 = 0x0007;
    pub const FIRM_NOT_AUTHORIZED: u16 = 0x000C;
    pub const OTHER: u16 = 0x000F;
    pub const CROSSED_MARKET: u16 = 0x0012;
    pub const INVALID_QUANTITY: u16 = 0x0013;
    pub const INVALID_SYMBOL: u16 = 0x0017;
    pub const INVALID_PRICE: u16 = 0x001D;
    pub const AGGREGATE_EXPOSURE: u16 = 0x0020;
    pub const RISK_FAT_FINGER: u16 = 0x0026;
    pub const PORT_RATE: u16 = 0x0029;
    pub const SINGLE_ORDER_NOTIONAL: u16 = 0x0030;
    pub const MAX_QUANTITY: u16 = 0x0031;
}

/// An outbound message as seen by a client (the fields a client needs).
#[derive(Clone, Debug, PartialEq)]
pub enum Outbound {
    System { ts: u64, code: u8 },
    Accepted { ts: u64, user_ref: u32, qty: u32, price: u64, order_ref: u64, live: bool },
    Replaced { ts: u64, orig_user_ref: u32, user_ref: u32, qty: u32, price: u64 },
    Canceled { ts: u64, user_ref: u32, qty: u32, reason: u8 },
    Executed { ts: u64, user_ref: u32, qty: u32, price: u64, liquidity: u8, match_number: u64 },
    Rejected { ts: u64, user_ref: u32, reason: u16 },
}

pub fn parse_outbound(p: &[u8]) -> Option<Outbound> {
    Some(match *p.first()? {
        b'S' if p.len() >= 10 => Outbound::System { ts: u64be(p, 1), code: p[9] },
        b'A' if p.len() >= 64 => Outbound::Accepted {
            ts: u64be(p, 1),
            user_ref: u32be(p, 9),
            qty: u32be(p, 14),
            price: u64be(p, 26),
            order_ref: u64be(p, 36),
            live: p[47] == b'L',
        },
        b'U' if p.len() >= 68 => Outbound::Replaced {
            ts: u64be(p, 1),
            orig_user_ref: u32be(p, 9),
            user_ref: u32be(p, 13),
            qty: u32be(p, 18),
            price: u64be(p, 30),
        },
        b'C' if p.len() >= 18 => Outbound::Canceled { ts: u64be(p, 1), user_ref: u32be(p, 9), qty: u32be(p, 13), reason: p[17] },
        b'E' if p.len() >= 36 => Outbound::Executed {
            ts: u64be(p, 1),
            user_ref: u32be(p, 9),
            qty: u32be(p, 13),
            price: u64be(p, 17),
            liquidity: p[25],
            match_number: u64be(p, 26),
        },
        b'J' if p.len() >= 29 => Outbound::Rejected { ts: u64be(p, 1), user_ref: u32be(p, 9), reason: u16be(p, 13) },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clid(s: &str) -> [u8; 14] {
        alpha(s, 14).try_into().unwrap()
    }

    #[test]
    fn inbound_round_trip_and_offsets() {
        let o = EnterOrder {
            user_ref: 7,
            side: b'B',
            qty: 300,
            symbol: "AAPL".into(),
            price: 1_543_200,
            tif: b'0',
            display: b'Y',
            capacity: b'A',
            iso: b'N',
            cross: b'N',
            cl_ord_id: clid("abc"),
            post_only: true,
        };
        let b = enter_order(&o);
        assert_eq!(b.len(), 47 + 3);
        assert_eq!(&b[10..18], b"AAPL    ");
        assert_eq!(u64::from_be_bytes(b[18..26].try_into().unwrap()), 1_543_200);
        assert_eq!(parse_inbound(&b), Some(Inbound::Enter(o)));
        let r = ReplaceOrder { orig_user_ref: 7, user_ref: 8, qty: 200, price: 1_543_100, tif: b'0', display: b'Y', iso: b'N', cl_ord_id: clid("x") };
        let b = replace_order(&r);
        assert_eq!(b.len(), 40);
        assert_eq!(parse_inbound(&b), Some(Inbound::Replace(r)));
        let c = CancelOrder { user_ref: 8, qty: 0 };
        assert_eq!(parse_inbound(&cancel_order(&c)), Some(Inbound::Cancel(c)));
    }

    #[test]
    fn outbound_lengths_and_round_trip() {
        let id = clid("abc");
        let a = accepted(&Accepted { ts: 5, user_ref: 7, side: b'B', qty: 300, symbol: "AAPL", price: 10, tif: b'0', order_ref: 99, cl_ord_id: &id, live: true });
        assert_eq!(a.len(), 64);
        assert_eq!(parse_outbound(&a), Some(Outbound::Accepted { ts: 5, user_ref: 7, qty: 300, price: 10, order_ref: 99, live: true }));
        let u = replaced(&Replaced { ts: 6, orig_user_ref: 7, user_ref: 8, side: b'B', qty: 100, symbol: "AAPL", price: 11, tif: b'0', order_ref: 100, cl_ord_id: &id });
        assert_eq!(u.len(), 68);
        assert_eq!(parse_outbound(&u), Some(Outbound::Replaced { ts: 6, orig_user_ref: 7, user_ref: 8, qty: 100, price: 11 }));
        let c = canceled(7, 8, 50, b'U');
        assert_eq!(c.len(), 20);
        assert_eq!(parse_outbound(&c), Some(Outbound::Canceled { ts: 7, user_ref: 8, qty: 50, reason: b'U' }));
        let e = executed(8, 8, 50, 11, b'A', 1234);
        assert_eq!(e.len(), 36);
        assert_eq!(parse_outbound(&e), Some(Outbound::Executed { ts: 8, user_ref: 8, qty: 50, price: 11, liquidity: b'A', match_number: 1234 }));
        let j = rejected(9, 10, reject::INVALID_SYMBOL, &id);
        assert_eq!(j.len(), 31);
        assert_eq!(parse_outbound(&j), Some(Outbound::Rejected { ts: 9, user_ref: 10, reason: 0x17 }));
        assert_eq!(system_event(1, b'S').len(), 10);
    }
}
