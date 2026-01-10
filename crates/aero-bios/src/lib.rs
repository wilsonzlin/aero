//! `aero-bios` is a small, clean-room legacy BIOS implementation intended for the
//! Aero emulator.
//!
//! This crate intentionally keeps the firmware logic in Rust (POST + interrupt
//! services) while also providing a ROM image builder that an emulator can map
//! at `0xF0000` with the reset vector at `0xFFFF_FFF0` (alias `F000:FFF0`).
//!
//! ### Sources (clean-room)
//! - Ralf Brown's Interrupt List (public domain compilation / informational)
//! - OSDev Wiki: BIOS, E820, EDD (Enhanced Disk Drive), VGA text mode
//! - Intel SDM: x86 real-mode and interrupt semantics

pub mod rom;

pub mod types;

pub mod firmware;

pub use firmware::{Bios, BiosConfig, BootDevice};
pub use rom::{build_bios_rom, BIOS_BASE, BIOS_SIZE, RESET_VECTOR_PHYS};
pub use types::{E820Entry, RealModeCpu, FLAG_CF, FLAG_IF};
