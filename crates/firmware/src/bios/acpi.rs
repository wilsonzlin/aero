use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables, PhysicalMemory as AcpiPhysicalMemory};

use super::{BiosBus, PCIE_ECAM_BASE, PCIE_ECAM_END_BUS, PCIE_ECAM_SEGMENT, PCIE_ECAM_START_BUS};

#[derive(Debug, Clone, Copy)]
pub struct AcpiInfo {
    pub rsdp_addr: u64,
    /// Reclaimable table blob window (E820 type 3).
    pub reclaimable: (u64, u64),
    /// ACPI NVS window (E820 type 4).
    pub nvs: (u64, u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BiosAcpiError {
    TableAddressOverflow {
        table: &'static str,
        addr: u64,
        len: u64,
    },
    TableOutOfBounds {
        table: &'static str,
        end: u64,
        memory_size_bytes: u64,
    },
    NvsAddressOverflow {
        base: u64,
        size: u64,
    },
    NvsOutOfBounds {
        end: u64,
        memory_size_bytes: u64,
    },
}

impl core::fmt::Display for BiosAcpiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            BiosAcpiError::TableAddressOverflow { table, addr, len } => write!(
                f,
                "ACPI {table} address overflow (addr=0x{addr:x} len=0x{len:x})"
            ),
            BiosAcpiError::TableOutOfBounds {
                table,
                end,
                memory_size_bytes,
            } => write!(
                f,
                "ACPI {table} out of bounds (end=0x{end:x} mem=0x{memory_size_bytes:x})"
            ),
            BiosAcpiError::NvsAddressOverflow { base, size } => {
                write!(
                    f,
                    "ACPI NVS address overflow (base=0x{base:x} size=0x{size:x})"
                )
            }
            BiosAcpiError::NvsOutOfBounds {
                end,
                memory_size_bytes,
            } => write!(
                f,
                "ACPI NVS out of bounds (end=0x{end:x} mem=0x{memory_size_bytes:x})"
            ),
        }
    }
}

impl std::error::Error for BiosAcpiError {}

pub trait AcpiBuilder: Send {
    fn build_and_write(
        &mut self,
        bus: &mut dyn BiosBus,
        memory_size_bytes: u64,
        cpu_count: u8,
        pirq_to_gsi: [u32; 4],
        placement: AcpiPlacement,
    ) -> Result<AcpiInfo, BiosAcpiError>;
}

#[derive(Debug, Default)]
pub struct AeroAcpiBuilder;

impl AcpiBuilder for AeroAcpiBuilder {
    fn build_and_write(
        &mut self,
        bus: &mut dyn BiosBus,
        memory_size_bytes: u64,
        cpu_count: u8,
        pirq_to_gsi: [u32; 4],
        placement: AcpiPlacement,
    ) -> Result<AcpiInfo, BiosAcpiError> {
        build_and_write(bus, memory_size_bytes, cpu_count, pirq_to_gsi, placement)
    }
}

pub fn build_and_write(
    bus: &mut dyn BiosBus,
    memory_size_bytes: u64,
    cpu_count: u8,
    pirq_to_gsi: [u32; 4],
    placement: AcpiPlacement,
) -> Result<AcpiInfo, BiosAcpiError> {
    let cfg = AcpiConfig {
        cpu_count: cpu_count.max(1),
        pirq_to_gsi,
        // Enable PCIe-friendly config space access via MMCONFIG/ECAM.
        //
        // This must match the platform MMIO mapping (see `aero-pc-platform`).
        pcie_ecam_base: PCIE_ECAM_BASE,
        pcie_segment: PCIE_ECAM_SEGMENT,
        pcie_start_bus: PCIE_ECAM_START_BUS,
        pcie_end_bus: PCIE_ECAM_END_BUS,
        ..Default::default()
    };

    let tables = AcpiTables::build(&cfg, placement);

    // Validate everything fits inside guest RAM.
    let mut to_check = vec![
        ("RSDP", tables.addresses.rsdp, tables.rsdp.len()),
        ("RSDT", tables.addresses.rsdt, tables.rsdt.len()),
        ("XSDT", tables.addresses.xsdt, tables.xsdt.len()),
        ("FADT", tables.addresses.fadt, tables.fadt.len()),
        ("MADT", tables.addresses.madt, tables.madt.len()),
        ("HPET", tables.addresses.hpet, tables.hpet.len()),
        ("DSDT", tables.addresses.dsdt, tables.dsdt.len()),
        ("FACS", tables.addresses.facs, tables.facs.len()),
    ];
    if let (Some(addr), Some(table)) = (tables.addresses.mcfg, tables.mcfg.as_ref()) {
        to_check.push(("MCFG", addr, table.len()));
    }
    for (name, addr, len) in to_check {
        let Some(end) = addr.checked_add(len as u64) else {
            return Err(BiosAcpiError::TableAddressOverflow {
                table: name,
                addr,
                len: len as u64,
            });
        };
        if end > memory_size_bytes {
            return Err(BiosAcpiError::TableOutOfBounds {
                table: name,
                end,
                memory_size_bytes,
            });
        }
    }
    let Some(nvs_end) = placement.nvs_base.checked_add(placement.nvs_size) else {
        return Err(BiosAcpiError::NvsAddressOverflow {
            base: placement.nvs_base,
            size: placement.nvs_size,
        });
    };
    if nvs_end > memory_size_bytes {
        return Err(BiosAcpiError::NvsOutOfBounds {
            end: nvs_end,
            memory_size_bytes,
        });
    }

    struct Writer<'a> {
        bus: &'a mut dyn BiosBus,
    }

    impl AcpiPhysicalMemory for Writer<'_> {
        fn write(&mut self, paddr: u64, bytes: &[u8]) {
            self.bus.write_physical(paddr, bytes);
        }
    }

    tables.write_to(&mut Writer { bus });

    let reclaimable = acpi_reclaimable_region_from_tables(&tables);
    Ok(AcpiInfo {
        rsdp_addr: tables.addresses.rsdp,
        reclaimable,
        nvs: (placement.nvs_base, placement.nvs_size),
    })
}

fn acpi_reclaimable_region_from_tables(tables: &AcpiTables) -> (u64, u64) {
    let addrs = &tables.addresses;
    let mut start = addrs.dsdt;
    start = start.min(addrs.fadt);
    start = start.min(addrs.madt);
    start = start.min(addrs.hpet);
    if let Some(mcfg) = addrs.mcfg {
        start = start.min(mcfg);
    }
    start = start.min(addrs.rsdt);
    start = start.min(addrs.xsdt);

    let mut end = start;
    end = end.max(addrs.dsdt.saturating_add(tables.dsdt.len() as u64));
    end = end.max(addrs.fadt.saturating_add(tables.fadt.len() as u64));
    end = end.max(addrs.madt.saturating_add(tables.madt.len() as u64));
    end = end.max(addrs.hpet.saturating_add(tables.hpet.len() as u64));
    if let (Some(addr), Some(table)) = (addrs.mcfg, tables.mcfg.as_ref()) {
        end = end.max(addr.saturating_add(table.len() as u64));
    }
    end = end.max(addrs.rsdt.saturating_add(tables.rsdt.len() as u64));
    end = end.max(addrs.xsdt.saturating_add(tables.xsdt.len() as u64));

    (start, end.saturating_sub(start))
}
