//! tcprtt: the round-trip time of plain TCP over loopback on this machine
//! (an echo server and a client in one process), the floor under any
//! order-entry latency measured through a gateway.
//!
//!   tcprtt [--count 50000] [--size 49]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |n: &str, d: usize| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d);
    let count = get("--count", 50_000);
    let size = get("--size", 49); // an OUCH Enter Order in a SoupBinTCP packet
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.set_nodelay(true).unwrap();
        let mut b = vec![0u8; 65536];
        loop {
            match s.read(&mut b) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if s.write_all(&b[..n]).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let mut c = TcpStream::connect(addr).unwrap();
    c.set_nodelay(true).unwrap();
    let msg = vec![7u8; size];
    let mut b = vec![0u8; 65536];
    let mut v = Vec::with_capacity(count);
    for i in 0..count + 2000 {
        let t = Instant::now();
        c.write_all(&msg).unwrap();
        let mut got = 0;
        while got < size {
            got += c.read(&mut b[got..]).unwrap();
        }
        if i >= 2000 {
            v.push(t.elapsed().as_nanos() as u64);
        }
    }
    v.sort_unstable();
    let p = |q: f64| v[((q * v.len() as f64) as usize).min(v.len() - 1)] as f64 / 1000.0;
    println!(
        "{{\"tcp_loopback_rtt_us\": {{\"p50\": {:.1}, \"p90\": {:.1}, \"p99\": {:.1}, \"p99_9\": {:.1}, \"max\": {:.1}}}, \"bytes\": {}}}",
        p(0.5),
        p(0.9),
        p(0.99),
        p(0.999),
        *v.last().unwrap() as f64 / 1000.0,
        size
    );
}
