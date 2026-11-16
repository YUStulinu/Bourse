//! Pre-trade risk checks in the gateway, with limits per account. The
//! limits file is written by the risk and clearing service (Kotlin) and
//! reloaded when it changes.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Limits {
    /// largest quantity of one order
    pub max_order_qty: u32,
    /// largest value of one order, in dollars
    pub max_order_notional: f64,
    /// a limit price may be at most this many percent away from the last trade
    pub collar_pct: f64,
    /// largest total value of the account's open orders, in dollars
    pub max_open_notional: f64,
    /// messages per second per session
    pub max_msgs_per_sec: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { max_order_qty: 100_000, max_order_notional: 5_000_000.0, collar_pct: 10.0, max_open_notional: 50_000_000.0, max_msgs_per_sec: 1_000_000 }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LimitsFile {
    #[serde(default)]
    pub default: Limits,
    #[serde(default)]
    pub accounts: HashMap<String, Limits>,
    /// accounts blocked by the risk service
    #[serde(default)]
    pub blocked: Vec<u32>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Breach {
    MaxQuantity,
    OrderNotional,
    Collar,
    OpenNotional,
    Rate,
    Blocked,
}

/// Exposure of one account: the value of its open orders (1/10000 dollar × shares).
#[derive(Default, Clone, Copy)]
struct Exposure {
    open_value: i128,
}

#[derive(Default)]
pub struct Risk {
    pub file: LimitsFile,
    exposure: HashMap<u32, Exposure>,
    last_trade: HashMap<u32, i64>,
}

impl Risk {
    pub fn limits(&self, account: u32) -> &Limits {
        self.file.accounts.get(&account.to_string()).unwrap_or(&self.file.default)
    }

    /// Check a new order (price in 1/10000 dollar; None for a market order).
    pub fn check(&self, account: u32, symbol: u32, price: Option<i64>, qty: u32) -> Result<(), Breach> {
        if self.file.blocked.contains(&account) {
            return Err(Breach::Blocked);
        }
        let l = self.limits(account);
        if qty > l.max_order_qty {
            return Err(Breach::MaxQuantity);
        }
        let reference = price.or_else(|| self.last_trade.get(&symbol).copied());
        if let Some(p) = reference {
            let value = p as f64 * qty as f64 / 1e4;
            if value > l.max_order_notional {
                return Err(Breach::OrderNotional);
            }
            let open = self.exposure.get(&account).map(|e| e.open_value).unwrap_or(0) as f64 / 1e4;
            if open + value > l.max_open_notional {
                return Err(Breach::OpenNotional);
            }
        }
        if let (Some(p), Some(&last)) = (price, self.last_trade.get(&symbol)) {
            if ((p - last).abs() as f64) > last as f64 * l.collar_pct / 100.0 {
                return Err(Breach::Collar);
            }
        }
        Ok(())
    }

    /// Open quantity of an account changed by `shares` (signed) at `price`.
    pub fn open(&mut self, account: u32, price: i64, shares: i64) {
        self.exposure.entry(account).or_default().open_value += price as i128 * shares as i128;
    }

    pub fn trade(&mut self, symbol: u32, price: i64) {
        self.last_trade.insert(symbol, price);
    }

    pub fn open_notional(&self, account: u32) -> f64 {
        self.exposure.get(&account).map(|e| e.open_value).unwrap_or(0) as f64 / 1e4
    }
}

/// A token bucket: `rate` messages per second, bursts up to one second's worth.
pub struct RateLimit {
    tokens: f64,
    last_ns: u64,
}

impl RateLimit {
    pub fn new() -> RateLimit {
        RateLimit { tokens: -1.0, last_ns: 0 }
    }
    pub fn allow(&mut self, now_ns: u64, rate: u32) -> bool {
        let rate = rate as f64;
        if self.tokens < 0.0 {
            self.tokens = rate;
            self.last_ns = now_ns;
        }
        self.tokens = (self.tokens + (now_ns.saturating_sub(self.last_ns)) as f64 * rate / 1e9).min(rate);
        self.last_ns = now_ns;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl Default for RateLimit {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks() {
        let mut r = Risk::default();
        r.file.default = Limits { max_order_qty: 1000, max_order_notional: 50_000.0, collar_pct: 5.0, max_open_notional: 100_000.0, max_msgs_per_sec: 10 };
        assert_eq!(r.check(1, 0, Some(100_0000), 2000), Err(Breach::MaxQuantity));
        assert_eq!(r.check(1, 0, Some(100_0000), 600), Err(Breach::OrderNotional));
        assert_eq!(r.check(1, 0, Some(100_0000), 400), Ok(()));
        r.open(1, 100_0000, 900); // $90,000 open
        assert_eq!(r.check(1, 0, Some(100_0000), 200), Err(Breach::OpenNotional));
        assert_eq!(r.check(2, 0, Some(100_0000), 200), Ok(()));
        r.trade(0, 100_0000);
        assert_eq!(r.check(2, 0, Some(106_0000), 10), Err(Breach::Collar));
        assert_eq!(r.check(2, 0, Some(104_0000), 10), Ok(()));
        r.file.blocked.push(2);
        assert_eq!(r.check(2, 0, Some(104_0000), 10), Err(Breach::Blocked));
        let f: LimitsFile = serde_json::from_str(r#"{"default": {"max_order_qty": 5, "max_order_notional": 1, "collar_pct": 1, "max_open_notional": 1, "max_msgs_per_sec": 1}, "accounts": {"7": {"max_order_qty": 9, "max_order_notional": 1, "collar_pct": 1, "max_open_notional": 1, "max_msgs_per_sec": 1}}}"#).unwrap();
        r.file = f;
        assert_eq!(r.limits(7).max_order_qty, 9);
        assert_eq!(r.limits(8).max_order_qty, 5);
    }

    #[test]
    fn rate() {
        let mut b = RateLimit::new();
        let n = (0..20).filter(|_| b.allow(0, 10)).count();
        assert_eq!(n, 10);
        assert!(b.allow(100_000_000, 10)); // 0.1 s later: one more token
        assert!(!b.allow(100_000_000, 10));
    }
}
