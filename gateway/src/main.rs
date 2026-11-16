//! bourse-gateway: OUCH 5.0 and FIX 4.4 order entry, ITCH 5.0 market data.
//!
//!   bourse-gateway --bus run/bus.dat --accounts config/accounts.json
//!                  [--ouch 127.0.0.1:9001] [--fix 127.0.0.1:9002] [--itch 127.0.0.1:9003]
//!                  [--limits run/limits.json] [--idle] [--stats OUT.json]
//!
//! Start it after the engine. A gateway with --itch should start before
//! trading begins: the feed only knows the orders it has seen.

use bourse_gateway::bus::{self, Bus};
use bourse_gateway::server::{load_accounts, Accounts, Config, Gateway};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let bus_path = get("--bus").unwrap_or_else(|| "run/bus.dat".into());
    let accounts = match get("--accounts") {
        Some(p) => load_accounts(&p).unwrap_or_else(|e| {
            eprintln!("gateway: cannot read accounts {}: {}", p, e);
            std::process::exit(2)
        }),
        None => Accounts::default(),
    };
    let addr = |name: &str| get(name).map(|a| a.parse().unwrap_or_else(|_| panic!("bad address for {}", name)));
    let cfg = Config {
        bus: bus_path.clone(),
        ouch: addr("--ouch"),
        fix: addr("--fix"),
        itch: addr("--itch"),
        accounts,
        limits_path: get("--limits"),
        spin: !args.iter().any(|a| a == "--idle"),
        comp_id: get("--comp-id").unwrap_or_else(|| "BOURSE".into()),
    };
    // wait for the engine to create the bus
    let start = Instant::now();
    let bus = loop {
        match Bus::open(&bus_path) {
            Ok(b) if b.state() == bus::BUS_RUNNING => break b,
            _ if start.elapsed() > Duration::from_secs(30) => {
                eprintln!("gateway: no running engine at {}", bus_path);
                std::process::exit(1)
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let (o, f, i) = (cfg.ouch, cfg.fix, cfg.itch);
    let mut g = Gateway::new(&bus, cfg).unwrap_or_else(|e| {
        eprintln!("gateway: {}", e);
        std::process::exit(1)
    });
    println!("bourse-gateway: ouch {:?}, fix {:?}, itch {:?}, ring {:?}", o, f, i, g.ring());
    let stop = Arc::new(AtomicBool::new(false));
    if let Err(e) = g.run(stop) {
        eprintln!("gateway: {}", e);
        std::process::exit(1);
    }
    println!("bourse-gateway: {} commands, {} events, {} messages out", g.stats.commands, g.stats.events, g.stats.messages_out);
    if let Some(p) = g.stats.percentiles_us() {
        let json = format!(
            "{{\"acknowledged\": {}, \"internal_ack_latency_us\": {{\"p50\": {:.1}, \"p90\": {:.1}, \"p99\": {:.1}, \"p99_9\": {:.1}, \"p99_99\": {:.1}, \"max\": {:.1}}}}}",
            g.stats.ack_ns.len(),
            p[0], p[1], p[2], p[3], p[4], p[5]
        );
        println!("{}", json);
        if let Some(path) = get("--stats") {
            let _ = std::fs::write(path, json);
        }
    }
}
