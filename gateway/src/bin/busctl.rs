//! busctl: look at or stop a running exchange.
//!
//!   busctl [--bus run/bus.dat] status|stop

use bourse_gateway::bus::{wall_ns, Bus};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bus_path = args.iter().position(|a| a == "--bus").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "run/bus.dat".into());
    let cmd = args.last().cloned().unwrap_or_default();
    let bus = match Bus::open(&bus_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("busctl: {}: {}", bus_path, e);
            std::process::exit(1)
        }
    };
    match cmd.as_str() {
        "stop" => {
            bus.request_stop();
            for _ in 0..100 {
                if bus.state() == 3 {
                    println!("engine stopped");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            eprintln!("busctl: the engine did not stop");
            std::process::exit(1);
        }
        _ => {
            let age = wall_ns().saturating_sub(bus.heartbeat()) as f64 / 1e9;
            println!("state {} (0 starting, 1 running, 2 stopping, 3 stopped), heartbeat {:.2} s ago", bus.state(), age);
            println!("events in the log: {}", bus.log_cursor());
            for (pid, cursor) in bus.consumers() {
                println!("reader pid {}: at {}", pid, cursor);
            }
            for s in bus.symbols() {
                println!("symbol {} lot {} tick {}", s.name, s.round_lot, s.tick);
            }
        }
    }
}
