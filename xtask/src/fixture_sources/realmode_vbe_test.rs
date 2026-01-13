//! Tiny 16-bit real-mode VBE exerciser.
//!
//! `cargo xtask fixtures` is the **single source of truth** for
//! `tests/fixtures/realmode_vbe_test.bin`.
//!
//! The original GNU `as` source (`tests/fixtures/realmode_vbe_test.s`) is kept
//! in-repo as documentation, but CI/agents are not expected to have an assembler
//! installed.
//!
//! Expected semantics (executed as a flat `.code16` binary with `CS:IP` starting
//! at offset 0):
//!   - Zero `DS/ES/SS` and set `SP=0x7C00`
//!   - Prepare a VBE2 controller info request at `0000:0500` by writing the
//!     signature `"VBE2"` (little-endian words)
//!   - Call `int 10h` with `AX=4F00h` ("VBE Controller Information")
//!   - If `AX != 004Fh` (failure), hang
//!   - Otherwise call `int 10h` with `AX=4F02h`, `CX=4118h` (set mode 0x118 with
//!     LFB bit) and then hang

/// Flat 16-bit machine code bytes.
pub const BIN: [u8; 44] = [
    0x31, 0xC0, 0x8E, 0xD8, 0x8E, 0xC0, 0x8E, 0xD0, 0xBC, 0x00, 0x7C, 0xBF, 0x00, 0x05, 0xC7, 0x05,
    0x42, 0x56, 0xC7, 0x45, 0x02, 0x32, 0x45, 0xB8, 0x00, 0x4F, 0xCD, 0x10, 0x83, 0xF8, 0x4F, 0x75,
    0x08, 0xB8, 0x02, 0x4F, 0xB9, 0x18, 0x41, 0xCD, 0x10, 0xF4, 0xEB, 0xFD,
];
