/**
 * Guest physical address space layout (wasm32 web runtime).
 *
 * The web runtime treats guest RAM as a single contiguous region starting at
 * physical address 0. PCI MMIO BARs are auto-assigned into a fixed high-memory
 * aperture so they never overlap guest RAM.
 *
 * NOTE: Keep these constants in sync with the Rust-side guest layout contract:
 * `crates/aero-wasm/src/guest_layout.rs`.
 */

/**
 * Start of the 32-bit PCI MMIO aperture (512MiB window up to 4GiB).
 *
 * Guest RAM is clamped so it never covers `[PCI_MMIO_BASE, 0x1_0000_0000)`.
 */
export const PCI_MMIO_BASE = 0xe000_0000;

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
