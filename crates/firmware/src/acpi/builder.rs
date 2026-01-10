use crate::acpi::constants::{
    ACPI_TABLE_ALIGNMENT, DEFAULT_ACPI_NVS_WINDOW_SIZE, DEFAULT_ACPI_RECLAIM_WINDOW_SIZE,
    DEFAULT_EBDA_BASE, DEFAULT_PCI_MMIO_START, HPET_BASE, IO_APIC_BASE, LOCAL_APIC_BASE,
};
use crate::acpi::structures::{
    as_bytes, AcpiHeader, Facs, Fadt, GenericAddress, Hpet, RsdpV2, ACPI_HEADER_CHECKSUM_OFFSET,
    ACPI_HEADER_SIZE, RSDP_CHECKSUM_LEN_V1, RSDP_V2_SIZE,
};
use memory::{GuestMemory, GuestMemoryError};

pub type PhysAddr = u64;
pub type RsdpPhysAddr = PhysAddr;

#[derive(Debug, Clone)]
pub struct AcpiConfig {
    pub cpu_count: u8,
    pub guest_memory_size: u64,

    /// Start of the PCI MMIO hole used for placing ACPI windows.
    pub pci_mmio_start: PhysAddr,

    /// Address where the RSDP is written (must be 16-byte aligned).
    pub rsdp_addr: RsdpPhysAddr,

    /// Size reserved for reclaimable tables (E820 type 3).
    pub reclaim_window_size: u64,

    /// Size reserved for ACPI NVS structures (E820 type 4).
    pub nvs_window_size: u64,
}

impl AcpiConfig {
    pub fn new(cpu_count: u8, guest_memory_size: u64) -> Self {
        Self {
            cpu_count,
            guest_memory_size,
            pci_mmio_start: DEFAULT_PCI_MMIO_START,
            rsdp_addr: DEFAULT_EBDA_BASE,
            reclaim_window_size: DEFAULT_ACPI_RECLAIM_WINDOW_SIZE,
            nvs_window_size: DEFAULT_ACPI_NVS_WINDOW_SIZE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpiBuildError {
    CpuCountMustBeNonZero,
    RsdpNotAligned(RsdpPhysAddr),
    GuestMemoryTooSmall {
        guest_memory_size: u64,
        required: u64,
    },
    AddressDoesNotFitInU32 {
        table: &'static str,
        addr: u64,
    },
    TablesOverflowReclaimWindow {
        reclaim_window_size: u64,
        used: u64,
    },
    TablesOverflowNvsWindow {
        nvs_window_size: u64,
        used: u64,
    },
    GuestMemory(GuestMemoryError),
}

impl From<GuestMemoryError> for AcpiBuildError {
    fn from(err: GuestMemoryError) -> Self {
        Self::GuestMemory(err)
    }
}

/// ACPI table set along with their guest physical placement.
#[derive(Debug, Clone)]
pub struct AcpiTables {
    // Placement windows (E820 type 3 and type 4 respectively).
    pub reclaim_base: PhysAddr,
    pub reclaim_size: u64,
    pub nvs_base: PhysAddr,
    pub nvs_size: u64,

    // Individual table placements.
    pub rsdp_addr: RsdpPhysAddr,
    pub rsdt_addr: PhysAddr,
    pub xsdt_addr: PhysAddr,
    pub fadt_addr: PhysAddr,
    pub madt_addr: PhysAddr,
    pub hpet_addr: PhysAddr,
    pub dsdt_addr: PhysAddr,
    pub facs_addr: PhysAddr,

    // Serialized table bytes.
    pub rsdp: [u8; RSDP_V2_SIZE],
    pub rsdt: Vec<u8>,
    pub xsdt: Vec<u8>,
    pub fadt: Vec<u8>,
    pub madt: Vec<u8>,
    pub hpet: Vec<u8>,
    pub dsdt: Vec<u8>,
    pub facs: Vec<u8>,
}

impl AcpiTables {
    pub fn build(config: &AcpiConfig) -> Result<Self, AcpiBuildError> {
        if config.cpu_count == 0 {
            return Err(AcpiBuildError::CpuCountMustBeNonZero);
        }
        if config.rsdp_addr % ACPI_TABLE_ALIGNMENT != 0 {
            return Err(AcpiBuildError::RsdpNotAligned(config.rsdp_addr));
        }

        let low_ram_top = config.guest_memory_size.min(config.pci_mmio_start);
        let total_acpi_window = config
            .reclaim_window_size
            .checked_add(config.nvs_window_size)
            .expect("ACPI window sizes should not overflow u64");
        if low_ram_top < total_acpi_window {
            return Err(AcpiBuildError::GuestMemoryTooSmall {
                guest_memory_size: config.guest_memory_size,
                required: total_acpi_window,
            });
        }

        // Place ACPI windows at the top of low RAM, below the PCI MMIO hole.
        let reclaim_base =
            align_down(low_ram_top - total_acpi_window, ACPI_TABLE_ALIGNMENT);
        let nvs_base = reclaim_base + config.reclaim_window_size;

        // NVS: allocate FACS first.
        let facs_addr = align_up(nvs_base, ACPI_TABLE_ALIGNMENT);
        let facs = build_facs();
        let nvs_used = (facs_addr - nvs_base) + facs.len() as u64;
        if nvs_used > config.nvs_window_size {
            return Err(AcpiBuildError::TablesOverflowNvsWindow {
                nvs_window_size: config.nvs_window_size,
                used: nvs_used,
            });
        }

        // Reclaimable: DSDT, FADT, MADT, HPET, RSDT, XSDT.
        let mut cursor = reclaim_base;

        let dsdt_addr = align_up(cursor, ACPI_TABLE_ALIGNMENT);
        let dsdt = build_dsdt();
        cursor = align_up(dsdt_addr + dsdt.len() as u64, ACPI_TABLE_ALIGNMENT);

        let fadt_addr = cursor;
        let fadt = build_fadt(dsdt_addr, facs_addr)?;
        cursor = align_up(fadt_addr + fadt.len() as u64, ACPI_TABLE_ALIGNMENT);

        let madt_addr = cursor;
        let madt = build_madt(config.cpu_count);
        cursor = align_up(madt_addr + madt.len() as u64, ACPI_TABLE_ALIGNMENT);

        let hpet_addr = cursor;
        let hpet = build_hpet();
        cursor = align_up(hpet_addr + hpet.len() as u64, ACPI_TABLE_ALIGNMENT);

        let rsdt_addr = cursor;
        let rsdt = build_rsdt(&[fadt_addr, madt_addr, hpet_addr])?;
        cursor = align_up(rsdt_addr + rsdt.len() as u64, ACPI_TABLE_ALIGNMENT);

        let xsdt_addr = cursor;
        let xsdt = build_xsdt(&[fadt_addr, madt_addr, hpet_addr]);
        cursor = align_up(xsdt_addr + xsdt.len() as u64, ACPI_TABLE_ALIGNMENT);

        let reclaim_used = cursor - reclaim_base;
        if reclaim_used > config.reclaim_window_size {
            return Err(AcpiBuildError::TablesOverflowReclaimWindow {
                reclaim_window_size: config.reclaim_window_size,
                used: reclaim_used,
            });
        }

        // RSDP (separately placed in EBDA/scan region).
        let mut rsdp = build_rsdp(config.rsdp_addr, rsdt_addr, xsdt_addr)?;
        finalize_rsdp_checksums(&mut rsdp);

        Ok(Self {
            reclaim_base,
            reclaim_size: config.reclaim_window_size,
            nvs_base,
            nvs_size: config.nvs_window_size,
            rsdp_addr: config.rsdp_addr,
            rsdt_addr,
            xsdt_addr,
            fadt_addr,
            madt_addr,
            hpet_addr,
            dsdt_addr,
            facs_addr,
            rsdp,
            rsdt,
            xsdt,
            fadt,
            madt,
            hpet,
            dsdt,
            facs,
        })
    }

    pub fn write_to<M: GuestMemory>(&self, mem: &mut M) -> Result<(), AcpiBuildError> {
        // Best-effort sanity check: the memory implementation might cover sparse
        // address spaces, but for the simple `VecGuestMemory` used in tests this
        // is helpful.
        let reclaim_end = self.reclaim_base.saturating_add(self.reclaim_size);
        let nvs_end = self.nvs_base.saturating_add(self.nvs_size);
        let rsdp_end = self.rsdp_addr.saturating_add(self.rsdp.len() as u64);
        let min_mem = reclaim_end.max(nvs_end).max(rsdp_end);
        if mem.size() < min_mem {
            return Err(AcpiBuildError::GuestMemoryTooSmall {
                guest_memory_size: mem.size(),
                required: min_mem,
            });
        }

        mem.write_from(self.dsdt_addr, &self.dsdt)?;
        mem.write_from(self.fadt_addr, &self.fadt)?;
        mem.write_from(self.madt_addr, &self.madt)?;
        mem.write_from(self.hpet_addr, &self.hpet)?;
        mem.write_from(self.rsdt_addr, &self.rsdt)?;
        mem.write_from(self.xsdt_addr, &self.xsdt)?;
        mem.write_from(self.facs_addr, &self.facs)?;
        mem.write_from(self.rsdp_addr, &self.rsdp)?;
        Ok(())
    }

    pub fn build_and_write<M: GuestMemory>(
        config: &AcpiConfig,
        mem: &mut M,
    ) -> Result<RsdpPhysAddr, AcpiBuildError> {
        let tables = Self::build(config)?;
        tables.write_to(mem)?;
        Ok(tables.rsdp_addr)
    }
}

pub fn align_up(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    (value + (alignment - 1)) & !(alignment - 1)
}

pub fn align_down(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    value & !(alignment - 1)
}

pub fn checksum8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

fn set_checksum(data: &mut [u8], checksum_offset: usize) {
    data[checksum_offset] = 0;
    let sum = checksum8(data);
    data[checksum_offset] = (0u8).wrapping_sub(sum);
}

fn finalize_rsdp_checksums(rsdp: &mut [u8; RSDP_V2_SIZE]) {
    rsdp[8] = 0;
    rsdp[32] = 0;
    let sum_v1 = checksum8(&rsdp[..RSDP_CHECKSUM_LEN_V1]);
    rsdp[8] = (0u8).wrapping_sub(sum_v1);
    let sum_v2 = checksum8(&rsdp[..RSDP_V2_SIZE]);
    rsdp[32] = (0u8).wrapping_sub(sum_v2);
}

fn make_header(signature: [u8; 4], length: u32, revision: u8, oem_table_id: [u8; 8]) -> AcpiHeader {
    AcpiHeader {
        signature,
        length: length.to_le(),
        revision,
        checksum: 0,
        oem_id: *b"AERO  ",
        oem_table_id,
        oem_revision: 1u32.to_le(),
        creator_id: u32::from_le_bytes(*b"Aero").to_le(),
        creator_revision: 1u32.to_le(),
    }
}

fn build_dsdt() -> Vec<u8> {
    // Use the clean-room DSDT bundled with the firmware crate.
    crate::acpi::dsdt::DSDT_AML.to_vec()
}

fn build_facs() -> Vec<u8> {
    let mut facs = Facs::default();
    facs.signature = *b"FACS";
    facs.length = (core::mem::size_of::<Facs>() as u32).to_le();
    facs.version = 1;
    as_bytes(&facs).to_vec()
}

fn ensure_u32(table: &'static str, addr: u64) -> Result<u32, AcpiBuildError> {
    if addr > u32::MAX as u64 {
        return Err(AcpiBuildError::AddressDoesNotFitInU32 { table, addr });
    }
    Ok(addr as u32)
}

fn build_fadt(dsdt_addr: PhysAddr, facs_addr: PhysAddr) -> Result<Vec<u8>, AcpiBuildError> {
    let mut fadt = Fadt::default();
    fadt.header = make_header(
        *b"FACP",
        core::mem::size_of::<Fadt>() as u32,
        4, // ACPI 3.0-era FADT revision; widely accepted by Windows 7.
        *b"AEROFACP",
    );

    fadt.FirmwareCtrl = ensure_u32("FADT.FirmwareCtrl", facs_addr)?.to_le();
    fadt.Dsdt = ensure_u32("FADT.Dsdt", dsdt_addr)?.to_le();
    fadt.X_FirmwareCtrl = (facs_addr as u64).to_le();
    fadt.X_Dsdt = (dsdt_addr as u64).to_le();

    // Desktop profile.
    fadt.PreferredPmProfile = 2;

    // SCI is traditionally IRQ9 on PC platforms; the MADT includes an ISO for
    // ISA IRQ9 -> GSI9.
    fadt.SciInt = 9u16.to_le();

    // If we do not implement the ACPI PM I/O device yet, advertise fixed
    // registers as not present. This avoids the guest OS touching unhandled I/O
    // ports during early boot.
    //
    // When an ACPI PM device is implemented, populate Pm1aEvtBlk/Pm1aCntBlk/
    // PmTmrBlk (+ their X_* GAS versions) coherently.
    fadt.Pm1EvtLen = 0;
    fadt.Pm1CntLen = 0;
    fadt.Pm2CntLen = 0;
    fadt.PmTmrLen = 0;
    fadt.Gpe0BlkLen = 0;
    fadt.Gpe1BlkLen = 0;

    // Legacy devices present (PIC/PIT/RTC), 8042 present, VGA present.
    fadt.IapcBootArch = 0x0007u16.to_le();

    // ACPI reset register support (FADT.ResetReg/ResetValue).
    //
    // Windows (and other OSes) commonly use the ACPI-defined reset register for
    // reboot, which on PC platforms conventionally points at the chipset reset
    // control port 0xCF9.
    const FADT_FLAG_RESET_REG_SUP: u32 = 1 << 10;
    fadt.Flags = FADT_FLAG_RESET_REG_SUP.to_le();
    fadt.ResetReg = GenericAddress {
        address_space_id: 1, // System I/O
        register_bit_width: 8,
        register_bit_offset: 0,
        access_size: 1, // byte access
        address: 0x0CF9u64.to_le(),
    };
    fadt.ResetValue = 0x06;

    let mut bytes = as_bytes(&fadt).to_vec();
    set_checksum(&mut bytes, ACPI_HEADER_CHECKSUM_OFFSET);
    Ok(bytes)
}

fn build_madt(cpu_count: u8) -> Vec<u8> {
    let header = make_header(*b"APIC", 0, 3, *b"AEROAPIC");
    let mut table = Vec::new();
    table.extend_from_slice(as_bytes(&header));
    table.extend_from_slice(&LOCAL_APIC_BASE.to_le_bytes());
    table.extend_from_slice(&1u32.to_le_bytes()); // PCAT_COMPAT

    for cpu in 0..cpu_count {
        // Processor Local APIC (type 0).
        table.push(0);
        table.push(8);
        table.push(cpu); // ACPI Processor ID
        table.push(cpu); // APIC ID
        table.extend_from_slice(&1u32.to_le_bytes()); // Enabled
    }

    // I/O APIC (type 1).
    table.push(1);
    table.push(12);
    table.push(0); // I/O APIC ID
    table.push(0); // reserved
    table.extend_from_slice(&IO_APIC_BASE.to_le_bytes());
    table.extend_from_slice(&0u32.to_le_bytes()); // GSI base

    // Interrupt Source Override (type 2): ISA IRQ0 -> GSI2.
    add_iso(&mut table, 0, 0, 2, 0);
    // Interrupt Source Override (type 2): ISA IRQ9 -> GSI9 (SCI).
    add_iso(&mut table, 0, 9, 9, 0x000D);

    // Patch length and checksum.
    let len = table.len() as u32;
    table[4..8].copy_from_slice(&len.to_le_bytes());
    set_checksum(&mut table, ACPI_HEADER_CHECKSUM_OFFSET);
    table
}

fn add_iso(table: &mut Vec<u8>, bus: u8, source_irq: u8, gsi: u32, flags: u16) {
    table.push(2);
    table.push(10);
    table.push(bus);
    table.push(source_irq);
    table.extend_from_slice(&gsi.to_le_bytes());
    table.extend_from_slice(&flags.to_le_bytes());
}

fn build_hpet() -> Vec<u8> {
    let mut hpet = Hpet::default();
    hpet.header = make_header(
        *b"HPET",
        core::mem::size_of::<Hpet>() as u32,
        1,
        *b"AEROHPET",
    );
    hpet.EventTimerBlockId = 0x8086_A201u32.to_le();
    hpet.BaseAddress = GenericAddress {
        address_space_id: 0, // System Memory
        register_bit_width: 0,
        register_bit_offset: 0,
        access_size: 0,
        address: HPET_BASE.to_le(),
    };
    hpet.HpetNumber = 0;
    hpet.MinimumTick = 0x0080u16.to_le();
    hpet.PageProtection = 0;

    let mut bytes = as_bytes(&hpet).to_vec();
    set_checksum(&mut bytes, ACPI_HEADER_CHECKSUM_OFFSET);
    bytes
}

fn build_rsdt(entries: &[PhysAddr]) -> Result<Vec<u8>, AcpiBuildError> {
    let mut table = Vec::with_capacity(ACPI_HEADER_SIZE + entries.len() * 4);
    let header = make_header(
        *b"RSDT",
        (ACPI_HEADER_SIZE + entries.len() * 4) as u32,
        1,
        *b"AERORSDT",
    );
    table.extend_from_slice(as_bytes(&header));
    for &addr in entries {
        table.extend_from_slice(&ensure_u32("RSDT entry", addr)?.to_le_bytes());
    }
    set_checksum(&mut table, ACPI_HEADER_CHECKSUM_OFFSET);
    Ok(table)
}

fn build_xsdt(entries: &[PhysAddr]) -> Vec<u8> {
    let mut table = Vec::with_capacity(ACPI_HEADER_SIZE + entries.len() * 8);
    let header = make_header(
        *b"XSDT",
        (ACPI_HEADER_SIZE + entries.len() * 8) as u32,
        1,
        *b"AEROXSDT",
    );
    table.extend_from_slice(as_bytes(&header));
    for &addr in entries {
        table.extend_from_slice(&(addr as u64).to_le_bytes());
    }
    set_checksum(&mut table, ACPI_HEADER_CHECKSUM_OFFSET);
    table
}

fn build_rsdp(
    _rsdp_addr: RsdpPhysAddr,
    rsdt_addr: PhysAddr,
    xsdt_addr: PhysAddr,
) -> Result<[u8; RSDP_V2_SIZE], AcpiBuildError> {
    let mut rsdp = RsdpV2::default();
    rsdp.signature = *b"RSD PTR ";
    rsdp.oem_id = *b"AERO  ";
    rsdp.revision = 2;
    rsdp.rsdt_address = ensure_u32("RSDP.rsdt_address", rsdt_addr)?.to_le();
    rsdp.length = (RSDP_V2_SIZE as u32).to_le();
    rsdp.xsdt_address = (xsdt_addr as u64).to_le();

    let mut bytes = [0u8; RSDP_V2_SIZE];
    bytes.copy_from_slice(as_bytes(&rsdp));
    Ok(bytes)
}
