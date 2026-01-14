//! ACPI table generation for Aero firmware.
//!
//! This crate focuses on generating a minimal, self-consistent set of ACPI
//! tables that Windows 7 will accept:
//! - RSDP (ACPI 2.0+)
//! - RSDT + XSDT
//! - FADT (FACP) with DSDT pointer and basic PM blocks
//! - MADT (APIC) with LAPICs, IOAPIC, ISA overrides (timer + SCI)
//! - HPET table
//! - Minimal DSDT AML exposing PCI0 + HPET + CPU objects

mod tables;

pub use tables::{
    AcpiConfig, AcpiPlacement, AcpiTables, PhysicalMemory, DEFAULT_ACPI_ALIGNMENT,
    DEFAULT_ACPI_NVS_SIZE, FADT_FLAG_FIX_RTC, FADT_FLAG_PWR_BUTTON, FADT_FLAG_RESET_REG_SUP,
    FADT_FLAG_SLP_BUTTON,
};
