//! Deprecated shim around the canonical `aero-net-trace` crate.
//!
//! Historically, the Rust reference implementation of network tracing lived under
//! `crates/emulator/src/io/net/trace/`. It has been moved into `crates/aero-net-trace` so it can be
//! used by networking tooling (and tests) without pulling in the emulator crate.

pub use aero_net_trace::*;
