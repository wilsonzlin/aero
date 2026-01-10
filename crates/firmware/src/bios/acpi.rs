use crate::acpi::build_acpi_table_set;

use super::{BiosBus, ACPI_TABLE_BASE, ACPI_TABLE_SIZE, EBDA_BASE};

#[derive(Debug, Clone, Copy)]
pub struct AcpiPlacement {
    /// Physical base address reserved for ACPI tables.
    pub tables_base: u64,
    /// Size of the reserved ACPI table area in bytes.
    pub tables_size: usize,
    /// Suggested physical address for the RSDP (must be in EBDA or 0xE0000-0xFFFFF).
    pub rsdp_addr: u64,
}

pub trait AcpiBuilder {
    /// Build ACPI tables and place them into the reserved regions.
    ///
    /// Returns the physical address of the RSDP if one was written.
    fn build(&mut self, bus: &mut dyn BiosBus, placement: AcpiPlacement) -> Option<u64>;
}

/// Default ACPI builder backed by the firmware crate's clean-room ACPI table set.
///
/// This places the core tables (DSDT/FADT/MADT/HPET/RSDT/XSDT) in the reserved
/// table region and writes a copy of the RSDP at `placement.rsdp_addr` so the
/// guest can find it by scanning the EBDA.
#[derive(Debug, Default)]
pub struct FirmwareAcpiBuilder;

impl AcpiBuilder for FirmwareAcpiBuilder {
    fn build(&mut self, bus: &mut dyn BiosBus, placement: AcpiPlacement) -> Option<u64> {
        if placement.rsdp_addr % 16 != 0 {
            eprintln!(
                "BIOS: refusing to write unaligned RSDP at 0x{:x}",
                placement.rsdp_addr
            );
            return None;
        }

        let tables = build_acpi_table_set(placement.tables_base);

        let table_region_end = placement.tables_base + placement.tables_size as u64;
        for (name, addr, len) in [
            ("DSDT", tables.dsdt_address, tables.dsdt.len()),
            ("FADT", tables.fadt_address, tables.fadt.len()),
            ("MADT", tables.madt_address, tables.madt.len()),
            ("HPET", tables.hpet_address, tables.hpet.len()),
            ("RSDT", tables.rsdt_address, tables.rsdt.len()),
            ("XSDT", tables.xsdt_address, tables.xsdt.len()),
        ] {
            let end = addr + len as u64;
            if end > table_region_end {
                eprintln!(
                    "BIOS: ACPI {name} does not fit in reserved region: end=0x{end:x} region_end=0x{table_region_end:x}"
                );
                return None;
            }
        }

        bus.write_physical(tables.dsdt_address, &tables.dsdt);
        bus.write_physical(tables.fadt_address, &tables.fadt);
        bus.write_physical(tables.madt_address, &tables.madt);
        bus.write_physical(tables.hpet_address, &tables.hpet);
        bus.write_physical(tables.rsdt_address, &tables.rsdt);
        bus.write_physical(tables.xsdt_address, &tables.xsdt);
        bus.write_physical(placement.rsdp_addr, &tables.rsdp);

        Some(placement.rsdp_addr)
    }
}

pub fn default_placement() -> AcpiPlacement {
    AcpiPlacement {
        tables_base: ACPI_TABLE_BASE,
        tables_size: ACPI_TABLE_SIZE,
        rsdp_addr: EBDA_BASE + 0x100,
    }
}
