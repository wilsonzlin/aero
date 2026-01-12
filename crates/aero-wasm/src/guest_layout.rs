//! Shared constants for the wasm32 guest RAM layout contract.
//!
//! Keep this module small and dependency-free: it is used by both the
//! `guest_ram_layout` API and the wasm32 runtime allocator enforcement.

#![cfg(target_arch = "wasm32")]

/// WebAssembly linear memory page size (wasm32 / wasm64).
pub const WASM_PAGE_BYTES: u64 = 64 * 1024;

/// Max pages addressable by wasm32 (2^32 bytes).
pub const MAX_WASM32_PAGES: u64 = 65536;

/// Bytes reserved at the bottom of the linear memory for the Rust/WASM runtime.
///
/// Guest physical address 0 maps to `guest_base = align_up(RUNTIME_RESERVED_BYTES, 64KiB)`.
///
/// NOTE: Keep this in sync with `web/src/runtime/shared_layout.ts` (`RUNTIME_RESERVED_BYTES`).
pub const RUNTIME_RESERVED_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB

/// Start of the guest-physical PCI MMIO BAR allocation window (32-bit) used by the web runtime.
///
/// The canonical PC/Q35 platform reserves a larger PCI/MMIO hole below 4 GiB
/// (`0xC000_0000..0x1_0000_0000`). In the web runtime we allocate device BARs out of the high
/// sub-window starting at `PCI_MMIO_BASE`, and clamp the *backing* guest RAM size to
/// `<= PCI_MMIO_BASE` so BARs never overlap the contiguous wasm linear-memory guest RAM buffer.
///
/// NOTE: Keep this in sync with `web/src/arch/guest_phys.ts` (`PCI_MMIO_BASE`).
pub const PCI_MMIO_BASE: u64 = 0xE000_0000;

/// Guest-physical base of the PCI MMIO BAR allocation window.
///
/// NOTE: Keep this in sync with `web/src/runtime/shared_layout.ts` (`GUEST_PCI_MMIO_BASE`).
pub const GUEST_PCI_MMIO_BASE: u64 = PCI_MMIO_BASE;

pub const fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return value;
    }
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value + (alignment - rem)
    }
}
