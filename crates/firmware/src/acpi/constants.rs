//! Constants used by ACPI table generation.

/// Start of the typical PCI MMIO hole on PC-compatible systems (3GiB).
///
/// The firmware places ACPI reclaimable and NVS windows below this address so
/// that they are addressable by both 32-bit and 64-bit guests.
pub const DEFAULT_PCI_MMIO_START: u64 = 0xC000_0000;

/// LAPIC MMIO base as expected by Windows on PC-compatible platforms.
pub const LOCAL_APIC_BASE: u32 = 0xFEE0_0000;

/// IOAPIC MMIO base as expected by Windows on PC-compatible platforms.
pub const IO_APIC_BASE: u32 = 0xFEC0_0000;

/// HPET MMIO base address.
pub const HPET_BASE: u64 = 0xFED0_0000;

/// Default EBDA base used by the BIOS docs.
///
/// The OS searches the first KiB of the EBDA (and the 0xE0000-0xFFFFF scan
/// region) for the RSDP on 16-byte boundaries.
pub const DEFAULT_EBDA_BASE: u64 = 0x0009_FC00;

/// Required alignment for ACPI tables.
pub const ACPI_TABLE_ALIGNMENT: u64 = 16;

/// Size reserved for ACPI reclaimable tables.
///
/// This is a window reserved in the E820 map as type 3 (ACPI reclaimable). The
/// builder ensures that the generated tables fit within this window.
pub const DEFAULT_ACPI_RECLAIM_WINDOW_SIZE: u64 = 0x20_000; // 128KiB

/// Size reserved for ACPI NVS structures (e.g. FACS).
pub const DEFAULT_ACPI_NVS_WINDOW_SIZE: u64 = 0x20_000; // 128KiB
