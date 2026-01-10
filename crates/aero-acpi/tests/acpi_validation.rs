use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables, PhysicalMemory, DEFAULT_ACPI_ALIGNMENT};

struct TestMemory {
    mem: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn read(&self, paddr: u64, len: usize) -> &[u8] {
        let start = paddr as usize;
        let end = start + len;
        &self.mem[start..end]
    }
}

impl PhysicalMemory for TestMemory {
    fn write(&mut self, paddr: u64, bytes: &[u8]) {
        let start = paddr as usize;
        let end = start + bytes.len();
        self.mem[start..end].copy_from_slice(bytes);
    }
}

fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

fn read_u16_le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

#[derive(Debug)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
}

fn parse_sdt_header(buf: &[u8]) -> SdtHeader {
    let signature = [buf[0], buf[1], buf[2], buf[3]];
    let length = read_u32_le(buf, 4);
    let revision = buf[8];
    SdtHeader {
        signature,
        length,
        revision,
    }
}

#[test]
fn generated_tables_are_self_consistent_and_checksums_pass() {
    let mut cfg = AcpiConfig::default();
    cfg.cpu_count = 2;
    cfg.local_apic_addr = 0xFEE0_0000;
    cfg.io_apic_addr = 0xFEC0_0000;
    cfg.hpet_addr = 0xFED0_0000;
    cfg.sci_irq = 9;

    let placement = AcpiPlacement {
        tables_base: 0x0010_0000,
        rsdp_addr: 0x000F_0000,
        alignment: DEFAULT_ACPI_ALIGNMENT,
    };

    let tables = AcpiTables::build(&cfg, placement);

    // Allocate enough physical memory to cover all the written tables.
    let mem_size = (tables.addresses.xsdt + tables.xsdt.len() as u64 + 0x1000) as usize;
    let mut mem = TestMemory::new(mem_size);
    tables.write_to(&mut mem);

    // --- RSDP ---
    let rsdp = mem.read(tables.addresses.rsdp, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");
    assert_eq!(rsdp[15], 2); // revision
    assert_eq!(checksum(&rsdp[..20]), 0, "RSDP v1 checksum");
    assert_eq!(checksum(rsdp), 0, "RSDP extended checksum");

    let rsdt_addr = read_u32_le(rsdp, 16) as u64;
    let xsdt_addr = read_u64_le(rsdp, 24);
    assert_eq!(rsdt_addr, tables.addresses.rsdt);
    assert_eq!(xsdt_addr, tables.addresses.xsdt);
    assert_eq!(tables.addresses.rsdp % 16, 0);

    // --- XSDT ---
    let xsdt_hdr_raw = mem.read(xsdt_addr, 36);
    let xsdt_hdr = parse_sdt_header(xsdt_hdr_raw);
    assert_eq!(&xsdt_hdr.signature, b"XSDT");
    assert_eq!(xsdt_hdr.revision, 1);
    assert_eq!(checksum(mem.read(xsdt_addr, xsdt_hdr.length as usize)), 0);
    assert!(xsdt_hdr.length >= 36);
    let xsdt_entries = (xsdt_hdr.length as usize - 36) / 8;
    assert_eq!(xsdt_entries, 3);

    let xsdt_blob = mem.read(xsdt_addr, xsdt_hdr.length as usize);
    let mut found = Vec::new();
    for i in 0..xsdt_entries {
        found.push(read_u64_le(xsdt_blob, 36 + i * 8));
    }
    found.sort_unstable();
    let mut expected = vec![tables.addresses.fadt, tables.addresses.madt, tables.addresses.hpet];
    expected.sort_unstable();
    assert_eq!(found, expected);

    // --- RSDT ---
    let rsdt_hdr_raw = mem.read(rsdt_addr, 36);
    let rsdt_hdr = parse_sdt_header(rsdt_hdr_raw);
    assert_eq!(&rsdt_hdr.signature, b"RSDT");
    assert_eq!(rsdt_hdr.revision, 1);
    assert_eq!(checksum(mem.read(rsdt_addr, rsdt_hdr.length as usize)), 0);
    let rsdt_entries = (rsdt_hdr.length as usize - 36) / 4;
    assert_eq!(rsdt_entries, 3);

    // --- FADT / DSDT / FACS ---
    let fadt_hdr_raw = mem.read(tables.addresses.fadt, 36);
    let fadt_hdr = parse_sdt_header(fadt_hdr_raw);
    assert_eq!(&fadt_hdr.signature, b"FACP");
    assert_eq!(fadt_hdr.revision, 3);
    assert_eq!(fadt_hdr.length as usize, tables.fadt.len());
    assert_eq!(checksum(mem.read(tables.addresses.fadt, fadt_hdr.length as usize)), 0);

    let fadt = mem.read(tables.addresses.fadt, fadt_hdr.length as usize);
    let dsdt32 = read_u32_le(fadt, 40) as u64;
    let facs32 = read_u32_le(fadt, 36) as u64;
    let x_dsdt = read_u64_le(fadt, 140);
    let x_facs = read_u64_le(fadt, 132);

    assert_eq!(dsdt32, tables.addresses.dsdt);
    assert_eq!(x_dsdt, tables.addresses.dsdt);
    assert_eq!(facs32, tables.addresses.facs);
    assert_eq!(x_facs, tables.addresses.facs);

    // PM blocks should match config and be internally consistent.
    assert_eq!(read_u32_le(fadt, 56) as u16, cfg.pm1a_evt_blk);
    assert_eq!(read_u32_le(fadt, 64) as u16, cfg.pm1a_cnt_blk);
    assert_eq!(read_u32_le(fadt, 76) as u16, cfg.pm_tmr_blk);
    assert_eq!(read_u32_le(fadt, 80) as u16, cfg.gpe0_blk);
    assert_eq!(fadt[88], 4); // PM1_EVT_LEN
    assert_eq!(fadt[89], 2); // PM1_CNT_LEN
    assert_eq!(fadt[91], 4); // PM_TMR_LEN
    assert_eq!(fadt[92], cfg.gpe0_blk_len);

    // DSDT header + checksum.
    let dsdt_hdr_raw = mem.read(tables.addresses.dsdt, 36);
    let dsdt_hdr = parse_sdt_header(dsdt_hdr_raw);
    assert_eq!(&dsdt_hdr.signature, b"DSDT");
    assert_eq!(dsdt_hdr.revision, 2);
    assert_eq!(checksum(mem.read(tables.addresses.dsdt, dsdt_hdr.length as usize)), 0);

    // Minimal AML sanity check: should contain the device names we emit.
    let dsdt = mem.read(tables.addresses.dsdt, dsdt_hdr.length as usize);
    let aml = &dsdt[36..];
    assert!(aml.windows(4).any(|w| w == b"PCI0"));
    assert!(aml.windows(4).any(|w| w == b"HPET"));
    assert!(aml.windows(4).any(|w| w == b"_PRT"));
    assert!(aml.windows(4).any(|w| w == b"RTC_"));
    assert!(aml.windows(4).any(|w| w == b"TIMR"));

    // FACS signature/length.
    let facs = mem.read(tables.addresses.facs, 64);
    assert_eq!(&facs[0..4], b"FACS");
    assert_eq!(read_u32_le(facs, 4), 64);

    // --- MADT ---
    let madt_hdr_raw = mem.read(tables.addresses.madt, 36);
    let madt_hdr = parse_sdt_header(madt_hdr_raw);
    assert_eq!(&madt_hdr.signature, b"APIC");
    assert_eq!(madt_hdr.revision, 3);
    assert_eq!(checksum(mem.read(tables.addresses.madt, madt_hdr.length as usize)), 0);

    let madt = mem.read(tables.addresses.madt, madt_hdr.length as usize);
    assert_eq!(read_u32_le(madt, 36), cfg.local_apic_addr);
    assert_eq!(read_u32_le(madt, 40), 1); // PCAT_COMPAT

    // Count processor LAPIC entries and check for ISOs.
    let mut off = 44;
    let mut lapic_count = 0;
    let mut found_irq0_iso = false;
    let mut found_sci_iso = false;
    while off < madt.len() {
        let entry_type = madt[off];
        let entry_len = madt[off + 1] as usize;
        assert!(entry_len >= 2);
        match entry_type {
            0 => {
                lapic_count += 1;
            }
            2 => {
                let src = madt[off + 3];
                let gsi = read_u32_le(madt, off + 4);
                if src == 0 && gsi == 2 {
                    found_irq0_iso = true;
                }
                if src == cfg.sci_irq && gsi == cfg.sci_irq as u32 {
                    found_sci_iso = true;
                    let flags = read_u16_le(madt, off + 8);
                    assert_eq!(flags, 0x000D);
                }
            }
            _ => {}
        }
        off += entry_len;
    }
    assert_eq!(lapic_count, cfg.cpu_count as usize);
    assert!(found_irq0_iso);
    assert!(found_sci_iso);

    // --- HPET ---
    let hpet_hdr_raw = mem.read(tables.addresses.hpet, 36);
    let hpet_hdr = parse_sdt_header(hpet_hdr_raw);
    assert_eq!(&hpet_hdr.signature, b"HPET");
    assert_eq!(hpet_hdr.revision, 1);
    assert_eq!(checksum(mem.read(tables.addresses.hpet, hpet_hdr.length as usize)), 0);
    let hpet = mem.read(tables.addresses.hpet, hpet_hdr.length as usize);
    let hpet_gas_addr = read_u64_le(hpet, 44);
    assert_eq!(hpet_gas_addr, cfg.hpet_addr);

    // --- Alignment ---
    for &addr in &[
        tables.addresses.dsdt,
        tables.addresses.facs,
        tables.addresses.fadt,
        tables.addresses.madt,
        tables.addresses.hpet,
        tables.addresses.rsdt,
        tables.addresses.xsdt,
    ] {
        assert_eq!(addr % DEFAULT_ACPI_ALIGNMENT, 0);
    }
}
