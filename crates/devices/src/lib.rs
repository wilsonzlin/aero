#![forbid(unsafe_code)]

pub mod a20_gate;
pub mod apic;
pub mod pci;
pub mod pic8259;

pub mod io;
pub mod storage;

pub use pic8259::DualPic8259;
pub mod clock;
pub mod hpet;
pub mod ioapic;
