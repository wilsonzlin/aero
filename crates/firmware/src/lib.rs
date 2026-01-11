//! Firmware helpers used by multiple parts of the emulator.
//!
//! The crate also contains a host-side validation suite (ACPI + BIOS interrupt
//! surface) that can run without a full CPU emulator.

pub mod acpi;
pub mod bda;
pub mod bios;
pub mod cpu;
pub mod memory;
pub mod rtc;
pub mod smbios;
pub mod video;
