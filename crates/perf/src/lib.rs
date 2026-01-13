//! Performance telemetry utilities (compatibility shim).
//!
//! The canonical perf + telemetry implementation lives in `crates/aero-perf`.
//! This crate remains as a tiny wrapper to preserve historical paths/examples:
//! `perf::jit` and `perf::telemetry`.

pub use aero_perf::jit;
pub use aero_perf::telemetry;
