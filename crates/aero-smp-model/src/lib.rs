//! Deterministic SMP/APIC model + snapshot validation harness.
//!
//! This crate intentionally models only a very small subset of x86 SMP bring-up:
//! per-vCPU run state, local APIC IPI delivery (INIT/SIPI/fixed), a deterministic
//! round-robin scheduler, and a snapshot adapter used by tests.
//!
//! It is **not** the canonical full-system VM wiring layer. For that see
//! `crates/aero-machine` (`aero_machine::Machine`).

pub mod cpu;
pub mod lapic;
pub mod machine;
pub mod scheduler;
pub mod snapshot;

pub use cpu::{CpuState, VcpuRunState, RESET_VECTOR};
pub use lapic::{
    DeliveryMode, DestinationShorthand, Icr, Level, LocalApic, APIC_REG_EOI, APIC_REG_ICR_HIGH,
    APIC_REG_ICR_LOW, APIC_REG_ID, LOCAL_APIC_BASE,
};
pub use machine::{MemoryError, SmpMachine, Trampoline, Vcpu};
pub use scheduler::{DeterministicScheduler, Guest};

