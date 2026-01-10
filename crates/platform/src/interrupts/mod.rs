pub mod local_apic;
pub mod msi;

pub use local_apic::LocalApic;
pub use msi::{ApicSystem, MsiMessage, MsiTrigger};
