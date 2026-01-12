#![forbid(unsafe_code)]

pub mod clock;

pub mod a20_gate;
pub mod acpi_pm;
pub mod apic;
pub mod dma;
pub mod i8042;
pub mod pci;
pub mod pic8259;
pub mod pit8254;
pub mod serial;
pub mod usb;

pub mod io;
pub mod storage;

pub mod hpet;
pub mod ioapic;
pub mod irq;
pub mod reset_ctrl;
pub mod rtc_cmos;

pub use pic8259::DualPic8259;
pub use pit8254::Pit8254;
pub use rtc_cmos::{RtcCmos, RtcDateTime};
