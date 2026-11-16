//! loadgen: round-trip latency of order entry through a gateway, open loop.
//!
//!   loadgen --protocol ouch --addr 127.0.0.1:9001 --sessions 4 --rate 100000 --orders 1000000
//!           [--users ALICE:alice,BOB:bob] [--symbols AAPL,MSFT] [--json OUT]
//!
//! Each session sends on a fixed schedule (rate / sessions per second): new
//! limit orders around a price (most of them passive, some marketable) and
//! cancels of its own earlier orders. The latency of a new order is the time
//! from its scheduled send time to the arrival of its acknowledgment (OUCH
//! Order Accepted or Rejected, FIX ExecutionReport New or Rejected), so a
//! client that falls behind its schedule is measured too.

use bourse_gateway::fix::{self, Message};
use bourse_gateway::ouch;
use bourse_gateway::soup;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Clone)]
enum Req {
    New { side: u8, price: u64, qty: u32, symbol: usize },
    Cancel { target: u32 },
}

fn plan(n: usize, seed: u64, symbols: usize) -> Vec<Req> {
    let mut r = Rng(seed | 1);
    let mut live: Vec<u32> = Vec::new();
    let mut out = Vec::with_capacity(n);
    let mut news = 0u32;
    for _ in 0..n {
        if !live.is_empty() && (r.below(100) < 30 || live.len() > 200) {
            let i = r.below(live.len() as u64) as usize;
            let target = live.swap_remove(i);
            out.push(Req::Cancel { target });
        } else {
            news += 1;
            let side = if r.below(2) == 0 { b'B' } else { b'S' };
            let mid: u64 = 100_0000;
            // 90% passive 1..10 ticks away, 10% marketable by up to 2 ticks
            let price = if r.below(10) == 0 {
                if side == b'B' { mid + 100 * r.below(3) } else { mid - 100 * r.below(3) }
            } else {
                let d = 100 * (1 + r.below(10));
                if side == b'B' { mid - d } else { mid + d }
            };
            out.push(Req::New { side, price, qty: 100 * (1 + r.below(5) as u32), symbol: r.below(symbols as u64) as usize });
            live.push(news);
        }
    }
    out
}

fn read_exact_or_none(s: &mut TcpStream, buf: &mut Vec<u8>) -> bool {
    let mut tmp = [0u8; 65536];
    match s.read(&mut tmp) {
        Ok(0) => false,
        Ok(n) => {
            buf.extend_from_slice(&tmp[..n]);
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => true,
        Err(_) => false,
    }
}

struct Result_ {
    lat: Vec<u64>,
    acked: u64,
    rejected: u64,
    executions: u64,
    behind_ns: u64,
}

#[allow(clippy::too_many_arguments)]
fn run_session(protocol: &str, addr: &str, user: (String, String), symbols: Vec<String>, reqs: Vec<Req>, period_ns: f64, t0: Instant, seed: u64, base: u32) -> Result_ {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_nodelay(true).unwrap();
    let news = reqs.iter().filter(|r| matches!(r, Req::New { .. })).count();
    // scheduled time of the k-th new order (1-based user reference)
    let mut sched = vec![0u64; news + 1];
    {
        let mut k = 0;
        for (i, r) in reqs.iter().enumerate() {
            if let Req::New { .. } = r {
                k += 1;
                sched[k] = (i as f64 * period_ns) as u64;
            }
        }
    }
    let sched = Arc::new(sched);
    let lat: Arc<Vec<AtomicU64>> = Arc::new((0..=news).map(|_| AtomicU64::new(0)).collect());
    let fix_mode = protocol == "fix";
    // log in
    let mut seq_out = 1u64;
    let sender = user.0.clone();
    let fix_msg = |m: &Message, seq: u64| fix::encode(m, &sender, "BOURSE", seq, &fix::utc_timestamp(0));
    if fix_mode {
        let mut l = Message::new("A");
        l.set(98, 0).set(108, 30).set(141, "Y");
        s.write_all(&fix_msg(&l, seq_out)).unwrap();
        seq_out += 1;
    } else {
        // sequence 0: only messages from now on (the session may have a history)
        s.write_all(&soup::login_request(&user.0, &user.1, "", 0)).unwrap();
    }
    // wait for the logon answer
    let mut buf = Vec::new();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    loop {
        if !read_exact_or_none(&mut s, &mut buf) {
            panic!("login failed");
        }
        if fix_mode {
            if let Ok(Some((m, n))) = fix::next_message(&buf) {
                assert_eq!(m.msg_type(), "A", "FIX logon refused");
                buf.drain(..n);
                break;
            }
        } else if let Some((k, _, n)) = soup::next_packet(&buf) {
            assert_eq!(k, soup::LOGIN_ACCEPTED, "SoupBinTCP login refused");
            buf.drain(..n);
            break;
        }
    }
    s.set_read_timeout(None).unwrap();
    let mut reader = s.try_clone().unwrap();
    reader.set_nonblocking(true).unwrap();
    let (sched_r, lat_r) = (sched.clone(), lat.clone());
    let expect = news as u64;
    let handle = std::thread::spawn(move || {
        let (mut acked, mut rejected, mut executions) = (0u64, 0u64, 0u64);
        let mut buf = buf;
        let deadline_after_done = Duration::from_secs(2);
        let mut last_progress = Instant::now();
        while acked + rejected < expect && last_progress.elapsed() < deadline_after_done + Duration::from_secs(30) {
            if !read_exact_or_none(&mut reader, &mut buf) {
                break;
            }
            let now = t0.elapsed().as_nanos() as u64;
            let mut used = 0;
            loop {
                let mut on_ack = |k: usize, ok: bool| {
                    let k = k.wrapping_sub(base as usize);
                    if k < lat_r.len() && lat_r[k].load(Ordering::Relaxed) == 0 {
                        lat_r[k].store(now.saturating_sub(sched_r[k]).max(1), Ordering::Relaxed);
                        if ok {
                            acked += 1
                        } else {
                            rejected += 1
                        }
                    }
                };
                if fix_mode {
                    match fix::next_message(&buf[used..]) {
                        Ok(Some((m, n))) => {
                            used += n;
                            if m.msg_type() == "8" {
                                let k: usize = m.get(11).and_then(|c| c.strip_prefix('c')).and_then(|c| c.parse().ok()).unwrap_or(usize::MAX);
                                match m.get(150) {
                                    Some("0") => on_ack(k, true),
                                    Some("8") => on_ack(k, false),
                                    Some("F") => executions += 1,
                                    _ => {}
                                }
                            }
                        }
                        _ => break,
                    }
                } else {
                    match soup::next_packet(&buf[used..]) {
                        Some((k, r, n)) => {
                            if k == soup::SEQUENCED {
                                match ouch::parse_outbound(&buf[used + r.start..used + r.end]) {
                                    Some(ouch::Outbound::Accepted { user_ref, .. }) => on_ack(user_ref as usize, true),
                                    Some(ouch::Outbound::Rejected { user_ref, .. }) => on_ack(user_ref as usize, false),
                                    Some(ouch::Outbound::Executed { .. }) => executions += 1,
                                    _ => {}
                                }
                            }
                            used += n;
                        }
                        None => break,
                    }
                }
            }
            if used > 0 {
                buf.drain(..used);
                last_progress = Instant::now();
            }
        }
        (acked, rejected, executions)
    });
    // send on schedule
    let mut k = 0u32;
    let mut behind = 0u64;
    let mut out = Vec::with_capacity(256);
    let mut rng = Rng(seed);
    for (i, r) in reqs.iter().enumerate() {
        let at = (i as f64 * period_ns) as u64;
        loop {
            let now = t0.elapsed().as_nanos() as u64;
            if now >= at {
                behind = behind.max(now - at);
                break;
            }
            if at - now > 50_000_000 {
                std::thread::sleep(Duration::from_millis(10));
            } else {
                std::hint::spin_loop();
            }
        }
        out.clear();
        match r {
            Req::New { side, price, qty, symbol } => {
                k += 1;
                if fix_mode {
                    let mut m = Message::new("D");
                    m.set(11, format!("c{}", base + k))
                        .set(55, &symbols[*symbol])
                        .set(54, if *side == b'B' { "1" } else { "2" })
                        .set(38, qty)
                        .set(40, "2")
                        .set(44, bourse_gateway::bus::price_str(*price as i64))
                        .set(59, "0");
                    out.extend(fix_msg(&m, seq_out));
                    seq_out += 1;
                } else {
                    let o = ouch::EnterOrder {
                        user_ref: base + k,
                        side: *side,
                        qty: *qty,
                        symbol: symbols[*symbol].clone(),
                        price: *price,
                        tif: b'0',
                        display: b'Y',
                        capacity: b'A',
                        iso: b'N',
                        cross: b'N',
                        cl_ord_id: *b"loadgen       ",
                        post_only: false,
                    };
                    soup::packet(&mut out, soup::UNSEQUENCED, &ouch::enter_order(&o));
                }
            }
            Req::Cancel { target } => {
                if fix_mode {
                    let mut m = Message::new("F");
                    m.set(11, format!("x{}", rng.next())).set(41, format!("c{}", base + target)).set(55, &symbols[0]).set(54, "1");
                    out.extend(fix_msg(&m, seq_out));
                    seq_out += 1;
                } else {
                    soup::packet(&mut out, soup::UNSEQUENCED, &ouch::cancel_order(&ouch::CancelOrder { user_ref: base + *target, qty: 0 }));
                }
            }
        }
        s.write_all(&out).unwrap();
    }
    let (acked, rejected, executions) = handle.join().unwrap();
    let mut v: Vec<u64> = lat.iter().skip(1).map(|a| a.load(Ordering::Relaxed)).filter(|&x| x > 0).collect();
    v.sort_unstable();
    Result_ { lat: v, acked, rejected, executions, behind_ns: behind }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |name: &str, d: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| d.to_string());
    let protocol = get("--protocol", "ouch");
    let addr = get("--addr", if protocol == "fix" { "127.0.0.1:9002" } else { "127.0.0.1:9001" });
    let sessions: usize = get("--sessions", "1").parse().unwrap();
    let rate: f64 = get("--rate", "10000").parse().unwrap();
    let orders: usize = get("--orders", "100000").parse().unwrap();
    let symbols: Vec<String> = get("--symbols", "AAPL").split(',').map(|s| s.to_string()).collect();
    let users: Vec<(String, String)> = get("--users", if protocol == "fix" { "CLIENT1:x" } else { "ALICE:alice" })
        .split(',')
        .map(|u| {
            let (a, b) = u.split_once(':').unwrap_or((u, ""));
            (a.to_string(), b.to_string())
        })
        .collect();
    assert!(users.len() >= sessions, "need one user per session");
    let per = orders / sessions;
    let period = 1e9 / (rate / sessions as f64);
    let t0 = Instant::now() + Duration::from_millis(200);
    // user references must keep increasing across runs on the same sessions:
    // start above anything an earlier run (at most 10 s per million) used
    let base: u32 = get("--ref-base", &(((bourse_gateway::bus::wall_ns() / 1_000_000_000) % 400) as u32 * 10_000_000).to_string()).parse().unwrap();
    let handles: Vec<_> = (0..sessions)
        .map(|i| {
            let (p, a, u, sy) = (protocol.clone(), addr.clone(), users[i].clone(), symbols.clone());
            let reqs = plan(per, 1000 + i as u64, sy.len());
            std::thread::spawn(move || run_session(&p, &a, u, sy, reqs, period, t0, 77 + i as u64, base))
        })
        .collect();
    let rs: Vec<Result_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let mut all: Vec<u64> = rs.iter().flat_map(|r| r.lat.iter().copied()).collect();
    all.sort_unstable();
    let pct = |q: f64| if all.is_empty() { 0.0 } else { all[((q * all.len() as f64) as usize).min(all.len() - 1)] as f64 / 1000.0 };
    let acked: u64 = rs.iter().map(|r| r.acked).sum();
    let rejected: u64 = rs.iter().map(|r| r.rejected).sum();
    let executions: u64 = rs.iter().map(|r| r.executions).sum();
    let behind = rs.iter().map(|r| r.behind_ns).max().unwrap_or(0);
    let json = format!(
        "{{\"protocol\": \"{}\", \"sessions\": {}, \"rate\": {}, \"requests\": {}, \"new_orders_acknowledged\": {}, \"rejected\": {}, \"executions\": {}, \"sender_max_behind_us\": {:.1}, \"latency_us\": {{\"p50\": {:.1}, \"p90\": {:.1}, \"p99\": {:.1}, \"p99_9\": {:.1}, \"p99_99\": {:.1}, \"max\": {:.1}}}}}",
        protocol,
        sessions,
        rate,
        per * sessions,
        acked,
        rejected,
        executions,
        behind as f64 / 1000.0,
        pct(0.5),
        pct(0.9),
        pct(0.99),
        pct(0.999),
        pct(0.9999),
        all.last().copied().unwrap_or(0) as f64 / 1000.0
    );
    println!("{}", json);
    if let Some(i) = args.iter().position(|a| a == "--json") {
        let path = &args[i + 1];
        let mut prev: Vec<serde_json::Value> = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        prev.push(serde_json::from_str(&json).unwrap());
        std::fs::write(path, serde_json::to_string_pretty(&prev).unwrap()).unwrap();
    }
}
