use aero_acpi::{
    AcpiConfig, AcpiPlacement, AcpiTables, PhysicalMemory, DEFAULT_ACPI_ALIGNMENT,
    DEFAULT_ACPI_NVS_SIZE,
};

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

fn parse_pkg_length(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    let b0 = *bytes.get(offset)?;
    let follow_bytes = (b0 >> 6) as usize;
    let mut len: usize = (b0 & 0x3F) as usize;
    for i in 0..follow_bytes {
        let b = *bytes.get(offset + 1 + i)?;
        len |= (b as usize) << (4 + i * 8);
    }
    Some((len, 1 + follow_bytes))
}

fn parse_integer(bytes: &[u8], offset: usize) -> Option<(u64, usize)> {
    match *bytes.get(offset)? {
        0x00 => Some((0, 1)), // ZeroOp
        0x01 => Some((1, 1)), // OneOp
        0x0A => Some((*bytes.get(offset + 1)? as u64, 2)), // BytePrefix
        0x0B => Some((
            u16::from_le_bytes(bytes.get(offset + 1..offset + 3)?.try_into().ok()?) as u64,
            3,
        )),
        0x0C => Some((
            u32::from_le_bytes(bytes.get(offset + 1..offset + 5)?.try_into().ok()?) as u64,
            5,
        )),
        0x0E => Some((
            u64::from_le_bytes(bytes.get(offset + 1..offset + 9)?.try_into().ok()?),
            9,
        )),
        _ => None,
    }
}

/// Parse the static `_PRT` package emitted by the DSDT AML.
///
/// Returns entries of the form: (PCI address, pin, GSI).
fn parse_prt_entries(aml: &[u8]) -> Option<Vec<(u32, u8, u32)>> {
    // Look for: NameOp (0x08) + NameSeg("_PRT")
    let mut prt_off = None;
    for i in 0..aml.len().saturating_sub(5) {
        if aml[i] == 0x08 && &aml[i + 1..i + 5] == b"_PRT" {
            prt_off = Some(i);
            break;
        }
    }
    let prt_off = prt_off?;

    let mut offset = prt_off + 1 + 4;
    if *aml.get(offset)? != 0x12 {
        return None; // PackageOp
    }
    offset += 1;

    let (pkg_len, pkg_len_bytes) = parse_pkg_length(aml, offset)?;
    offset += pkg_len_bytes;
    let pkg_end = offset + pkg_len;

    let count = *aml.get(offset)? as usize;
    offset += 1;

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if *aml.get(offset)? != 0x12 {
            return None;
        }
        offset += 1;
        let (entry_len, entry_len_bytes) = parse_pkg_length(aml, offset)?;
        offset += entry_len_bytes;
        let entry_end = offset + entry_len;

        let entry_count = *aml.get(offset)? as usize;
        if entry_count != 4 {
            return None;
        }
        offset += 1;

        let (addr, addr_bytes) = parse_integer(aml, offset)?;
        offset += addr_bytes;
        let (pin, pin_bytes) = parse_integer(aml, offset)?;
        offset += pin_bytes;
        let (source, source_bytes) = parse_integer(aml, offset)?;
        offset += source_bytes;
        if source != 0 {
            return None;
        }
        let (gsi, gsi_bytes) = parse_integer(aml, offset)?;
        offset += gsi_bytes;

        if offset != entry_end {
            return None;
        }
        out.push((addr as u32, pin as u8, gsi as u32));
    }

    if offset != pkg_end {
        return None;
    }

    Some(out)
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
    cfg.pirq_to_gsi = [20, 21, 22, 23];

    let placement = AcpiPlacement {
        tables_base: 0x0010_0000,
        nvs_base: 0x0011_0000,
        nvs_size: DEFAULT_ACPI_NVS_SIZE,
        rsdp_addr: 0x000F_0000,
        alignment: DEFAULT_ACPI_ALIGNMENT,
    };

    let tables = AcpiTables::build(&cfg, placement);

    // Allocate enough physical memory to cover all the written tables.
    let max_end = [
        tables.addresses.rsdp + tables.rsdp.len() as u64,
        tables.addresses.xsdt + tables.xsdt.len() as u64,
        tables.addresses.facs + tables.facs.len() as u64,
    ]
    .into_iter()
    .max()
    .unwrap();
    let mem_size = (max_end + 0x1000) as usize;
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
    assert!(
        tables.addresses.facs >= placement.nvs_base
            && tables.addresses.facs + tables.facs.len() as u64 <= placement.nvs_base + placement.nvs_size
    );

    // PM blocks should match config and be internally consistent.
    assert_eq!(read_u32_le(fadt, 56) as u16, cfg.pm1a_evt_blk);
    assert_eq!(read_u32_le(fadt, 64) as u16, cfg.pm1a_cnt_blk);
    assert_eq!(read_u32_le(fadt, 76) as u16, cfg.pm_tmr_blk);
    assert_eq!(read_u32_le(fadt, 80) as u16, cfg.gpe0_blk);
    assert_eq!(fadt[88], 4); // PM1_EVT_LEN
    assert_eq!(fadt[89], 2); // PM1_CNT_LEN
    assert_eq!(fadt[91], 4); // PM_TMR_LEN
    assert_eq!(fadt[92], cfg.gpe0_blk_len);

    // ACPI enable/disable handshake fields.
    assert_ne!(read_u32_le(fadt, 48), 0, "SMI_CMD must be populated");
    assert_eq!(read_u32_le(fadt, 48) as u16, cfg.smi_cmd_port);
    assert_eq!(fadt[52], cfg.acpi_enable_cmd);
    assert_eq!(fadt[53], cfg.acpi_disable_cmd);

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

    let prt = parse_prt_entries(aml).expect("_PRT package should parse");
    assert_eq!(prt.len(), 31 * 4);
    let mut expected = Vec::new();
    for dev in 1u32..=31 {
        let addr = (dev << 16) | 0xFFFF;
        for pin in 0u8..=3 {
            let pirq = ((dev as usize) + (pin as usize)) & 3;
            let gsi = cfg.pirq_to_gsi[pirq];
            expected.push((addr, pin, gsi));
        }
    }
    assert_eq!(prt, expected);
    for cpu_id in 0..cfg.cpu_count {
        let name = if cpu_id < 16 {
            let b = b"0123456789ABCDEF"[cpu_id as usize];
            [b'C', b'P', b'U', b]
        } else {
            let hi = b"0123456789ABCDEF"[(cpu_id >> 4) as usize];
            let lo = b"0123456789ABCDEF"[(cpu_id & 0x0F) as usize];
            [b'C', b'P', hi, lo]
        };
        assert!(
            aml.windows(4).any(|w| w == name),
            "missing CPU object for {:?}",
            core::str::from_utf8(&name).unwrap()
        );
    }

    // FACS signature/length.
    let facs = mem.read(tables.addresses.facs, 64);
    assert_eq!(&facs[0..4], b"FACS");
    assert_eq!(read_u32_le(facs, 4), 64);
    assert_eq!(facs[32], 2);

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
    let mut lapic_ids = Vec::new();
    let mut found_irq0_iso = false;
    let mut found_sci_iso = false;
    while off < madt.len() {
        let entry_type = madt[off];
        let entry_len = madt[off + 1] as usize;
        assert!(entry_len >= 2);
        match entry_type {
            0 => {
                let acpi_id = madt[off + 2];
                let apic_id = madt[off + 3];
                assert_eq!(acpi_id, apic_id);
                assert!(acpi_id < cfg.cpu_count);
                lapic_ids.push(acpi_id);
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
    lapic_ids.sort_unstable();
    assert_eq!(lapic_ids.len(), cfg.cpu_count as usize);
    assert_eq!(
        lapic_ids,
        (0..cfg.cpu_count).collect::<Vec<u8>>(),
        "MADT LAPIC IDs do not match cpu_count"
    );
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
