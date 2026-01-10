//! SMP (multi-vCPU) support: per-vCPU state, local APIC IPI delivery, and scheduling.

pub mod cpu;
pub mod lapic;
pub mod machine;
pub mod scheduler;

pub use cpu::{CpuState, VcpuRunState, RESET_VECTOR};
pub use lapic::{
    DeliveryMode, DestinationShorthand, Icr, Level, LocalApic, APIC_REG_EOI, APIC_REG_ICR_HIGH,
    APIC_REG_ICR_LOW, APIC_REG_ID, LOCAL_APIC_BASE,
};
pub use machine::{Machine, MemoryError, Trampoline, Vcpu};
pub use scheduler::{DeterministicScheduler, Guest};
