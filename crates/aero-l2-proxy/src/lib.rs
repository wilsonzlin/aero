#![forbid(unsafe_code)]

mod capture;
mod metrics;
mod pcapng;

pub mod auth;
pub mod protocol;

mod config;
mod dns;
pub mod origin;
mod overrides;
mod policy;
mod server;
mod session;
mod session_limits;

pub use config::{AllowedOrigins, AuthMode, ProxyConfig, SecurityConfig};
pub use policy::EgressPolicy;
pub use server::{start_server, ServerHandle};

pub const TUNNEL_SUBPROTOCOL: &str = aero_l2_protocol::L2_TUNNEL_SUBPROTOCOL;
