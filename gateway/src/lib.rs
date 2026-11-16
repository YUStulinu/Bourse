//! The protocol gateways of Bourse: they turn OUCH 5.0 and FIX 4.4 order
//! entry into engine commands on the shared-memory bus, turn the engine's
//! event log back into protocol messages, and publish the public market data
//! as ITCH 5.0 over SoupBinTCP.

pub mod bus;
pub mod fix;
pub mod itch;
pub mod ouch;
pub mod risk;
pub mod server;
pub mod soup;
