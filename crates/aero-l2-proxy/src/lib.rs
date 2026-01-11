#![forbid(unsafe_code)]

mod config;
mod dns;
mod overrides;
mod policy;
mod server;
mod session;

pub use config::ProxyConfig;
pub use policy::EgressPolicy;
pub use server::{start_server, ServerHandle};

pub const TUNNEL_SUBPROTOCOL: &str = "aero-l2-tunnel-v1";
