#![forbid(unsafe_code)]

pub mod a20_gate;
pub mod apic;
pub mod pci;
pub mod pic8259;

pub use pic8259::DualPic8259;
