//! Performance telemetry utilities (compatibility shim).
//!
//! PF-001: The canonical perf + telemetry implementation lives in `crates/aero-perf`
//! (`aero_perf::jit`, `aero_perf::telemetry`). This crate remains as a thin wrapper
//! to preserve historical import paths/examples (`perf::jit`, `perf::telemetry`).
pub use aero_perf::jit;
pub use aero_perf::telemetry;
