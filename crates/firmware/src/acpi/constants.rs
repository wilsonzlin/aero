//! Constants used by ACPI table generation.

/// Start of the typical PCI MMIO window for PCI device BAR allocations on PC-compatible systems
/// (3GiB).
///
/// The firmware places ACPI reclaimable and NVS windows below the first reserved MMIO region. On
/// the PC platform that means clamping below both this address and the PCIe ECAM window
/// (`aero_pc_constants::PCIE_ECAM_BASE`).
pub const DEFAULT_PCI_MMIO_START: u64 = 0xC000_0000;

/// LAPIC MMIO base as expected by Windows on PC-compatible platforms.
pub const LOCAL_APIC_BASE: u32 = 0xFEE0_0000;

/// IOAPIC MMIO base as expected by Windows on PC-compatible platforms.
pub const IO_APIC_BASE: u32 = 0xFEC0_0000;

/// HPET MMIO base address.
pub const HPET_BASE: u64 = 0xFED0_0000;

/// Default EBDA base used by the firmware BIOS implementation.
///
/// The OS searches the first KiB of the EBDA (and the 0xE0000-0xFFFFF scan
/// region) for the RSDP on 16-byte boundaries.
pub const DEFAULT_EBDA_BASE: u64 = 0x0009_F000;

/// Required alignment for ACPI tables.
pub const ACPI_TABLE_ALIGNMENT: u64 = 16;

/// Size reserved for ACPI reclaimable tables.
///
/// This is a window reserved in the E820 map as type 3 (ACPI reclaimable). The
/// builder ensures that the generated tables fit within this window.
pub const DEFAULT_ACPI_RECLAIM_WINDOW_SIZE: u64 = 0x10_000; // 64KiB

/// Size reserved for ACPI NVS structures (e.g. FACS).
pub const DEFAULT_ACPI_NVS_WINDOW_SIZE: u64 = 0x1000; // 4KiB
