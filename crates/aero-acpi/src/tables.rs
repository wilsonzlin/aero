use core::fmt;

pub const DEFAULT_ACPI_ALIGNMENT: u64 = 16;
pub const DEFAULT_ACPI_NVS_SIZE: u64 = 0x1000;

/// Physical memory writing abstraction used by firmware to place tables in
/// guest RAM.
pub trait PhysicalMemory {
    fn write(&mut self, paddr: u64, bytes: &[u8]);
}

#[derive(Clone, Copy, Debug)]
pub struct AcpiPlacement {
    /// Base address for the SDT blobs (DSDT/FADT/MADT/HPET/RSDT/XSDT).
    pub tables_base: u64,
    /// Base address for ACPI NVS blobs (E820 type 4). Currently this is used to
    /// place the FACS, which is referenced by the FADT but must *not* appear in
    /// the RSDT/XSDT.
    pub nvs_base: u64,
    /// Size of the ACPI NVS window reserved for firmware structures (bytes).
    pub nvs_size: u64,
    /// Physical address where the RSDP will be written (must be < 1MiB for PC
    /// firmware discovery; 16-byte aligned is recommended).
    pub rsdp_addr: u64,
    /// Alignment for each table start.
    pub alignment: u64,
}

impl Default for AcpiPlacement {
    fn default() -> Self {
        Self {
            tables_base: 0x0010_0000, // 1MiB (common for firmware table blobs)
            nvs_base: 0x0011_0000,    // adjacent, but marked ACPI NVS (type 4)
            nvs_size: DEFAULT_ACPI_NVS_SIZE,
            rsdp_addr: 0x000F_0000, // within the BIOS search range
            alignment: DEFAULT_ACPI_ALIGNMENT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AcpiConfig {
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: [u8; 4],
    pub creator_revision: u32,

    pub cpu_count: u8,

    pub local_apic_addr: u32,
    pub io_apic_addr: u32,
    pub hpet_addr: u64,

    /// ACPI SCI interrupt (legacy IRQ number).
    pub sci_irq: u8,

    /// FADT SMI command port used for the ACPI enable/disable handshake.
    ///
    /// Windows (and many other OSes) will write `acpi_enable_cmd` to this port
    /// to request firmware to set `PM1a_CNT.SCI_EN`.
    pub smi_cmd_port: u16,
    /// Value written to `smi_cmd_port` to enable ACPI (set `SCI_EN`).
    pub acpi_enable_cmd: u8,
    /// Value written to `smi_cmd_port` to disable ACPI (clear `SCI_EN`).
    pub acpi_disable_cmd: u8,

    /// PM1a event block I/O port base.
    pub pm1a_evt_blk: u16,
    /// PM1a control block I/O port base.
    pub pm1a_cnt_blk: u16,
    /// PM timer block I/O port base.
    pub pm_tmr_blk: u16,
    /// GPE0 block I/O port base.
    pub gpe0_blk: u16,
    pub gpe0_blk_len: u8,

    /// MMIO window available to PCI devices.
    pub pci_mmio_base: u32,
    pub pci_mmio_size: u32,

    /// Base physical address of the PCIe ECAM ("MMCONFIG") window.
    ///
    /// When set to a non-zero value, [`AcpiTables::build`] will emit an `MCFG`
    /// table describing the ECAM region and the PCI root bridge will report a
    /// PCIe-compatible HID (`PNP0A08`).
    ///
    /// Set to 0 to omit the `MCFG` table and expose a legacy PCI root bridge
    /// (`PNP0A03`) only.
    pub pcie_ecam_base: u64,
    /// PCI segment group number for the ECAM region (usually 0).
    pub pcie_segment: u16,
    /// First bus number covered by the ECAM region.
    pub pcie_start_bus: u8,
    /// Last bus number covered by the ECAM region.
    pub pcie_end_bus: u8,

    /// Mapping of PCI PIRQ[A-D] to platform GSIs (used by the DSDT `_PRT`).
    ///
    /// The swizzle follows: `pirq = (device + pin) mod 4` where `pin` is
    /// 0 for INTA#, 1 for INTB#, etc.
    pub pirq_to_gsi: [u32; 4],
}

impl Default for AcpiConfig {
    fn default() -> Self {
        Self {
            oem_id: *b"AERO  ",
            oem_table_id: *b"AEROACPI",
            oem_revision: 1,
            creator_id: *b"AERO",
            creator_revision: 1,

            cpu_count: 1,

            local_apic_addr: 0xFEE0_0000,
            io_apic_addr: 0xFEC0_0000,
            hpet_addr: 0xFED0_0000,

            sci_irq: 9,

            smi_cmd_port: 0x00B2,
            acpi_enable_cmd: 0xA0,
            acpi_disable_cmd: 0xA1,

            pm1a_evt_blk: 0x0400,
            pm1a_cnt_blk: 0x0404,
            pm_tmr_blk: 0x0408,
            gpe0_blk: 0x0420,
            gpe0_blk_len: 0x08,

            pci_mmio_base: 0xC000_0000,
            pci_mmio_size: 0x3EC0_0000,

            // Disabled by default. Platforms that want PCIe-friendly config
            // access should set this to the mapped ECAM base (and optionally
            // adjust `pci_mmio_base/pci_mmio_size` to avoid overlaps).
            pcie_ecam_base: 0,
            pcie_segment: 0,
            pcie_start_bus: 0,
            pcie_end_bus: 0xFF,

            // Match the default routing in `devices::pci::irq_router::PciIntxRouterConfig`.
            pirq_to_gsi: [10, 11, 12, 13],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcpiAddresses {
    pub rsdp: u64,
    pub rsdt: u64,
    pub xsdt: u64,
    pub fadt: u64,
    pub madt: u64,
    pub hpet: u64,
    pub mcfg: Option<u64>,
    pub dsdt: u64,
    pub facs: u64,
}

#[derive(Clone)]
pub struct AcpiTables {
    pub addresses: AcpiAddresses,
    pub rsdp: Vec<u8>,
    pub rsdt: Vec<u8>,
    pub xsdt: Vec<u8>,
    pub fadt: Vec<u8>,
    pub madt: Vec<u8>,
    pub hpet: Vec<u8>,
    pub mcfg: Option<Vec<u8>>,
    pub dsdt: Vec<u8>,
    pub facs: Vec<u8>,
}

impl fmt::Debug for AcpiTables {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcpiTables")
            .field("addresses", &self.addresses)
            .field("rsdp_len", &self.rsdp.len())
            .field("rsdt_len", &self.rsdt.len())
            .field("xsdt_len", &self.xsdt.len())
            .field("fadt_len", &self.fadt.len())
            .field("madt_len", &self.madt.len())
            .field("hpet_len", &self.hpet.len())
            .field("mcfg_len", &self.mcfg.as_ref().map(|t| t.len()))
            .field("dsdt_len", &self.dsdt.len())
            .field("facs_len", &self.facs.len())
            .finish()
    }
}

impl AcpiTables {
    pub fn build(cfg: &AcpiConfig, placement: AcpiPlacement) -> Self {
        let mut next = placement.tables_base;
        let align = placement.alignment.max(1);

        let dsdt_addr = align_up(next, align);
        let dsdt = build_dsdt(cfg);
        next = align_up(dsdt_addr + dsdt.len() as u64, align);

        let facs_addr = align_up(placement.nvs_base, align);
        let facs = build_facs();
        assert!(
            facs_addr >= placement.nvs_base
                && facs_addr + facs.len() as u64 <= placement.nvs_base + placement.nvs_size,
            "FACS does not fit in the ACPI NVS window"
        );

        let fadt_addr = align_up(next, align);
        let fadt = build_fadt(cfg, dsdt_addr, facs_addr);
        next = align_up(fadt_addr + fadt.len() as u64, align);

        let madt_addr = align_up(next, align);
        let madt = build_madt(cfg);
        next = align_up(madt_addr + madt.len() as u64, align);

        let hpet_addr = align_up(next, align);
        let hpet = build_hpet(cfg);
        next = align_up(hpet_addr + hpet.len() as u64, align);

        let (mcfg_addr, mcfg) = if cfg.pcie_ecam_base != 0 {
            let mcfg_addr = align_up(next, align);
            let mcfg = build_mcfg(cfg);
            next = align_up(mcfg_addr + mcfg.len() as u64, align);
            (Some(mcfg_addr), Some(mcfg))
        } else {
            (None, None)
        };

        let rsdt_addr = align_up(next, align);
        let fadt32: u32 = fadt_addr
            .try_into()
            .expect("ACPI tables must be placed below 4GiB to populate the RSDT");
        let madt32: u32 = madt_addr
            .try_into()
            .expect("ACPI tables must be placed below 4GiB to populate the RSDT");
        let hpet32: u32 = hpet_addr
            .try_into()
            .expect("ACPI tables must be placed below 4GiB to populate the RSDT");
        let mut rsdt_entries = vec![fadt32, madt32, hpet32];
        if let Some(addr) = mcfg_addr {
            let addr32: u32 = addr
                .try_into()
                .expect("ACPI tables must be placed below 4GiB to populate the RSDT");
            rsdt_entries.push(addr32);
        }
        let rsdt = build_rsdt(cfg, &rsdt_entries);
        next = align_up(rsdt_addr + rsdt.len() as u64, align);

        let xsdt_addr = align_up(next, align);
        let mut xsdt_entries = vec![fadt_addr, madt_addr, hpet_addr];
        if let Some(addr) = mcfg_addr {
            xsdt_entries.push(addr);
        }
        let xsdt = build_xsdt(cfg, &xsdt_entries);
        next = align_up(xsdt_addr + xsdt.len() as u64, align);

        let rsdp_addr = align_up(placement.rsdp_addr, 16);
        let rsdp = build_rsdp(cfg, rsdt_addr as u32, xsdt_addr);

        let addresses = AcpiAddresses {
            rsdp: rsdp_addr,
            rsdt: rsdt_addr,
            xsdt: xsdt_addr,
            fadt: fadt_addr,
            madt: madt_addr,
            hpet: hpet_addr,
            mcfg: mcfg_addr,
            dsdt: dsdt_addr,
            facs: facs_addr,
        };

        // Ensure the NVS region does not overlap the reclaimable table blobs.
        let tables_end = next;
        let nvs_start = placement.nvs_base;
        let nvs_end = placement
            .nvs_base
            .checked_add(placement.nvs_size)
            .expect("ACPI NVS range overflow");
        assert!(
            nvs_end <= placement.tables_base || nvs_start >= tables_end,
            "ACPI NVS window overlaps ACPI table blob region"
        );

        Self {
            addresses,
            rsdp,
            rsdt,
            xsdt,
            fadt,
            madt,
            hpet,
            mcfg,
            dsdt,
            facs,
        }
    }

    pub fn write_to(&self, mem: &mut impl PhysicalMemory) {
        mem.write(self.addresses.dsdt, &self.dsdt);
        mem.write(self.addresses.facs, &self.facs);
        mem.write(self.addresses.fadt, &self.fadt);
        mem.write(self.addresses.madt, &self.madt);
        mem.write(self.addresses.hpet, &self.hpet);
        if let (Some(addr), Some(table)) = (self.addresses.mcfg, self.mcfg.as_ref()) {
            mem.write(addr, table);
        }
        mem.write(self.addresses.rsdt, &self.rsdt);
        mem.write(self.addresses.xsdt, &self.xsdt);
        mem.write(self.addresses.rsdp, &self.rsdp);
    }
}

fn align_up(value: u64, align: u64) -> u64 {
    assert_ne!(align, 0, "alignment must be non-zero");
    if align == 1 {
        return value;
    }
    let rem = value % align;
    if rem == 0 {
        value
    } else {
        value.checked_add(align - rem).expect("alignment overflow")
    }
}

fn checksum(data: &[u8]) -> u8 {
    let sum: u8 = data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    (0u8).wrapping_sub(sum)
}

fn build_sdt_header(
    signature: [u8; 4],
    revision: u8,
    total_len: u32,
    cfg: &AcpiConfig,
) -> [u8; 36] {
    let mut out = [0u8; 36];
    out[0..4].copy_from_slice(&signature);
    out[4..8].copy_from_slice(&total_len.to_le_bytes());
    out[8] = revision;
    out[9] = 0; // checksum to be filled in
    out[10..16].copy_from_slice(&cfg.oem_id);
    out[16..24].copy_from_slice(&cfg.oem_table_id);
    out[24..28].copy_from_slice(&cfg.oem_revision.to_le_bytes());
    out[28..32].copy_from_slice(&u32::from_le_bytes(cfg.creator_id).to_le_bytes());
    out[32..36].copy_from_slice(&cfg.creator_revision.to_le_bytes());
    out
}

fn finalize_sdt(mut table: Vec<u8>) -> Vec<u8> {
    debug_assert!(table.len() >= 36);
    table[9] = 0;
    let csum = checksum(&table);
    table[9] = csum;
    table
}

fn build_rsdp(cfg: &AcpiConfig, rsdt_addr: u32, xsdt_addr: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(b"RSD PTR ");
    out.push(0); // checksum placeholder
    out.extend_from_slice(&cfg.oem_id);
    out.push(2); // ACPI 2.0+
    out.extend_from_slice(&rsdt_addr.to_le_bytes());
    out.extend_from_slice(&(36u32).to_le_bytes());
    out.extend_from_slice(&xsdt_addr.to_le_bytes());
    out.push(0); // extended checksum placeholder
    out.extend_from_slice(&[0u8; 3]); // reserved

    // Checksum first 20 bytes.
    out[8] = 0;
    let csum1 = checksum(&out[..20]);
    out[8] = csum1;

    // Extended checksum.
    out[32] = 0;
    let csum2 = checksum(&out);
    out[32] = csum2;

    out
}

fn build_rsdt(cfg: &AcpiConfig, addrs: &[u32]) -> Vec<u8> {
    let total_len = 36 + (addrs.len() * 4);
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&build_sdt_header(*b"RSDT", 1, total_len as u32, cfg));
    for &addr in addrs {
        out.extend_from_slice(&addr.to_le_bytes());
    }
    finalize_sdt(out)
}

fn build_xsdt(cfg: &AcpiConfig, addrs: &[u64]) -> Vec<u8> {
    let total_len = 36 + (addrs.len() * 8);
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&build_sdt_header(*b"XSDT", 1, total_len as u32, cfg));
    for &addr in addrs {
        out.extend_from_slice(&addr.to_le_bytes());
    }
    finalize_sdt(out)
}

fn build_mcfg(cfg: &AcpiConfig) -> Vec<u8> {
    assert!(
        cfg.pcie_ecam_base != 0,
        "MCFG requested with pcie_ecam_base=0"
    );
    assert_eq!(
        cfg.pcie_ecam_base & ((1u64 << 20) - 1),
        0,
        "pcie_ecam_base must be 1MiB-aligned"
    );
    assert!(
        cfg.pcie_start_bus <= cfg.pcie_end_bus,
        "pcie_start_bus must be <= pcie_end_bus"
    );

    // MCFG revision 1 (PCI firmware spec / ACPI 3.0+).
    //
    // Layout:
    // - SDT header (36 bytes)
    // - reserved (8 bytes)
    // - one allocation structure (16 bytes)
    let total_len = 36 + 8 + 16;
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&build_sdt_header(*b"MCFG", 1, total_len as u32, cfg));

    out.extend_from_slice(&[0u8; 8]); // reserved

    // Configuration Space Base Address Allocation Structure.
    out.extend_from_slice(&cfg.pcie_ecam_base.to_le_bytes());
    out.extend_from_slice(&cfg.pcie_segment.to_le_bytes());
    out.push(cfg.pcie_start_bus);
    out.push(cfg.pcie_end_bus);
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved

    debug_assert_eq!(out.len(), total_len);
    finalize_sdt(out)
}

fn build_facs() -> Vec<u8> {
    // Minimal FACS (no checksum per spec). ACPI 2.0+ defines a 64-byte base.
    let mut out = vec![0u8; 64];
    out[0..4].copy_from_slice(b"FACS");
    out[4..8].copy_from_slice(&(64u32).to_le_bytes());
    // HW signature, waking vectors, global lock, flags remain zero.
    // Version (offset 32): set to 2 (ACPI 2.0+).
    out[32] = 2;
    out
}

#[derive(Clone, Copy)]
struct Gas {
    address_space_id: u8,
    register_bit_width: u8,
    register_bit_offset: u8,
    access_size: u8,
    address: u64,
}

impl Gas {
    fn new_io(bit_width: u8, port: u16) -> Self {
        Self {
            address_space_id: 1, // System I/O
            register_bit_width: bit_width,
            register_bit_offset: 0,
            // For ACPI fixed register *blocks* (PM1/GPE/etc), firmware commonly
            // leaves AccessSize as "unspecified" (0) and relies on the block
            // length fields elsewhere in the FADT. This avoids forcing the OS
            // to use a specific access width for multi-register blocks.
            access_size: 0,
            address: port as u64,
        }
    }

    fn new_io_with_access(bit_width: u8, access_size: u8, port: u16) -> Self {
        Self {
            address_space_id: 1, // System I/O
            register_bit_width: bit_width,
            register_bit_offset: 0,
            access_size,
            address: port as u64,
        }
    }

    fn new_mmio(address: u64) -> Self {
        Self {
            address_space_id: 0, // System Memory
            register_bit_width: 0,
            register_bit_offset: 0,
            access_size: 0,
            address,
        }
    }

    fn as_bytes(&self) -> [u8; 12] {
        let mut out = [0u8; 12];
        out[0] = self.address_space_id;
        out[1] = self.register_bit_width;
        out[2] = self.register_bit_offset;
        out[3] = self.access_size;
        out[4..12].copy_from_slice(&self.address.to_le_bytes());
        out
    }
}

fn build_fadt(cfg: &AcpiConfig, dsdt_addr: u64, facs_addr: u64) -> Vec<u8> {
    // FADT revision 3 (ACPI 2.0) with the fields up to (and including) X_GPE1_BLK.
    // This is enough for Windows 7, and avoids newer fields introduced after ACPI 2.0.
    const FADT_LEN: usize = 244;
    let mut out = Vec::with_capacity(FADT_LEN);
    out.extend_from_slice(&build_sdt_header(*b"FACP", 3, FADT_LEN as u32, cfg));

    // Firmware Control / FACS
    out.extend_from_slice(&(facs_addr as u32).to_le_bytes());
    // DSDT
    out.extend_from_slice(&(dsdt_addr as u32).to_le_bytes());

    out.push(0); // reserved: Model (deprecated)
    out.push(1); // preferred PM profile: Desktop
    out.extend_from_slice(&(cfg.sci_irq as u16).to_le_bytes());
    out.extend_from_slice(&(cfg.smi_cmd_port as u32).to_le_bytes());
    out.push(cfg.acpi_enable_cmd);
    out.push(cfg.acpi_disable_cmd);
    out.push(0); // S4BIOS_REQ
    out.push(0); // PSTATE_CNT

    out.extend_from_slice(&(cfg.pm1a_evt_blk as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // PM1B_EVT_BLK
    out.extend_from_slice(&(cfg.pm1a_cnt_blk as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // PM1B_CNT_BLK
    out.extend_from_slice(&0u32.to_le_bytes()); // PM2_CNT_BLK
    out.extend_from_slice(&(cfg.pm_tmr_blk as u32).to_le_bytes());
    out.extend_from_slice(&(cfg.gpe0_blk as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // GPE1_BLK

    out.push(4); // PM1_EVT_LEN (4 bytes: status + enable)
    out.push(2); // PM1_CNT_LEN
    out.push(0); // PM2_CNT_LEN
    out.push(4); // PM_TMR_LEN
    out.push(cfg.gpe0_blk_len); // GPE0_BLK_LEN
    out.push(0); // GPE1_BLK_LEN
    out.push(0); // GPE1_BASE
    out.push(0); // CST_CNT

    out.extend_from_slice(&0u16.to_le_bytes()); // P_LVL2_LAT
    out.extend_from_slice(&0u16.to_le_bytes()); // P_LVL3_LAT
    out.extend_from_slice(&0u16.to_le_bytes()); // FLUSH_SIZE
    out.extend_from_slice(&0u16.to_le_bytes()); // FLUSH_STRIDE
    out.push(0); // DUTY_OFFSET
    out.push(0); // DUTY_WIDTH
    out.push(0); // DAY_ALRM
    out.push(0); // MON_ALRM
    out.push(0); // CENTURY
    out.extend_from_slice(&(0x0003u16).to_le_bytes()); // IAPC_BOOT_ARCH (legacy devices + 8042)
    out.push(0); // reserved

    let flags = 0x0000_0400u32; // RESET_REG_SUP
    out.extend_from_slice(&flags.to_le_bytes());

    // RESET_REG + RESET_VALUE (use standard PCI reset port 0xCF9).
    let reset_reg = Gas::new_io_with_access(8, 1, 0x0CF9);
    out.extend_from_slice(&reset_reg.as_bytes());
    out.push(0x06); // RESET_VALUE
    out.extend_from_slice(&0u16.to_le_bytes()); // ARM_BOOT_ARCH
    out.push(0); // FADT_MINOR_VERSION

    // X_FIRMWARE_CTRL + X_DSDT
    out.extend_from_slice(&facs_addr.to_le_bytes());
    out.extend_from_slice(&dsdt_addr.to_le_bytes());

    // Extended GAS fields
    let x_pm1a_evt = Gas::new_io(32, cfg.pm1a_evt_blk);
    let x_pm1b_evt = Gas::new_io(0, 0);
    let x_pm1a_cnt = Gas::new_io(16, cfg.pm1a_cnt_blk);
    let x_pm1b_cnt = Gas::new_io(0, 0);
    let x_pm2_cnt = Gas::new_io(0, 0);
    let x_pm_tmr = Gas::new_io(32, cfg.pm_tmr_blk);
    let x_gpe0 = Gas::new_io(cfg.gpe0_blk_len.saturating_mul(8), cfg.gpe0_blk);
    let x_gpe1 = Gas::new_io(0, 0);

    out.extend_from_slice(&x_pm1a_evt.as_bytes());
    out.extend_from_slice(&x_pm1b_evt.as_bytes());
    out.extend_from_slice(&x_pm1a_cnt.as_bytes());
    out.extend_from_slice(&x_pm1b_cnt.as_bytes());
    out.extend_from_slice(&x_pm2_cnt.as_bytes());
    out.extend_from_slice(&x_pm_tmr.as_bytes());
    out.extend_from_slice(&x_gpe0.as_bytes());
    out.extend_from_slice(&x_gpe1.as_bytes());

    debug_assert_eq!(out.len(), FADT_LEN);

    finalize_sdt(out)
}

// MADT Interrupt Source Override (ISO) flags.
//
// Encoding is defined by ACPI ("MPS INTI Flags"):
// - bits 1:0 = polarity
// - bits 3:2 = trigger mode
const ISO_POLARITY_CONFORMS: u16 = 0b00;
const ISO_POLARITY_ACTIVE_LOW: u16 = 0b11;
const ISO_TRIGGER_CONFORMS: u16 = 0b00 << 2;
const ISO_TRIGGER_LEVEL: u16 = 0b11 << 2;

const ISO_ACTIVE_LOW_LEVEL: u16 = ISO_POLARITY_ACTIVE_LOW | ISO_TRIGGER_LEVEL;

fn build_madt(cfg: &AcpiConfig) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&cfg.local_apic_addr.to_le_bytes());
    body.extend_from_slice(&1u32.to_le_bytes()); // flags: PCAT_COMPAT

    // Processor Local APIC entries.
    for cpu_id in 0..cfg.cpu_count {
        body.push(0); // type
        body.push(8); // length
        body.push(cpu_id); // ACPI Processor ID
        body.push(cpu_id); // APIC ID
        body.extend_from_slice(&1u32.to_le_bytes()); // flags: enabled
    }

    // I/O APIC entry.
    body.push(1); // type
    body.push(12); // length
    body.push(0); // IOAPIC ID
    body.push(0); // reserved
    body.extend_from_slice(&cfg.io_apic_addr.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // GSI base

    // Interrupt Source Override: ISA IRQ0 -> GSI2 (PIT).
    body.extend_from_slice(&madt_iso(
        0,
        0,
        2,
        ISO_POLARITY_CONFORMS | ISO_TRIGGER_CONFORMS,
    ));
    // Interrupt Source Override: ISA IRQ9 -> GSI9 (SCI), active low, level triggered.
    body.extend_from_slice(&madt_iso(
        0,
        cfg.sci_irq,
        cfg.sci_irq as u32,
        ISO_ACTIVE_LOW_LEVEL,
    ));

    // Local APIC NMI: LINT1 for all processors.
    body.extend_from_slice(&madt_lapic_nmi(0xFF, 0x0000, 1));

    let total_len = 36 + body.len();
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&build_sdt_header(*b"APIC", 3, total_len as u32, cfg));
    out.extend_from_slice(&body);
    finalize_sdt(out)
}

fn madt_iso(bus: u8, source_irq: u8, gsi: u32, flags: u16) -> [u8; 10] {
    let mut out = [0u8; 10];
    out[0] = 2;
    out[1] = 10;
    out[2] = bus;
    out[3] = source_irq;
    out[4..8].copy_from_slice(&gsi.to_le_bytes());
    out[8..10].copy_from_slice(&flags.to_le_bytes());
    out
}

fn madt_lapic_nmi(acpi_processor_id: u8, flags: u16, lint: u8) -> [u8; 6] {
    let mut out = [0u8; 6];
    out[0] = 4;
    out[1] = 6;
    out[2] = acpi_processor_id;
    out[3..5].copy_from_slice(&flags.to_le_bytes());
    out[5] = lint;
    out
}

fn build_hpet(cfg: &AcpiConfig) -> Vec<u8> {
    let total_len = 56;
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&build_sdt_header(*b"HPET", 1, total_len as u32, cfg));

    // Event Timer Block ID:
    // - hardware rev id: 0x01
    // - number of comparators: 3 (encoded as N-1 -> 2)
    // - counter size: 1 (64-bit)
    // - vendor id: 0x8086 (Intel)
    let hw_rev: u32 = 0x01;
    let comparators: u32 = 2 << 8;
    let counter_size: u32 = 1 << 13;
    let legacy_route: u32 = 1 << 15;
    let vendor: u32 = 0x8086 << 16;
    let block_id = hw_rev | comparators | counter_size | legacy_route | vendor;
    out.extend_from_slice(&block_id.to_le_bytes());

    let gas = Gas::new_mmio(cfg.hpet_addr);
    out.extend_from_slice(&gas.as_bytes());

    out.push(0); // HPET number
    out.extend_from_slice(&0x0080u16.to_le_bytes()); // minimum clock tick
    out.push(0); // page protection

    debug_assert_eq!(out.len(), total_len);
    finalize_sdt(out)
}

fn build_dsdt(cfg: &AcpiConfig) -> Vec<u8> {
    let aml = build_dsdt_aml(cfg);
    let total_len = 36 + aml.len();
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&build_sdt_header(*b"DSDT", 2, total_len as u32, cfg));
    out.extend_from_slice(&aml);
    finalize_sdt(out)
}

fn build_dsdt_aml(cfg: &AcpiConfig) -> Vec<u8> {
    // AML is emitted manually (minimal subset).
    let mut out = Vec::new();

    // Name (PICM, Zero)
    out.extend_from_slice(&aml_name_integer(*b"PICM", 0));
    // OperationRegion (IMCR, SystemIO, 0x22, 0x02)
    out.extend_from_slice(&aml_op_region(
        *b"IMCR",
        0x01, // SystemIO
        0x22,
        0x02,
    ));
    // Field (IMCR, ByteAcc, NoLock, Preserve) { IMCS, 8, IMCD, 8 }
    out.extend_from_slice(&aml_field(
        *b"IMCR",
        0x01, // ByteAcc + NoLock + Preserve
        &[
            (*b"IMCS", 8), // IMCR select port (0x22)
            (*b"IMCD", 8), // IMCR data port (0x23)
        ],
    ));
    // Method (_PIC, 1, NotSerialized)
    // {
    //   Store (Arg0, PICM)
    //   Store (0x70, IMCS)
    //   And (Arg0, One, IMCD)
    // }
    out.extend_from_slice(&aml_method_pic());

    // Scope (_SB_) { ... }
    let mut sb = Vec::new();
    sb.extend_from_slice(&aml_device_pci0(cfg));
    sb.extend_from_slice(&aml_device_hpet(cfg));
    sb.extend_from_slice(&aml_device_rtc());
    sb.extend_from_slice(&aml_device_timr());
    out.extend_from_slice(&aml_scope(*b"_SB_", &sb));

    // Scope (_PR_) { Processor (CPUx, ...) }
    let mut pr = Vec::new();
    for cpu_id in 0..cfg.cpu_count {
        pr.extend_from_slice(&aml_processor(cpu_id));
    }
    out.extend_from_slice(&aml_scope(*b"_PR_", &pr));

    // Name (_S5_, Package () { 0x05, 0x05 })
    out.extend_from_slice(&aml_s5());

    out
}

fn aml_pkg_length(len: usize) -> Vec<u8> {
    // Encoding used by ACPICA; bits 4-5 are reserved and set to zero when
    // additional bytes are present.
    if len <= 0x3F {
        return vec![len as u8];
    }
    if len <= 0x0FFF {
        return vec![((len & 0x0F) as u8) | 0x40, (len >> 4) as u8];
    }
    if len <= 0x0F_FFFF {
        return vec![
            ((len & 0x0F) as u8) | 0x80,
            (len >> 4) as u8,
            (len >> 12) as u8,
        ];
    }
    vec![
        ((len & 0x0F) as u8) | 0xC0,
        (len >> 4) as u8,
        (len >> 12) as u8,
        (len >> 20) as u8,
    ]
}

fn aml_name_integer(name: [u8; 4], value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x08); // NameOp
    out.extend_from_slice(&name);
    out.extend_from_slice(&aml_integer(value));
    out
}

fn aml_integer(value: u64) -> Vec<u8> {
    match value {
        0 => vec![0x00], // ZeroOp
        1 => vec![0x01], // OneOp
        v if v <= u8::MAX as u64 => vec![0x0A, v as u8],
        v if v <= u16::MAX as u64 => {
            let mut out = vec![0x0B];
            out.extend_from_slice(&(v as u16).to_le_bytes());
            out
        }
        v if v <= u32::MAX as u64 => {
            let mut out = vec![0x0C];
            out.extend_from_slice(&(v as u32).to_le_bytes());
            out
        }
        v => {
            let mut out = vec![0x0E];
            out.extend_from_slice(&v.to_le_bytes());
            out
        }
    }
}

fn aml_scope(name: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&name);
    payload.extend_from_slice(body);

    let mut out = Vec::new();
    out.push(0x10); // ScopeOp
    out.extend_from_slice(&aml_pkg_length(payload.len()));
    out.extend_from_slice(&payload);
    out
}

fn aml_device(name: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&name);
    payload.extend_from_slice(body);

    let mut out = Vec::new();
    out.extend_from_slice(&[0x5B, 0x82]); // DeviceOp
    out.extend_from_slice(&aml_pkg_length(payload.len()));
    out.extend_from_slice(&payload);
    out
}

fn aml_processor(cpu_id: u8) -> Vec<u8> {
    // ACPI NameSeg is always 4 bytes, so we need a 4-character scheme.
    // Common firmware uses CPU0..CPUx; we support:
    // - CPU0..CPUF for 0..=15
    // - CP10..CPFF for 16..=255
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let name = if cpu_id < 16 {
        [b'C', b'P', b'U', HEX[cpu_id as usize]]
    } else {
        [
            b'C',
            b'P',
            HEX[(cpu_id >> 4) as usize],
            HEX[(cpu_id & 0x0F) as usize],
        ]
    };

    let mut payload = Vec::new();
    payload.extend_from_slice(&name);
    payload.push(cpu_id); // Processor ID
    payload.extend_from_slice(&0x0000_0810u32.to_le_bytes()); // PblkAddress
    payload.push(0x06); // PblkLength

    let mut out = Vec::new();
    out.extend_from_slice(&[0x5B, 0x83]); // ProcessorOp
    out.extend_from_slice(&aml_pkg_length(payload.len()));
    out.extend_from_slice(&payload);
    out
}

fn aml_op_region(name: [u8; 4], space: u8, offset: u64, len: u64) -> Vec<u8> {
    // NOTE: `OperationRegion` is *not* a package opcode in AML, so it does NOT
    // carry a PkgLength byte. Encoding:
    //   ExtOpPrefix(0x5B), OperationRegionOp(0x80),
    //   NameString, RegionSpace, RegionOffset, RegionLen
    let mut out = Vec::new();
    out.extend_from_slice(&[0x5B, 0x80]); // OperationRegionOp
    out.extend_from_slice(&name);
    out.push(space);
    out.extend_from_slice(&aml_integer(offset));
    out.extend_from_slice(&aml_integer(len));
    out
}

fn aml_field(region: [u8; 4], field_flags: u8, fields: &[([u8; 4], usize)]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&region);
    payload.push(field_flags);
    for (name, bits) in fields {
        payload.extend_from_slice(name);
        payload.extend_from_slice(&aml_pkg_length(*bits));
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0x5B, 0x81]); // FieldOp
    out.extend_from_slice(&aml_pkg_length(payload.len()));
    out.extend_from_slice(&payload);
    out
}

fn aml_method_pic() -> Vec<u8> {
    let mut body = Vec::new();
    // Store (Arg0, PICM)
    body.push(0x70); // StoreOp
    body.push(0x68); // Arg0Op
    body.extend_from_slice(b"PICM"); // NameString: NameSeg
    // Store (0x70, IMCS)
    body.push(0x70); // StoreOp
    body.extend_from_slice(&aml_integer(0x70));
    body.extend_from_slice(b"IMCS");
    // And (Arg0, One, IMCD)
    body.push(0x7B); // AndOp
    body.push(0x68); // Arg0Op
    body.push(0x01); // OneOp
    body.extend_from_slice(b"IMCD");

    let mut payload = Vec::new();
    payload.extend_from_slice(b"_PIC");
    payload.push(0x01); // method flags: 1 argument, NotSerialized, sync level 0
    payload.extend_from_slice(&body);

    let mut out = Vec::new();
    out.push(0x14); // MethodOp
    out.extend_from_slice(&aml_pkg_length(payload.len()));
    out.extend_from_slice(&payload);
    out
}

fn aml_s5() -> Vec<u8> {
    // Name (_S5_, Package (2) { 5, 5 })
    let mut pkg = Vec::new();
    pkg.push(0x12); // PackageOp
    let mut pkg_payload = Vec::new();
    pkg_payload.push(0x02); // element count
    pkg_payload.extend_from_slice(&aml_integer(5));
    pkg_payload.extend_from_slice(&aml_integer(5));
    pkg.extend_from_slice(&aml_pkg_length(pkg_payload.len()));
    pkg.extend_from_slice(&pkg_payload);

    let mut out = Vec::new();
    out.push(0x08); // NameOp
    out.extend_from_slice(b"_S5_");
    out.extend_from_slice(&pkg);
    out
}

fn aml_device_pci0(cfg: &AcpiConfig) -> Vec<u8> {
    let mut body = Vec::new();
    let pcie = cfg.pcie_ecam_base != 0;
    if pcie {
        body.extend_from_slice(&aml_name_eisa_id(*b"_HID", "PNP0A08"));
        // Provide a compatible ID for OSes that still look for a legacy PCI root bridge.
        body.extend_from_slice(&aml_name_eisa_id(*b"_CID", "PNP0A03"));
    } else {
        body.extend_from_slice(&aml_name_eisa_id(*b"_HID", "PNP0A03"));
    }
    body.extend_from_slice(&aml_name_integer(*b"_UID", 0));
    body.extend_from_slice(&aml_name_integer(*b"_ADR", 0));
    body.extend_from_slice(&aml_name_integer(*b"_BBN", u64::from(cfg.pcie_start_bus)));
    body.extend_from_slice(&aml_name_integer(*b"_SEG", u64::from(cfg.pcie_segment)));
    if pcie {
        // Base address of the ECAM configuration window.
        body.extend_from_slice(&aml_name_integer(*b"_CBA", cfg.pcie_ecam_base));
    }
    body.extend_from_slice(&aml_name_buffer(*b"_CRS", &pci0_crs(cfg)));
    body.extend_from_slice(&aml_name_pkg(*b"_PRT", &pci0_prt(cfg)));

    aml_device(*b"PCI0", &body)
}

fn aml_device_hpet(cfg: &AcpiConfig) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&aml_name_eisa_id(*b"_HID", "PNP0103"));
    body.extend_from_slice(&aml_name_integer(*b"_UID", 0));
    body.extend_from_slice(&aml_name_integer(*b"_STA", 0x0F));
    body.extend_from_slice(&aml_name_buffer(*b"_CRS", &hpet_crs(cfg)));
    aml_device(*b"HPET", &body)
}

fn aml_device_rtc() -> Vec<u8> {
    // Matches typical PC/AT RTC resources (ports 0x70-0x71, IRQ8).
    let mut body = Vec::new();
    body.extend_from_slice(&aml_name_eisa_id(*b"_HID", "PNP0B00"));
    body.extend_from_slice(&aml_name_integer(*b"_UID", 0));
    body.extend_from_slice(&aml_name_integer(*b"_STA", 0x0F));
    body.extend_from_slice(&aml_name_buffer(*b"_CRS", &rtc_crs()));
    aml_device(*b"RTC_", &body)
}

fn aml_device_timr() -> Vec<u8> {
    // Matches typical PC/AT PIT resources (ports 0x40-0x43, IRQ0).
    let mut body = Vec::new();
    body.extend_from_slice(&aml_name_eisa_id(*b"_HID", "PNP0100"));
    body.extend_from_slice(&aml_name_integer(*b"_UID", 0));
    body.extend_from_slice(&aml_name_integer(*b"_STA", 0x0F));
    body.extend_from_slice(&aml_name_buffer(*b"_CRS", &timr_crs()));
    aml_device(*b"TIMR", &body)
}

fn aml_name_eisa_id(name: [u8; 4], id: &str) -> Vec<u8> {
    let eisa = eisa_id_to_u32(id).expect("invalid EISA ID");
    let mut out = Vec::new();
    out.push(0x08); // NameOp
    out.extend_from_slice(&name);
    out.push(0x0C); // DWordConst
    out.extend_from_slice(&eisa.to_le_bytes());
    out
}

fn aml_name_buffer(name: [u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    // BufferOp: 0x11, pkglen, size, data
    let mut buf_payload = Vec::new();
    buf_payload.extend_from_slice(&aml_integer(bytes.len() as u64));
    buf_payload.extend_from_slice(bytes);
    buf.push(0x11);
    buf.extend_from_slice(&aml_pkg_length(buf_payload.len()));
    buf.extend_from_slice(&buf_payload);

    let mut out = Vec::new();
    out.push(0x08); // NameOp
    out.extend_from_slice(&name);
    out.extend_from_slice(&buf);
    out
}

fn aml_name_pkg(name: [u8; 4], pkg_elements: &[Vec<u8>]) -> Vec<u8> {
    let pkg = aml_package(pkg_elements);
    let mut out = Vec::new();
    out.push(0x08); // NameOp
    out.extend_from_slice(&name);
    out.extend_from_slice(&pkg);
    out
}

fn aml_package(elements: &[Vec<u8>]) -> Vec<u8> {
    assert!(elements.len() <= 0xFF, "AML package too large");
    let mut payload = Vec::new();
    payload.push(elements.len() as u8);
    for el in elements {
        payload.extend_from_slice(el);
    }

    let mut out = Vec::new();
    out.push(0x12); // PackageOp
    out.extend_from_slice(&aml_pkg_length(payload.len()));
    out.extend_from_slice(&payload);
    out
}

fn eisa_id_to_u32(id: &str) -> Option<u32> {
    let bytes = id.as_bytes();
    if bytes.len() != 7 {
        return None;
    }
    let c1 = bytes[0];
    let c2 = bytes[1];
    let c3 = bytes[2];
    if !c1.is_ascii_uppercase() || !c2.is_ascii_uppercase() || !c3.is_ascii_uppercase() {
        return None;
    }

    fn hex_val(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'A'..=b'F' => Some(c - b'A' + 10),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }

    let b0 = ((c1 - b'@') << 2) | ((c2 - b'@') >> 3);
    let b1 = (((c2 - b'@') & 0x07) << 5) | (c3 - b'@');
    let b2 = (hex_val(bytes[3])? << 4) | hex_val(bytes[4])?;
    let b3 = (hex_val(bytes[5])? << 4) | hex_val(bytes[6])?;
    Some(u32::from_le_bytes([b0, b1, b2, b3]))
}

fn pci0_crs(cfg: &AcpiConfig) -> Vec<u8> {
    let mut out = Vec::new();

    // Word Address Space Descriptor (Bus Number).
    let start_bus = u16::from(cfg.pcie_start_bus);
    let end_bus_raw = u16::from(cfg.pcie_end_bus);
    let end_bus = end_bus_raw.max(start_bus);
    let bus_len = end_bus - start_bus + 1;
    out.extend_from_slice(&word_addr_space_descriptor(
        AddrSpaceDescriptorHeader {
            resource_type: 0x02,
            general_flags: 0x0D, // producer + min/max fixed
            type_specific_flags: 0x00,
        },
        AddrSpaceDescriptorRange {
            granularity: 0x0000,
            min: start_bus,
            max: end_bus,
            translation: 0x0000,
            length: bus_len,
        },
    ));

    // I/O Port Descriptor for PCI config mechanism 1 (0xCF8..0xCFF).
    out.extend_from_slice(&io_port_descriptor(0x0CF8, 0x0CF8, 1, 8));

    // Word Address Space Descriptor (I/O): 0x0000..0x0CF7
    out.extend_from_slice(&word_addr_space_descriptor(
        AddrSpaceDescriptorHeader {
            resource_type: 0x01,
            general_flags: 0x0D,
            type_specific_flags: 0x00,
        },
        AddrSpaceDescriptorRange {
            granularity: 0x0000,
            min: 0x0000,
            max: 0x0CF7,
            translation: 0x0000,
            length: 0x0CF8,
        },
    ));

    // Word Address Space Descriptor (I/O): 0x0D00..0xFFFF
    out.extend_from_slice(&word_addr_space_descriptor(
        AddrSpaceDescriptorHeader {
            resource_type: 0x01,
            general_flags: 0x0D,
            type_specific_flags: 0x00,
        },
        AddrSpaceDescriptorRange {
            granularity: 0x0000,
            min: 0x0D00,
            max: 0xFFFF,
            translation: 0x0000,
            length: 0xF300,
        },
    ));

    // DWord Address Space Descriptor (Memory): PCI MMIO window.
    //
    // When ECAM/MMCONFIG is enabled, make sure the configuration space window is not reported as
    // part of the MMIO window available for PCI BAR allocation.
    let mmio_start = u64::from(cfg.pci_mmio_base);
    let mmio_end = mmio_start.saturating_add(u64::from(cfg.pci_mmio_size));
    let pcie = cfg.pcie_ecam_base != 0;
    let ecam_start = cfg.pcie_ecam_base;
    let bus_count = u64::from(cfg.pcie_end_bus.saturating_sub(cfg.pcie_start_bus)) + 1;
    let ecam_end = ecam_start.saturating_add(bus_count.saturating_mul(1 << 20));
    {
        let mut emit_mmio = |range_start: u64, range_end: u64| {
            if range_end <= range_start {
                return;
            }
            let start: u32 = range_start
                .try_into()
                .expect("PCI MMIO window must fit in 32-bit address space");
            let end_inclusive: u32 = range_end
                .saturating_sub(1)
                .try_into()
                .expect("PCI MMIO window must fit in 32-bit address space");
            let len: u32 = range_end
                .saturating_sub(range_start)
                .try_into()
                .expect("PCI MMIO window size must fit in 32-bit address space");

            out.extend_from_slice(&dword_addr_space_descriptor(
                AddrSpaceDescriptorHeader {
                    resource_type: 0x00,
                    general_flags: 0x0D,
                    type_specific_flags: 0x06, // cacheable, read/write
                },
                AddrSpaceDescriptorRange {
                    granularity: 0x0000_0000,
                    min: start,
                    max: end_inclusive,
                    translation: 0x0000_0000,
                    length: len,
                },
            ));
        };

        if !pcie || ecam_end <= mmio_start || ecam_start >= mmio_end {
            emit_mmio(mmio_start, mmio_end);
        } else {
            // Split the MMIO window around the ECAM region (which is described separately by MCFG).
            emit_mmio(mmio_start, ecam_start.min(mmio_end));
            emit_mmio(ecam_end.max(mmio_start), mmio_end);
        }
    }

    // EndTag.
    out.extend_from_slice(&[0x79, 0x00]);
    out
}

fn pci0_prt(cfg: &AcpiConfig) -> Vec<Vec<u8>> {
    // Provide a simple static _PRT mapping for PCI INTx. We follow the common
    // PIRQ swizzle used by many virtual platforms:
    //   PIRQA-D -> GSIs 10,11,12,13.
    let pirq_gsi: [u64; 4] = cfg.pirq_to_gsi.map(u64::from);
    let mut entries = Vec::new();
    for dev in 1u32..=31 {
        let addr = (dev << 16) | 0xFFFF;
        for pin in 0u64..=3 {
            let gsi = pirq_gsi[((dev as usize) + (pin as usize)) & 3];
            entries.push(aml_package(&[
                aml_integer(addr as u64),
                aml_integer(pin),
                aml_integer(0), // Source (always Zero)
                aml_integer(gsi),
            ]));
        }
    }
    entries
}

fn hpet_crs(cfg: &AcpiConfig) -> Vec<u8> {
    let mut out = Vec::new();
    // Memory32Fixed descriptor.
    out.extend_from_slice(&memory32_fixed_descriptor(cfg.hpet_addr as u32, 0x400));
    out.extend_from_slice(&[0x79, 0x00]);
    out
}

#[derive(Debug, Clone, Copy)]
struct AddrSpaceDescriptorHeader {
    resource_type: u8,
    general_flags: u8,
    type_specific_flags: u8,
}

#[derive(Debug, Clone, Copy)]
struct AddrSpaceDescriptorRange<T> {
    granularity: T,
    min: T,
    max: T,
    translation: T,
    length: T,
}

fn word_addr_space_descriptor(
    header: AddrSpaceDescriptorHeader,
    range: AddrSpaceDescriptorRange<u16>,
) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0] = 0x88;
    out[1..3].copy_from_slice(&0x000Du16.to_le_bytes());
    out[3] = header.resource_type;
    out[4] = header.general_flags;
    out[5] = header.type_specific_flags;
    out[6..8].copy_from_slice(&range.granularity.to_le_bytes());
    out[8..10].copy_from_slice(&range.min.to_le_bytes());
    out[10..12].copy_from_slice(&range.max.to_le_bytes());
    out[12..14].copy_from_slice(&range.translation.to_le_bytes());
    out[14..16].copy_from_slice(&range.length.to_le_bytes());
    out
}

fn dword_addr_space_descriptor(
    header: AddrSpaceDescriptorHeader,
    range: AddrSpaceDescriptorRange<u32>,
) -> [u8; 26] {
    let mut out = [0u8; 26];
    out[0] = 0x87;
    out[1..3].copy_from_slice(&0x0017u16.to_le_bytes());
    out[3] = header.resource_type;
    out[4] = header.general_flags;
    out[5] = header.type_specific_flags;
    out[6..10].copy_from_slice(&range.granularity.to_le_bytes());
    out[10..14].copy_from_slice(&range.min.to_le_bytes());
    out[14..18].copy_from_slice(&range.max.to_le_bytes());
    out[18..22].copy_from_slice(&range.translation.to_le_bytes());
    out[22..26].copy_from_slice(&range.length.to_le_bytes());
    out
}

fn io_port_descriptor(min: u16, max: u16, alignment: u8, length: u8) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0] = 0x47; // tag + length
    out[1] = 0x01; // decode16
    out[2..4].copy_from_slice(&min.to_le_bytes());
    out[4..6].copy_from_slice(&max.to_le_bytes());
    out[6] = alignment;
    out[7] = length;
    out
}

fn memory32_fixed_descriptor(address: u32, length: u32) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0] = 0x86;
    out[1..3].copy_from_slice(&0x0009u16.to_le_bytes());
    out[3] = 0; // read/write
    out[4..8].copy_from_slice(&address.to_le_bytes());
    out[8..12].copy_from_slice(&length.to_le_bytes());
    out
}

fn rtc_crs() -> Vec<u8> {
    let mut out = Vec::new();
    // IO(Decode16, 0x70, 0x70, 1, 2)
    out.extend_from_slice(&io_port_descriptor(0x0070, 0x0070, 1, 2));
    // IRQNoFlags {8} => bitmask 1<<8 = 0x0100
    out.extend_from_slice(&[0x22, 0x00, 0x01]);
    out.extend_from_slice(&[0x79, 0x00]);
    out
}

fn timr_crs() -> Vec<u8> {
    let mut out = Vec::new();
    // IO(Decode16, 0x40, 0x40, 1, 4)
    out.extend_from_slice(&io_port_descriptor(0x0040, 0x0040, 1, 4));
    // IRQNoFlags {0} => bitmask 1<<0 = 0x0001
    out.extend_from_slice(&[0x22, 0x01, 0x00]);
    out.extend_from_slice(&[0x79, 0x00]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn pkg_length_encoding_matches_acpica_examples() {
        assert_eq!(aml_pkg_length(0x3F), vec![0x3F]);
        assert_eq!(aml_pkg_length(0x40), vec![0x40, 0x04]);
        assert_eq!(aml_pkg_length(0x70), vec![0x40, 0x07]);
        assert_eq!(aml_pkg_length(0x0FFF), vec![0x4F, 0xFF]);
        assert_eq!(aml_pkg_length(0x1000), vec![0x80, 0x00, 0x01]);
    }

    #[test]
    fn eisa_id_encoding_matches_known_values() {
        assert_eq!(eisa_id_to_u32("PNP0A03"), Some(0x030A_D041));
        assert_eq!(eisa_id_to_u32("PNP0A08"), Some(0x080A_D041));
        assert_eq!(eisa_id_to_u32("PNP0103"), Some(0x0301_D041));
    }

    #[test]
    fn rsdp_checksums_validate() {
        let cfg = AcpiConfig::default();
        let rsdp = build_rsdp(&cfg, 0x1122_3344, 0x5566_7788_99AA_BBCC);
        assert_eq!(rsdp.len(), 36);
        assert_eq!(&rsdp[0..8], b"RSD PTR ");
        assert_eq!(checksum(&rsdp[..20]), 0);
        assert_eq!(checksum(&rsdp), 0);
    }

    #[test]
    fn placement_alignment_supports_non_power_of_two_values() {
        // `AcpiPlacement::alignment` is a configuration knob and should behave sensibly even when
        // set to values that are not powers of two.
        let cfg = AcpiConfig::default();
        let placement = AcpiPlacement {
            alignment: 24,
            ..Default::default()
        };
        let tables = AcpiTables::build(&cfg, placement);
        let addrs = tables.addresses;

        for (name, addr) in [
            ("DSDT", addrs.dsdt),
            ("FACS", addrs.facs),
            ("FADT", addrs.fadt),
            ("MADT", addrs.madt),
            ("HPET", addrs.hpet),
            ("RSDT", addrs.rsdt),
            ("XSDT", addrs.xsdt),
        ] {
            assert_eq!(
                addr % placement.alignment,
                0,
                "{name} not aligned to {} (addr=0x{addr:x})",
                placement.alignment
            );
        }
    }

    #[test]
    fn dsdt_uses_legacy_pci_hid_when_ecam_disabled() {
        let cfg = AcpiConfig::default();
        assert_eq!(cfg.pcie_ecam_base, 0);

        let dsdt = build_dsdt(&cfg);

        let pnp0a03 = eisa_id_to_u32("PNP0A03").unwrap().to_le_bytes();
        let hid_pnp0a03 = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0a03[..]].concat();
        let cid_pnp0a03 = [&[0x08][..], &b"_CID"[..], &[0x0C][..], &pnp0a03[..]].concat();

        assert!(
            contains_subslice(&dsdt, &hid_pnp0a03),
            "expected PCI0._HID to be PNP0A03 when ECAM is disabled"
        );
        assert!(
            !contains_subslice(&dsdt, &cid_pnp0a03),
            "did not expect PCI0._CID when ECAM is disabled"
        );
    }

    #[test]
    fn dsdt_uses_pcie_pci_hid_when_ecam_enabled() {
        let cfg = AcpiConfig {
            pcie_ecam_base: 0xC000_0000,
            ..Default::default()
        };

        let dsdt = build_dsdt(&cfg);

        let pnp0a03 = eisa_id_to_u32("PNP0A03").unwrap().to_le_bytes();
        let pnp0a08 = eisa_id_to_u32("PNP0A08").unwrap().to_le_bytes();
        let hid_pnp0a03 = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0a03[..]].concat();
        let hid_pnp0a08 = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0a08[..]].concat();
        let cid_pnp0a03 = [&[0x08][..], &b"_CID"[..], &[0x0C][..], &pnp0a03[..]].concat();

        assert!(
            contains_subslice(&dsdt, &hid_pnp0a08),
            "expected PCI0._HID to be PNP0A08 when ECAM is enabled"
        );
        assert!(
            contains_subslice(&dsdt, &cid_pnp0a03),
            "expected PCI0._CID to include legacy PNP0A03 when ECAM is enabled"
        );
        assert!(
            !contains_subslice(&dsdt, &hid_pnp0a03),
            "did not expect PCI0._HID to be PNP0A03 when ECAM is enabled"
        );
    }

    #[test]
    fn dsdt_pic_method_programs_imcr() {
        let cfg = AcpiConfig::default();
        let placement = AcpiPlacement::default();
        let tables = AcpiTables::build(&cfg, placement);
        let aml = &tables.dsdt[36..];

        // OperationRegion (IMCR, SystemIO, 0x22, 0x02)
        let op_region = [
            &[0x5B, 0x80][..], // OperationRegionOp
            &b"IMCR"[..],
            &[0x01, 0x0A, 0x22, 0x0A, 0x02][..], // SystemIO, 0x22, len 2
        ]
        .concat();
        assert!(
            contains_subslice(aml, &op_region),
            "expected DSDT AML to contain IMCR SystemIO OperationRegion at ports 0x22..0x23"
        );

        // Method (_PIC, 1) {
        //   Store (Arg0, PICM)
        //   Store (0x70, IMCS)
        //   And (Arg0, One, IMCD)
        // }
        let pic_body = [
            &b"_PIC"[..],
            &[0x01][..], // flags: 1 arg
            &[0x70, 0x68][..],
            &b"PICM"[..],
            &[0x70, 0x0A, 0x70][..],
            &b"IMCS"[..],
            &[0x7B, 0x68, 0x01][..],
            &b"IMCD"[..],
        ]
        .concat();
        assert!(
            contains_subslice(aml, &pic_body),
            "expected _PIC method to program the IMCR (0x22/0x23) for PIC/APIC routing"
        );
    }
}
