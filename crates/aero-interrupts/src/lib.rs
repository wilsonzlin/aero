#![forbid(unsafe_code)]

pub mod apic;
pub mod clock;
pub mod pic8259;

pub use pic8259::DualPic8259;

