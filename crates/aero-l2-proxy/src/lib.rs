#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
mod capture;
#[cfg(not(target_arch = "wasm32"))]
mod gateway_session;
#[cfg(not(target_arch = "wasm32"))]
mod metrics;
#[cfg(not(target_arch = "wasm32"))]
mod pcapng;

#[cfg(not(target_arch = "wasm32"))]
pub mod auth;
#[cfg(not(target_arch = "wasm32"))]
pub mod protocol;

#[cfg(not(target_arch = "wasm32"))]
mod config;
#[cfg(not(target_arch = "wasm32"))]
mod dns;
#[cfg(not(target_arch = "wasm32"))]
pub mod origin;
#[cfg(not(target_arch = "wasm32"))]
mod overrides;
#[cfg(not(target_arch = "wasm32"))]
mod policy;
#[cfg(not(target_arch = "wasm32"))]
mod server;
#[cfg(not(target_arch = "wasm32"))]
mod session;
#[cfg(not(target_arch = "wasm32"))]
mod session_limits;

#[cfg(not(target_arch = "wasm32"))]
pub use config::{AllowedOrigins, AuthMode, ProxyConfig, SecurityConfig};
#[cfg(not(target_arch = "wasm32"))]
pub use policy::EgressPolicy;
#[cfg(not(target_arch = "wasm32"))]
pub use server::{start_server, ServerHandle};

pub const TUNNEL_SUBPROTOCOL: &str = aero_l2_protocol::L2_TUNNEL_SUBPROTOCOL;
