use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

#[derive(Debug, Clone)]
pub struct BuiltAcpiTables {
    pub base_address: u64,

    pub dsdt_address: u64,
    pub dsdt: Vec<u8>,

    pub facs_address: u64,
    pub facs: Vec<u8>,

    pub fadt_address: u64,
    pub fadt: Vec<u8>,

    pub madt_address: u64,
    pub madt: Vec<u8>,

    pub hpet_address: u64,
    pub hpet: Vec<u8>,

    pub rsdt_address: u64,
    pub rsdt: Vec<u8>,

    pub xsdt_address: u64,
    pub xsdt: Vec<u8>,

    pub rsdp_address: u64,
    pub rsdp: Vec<u8>,
}

fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}

pub fn build_acpi_table_set(base_address: u64) -> BuiltAcpiTables {
    build_acpi_table_set_with_hpet(base_address, AcpiConfig::default().hpet_addr)
}

pub fn build_acpi_table_set_with_hpet(base_address: u64, hpet_base: u64) -> BuiltAcpiTables {
    let mut cfg = AcpiConfig::default();
    cfg.hpet_addr = hpet_base;

    // Keep the legacy table-set builder API simple: the caller provides a base for the SDT blob.
    // Place the NVS window and RSDP at fixed offsets to avoid overlapping the table blob.
    let alignment = aero_acpi::DEFAULT_ACPI_ALIGNMENT;
    let placement = AcpiPlacement {
        tables_base: align_up(base_address, alignment),
        nvs_base: align_up(base_address + 0x10_000, alignment),
        nvs_size: aero_acpi::DEFAULT_ACPI_NVS_SIZE,
        rsdp_addr: align_up(base_address + 0x8_000, 16),
        alignment,
    };

    let tables = AcpiTables::build(&cfg, placement);

    BuiltAcpiTables {
        base_address,
        dsdt_address: tables.addresses.dsdt,
        dsdt: tables.dsdt,
        facs_address: tables.addresses.facs,
        facs: tables.facs,
        fadt_address: tables.addresses.fadt,
        fadt: tables.fadt,
        madt_address: tables.addresses.madt,
        madt: tables.madt,
        hpet_address: tables.addresses.hpet,
        hpet: tables.hpet,
        rsdt_address: tables.addresses.rsdt,
        rsdt: tables.rsdt,
        xsdt_address: tables.addresses.xsdt,
        xsdt: tables.xsdt,
        rsdp_address: tables.addresses.rsdp,
        rsdp: tables.rsdp,
    }
}
