#![forbid(unsafe_code)]

mod capture;
mod metrics;
mod pcapng;

mod config;
mod dns;
mod overrides;
mod policy;
mod server;
mod session;

pub use config::{AllowedOrigins, ProxyConfig, SecurityConfig};
pub use policy::EgressPolicy;
pub use server::{start_server, ServerHandle};

pub const TUNNEL_SUBPROTOCOL: &str = "aero-l2-tunnel-v1";
