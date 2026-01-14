//! Legacy re-export of the deterministic SMP/APIC model.
//!
//! The deterministic SMP/APIC + snapshot harness lives in `crates/aero-smp`. This module is
//! retained for backwards compatibility so existing `emulator::smp::*` imports keep working.

pub use aero_smp::*;
