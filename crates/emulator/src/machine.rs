//! Canonical Aero "machine" wiring re-exports.
//!
//! The canonical VM wiring lives in the `aero-machine` crate as [`aero_machine::Machine`].
//! `crates/emulator` is primarily the device + I/O stack; this module exists as a migration
//! affordance so code that already depends on `emulator` can reach the canonical machine without
//! guessing which crate to import.
//!
//! Prefer constructing and driving the VM via [`Machine`] / [`MachineConfig`] here (or directly via
//! `aero_machine`), rather than using the deterministic SMP/APIC model types in `aero_smp`
//! (which are re-exported as `emulator::smp` for backwards compatibility).

pub use aero_machine::{Machine, MachineConfig, MachineError, RunExit};
pub use aero_machine::{PcMachine, PcMachineConfig};
