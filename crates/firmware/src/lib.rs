//! Firmware helpers used by multiple parts of the emulator.
//!
//! The crate also contains a host-side validation suite (ACPI + BIOS interrupt
//! surface) that can run without a full CPU emulator.

// `crates/machine` is deprecated in favor of `crates/aero-machine`, but this
// crate still contains legacy firmware validation helpers that depend on the
// older toy real-mode model. Keep the build output clean until the firmware
// validation stack is migrated.
#![allow(deprecated)]

pub mod acpi;
pub mod bda;
pub mod bios;
pub mod bus;
pub mod cpu;
pub mod legacy_bios;
pub mod memory;
pub mod realmode;
pub mod rtc;
pub mod smbios;
pub mod video;
