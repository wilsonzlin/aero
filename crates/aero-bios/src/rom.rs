//! BIOS ROM image construction.
//!
//! The ROM image is not an implementation of the firmware logic. In Aero, the
//! ROM is primarily used as the "system BIOS" mapping for guests to discover.
//! The actual POST and interrupt services are implemented in Rust and invoked
//! by the emulator when the guest executes `INT xx`.
//!
//! This file still ensures the ROM contains a valid reset vector at `F000:FFF0`
//! (physical `0xFFFF_FFF0`).

pub use crate::types::{BIOS_BASE, BIOS_SIZE, RESET_VECTOR_PHYS};

/// Build a 64 KiB BIOS ROM image.
///
/// Layout:
/// - Most bytes are `0xFF` (common erased-ROM value).
/// - At offset `0xFFF0` (reset vector), we place a far jump to `F000:E000`.
/// - At `F000:E000`, we place an infinite `hlt` loop as a safe fallback.
///
/// Emulators are expected to call into the Rust firmware (`Bios::post`) during
/// reset rather than executing this stub.
pub fn build_bios_rom() -> [u8; BIOS_SIZE] {
    let mut rom = [0xFFu8; BIOS_SIZE];

    // Reset vector in the top 16 bytes of the ROM: physical 0xFFFF_FFF0,
    // which maps to F000:FFF0.
    //
    // Encoding: JMP FAR ptr16:16  => EA iw (offset) iw (segment)
    // Target: F000:E000.
    let reset_off = 0xFFF0usize;
    rom[reset_off + 0] = 0xEA;
    rom[reset_off + 1] = 0x00; // offset low
    rom[reset_off + 2] = 0xE0; // offset high (0xE000)
    rom[reset_off + 3] = 0x00; // segment low
    rom[reset_off + 4] = 0xF0; // segment high (0xF000)

    // Fallback stub at F000:E000: "cli; hlt; jmp -2" (halts forever).
    // 0xFA = CLI, 0xF4 = HLT, 0xEB 0xFE = JMP $-0x0 (infinite loop).
    let stub_off = 0xE000usize;
    rom[stub_off + 0] = 0xFA;
    rom[stub_off + 1] = 0xF4;
    rom[stub_off + 2] = 0xEB;
    rom[stub_off + 3] = 0xFE;

    // Some tooling expects a `0x55AA` signature at the end of the ROM segment.
    // This isn't strictly required for a system BIOS, but it's harmless and
    // makes the blob easy to identify.
    rom[BIOS_SIZE - 2] = 0x55;
    rom[BIOS_SIZE - 1] = 0xAA;

    rom
}
