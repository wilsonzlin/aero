/**
 * Guest physical address space layout (wasm32 web runtime).
 *
 * The web runtime uses the PC/Q35 E820-style layout when the configured guest RAM exceeds the PCIe
 * ECAM base:
 *
 * - Low RAM:  `[0x0000_0000 .. 0xB000_0000)`
 * - Hole:     `[0xB000_0000 .. 0x1_0000_0000)` (ECAM + PCI/MMIO)
 * - High RAM: `[0x1_0000_0000 .. 0x1_0000_0000 + (ram_bytes - 0xB000_0000))`
 *
 * Note: the backing guest RAM buffer in wasm linear memory is still contiguous (length =
 * `guest_size`); wasm device bridges translate guest physical addresses back into that buffer.
 *
 * NOTE: Keep these constants in sync with the Rust-side guest layout contract:
 * `crates/aero-wasm/src/guest_layout.rs` and `crates/aero-wasm/src/guest_phys.rs`.
 */

/**
 * Start of the guest-physical PCI MMIO BAR allocation window used by the web runtime.
 *
 * The canonical PC/Q35 platform reserves a larger below-4 GiB PCI/MMIO hole
 * (`0xC000_0000..0x1_0000_0000`) and places PCIe ECAM at `0xB000_0000..0xC000_0000`.
 *
 * For the web runtime we currently allocate PCI MMIO BARs out of the high 512 MiB sub-window
 * `[PCI_MMIO_BASE, 0x1_0000_0000)`, and clamp the *backing* guest RAM size below `PCI_MMIO_BASE` so
 * BARs never overlap the contiguous wasm linear-memory RAM buffer.
 *
 * Guest RAM is clamped so it never covers `[PCI_MMIO_BASE, 0x1_0000_0000)`.
 */
export const PCI_MMIO_BASE = 0xe000_0000;

/**
 * Guest-physical base address of the shared VRAM aperture (BAR1 backing) used by the web runtime.
 *
 * This lives inside the PCI MMIO BAR allocation window starting at {@link PCI_MMIO_BASE}. The
 * browser runtime uses a dedicated `SharedArrayBuffer` for VRAM so the I/O worker (MMIO writes)
 * and GPU worker (scanout readback) can share the same bytes without embedding VRAM in WASM
 * linear memory.
 *
 * NOTE: Keep this in sync with any Rust-side BAR / guest layout constants that assume a fixed
 * BAR1/VRAM placement.
 */
export const VRAM_BASE_PADDR = PCI_MMIO_BASE;

/**
 * PCI MMIO base expressed in MiB (3.5GiB).
 *
 * This is useful for UI/config validation.
 */
export const PCI_MMIO_BASE_MIB = PCI_MMIO_BASE / (1024 * 1024);

/**
 * PCIe ECAM (MMCONFIG) base address.
 *
 * Exposed to guests via the ACPI MCFG table. The I/O worker must route MMIO
 * accesses in this window to PCI config space (see `web/src/io/bus/pci.ts`).
 *
 * NOTE: Keep these constants in sync with the Rust-side contract:
 * `aero_pc_constants::PCIE_ECAM_BASE`.
 */
export const PCIE_ECAM_BASE = 0xb000_0000n;

/**
 * PCIe ECAM (MMCONFIG) size (256MiB = 1MiB per bus for buses 0..255).
 *
 * NOTE: Keep these constants in sync with the Rust-side contract:
 * `aero_pc_constants::PCIE_ECAM_SIZE`.
 */
export const PCIE_ECAM_SIZE = 0x1000_0000n;

// Convenience number aliases (safe: values are < 2^32).
export const PCIE_ECAM_BASE_U32 = 0xb000_0000;
export const PCIE_ECAM_SIZE_U32 = 0x1000_0000;

/**
 * End of the low guest RAM region for the PC/Q35-style E820 map used by the wasm32 web runtime.
 *
 * When the configured guest RAM exceeds this boundary, the "extra" bytes are remapped above 4 GiB
 * and the region `[LOW_RAM_END, HIGH_RAM_START)` becomes a hole (ECAM + PCI/MMIO).
 *
 * NOTE: keep this in sync with the Rust-side mapping helper in `crates/aero-wasm/src/guest_phys.rs`.
 */
export const LOW_RAM_END = PCIE_ECAM_BASE_U32;

/**
 * Start of the high guest RAM remap region (4 GiB).
 *
 * NOTE: keep this in sync with the Rust-side mapping helper in `crates/aero-wasm/src/guest_phys.rs`.
 */
export const HIGH_RAM_START = 0x1_0000_0000;
