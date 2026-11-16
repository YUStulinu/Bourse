//! The gateway process: one thread that polls the client sockets and the
//! engine's event log in a loop. Client messages become commands on this
//! gateway's inbound ring; events become OUCH, FIX and ITCH messages.
//!
//! Every order of this gateway gets an engine id `(session << 40) | n`, where
//! the session number is `ring << 12 | local` so that events can be routed
//! back without any lookup outside this process.

use crate::bus::{self, Bus, Command, Consumer, Event, Producer};
use crate::fix::{self, Message};
use crate::itch::Feed;
use crate::ouch::{self, Inbound};
use crate::risk::{Breach, RateLimit, Risk};
use crate::soup;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const OUCH_LISTENER: Token = Token(0);
const FIX_LISTENER: Token = Token(1);
const ITCH_LISTENER: Token = Token(2);
const FIRST_CONN: usize = 16;
const SOUP_SESSION: &str = "1";

#[derive(Clone, Debug, Deserialize)]
pub struct OuchUser {
    pub user: String,
    pub password: String,
    pub account: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FixUser {
    pub sender: String,
    pub account: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Accounts {
    #[serde(default)]
    pub ouch: Vec<OuchUser>,
    #[serde(default)]
    pub fix: Vec<FixUser>,
}

pub struct Config {
    pub bus: String,
    pub ouch: Option<SocketAddr>,
    pub fix: Option<SocketAddr>,
    pub itch: Option<SocketAddr>,
    pub accounts: Accounts,
    pub limits_path: Option<String>,
    /// spin without sleeping (lowest latency, one busy core)
    pub spin: bool,
    pub comp_id: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Ouch,
    Fix,
    Itch,
}

struct Conn {
    stream: TcpStream,
    kind: Kind,
    inbuf: Vec<u8>,
    out: Vec<u8>,
    session: Option<usize>,
    last_rx: Instant,
    last_tx: Instant,
    closing: bool,
}

struct Session {
    kind: Kind,
    name: String,
    account: u32,
    global: u16,
    conn: Option<Token>,
    /// sequenced messages sent (SoupBinTCP payloads, or FIX application messages)
    store: Vec<Vec<u8>>,
    counter: u64,
    rate: RateLimit,
    // OUCH
    last_user_ref: u32,
    refs: HashMap<u32, u64>,
    // FIX
    out_seq: u64,
    in_seq: u64,
    clords: HashMap<String, u64>,
    heartbeat: Duration,
}

#[derive(Clone)]
struct OrderInfo {
    session: usize,
    user_ref: u32,
    ouch_clid: [u8; 14],
    cl_ord_id: String,
    orig_cl_ord_id: String,
    cancel_cl_ord_id: Option<String>,
    symbol: u32,
    side: u8,
    price: i64,
    total: u32,
    open: u32,
    cum: u32,
    cum_value: i128,
    tif: u8,
    account: u32,
    replace_of: Option<u64>,
    /// when the request was read off the socket (ns since the gateway started)
    t_in: u64,
}

pub struct Stats {
    pub commands: u64,
    pub events: u64,
    pub messages_out: u64,
    /// ns from reading a new order (or replace) off the socket to having
    /// written its acknowledgment to the socket
    pub ack_ns: Vec<u32>,
}

impl Stats {
    pub fn percentiles_us(&self) -> Option<[f64; 6]> {
        if self.ack_ns.is_empty() {
            return None;
        }
        let mut v = self.ack_ns.clone();
        v.sort_unstable();
        let at = |q: f64| v[((q * v.len() as f64) as usize).min(v.len() - 1)] as f64 / 1000.0;
        Some([at(0.5), at(0.9), at(0.99), at(0.999), at(0.9999), *v.last().unwrap() as f64 / 1000.0])
    }
}

pub struct Gateway<'a> {
    cfg: Config,
    bus: &'a Bus,
    producer: Option<Producer<'a>>,
    consumer: Consumer<'a>,
    symbols: Vec<bus::Symbol>,
    by_name: HashMap<String, u32>,
    poll: Poll,
    listeners: Vec<(Token, TcpListener)>,
    conns: HashMap<Token, Conn>,
    next_token: usize,
    sessions: Vec<Session>,
    by_user: HashMap<String, usize>,
    orders: HashMap<u64, OrderInfo>,
    risk: Risk,
    limits_mtime: Option<std::time::SystemTime>,
    feed: Option<Feed>,
    itch_store: Vec<Vec<u8>>,
    pub stats: Stats,
    scratch: Vec<Vec<u8>>,
    t_base: Instant,
    rx_ns: u64,
    /// arrival times of requests acknowledged in this batch
    acked: Vec<u64>,
}

fn ns_of_day(ts: u64) -> u64 {
    ts % crate::itch::NS_PER_DAY
}

impl<'a> Gateway<'a> {
    pub fn new(bus: &'a Bus, cfg: Config) -> io::Result<Gateway<'a>> {
        let poll = Poll::new()?;
        let mut listeners = Vec::new();
        for (tok, addr) in [(OUCH_LISTENER, cfg.ouch), (FIX_LISTENER, cfg.fix), (ITCH_LISTENER, cfg.itch)] {
            if let Some(a) = addr {
                let mut l = TcpListener::bind(a)?;
                poll.registry().register(&mut l, tok, Interest::READABLE)?;
                listeners.push((tok, l));
            }
        }
        let producer = if cfg.ouch.is_some() || cfg.fix.is_some() {
            Some(Producer::attach(bus).ok_or_else(|| io::Error::other("no free inbound ring"))?)
        } else {
            None
        };
        let consumer = Consumer::attach(bus).ok_or_else(|| io::Error::other("no free consumer slot"))?;
        let symbols = bus.symbols();
        let by_name = symbols.iter().enumerate().map(|(i, s)| (s.name.clone(), i as u32)).collect();
        let feed = cfg.itch.map(|_| Feed::new(symbols.clone()));
        let mut g = Gateway {
            cfg,
            bus,
            producer,
            consumer,
            symbols,
            by_name,
            poll,
            listeners,
            conns: HashMap::new(),
            next_token: FIRST_CONN,
            sessions: Vec::new(),
            by_user: HashMap::new(),
            orders: HashMap::new(),
            risk: Risk::default(),
            limits_mtime: None,
            feed,
            itch_store: Vec::new(),
            stats: Stats { commands: 0, events: 0, messages_out: 0, ack_ns: Vec::new() },
            scratch: Vec::new(),
            t_base: Instant::now(),
            rx_ns: 0,
            acked: Vec::new(),
        };
        if let Some(f) = &g.feed {
            g.itch_store = f.start_of_day(bus::wall_ns());
        }
        g.reload_limits();
        Ok(g)
    }

    pub fn ring(&self) -> Option<u32> {
        self.producer.as_ref().map(|p| p.ring())
    }

    fn reload_limits(&mut self) {
        let Some(p) = &self.cfg.limits_path else { return };
        let Ok(meta) = std::fs::metadata(p) else { return };
        let m = meta.modified().ok();
        if m == self.limits_mtime {
            return;
        }
        self.limits_mtime = m;
        match std::fs::read_to_string(p).map(|s| serde_json::from_str(&s)) {
            Ok(Ok(f)) => self.risk.file = f,
            Ok(Err(e)) => eprintln!("gateway: bad limits file {}: {}", p, e),
            Err(e) => eprintln!("gateway: cannot read {}: {}", p, e),
        }
    }

    pub fn run(&mut self, stop: Arc<AtomicBool>) -> io::Result<()> {
        let mut events = Events::with_capacity(1024);
        let mut idle: u64 = 0;
        let mut last_housekeeping = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let timeout = if self.cfg.spin || idle < 10_000 { Duration::ZERO } else { Duration::from_millis(1) };
            self.poll.poll(&mut events, Some(timeout))?;
            let mut busy = !events.is_empty();
            for ev in events.iter() {
                let tok = ev.token();
                if tok == OUCH_LISTENER || tok == FIX_LISTENER || tok == ITCH_LISTENER {
                    self.accept(tok)?;
                    continue;
                }
                if ev.is_readable() {
                    self.read(tok);
                }
                if ev.is_writable() {
                    self.flush(tok);
                }
            }
            let n = self.drain_log();
            busy |= n > 0;
            idle = if busy { 0 } else { idle + 1 };
            if last_housekeeping.elapsed() >= Duration::from_millis(200) {
                last_housekeeping = Instant::now();
                self.housekeeping();
                if self.bus.state() != bus::BUS_RUNNING {
                    eprintln!("gateway: engine stopped");
                    break;
                }
            }
            self.close_dead();
        }
        Ok(())
    }

    fn accept(&mut self, tok: Token) -> io::Result<()> {
        let kind = match tok {
            OUCH_LISTENER => Kind::Ouch,
            FIX_LISTENER => Kind::Fix,
            _ => Kind::Itch,
        };
        let l = &self.listeners.iter().find(|(t, _)| *t == tok).unwrap().1;
        loop {
            match l.accept() {
                Ok((mut s, _)) => {
                    s.set_nodelay(true)?;
                    let t = Token(self.next_token);
                    self.next_token += 1;
                    self.poll.registry().register(&mut s, t, Interest::READABLE | Interest::WRITABLE)?;
                    let now = Instant::now();
                    self.conns.insert(t, Conn { stream: s, kind, inbuf: Vec::new(), out: Vec::new(), session: None, last_rx: now, last_tx: now, closing: false });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn read(&mut self, tok: Token) {
        let mut buf = [0u8; 65536];
        loop {
            let Some(c) = self.conns.get_mut(&tok) else { return };
            match c.stream.read(&mut buf) {
                Ok(0) => {
                    c.closing = true;
                    return;
                }
                Ok(n) => {
                    c.inbuf.extend_from_slice(&buf[..n]);
                    c.last_rx = Instant::now();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    c.closing = true;
                    return;
                }
            }
        }
        self.rx_ns = self.t_base.elapsed().as_nanos() as u64;
        self.handle_input(tok);
        self.flush(tok);
    }

    fn handle_input(&mut self, tok: Token) {
        let kind = self.conns[&tok].kind;
        let mut inbuf = std::mem::take(&mut self.conns.get_mut(&tok).unwrap().inbuf);
        let mut used = 0;
        match kind {
            Kind::Ouch | Kind::Itch => {
                while let Some((k, r, n)) = soup::next_packet(&inbuf[used..]) {
                    let payload = inbuf[used + r.start..used + r.end].to_vec();
                    used += n;
                    self.on_soup(tok, kind, k, &payload);
                }
            }
            Kind::Fix => loop {
                match fix::next_message(&inbuf[used..]) {
                    Ok(Some((m, n))) => {
                        used += n;
                        self.on_fix(tok, m);
                    }
                    Ok(None) => break,
                    Err(_) => {
                        self.conns.get_mut(&tok).unwrap().closing = true;
                        used = inbuf.len();
                        break;
                    }
                }
            },
        }
        inbuf.drain(..used);
        if let Some(c) = self.conns.get_mut(&tok) {
            let rest = std::mem::take(&mut c.inbuf);
            inbuf.extend(rest);
            c.inbuf = inbuf;
        }
    }

    fn write_raw(&mut self, tok: Token, bytes: &[u8]) {
        if let Some(c) = self.conns.get_mut(&tok) {
            c.out.extend_from_slice(bytes);
            c.last_tx = Instant::now();
        }
    }

    fn flush(&mut self, tok: Token) {
        let Some(c) = self.conns.get_mut(&tok) else { return };
        while !c.out.is_empty() {
            match c.stream.write(&c.out) {
                Ok(0) => {
                    c.closing = true;
                    return;
                }
                Ok(n) => {
                    c.out.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    c.closing = true;
                    return;
                }
            }
        }
    }

    fn close_dead(&mut self) {
        let dead: Vec<Token> = self.conns.iter().filter(|(_, c)| c.closing).map(|(t, _)| *t).collect();
        for t in dead {
            self.flush(t);
            if let Some(mut c) = self.conns.remove(&t) {
                let _ = self.poll.registry().deregister(&mut c.stream);
                if let Some(s) = c.session.filter(|&s| s < self.sessions.len()) {
                    if self.sessions[s].conn == Some(t) {
                        self.sessions[s].conn = None;
                    }
                }
            }
        }
    }

    fn housekeeping(&mut self) {
        self.consumer.heartbeat();
        self.reload_limits();
        let now = Instant::now();
        let toks: Vec<Token> = self.conns.keys().copied().collect();
        for t in toks {
            let c = &self.conns[&t];
            match c.kind {
                Kind::Ouch | Kind::Itch => {
                    if c.last_rx.elapsed() > Duration::from_secs(15) {
                        self.conns.get_mut(&t).unwrap().closing = true;
                    } else if now.duration_since(c.last_tx) >= Duration::from_secs(1) && c.session.is_some() {
                        let mut out = Vec::new();
                        soup::packet(&mut out, soup::SERVER_HEARTBEAT, &[]);
                        self.write_raw(t, &out);
                    }
                }
                Kind::Fix => {
                    if let Some(s) = c.session {
                        let hb = self.sessions[s].heartbeat;
                        if c.last_rx.elapsed() > hb * 5 / 2 {
                            self.conns.get_mut(&t).unwrap().closing = true;
                        } else if now.duration_since(c.last_tx) >= hb {
                            self.send_fix_admin(s, Message::new("0"));
                        }
                    }
                }
            }
            self.flush(t);
        }
    }

    // ---- sessions ------------------------------------------------------------

    fn new_session(&mut self, kind: Kind, name: &str, account: u32) -> usize {
        let local = self.sessions.len() + 1;
        let ring = self.ring().unwrap_or(0) as usize;
        assert!(local < 4096, "too many sessions");
        self.sessions.push(Session {
            kind,
            name: name.to_string(),
            account,
            global: ((ring << 12) | local) as u16,
            conn: None,
            store: Vec::new(),
            counter: 0,
            rate: RateLimit::new(),
            last_user_ref: 0,
            refs: HashMap::new(),
            out_seq: 1,
            in_seq: 1,
            clords: HashMap::new(),
            heartbeat: Duration::from_secs(30),
        });
        self.by_user.insert(format!("{:?}:{}", kind, name), local - 1);
        local - 1
    }

    fn session_of_event(&self, global: u16) -> Option<usize> {
        let ring = self.ring()? as u16;
        if global >> 12 != ring {
            return None;
        }
        let local = (global & 0x0FFF) as usize;
        if local == 0 || local > self.sessions.len() {
            return None;
        }
        Some(local - 1)
    }

    fn next_order_id(&mut self, s: usize) -> u64 {
        let sess = &mut self.sessions[s];
        sess.counter += 1;
        ((sess.global as u64) << 40) | sess.counter
    }

    fn push(&mut self, c: &Command) {
        if let Some(p) = &mut self.producer {
            p.push(c);
            self.stats.commands += 1;
        }
    }

    // ---- SoupBinTCP (OUCH and ITCH) -------------------------------------------

    fn on_soup(&mut self, tok: Token, kind: Kind, k: u8, p: &[u8]) {
        match k {
            soup::LOGIN_REQUEST => {
                let Some(req) = soup::parse_login(p) else { return };
                let session = if kind == Kind::Ouch {
                    let Some(u) = self.cfg.accounts.ouch.iter().find(|u| u.user == req.username && u.password == req.password).cloned() else {
                        let mut out = Vec::new();
                        soup::packet(&mut out, soup::LOGIN_REJECTED, b"A");
                        self.write_raw(tok, &out);
                        self.conns.get_mut(&tok).unwrap().closing = true;
                        return;
                    };
                    match self.by_user.get(&format!("{:?}:{}", Kind::Ouch, u.user)) {
                        Some(&s) => s,
                        None => self.new_session(Kind::Ouch, &u.user, u.account),
                    }
                } else {
                    usize::MAX // ITCH: one shared stream
                };
                if session != usize::MAX {
                    if let Some(old) = self.sessions[session].conn {
                        if let Some(c) = self.conns.get_mut(&old) {
                            c.closing = true;
                        }
                    }
                    self.sessions[session].conn = Some(tok);
                }
                let store_len = if session == usize::MAX { self.itch_store.len() } else { self.sessions[session].store.len() } as u64;
                let from = if req.sequence == 0 { store_len + 1 } else { req.sequence.min(store_len + 1) };
                let mut out = Vec::new();
                let mut acc = format!("{:>10}", SOUP_SESSION).into_bytes();
                acc.extend(soup::numeric(from, 20));
                soup::packet(&mut out, soup::LOGIN_ACCEPTED, &acc);
                let store = if session == usize::MAX { &self.itch_store } else { &self.sessions[session].store };
                for m in &store[(from - 1) as usize..] {
                    soup::packet(&mut out, soup::SEQUENCED, m);
                }
                let c = self.conns.get_mut(&tok).unwrap();
                c.session = if session == usize::MAX { Some(usize::MAX) } else { Some(session) };
                self.write_raw(tok, &out);
            }
            soup::UNSEQUENCED if kind == Kind::Ouch => {
                let Some(s) = self.conns[&tok].session else { return };
                self.on_ouch(s, p);
            }
            soup::LOGOUT_REQUEST => {
                self.conns.get_mut(&tok).unwrap().closing = true;
            }
            _ => {} // client heartbeats only refresh last_rx
        }
    }

    fn send_ouch(&mut self, s: usize, msg: Vec<u8>) {
        if let Some(t) = self.sessions[s].conn {
            let mut out = Vec::with_capacity(msg.len() + 3);
            soup::packet(&mut out, soup::SEQUENCED, &msg);
            self.write_raw(t, &out);
        }
        self.sessions[s].store.push(msg);
        self.stats.messages_out += 1;
    }

    fn breach_ouch(b: Breach) -> u16 {
        match b {
            Breach::MaxQuantity => ouch::reject::MAX_QUANTITY,
            Breach::OrderNotional => ouch::reject::SINGLE_ORDER_NOTIONAL,
            Breach::Collar => ouch::reject::RISK_FAT_FINGER,
            Breach::OpenNotional => ouch::reject::AGGREGATE_EXPOSURE,
            Breach::Rate => ouch::reject::PORT_RATE,
            Breach::Blocked => ouch::reject::FIRM_NOT_AUTHORIZED,
        }
    }

    fn on_ouch(&mut self, s: usize, p: &[u8]) {
        let Some(msg) = ouch::parse_inbound(p) else { return };
        let now = bus::wall_ns();
        let account = self.sessions[s].account;
        let rate = self.risk.limits(account).max_msgs_per_sec;
        let allowed = self.sessions[s].rate.allow(now, rate);
        match msg {
            Inbound::Enter(o) => {
                if o.user_ref <= self.sessions[s].last_user_ref {
                    return; // a retransmission: ignored, as the specification says
                }
                self.sessions[s].last_user_ref = o.user_ref;
                let reject = |g: &mut Self, code: u16| {
                    let m = ouch::rejected(ns_of_day(now), o.user_ref, code, &o.cl_ord_id);
                    g.send_ouch(s, m);
                };
                if !allowed {
                    return reject(self, ouch::reject::PORT_RATE);
                }
                let Some(&symbol) = self.by_name.get(&o.symbol) else { return reject(self, ouch::reject::INVALID_SYMBOL) };
                if o.qty == 0 || o.qty >= 1_000_000 {
                    return reject(self, ouch::reject::INVALID_QUANTITY);
                }
                let market = o.price >= 0x7FFF_FFFF || o.price >= 2_000_000_000;
                let price = if market { 0 } else { o.price as i64 };
                if let Err(b) = self.risk.check(account, symbol, if market { None } else { Some(price) }, o.qty) {
                    return reject(self, Self::breach_ouch(b));
                }
                let id = self.next_order_id(s);
                let side = if o.side == b'B' { b'B' } else { b'S' };
                let tif = if o.tif == b'3' { bus::TIF_IOC } else { bus::TIF_DAY };
                let cmd = Command {
                    kind: bus::CMD_NEW,
                    side,
                    tif,
                    flags: (if o.post_only { bus::F_POST_ONLY } else { 0 }) | (if market { bus::F_MARKET } else { 0 }),
                    session: self.sessions[s].global,
                    symbol,
                    account,
                    order_id: id,
                    price,
                    qty: o.qty,
                    ts: now,
                    ..Default::default()
                };
                self.sessions[s].refs.insert(o.user_ref, id);
                self.orders.insert(
                    id,
                    OrderInfo {
                        session: s,
                        user_ref: o.user_ref,
                        ouch_clid: o.cl_ord_id,
                        cl_ord_id: String::new(),
                        orig_cl_ord_id: String::new(),
                        cancel_cl_ord_id: None,
                        symbol,
                        side,
                        price,
                        total: o.qty,
                        open: 0,
                        cum: 0,
                        cum_value: 0,
                        tif: o.tif,
                        account,
                        replace_of: None,
                        t_in: self.rx_ns,
                    },
                );
                self.push(&cmd);
            }
            Inbound::Replace(r) => {
                if r.user_ref <= self.sessions[s].last_user_ref {
                    return;
                }
                let Some(&old) = self.sessions[s].refs.get(&r.orig_user_ref) else { return }; // not live: ignored
                let Some(info) = self.orders.get(&old).cloned() else { return };
                self.sessions[s].last_user_ref = r.user_ref;
                if !allowed {
                    let m = ouch::rejected(ns_of_day(now), r.user_ref, ouch::reject::PORT_RATE, &r.cl_ord_id);
                    return self.send_ouch(s, m);
                }
                // the quantity is the total liability of the chain, executions included
                let open = r.qty.saturating_sub(info.cum);
                if open == 0 {
                    let c = Command { kind: bus::CMD_CANCEL, session: self.sessions[s].global, order_id: old, qty: 0, ts: now, ..Default::default() };
                    return self.push(&c);
                }
                let id = self.next_order_id(s);
                let mut n = info.clone();
                n.user_ref = r.user_ref;
                n.ouch_clid = r.cl_ord_id;
                n.price = r.price as i64;
                n.total = r.qty;
                n.open = 0;
                n.tif = r.tif;
                n.replace_of = Some(old);
                n.t_in = self.rx_ns;
                self.orders.insert(id, n);
                self.sessions[s].refs.insert(r.user_ref, id);
                let c = Command {
                    kind: bus::CMD_REPLACE,
                    session: self.sessions[s].global,
                    account,
                    order_id: old,
                    new_order_id: id,
                    price: r.price as i64,
                    qty: open,
                    ts: now,
                    ..Default::default()
                };
                self.push(&c);
            }
            Inbound::Cancel(c) => {
                let Some(&id) = self.sessions[s].refs.get(&c.user_ref) else { return };
                let Some(info) = self.orders.get(&id) else { return };
                let leave = c.qty.saturating_sub(info.cum);
                let cmd = Command { kind: bus::CMD_CANCEL, session: self.sessions[s].global, order_id: id, qty: leave, ts: now, ..Default::default() };
                self.push(&cmd);
            }
        }
    }

    // ---- FIX 4.4 ----------------------------------------------------------------

    fn fix_send(&mut self, s: usize, m: &Message, store: bool) {
        let seq = self.sessions[s].out_seq;
        self.sessions[s].out_seq += 1;
        let bytes = fix::encode(m, &self.cfg.comp_id, &self.sessions[s].name, seq, &fix::utc_timestamp(bus::wall_ns()));
        if let Some(t) = self.sessions[s].conn {
            self.write_raw(t, &bytes);
        }
        if store {
            // keep application messages for resends: index = seq - 1
            let st = &mut self.sessions[s].store;
            st.resize(seq as usize - 1, Vec::new());
            st.push(bytes);
        }
        self.stats.messages_out += 1;
    }

    fn send_fix_admin(&mut self, s: usize, m: Message) {
        self.fix_send(s, &m, false);
    }

    fn on_fix(&mut self, tok: Token, m: Message) {
        let session = self.conns[&tok].session;
        if m.msg_type() == "A" {
            let sender = m.get(49).unwrap_or("").to_string();
            let Some(u) = self.cfg.accounts.fix.iter().find(|u| u.sender == sender).cloned() else {
                self.conns.get_mut(&tok).unwrap().closing = true;
                return;
            };
            let s = match self.by_user.get(&format!("{:?}:{}", Kind::Fix, sender)) {
                Some(&s) => s,
                None => self.new_session(Kind::Fix, &sender, u.account),
            };
            if m.get(141) == Some("Y") {
                self.sessions[s].out_seq = 1;
                self.sessions[s].in_seq = 1;
                self.sessions[s].store.clear();
            }
            self.sessions[s].heartbeat = Duration::from_secs(m.int(108).unwrap_or(30).clamp(1, 600));
            self.sessions[s].conn = Some(tok);
            self.sessions[s].in_seq = m.int(34).unwrap_or(1) + 1;
            self.conns.get_mut(&tok).unwrap().session = Some(s);
            let mut r = Message::new("A");
            r.set(98, 0).set(108, self.sessions[s].heartbeat.as_secs());
            if m.get(141) == Some("Y") {
                r.set(141, "Y");
            }
            self.send_fix_admin(s, r);
            return;
        }
        let Some(s) = session else {
            self.conns.get_mut(&tok).unwrap().closing = true;
            return;
        };
        let seq = m.int(34).unwrap_or(0);
        let expected = self.sessions[s].in_seq;
        if seq > expected && m.msg_type() != "4" {
            let mut rr = Message::new("2");
            rr.set(7, expected).set(16, 0);
            self.send_fix_admin(s, rr);
        } else if seq < expected && m.get(43) != Some("Y") && m.msg_type() != "4" {
            let mut lo = Message::new("5");
            lo.set(58, format!("MsgSeqNum too low, expecting {} but received {}", expected, seq));
            self.send_fix_admin(s, lo);
            self.conns.get_mut(&tok).unwrap().closing = true;
            return;
        }
        self.sessions[s].in_seq = seq.max(expected) + 1;
        match m.msg_type() {
            "0" => {}
            "1" => {
                let mut h = Message::new("0");
                h.set(112, m.get(112).unwrap_or(""));
                self.send_fix_admin(s, h);
            }
            "2" => self.fix_resend(s, m.int(7).unwrap_or(1), m.int(16).unwrap_or(0)),
            "4" => {
                if let Some(n) = m.int(36) {
                    self.sessions[s].in_seq = n;
                }
            }
            "5" => {
                self.send_fix_admin(s, Message::new("5"));
                self.conns.get_mut(&tok).unwrap().closing = true;
            }
            "D" => self.fix_new_order(s, &m),
            "F" => self.fix_cancel(s, &m),
            "G" => self.fix_replace(s, &m),
            _ => {
                let mut r = Message::new("3");
                r.set(45, seq).set(372, m.msg_type()).set(373, 11).set(58, "unsupported message type");
                self.send_fix_admin(s, r);
            }
        }
    }

    /// Resend stored application messages (with PossDupFlag) and fill the
    /// gaps left by administrative messages with SequenceReset-GapFill.
    fn fix_resend(&mut self, s: usize, begin: u64, end: u64) {
        let last = self.sessions[s].out_seq - 1;
        let end = if end == 0 || end > last { last } else { end };
        let mut seq = begin.max(1);
        let mut gap_start: Option<u64> = None;
        let mut out: Vec<u8> = Vec::new();
        let comp = self.cfg.comp_id.clone();
        let name = self.sessions[s].name.clone();
        let now = fix::utc_timestamp(bus::wall_ns());
        let gap = |out: &mut Vec<u8>, from: u64, to: u64| {
            let mut g = Message::new("4");
            g.set(43, "Y").set(123, "Y").set(36, to);
            out.extend(fix::encode(&g, &comp, &name, from, &now));
        };
        while seq <= end {
            let stored = self.sessions[s].store.get(seq as usize - 1).filter(|b| !b.is_empty()).cloned();
            match stored {
                Some(bytes) => {
                    if let Some(g) = gap_start.take() {
                        gap(&mut out, g, seq);
                    }
                    let (mut msg, _) = fix::next_message(&bytes).ok().flatten().unwrap();
                    let orig_time = msg.get(52).unwrap_or("").to_string();
                    msg.fields.retain(|(t, _)| !matches!(*t, 49 | 56 | 34 | 52));
                    msg.set(43, "Y").set(122, orig_time);
                    out.extend(fix::encode(&msg, &comp, &name, seq, &now));
                }
                None => {
                    if gap_start.is_none() {
                        gap_start = Some(seq);
                    }
                }
            }
            seq += 1;
        }
        if let Some(g) = gap_start {
            gap(&mut out, g, end + 1);
        }
        if let Some(t) = self.sessions[s].conn {
            self.write_raw(t, &out);
        }
    }

    fn fix_reject(&mut self, s: usize, m: &Message, reason: u32, text: &str) {
        let mut er = Message::new("8");
        er.set(37, "NONE")
            .set(11, m.get(11).unwrap_or(""))
            .set(17, format!("R{}", bus::wall_ns()))
            .set(150, "8")
            .set(39, "8")
            .set(55, m.get(55).unwrap_or(""))
            .set(54, m.get(54).unwrap_or(""))
            .set(38, m.get(38).unwrap_or("0"))
            .set(151, 0)
            .set(14, 0)
            .set(6, 0)
            .set(103, reason)
            .set(58, text);
        self.fix_send(s, &er, true);
    }

    fn fix_new_order(&mut self, s: usize, m: &Message) {
        let now = bus::wall_ns();
        let account = self.sessions[s].account;
        let cl = m.get(11).unwrap_or("").to_string();
        if !self.sessions[s].rate.allow(now, self.risk.limits(account).max_msgs_per_sec) {
            return self.fix_reject(s, m, 99, "message rate exceeded");
        }
        if cl.is_empty() || self.sessions[s].clords.contains_key(&cl) {
            return self.fix_reject(s, m, 6, "duplicate or missing ClOrdID");
        }
        let Some(&symbol) = m.get(55).and_then(|x| self.by_name.get(x)) else { return self.fix_reject(s, m, 1, "unknown symbol") };
        let qty = m.int(38).unwrap_or(0) as u32;
        if qty == 0 || qty >= 1_000_000 {
            return self.fix_reject(s, m, 13, "bad quantity");
        }
        let market = m.get(40) == Some("1");
        let price = if market { 0 } else { m.get(44).and_then(bus::parse_price).unwrap_or(0) };
        if !market && price <= 0 {
            return self.fix_reject(s, m, 11, "bad price");
        }
        let side = match m.get(54) {
            Some("1") => b'B',
            Some("2") | Some("5") | Some("6") => b'S',
            _ => return self.fix_reject(s, m, 11, "bad side"),
        };
        if let Err(b) = self.risk.check(account, symbol, if market { None } else { Some(price) }, qty) {
            return self.fix_reject(s, m, 3, &format!("risk: {:?}", b));
        }
        let tif = match m.get(59) {
            Some("3") => bus::TIF_IOC,
            Some("4") => bus::TIF_FOK,
            _ => bus::TIF_DAY,
        };
        let post_only = m.get(18).map(|v| v.split(' ').any(|x| x == "6")).unwrap_or(false);
        let id = self.next_order_id(s);
        self.sessions[s].clords.insert(cl.clone(), id);
        self.orders.insert(
            id,
            OrderInfo {
                session: s,
                user_ref: 0,
                ouch_clid: [b' '; 14],
                cl_ord_id: cl,
                orig_cl_ord_id: String::new(),
                cancel_cl_ord_id: None,
                symbol,
                side,
                price,
                total: qty,
                open: 0,
                cum: 0,
                cum_value: 0,
                tif,
                account,
                replace_of: None,
                t_in: self.rx_ns,
            },
        );
        let cmd = Command {
            kind: bus::CMD_NEW,
            side,
            tif,
            flags: (if post_only { bus::F_POST_ONLY } else { 0 }) | (if market { bus::F_MARKET } else { 0 }),
            session: self.sessions[s].global,
            symbol,
            account,
            order_id: id,
            price,
            qty,
            ts: now,
            ..Default::default()
        };
        self.push(&cmd);
    }

    fn cancel_reject(&mut self, s: usize, m: &Message, response_to: u32, text: &str) {
        let mut r = Message::new("9");
        r.set(37, "NONE")
            .set(11, m.get(11).unwrap_or(""))
            .set(41, m.get(41).unwrap_or(""))
            .set(39, "8")
            .set(434, response_to)
            .set(102, 1)
            .set(58, text);
        self.fix_send(s, &r, true);
    }

    fn fix_cancel(&mut self, s: usize, m: &Message) {
        let orig = m.get(41).unwrap_or("");
        let Some(&id) = self.sessions[s].clords.get(orig) else { return self.cancel_reject(s, m, 1, "unknown order") };
        let Some(info) = self.orders.get_mut(&id) else { return self.cancel_reject(s, m, 1, "order is not open") };
        info.cancel_cl_ord_id = Some(m.get(11).unwrap_or("").to_string());
        let cmd = Command { kind: bus::CMD_CANCEL, session: self.sessions[s].global, order_id: id, qty: 0, ts: bus::wall_ns(), ..Default::default() };
        self.push(&cmd);
    }

    fn fix_replace(&mut self, s: usize, m: &Message) {
        let orig = m.get(41).unwrap_or("").to_string();
        let cl = m.get(11).unwrap_or("").to_string();
        let Some(&old) = self.sessions[s].clords.get(&orig) else { return self.cancel_reject(s, m, 2, "unknown order") };
        let Some(info) = self.orders.get(&old).cloned() else { return self.cancel_reject(s, m, 2, "order is not open") };
        if cl.is_empty() || self.sessions[s].clords.contains_key(&cl) {
            return self.cancel_reject(s, m, 2, "duplicate or missing ClOrdID");
        }
        let total = m.int(38).unwrap_or(info.total as u64) as u32;
        let price = m.get(44).and_then(bus::parse_price).unwrap_or(info.price);
        let open = total.saturating_sub(info.cum);
        if open == 0 || price <= 0 {
            return self.cancel_reject(s, m, 2, "quantity not above the executed quantity, or bad price");
        }
        let id = self.next_order_id(s);
        let mut n = info.clone();
        n.cl_ord_id = cl.clone();
        n.orig_cl_ord_id = orig;
        n.price = price;
        n.total = total;
        n.open = 0;
        n.replace_of = Some(old);
        n.t_in = self.rx_ns;
        self.orders.insert(id, n);
        self.sessions[s].clords.insert(cl, id);
        let c = Command {
            kind: bus::CMD_REPLACE,
            session: self.sessions[s].global,
            account: info.account,
            order_id: old,
            new_order_id: id,
            price,
            qty: open,
            ts: bus::wall_ns(),
            ..Default::default()
        };
        self.push(&c);
    }

    fn fix_report(&mut self, id: u64, exec_type: &str, last: Option<(u32, i64)>, exec_id: u64, extra: &[(u32, String)]) {
        let Some(o) = self.orders.get(&id).cloned() else { return };
        let status = match exec_type {
            "8" => "8",
            "4" => "4",
            _ if o.open == 0 && o.cum >= o.total => "2",
            _ if o.cum > 0 => "1",
            _ => "0",
        };
        let mut er = Message::new("8");
        er.set(37, id)
            .set(11, o.cancel_cl_ord_id.clone().filter(|_| exec_type == "4").unwrap_or_else(|| o.cl_ord_id.clone()));
        if exec_type == "4" && o.cancel_cl_ord_id.is_some() {
            er.set(41, &o.cl_ord_id);
        } else if !o.orig_cl_ord_id.is_empty() && exec_type == "5" {
            er.set(41, &o.orig_cl_ord_id);
        }
        er.set(17, exec_id)
            .set(150, exec_type)
            .set(39, status)
            .set(55, &self.symbols[o.symbol as usize].name)
            .set(54, if o.side == b'B' { "1" } else { "2" })
            .set(38, o.total)
            .set(44, bus::price_str(o.price));
        if let Some((q, p)) = last {
            er.set(32, q).set(31, bus::price_str(p));
        }
        let avg = if o.cum > 0 { (o.cum_value / o.cum as i128) as i64 } else { 0 };
        er.set(151, o.open).set(14, o.cum).set(6, bus::price_str(avg)).set(60, fix::utc_timestamp(bus::wall_ns()));
        for (t, v) in extra {
            er.set(*t, v);
        }
        self.fix_send(o.session, &er, true);
    }

    // ---- the event log ----------------------------------------------------------

    fn drain_log(&mut self) -> usize {
        let mut batch: Vec<(u64, Event)> = Vec::new();
        self.consumer.poll(4096, |seq, e| batch.push((seq, *e)));
        let n = batch.len();
        for (seq, e) in batch {
            self.on_event(seq, &e);
        }
        if n > 0 {
            self.stats.events += n as u64;
            let toks: Vec<Token> = self.conns.iter().filter(|(_, c)| !c.out.is_empty()).map(|(t, _)| *t).collect();
            for t in toks {
                self.flush(t);
            }
            let now = self.t_base.elapsed().as_nanos() as u64;
            for t_in in std::mem::take(&mut self.acked) {
                self.stats.ack_ns.push(now.saturating_sub(t_in).min(u32::MAX as u64) as u32);
            }
        }
        n
    }

    fn set_open(&mut self, id: u64, open: u32) {
        if let Some(o) = self.orders.get_mut(&id) {
            let d = open as i64 - o.open as i64;
            o.open = open;
            let (acc, px) = (o.account, o.price);
            self.risk.open(acc, px, d);
        }
    }

    fn forget(&mut self, id: u64) {
        if let Some(o) = self.orders.remove(&id) {
            self.risk.open(o.account, o.price, -(o.open as i64));
            let s = &mut self.sessions[o.session];
            if s.kind == Kind::Ouch {
                s.refs.remove(&o.user_ref);
            } else {
                s.clords.remove(&o.cl_ord_id);
            }
        }
    }

    fn on_event(&mut self, seq: u64, e: &Event) {
        if let Some(feed) = &mut self.feed {
            let mut msgs = std::mem::take(&mut self.scratch);
            feed.on_event(seq, e, &mut msgs);
            for m in msgs.drain(..) {
                let mut out = Vec::with_capacity(m.len() + 3);
                soup::packet(&mut out, soup::SEQUENCED, &m);
                let toks: Vec<Token> = self.conns.iter().filter(|(_, c)| c.kind == Kind::Itch && c.session.is_some()).map(|(t, _)| *t).collect();
                for t in toks {
                    self.write_raw(t, &out);
                }
                self.itch_store.push(m);
                self.stats.messages_out += 1;
            }
            self.scratch = msgs;
        }
        if e.kind == bus::EV_EXECUTED {
            self.risk.trade(e.symbol, e.price);
        }
        if self.producer.is_none() {
            return;
        }
        if let Some(s) = self.session_of_event(e.session) {
            self.order_event(s, seq, e, true);
        }
        if e.kind == bus::EV_EXECUTED {
            if let Some(s) = self.session_of_event(e.session2) {
                self.order_event(s, seq, e, false);
            }
        }
    }

    fn order_event(&mut self, s: usize, seq: u64, e: &Event, resting: bool) {
        let kind = self.sessions[s].kind;
        let ts = ns_of_day(e.ts);
        match e.kind {
            bus::EV_ACCEPTED => {
                let id = e.order_id;
                if let Some(o) = self.orders.get(&id) {
                    self.acked.push(o.t_in);
                }
                self.set_open(id, e.qty);
                let Some(o) = self.orders.get(&id).cloned() else { return };
                if kind == Kind::Ouch {
                    let m = ouch::accepted(&ouch::Accepted {
                        ts,
                        user_ref: o.user_ref,
                        side: o.side,
                        qty: e.qty,
                        symbol: &self.symbols[o.symbol as usize].name,
                        price: o.price as u64,
                        tif: o.tif,
                        order_ref: id,
                        cl_ord_id: &o.ouch_clid,
                        live: true,
                    });
                    self.send_ouch(s, m);
                } else {
                    self.fix_report(id, "0", None, seq, &[]);
                }
            }
            bus::EV_REJECTED => {
                let id = e.order_id;
                let Some(o) = self.orders.get(&id).cloned() else { return };
                self.stats.ack_ns.push((self.t_base.elapsed().as_nanos() as u64).saturating_sub(o.t_in).min(u32::MAX as u64) as u32);
                if kind == Kind::Ouch {
                    if !(o.replace_of.is_some() && e.reason == bus::REJ_UNKNOWN_ORDER) {
                        let code = match e.reason {
                            bus::REJ_SYMBOL => ouch::reject::INVALID_SYMBOL,
                            bus::REJ_QTY => ouch::reject::INVALID_QUANTITY,
                            bus::REJ_PRICE => ouch::reject::INVALID_PRICE,
                            bus::REJ_BLOCKED => ouch::reject::FIRM_NOT_AUTHORIZED,
                            bus::REJ_POST_ONLY => ouch::reject::CROSSED_MARKET,
                            bus::REJ_HALTED => ouch::reject::HALTED,
                            _ => ouch::reject::OTHER,
                        };
                        self.send_ouch(s, ouch::rejected(ts, o.user_ref, code, &o.ouch_clid));
                    }
                } else if o.replace_of.is_some() {
                    let mut m = Message::new("9");
                    m.set(37, o.replace_of.unwrap()).set(11, &o.cl_ord_id).set(41, &o.orig_cl_ord_id).set(39, "0").set(434, 2).set(102, 99);
                    self.fix_send(s, &m, true);
                } else {
                    let reason = match e.reason {
                        bus::REJ_SYMBOL => 1,
                        bus::REJ_DUPLICATE => 6,
                        bus::REJ_HALTED => 2,
                        bus::REJ_BLOCKED => 3,
                        _ => 99,
                    };
                    self.fix_report(id, "8", None, seq, &[(103, reason.to_string())]);
                }
                self.forget(id);
            }
            bus::EV_EXECUTED => {
                let id = if resting { e.order_id } else { e.order_id2 };
                let left = if resting { None } else { Some(e.qty2) };
                let Some(o) = self.orders.get_mut(&id) else { return };
                o.cum += e.qty;
                o.cum_value += e.price as i128 * e.qty as i128;
                let open = left.unwrap_or_else(|| o.open.saturating_sub(e.qty));
                let o = o.clone();
                self.set_open(id, open);
                if kind == Kind::Ouch {
                    let m = ouch::executed(ts, o.user_ref, e.qty, e.price as u64, if resting { b'A' } else { b'R' }, seq);
                    self.send_ouch(s, m);
                } else {
                    self.fix_report(id, "F", Some((e.qty, e.price)), seq, &[]);
                }
                // a resting order that is filled, or an incoming order with
                // nothing left, is done
                if open == 0 {
                    self.forget(id);
                }
            }
            bus::EV_CANCELLED => {
                let id = e.order_id;
                if !self.orders.contains_key(&id) {
                    return;
                }
                if e.qty == 0 && e.qty2 > 0 {
                    return; // a superfluous cancel: silently ignored
                }
                self.set_open(id, e.qty2);
                let o = self.orders[&id].clone();
                if kind == Kind::Ouch {
                    let reason = match e.reason {
                        bus::CXL_IOC | bus::CXL_FOK => b'I',
                        bus::CXL_STP => b'Q',
                        bus::CXL_BLOCKED => b'S',
                        _ => b'U',
                    };
                    self.send_ouch(s, ouch::canceled(ts, o.user_ref, e.qty, reason));
                } else {
                    self.fix_report(id, "4", None, seq, &[]);
                }
                if e.qty2 == 0 {
                    self.forget(id);
                }
            }
            bus::EV_REPLACED => {
                let (old, new) = (e.order_id, e.order_id2);
                let Some(prev) = self.orders.get(&old).cloned() else { return };
                if let Some(n) = self.orders.get_mut(&new) {
                    n.cum = prev.cum;
                    n.cum_value = prev.cum_value;
                }
                self.set_open(old, 0);
                self.set_open(new, e.qty);
                if let Some(o) = self.orders.get(&new) {
                    self.acked.push(o.t_in);
                }
                let Some(n) = self.orders.get(&new).cloned() else { return };
                if kind == Kind::Ouch {
                    let m = ouch::replaced(&ouch::Replaced {
                        ts,
                        orig_user_ref: prev.user_ref,
                        user_ref: n.user_ref,
                        side: n.side,
                        qty: e.qty,
                        symbol: &self.symbols[n.symbol as usize].name,
                        price: e.price as u64,
                        tif: n.tif,
                        order_ref: new,
                        cl_ord_id: &n.ouch_clid,
                    });
                    self.send_ouch(s, m);
                } else {
                    self.fix_report(new, "5", None, seq, &[]);
                }
                self.forget(old);
            }
            _ => {}
        }
    }
}

/// Load the accounts file (JSON with "ouch" and "fix" lists).
pub fn load_accounts(path: &str) -> io::Result<Accounts> {
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str(&s).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
