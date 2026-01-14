#![forbid(unsafe_code)]

pub mod clock;

pub mod a20_gate;
pub mod acpi_pm;
pub mod apic;
pub mod debugcon;
pub mod dma;
pub mod i8042;
pub mod pci;
pub mod pic8259;
pub mod pit8254;
pub mod serial;
pub mod usb;

// Legacy virtio stack; new code should use the canonical `aero_virtio` crate.
//
// NOTE: We only deprecate this module for non-test builds to keep the
// auto-generated unit test harness warning-free (it references test functions
// by full path, e.g. `aero_devices::io::virtio::...`).
#[cfg_attr(not(test), deprecated(note = "use aero_virtio crate instead"))]
#[allow(deprecated)]
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
