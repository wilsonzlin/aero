use crate::acpi::constants::{
    ACPI_TABLE_ALIGNMENT, DEFAULT_ACPI_NVS_WINDOW_SIZE, DEFAULT_ACPI_RECLAIM_WINDOW_SIZE,
    DEFAULT_EBDA_BASE, DEFAULT_PCI_MMIO_START,
};
use crate::acpi::structures::{RSDP_CHECKSUM_LEN_V1, RSDP_V2_SIZE};
use aero_pc_constants::PCIE_ECAM_BASE;
use memory::{GuestMemory, GuestMemoryError};

pub type PhysAddr = u64;
pub type RsdpPhysAddr = PhysAddr;

#[derive(Debug, Clone)]
pub struct AcpiConfig {
    pub cpu_count: u8,
    pub guest_memory_size: u64,

    /// Start of the PCI MMIO window reserved for PCI device BAR allocations.
    ///
    /// ACPI reclaimable + NVS windows are placed at the top of low RAM, below the first reserved
    /// MMIO region. On the PC platform that means clamping below both this address and the PCIe
    /// ECAM window (`aero_pc_constants::PCIE_ECAM_BASE`).
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
            // Keep the RSDP within the EBDA so it is discoverable via the legacy BIOS scan.
            rsdp_addr: DEFAULT_EBDA_BASE + 0x100,
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
        const ONE_MIB: u64 = 0x0010_0000;

        if config.cpu_count == 0 {
            return Err(AcpiBuildError::CpuCountMustBeNonZero);
        }
        if config.rsdp_addr % ACPI_TABLE_ALIGNMENT != 0 {
            return Err(AcpiBuildError::RsdpNotAligned(config.rsdp_addr));
        }

        // The PC platform also reserves a PCIe ECAM window (MCFG/MMCONFIG) below the typical PCI
        // BAR MMIO window, so the "top of low RAM" must sit below whichever reserved region starts
        // first.
        let low_ram_top = config
            .guest_memory_size
            .min(config.pci_mmio_start)
            .min(PCIE_ECAM_BASE);
        let total_acpi_window = config
            .reclaim_window_size
            .checked_add(config.nvs_window_size)
            .expect("ACPI window sizes should not overflow u64");

        // Keep SDTs above 1MiB so we don't overlap the BIOS/EBDA scan regions.
        if low_ram_top < ONE_MIB.saturating_add(total_acpi_window) {
            return Err(AcpiBuildError::GuestMemoryTooSmall {
                guest_memory_size: config.guest_memory_size,
                required: ONE_MIB.saturating_add(total_acpi_window),
            });
        }

        // Place ACPI windows at the top of low RAM, below the first reserved MMIO window.
        let end = align_down(low_ram_top, ACPI_TABLE_ALIGNMENT);
        let reclaim_base = align_down(end - total_acpi_window, ACPI_TABLE_ALIGNMENT);
        let nvs_base = reclaim_base + config.reclaim_window_size;

        // Ensure the guest is large enough to hold the RSDP.
        let rsdp_end = config.rsdp_addr.saturating_add(RSDP_V2_SIZE as u64);
        if config.guest_memory_size < rsdp_end {
            return Err(AcpiBuildError::GuestMemoryTooSmall {
                guest_memory_size: config.guest_memory_size,
                required: rsdp_end,
            });
        }

        let mut aero_cfg = aero_acpi::AcpiConfig::default();
        aero_cfg.cpu_count = config.cpu_count;

        aero_cfg.pci_mmio_base = u32::try_from(config.pci_mmio_start).map_err(|_| {
            AcpiBuildError::AddressDoesNotFitInU32 {
                table: "PCI MMIO base",
                addr: config.pci_mmio_start,
            }
        })?;

        // Keep the MMIO window ending right below the IOAPIC base (matching the default).
        if aero_cfg.io_apic_addr <= aero_cfg.pci_mmio_base {
            aero_cfg.pci_mmio_size = 0;
        } else {
            aero_cfg.pci_mmio_size = aero_cfg.io_apic_addr - aero_cfg.pci_mmio_base;
        }

        let placement = aero_acpi::AcpiPlacement {
            tables_base: reclaim_base,
            nvs_base,
            nvs_size: config.nvs_window_size,
            rsdp_addr: config.rsdp_addr,
            alignment: ACPI_TABLE_ALIGNMENT,
        };

        let tables = aero_acpi::AcpiTables::build(&aero_cfg, placement);

        let reclaim_end = reclaim_base + config.reclaim_window_size;
        let mut reclaim_max_end = reclaim_base;
        for &(_, addr, len) in &[
            ("DSDT", tables.addresses.dsdt, tables.dsdt.len()),
            ("FADT", tables.addresses.fadt, tables.fadt.len()),
            ("MADT", tables.addresses.madt, tables.madt.len()),
            ("HPET", tables.addresses.hpet, tables.hpet.len()),
            ("RSDT", tables.addresses.rsdt, tables.rsdt.len()),
            ("XSDT", tables.addresses.xsdt, tables.xsdt.len()),
        ] {
            let end = addr + len as u64;
            reclaim_max_end = reclaim_max_end.max(end);
            if addr < reclaim_base || end > reclaim_end {
                return Err(AcpiBuildError::TablesOverflowReclaimWindow {
                    reclaim_window_size: config.reclaim_window_size,
                    used: reclaim_max_end - reclaim_base,
                });
            }
        }

        let facs_end = tables.addresses.facs + tables.facs.len() as u64;
        let nvs_end = nvs_base + config.nvs_window_size;
        if tables.addresses.facs < nvs_base || facs_end > nvs_end {
            return Err(AcpiBuildError::TablesOverflowNvsWindow {
                nvs_window_size: config.nvs_window_size,
                used: facs_end.saturating_sub(nvs_base),
            });
        }

        let rsdp: [u8; RSDP_V2_SIZE] = tables
            .rsdp
            .as_slice()
            .try_into()
            .expect("aero-acpi must always emit a v2 RSDP");

        debug_assert_eq!(checksum8(&rsdp[..RSDP_CHECKSUM_LEN_V1]), 0);
        debug_assert_eq!(checksum8(&rsdp), 0);

        Ok(Self {
            reclaim_base,
            reclaim_size: config.reclaim_window_size,
            nvs_base,
            nvs_size: config.nvs_window_size,
            rsdp_addr: tables.addresses.rsdp,
            rsdt_addr: tables.addresses.rsdt,
            xsdt_addr: tables.addresses.xsdt,
            fadt_addr: tables.addresses.fadt,
            madt_addr: tables.addresses.madt,
            hpet_addr: tables.addresses.hpet,
            dsdt_addr: tables.addresses.dsdt,
            facs_addr: tables.addresses.facs,
            rsdp,
            rsdt: tables.rsdt,
            xsdt: tables.xsdt,
            fadt: tables.fadt,
            madt: tables.madt,
            hpet: tables.hpet,
            dsdt: tables.dsdt,
            facs: tables.facs,
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
        mem.write_from(self.facs_addr, &self.facs)?;
        mem.write_from(self.fadt_addr, &self.fadt)?;
        mem.write_from(self.madt_addr, &self.madt)?;
        mem.write_from(self.hpet_addr, &self.hpet)?;
        mem.write_from(self.rsdt_addr, &self.rsdt)?;
        mem.write_from(self.xsdt_addr, &self.xsdt)?;
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
