use aero_acpi::{
    AcpiConfig, AcpiPlacement, AcpiTables, PhysicalMemory, DEFAULT_ACPI_ALIGNMENT,
    DEFAULT_ACPI_NVS_SIZE, FADT_FLAG_FIX_RTC, FADT_FLAG_PWR_BUTTON, FADT_FLAG_RESET_REG_SUP,
    FADT_FLAG_SLP_BUTTON,
};
use aero_pci_routing as pci_routing;

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
        0x00 => Some((0, 1)),                              // ZeroOp
        0x01 => Some((1, 1)),                              // OneOp
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

fn find_device_body<'a>(aml: &'a [u8], device: &[u8; 4]) -> Option<&'a [u8]> {
    // DeviceOp: ExtOpPrefix (0x5B), DeviceOp (0x82), PkgLength, NameSeg, body...
    for i in 0..aml.len().saturating_sub(2) {
        if aml[i] != 0x5B || aml[i + 1] != 0x82 {
            continue;
        }
        let Some((pkg_len, pkg_len_bytes)) = parse_pkg_length(aml, i + 2) else {
            continue;
        };
        let payload_start = i + 2 + pkg_len_bytes;
        let pkg_end = payload_start.checked_add(pkg_len.saturating_sub(pkg_len_bytes))?;
        if pkg_end > aml.len() || payload_start + 4 > pkg_end {
            continue;
        }
        if &aml[payload_start..payload_start + 4] == device {
            return Some(&aml[payload_start + 4..pkg_end]);
        }
    }
    None
}

fn parse_named_buffer<'a>(aml: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    // NameOp (0x08), NameSeg, BufferOp (0x11), PkgLength, BufferSize, data...
    for i in 0..aml.len().saturating_sub(5) {
        if aml[i] != 0x08 || &aml[i + 1..i + 5] != name {
            continue;
        }
        let mut offset = i + 5;
        if *aml.get(offset)? != 0x11 {
            return None;
        }
        offset += 1;
        let (pkg_len, pkg_len_bytes) = parse_pkg_length(aml, offset)?;
        offset += pkg_len_bytes;
        let payload_end = offset + pkg_len.saturating_sub(pkg_len_bytes);
        let (buf_size, size_bytes) = parse_integer(aml, offset)?;
        offset += size_bytes;
        let data_start = offset;
        let data_end = data_start + (buf_size as usize);
        if data_end > payload_end || data_end > aml.len() {
            return None;
        }
        return Some(&aml[data_start..data_end]);
    }
    None
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
    let pkg_payload_len = pkg_len.checked_sub(pkg_len_bytes)?;
    let pkg_end = offset + pkg_payload_len;

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
        let entry_payload_len = entry_len.checked_sub(entry_len_bytes)?;
        let entry_end = offset + entry_payload_len;

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

fn read_sdt_signature(mem: &TestMemory, paddr: u64) -> [u8; 4] {
    let hdr = mem.read(paddr, 4);
    [hdr[0], hdr[1], hdr[2], hdr[3]]
}

#[test]
fn processor_objects_do_not_advertise_unimplemented_pblk_ports() {
    let cfg = AcpiConfig {
        cpu_count: 2,
        ..Default::default()
    };
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let aml = &tables.dsdt[36..];

    // Little-endian encoding of the historical hardcoded PBLK base (0x810).
    // Until we implement the corresponding device model, ProcessorOp must use
    // PblkAddress=0/PblkLength=0 to prevent OSes (e.g. Windows 7) from probing
    // unimplemented I/O ports.
    let needle = 0x0000_0810u32.to_le_bytes();
    assert!(
        !aml.windows(needle.len()).any(|w| w == needle),
        "DSDT AML must not contain legacy PBLK I/O base 0x810"
    );
}

#[test]
fn generated_tables_are_self_consistent_and_checksums_pass() {
    let cfg = AcpiConfig {
        cpu_count: 2,
        pirq_to_gsi: [20, 21, 22, 23],
        ..Default::default()
    };

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
    let mut expected = vec![
        tables.addresses.fadt,
        tables.addresses.madt,
        tables.addresses.hpet,
    ];
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
    assert_eq!(
        checksum(mem.read(tables.addresses.fadt, fadt_hdr.length as usize)),
        0
    );

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
            && tables.addresses.facs + tables.facs.len() as u64
                <= placement.nvs_base + placement.nvs_size
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

    // RTC century register.
    //
    // This must match the emulated RTC/CMOS device model (see `devices::rtc_cmos::REG_CENTURY`).
    assert_eq!(fadt[108], 0x32, "FADT CENTURY must be set to CMOS index 0x32");

    // FADT flags should advertise the fixed-feature PWRBTN/SLPBTN events (PM1_STS/PM1_EN),
    // matching the `AcpiPmIo` device model, along with FIX_RTC + RESET_REG_SUP.
    let flags = read_u32_le(fadt, 112);
    let expected =
        FADT_FLAG_PWR_BUTTON | FADT_FLAG_SLP_BUTTON | FADT_FLAG_FIX_RTC | FADT_FLAG_RESET_REG_SUP;
    assert_eq!(
        flags & expected,
        expected,
        "FADT Flags missing expected PWR/SLP/FIX_RTC/RESET bits (flags=0x{flags:08x})"
    );

    // DSDT header + checksum.
    let dsdt_hdr_raw = mem.read(tables.addresses.dsdt, 36);
    let dsdt_hdr = parse_sdt_header(dsdt_hdr_raw);
    assert_eq!(&dsdt_hdr.signature, b"DSDT");
    assert_eq!(dsdt_hdr.revision, 2);
    assert_eq!(
        checksum(mem.read(tables.addresses.dsdt, dsdt_hdr.length as usize)),
        0
    );

    // Minimal AML sanity check: should contain the device names we emit.
    let dsdt = mem.read(tables.addresses.dsdt, dsdt_hdr.length as usize);
    let aml = &dsdt[36..];
    assert!(aml.windows(4).any(|w| w == b"SYS0"));
    assert!(aml.windows(4).any(|w| w == b"PWRB"));
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
            let gsi = pci_routing::gsi_for_intx(cfg.pirq_to_gsi, dev as u8, pin);
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
    assert_eq!(
        checksum(mem.read(tables.addresses.madt, madt_hdr.length as usize)),
        0
    );

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
                    assert_eq!(flags, 0x000F);
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
    assert_eq!(
        checksum(mem.read(tables.addresses.hpet, hpet_hdr.length as usize)),
        0
    );
    let hpet = mem.read(tables.addresses.hpet, hpet_hdr.length as usize);
    // HPET Base Address is a Generic Address Structure (GAS) starting at offset 40:
    //   40: AddressSpaceId
    //   41: RegisterBitWidth
    //   42: RegisterBitOffset
    //   43: AccessSize
    //   44..52: Address
    assert_eq!(hpet[40], 0, "HPET GAS AddressSpaceId must be System Memory");
    assert_eq!(
        hpet[41], 64,
        "HPET GAS RegisterBitWidth must be 64 (ACPI spec / Windows expectation)"
    );
    assert_eq!(hpet[42], 0, "HPET GAS RegisterBitOffset must be 0");
    assert_eq!(hpet[43], 0, "HPET GAS AccessSize should be unspecified (0)");
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

#[test]
fn dsdt_system_resources_device_reserves_acpi_pm_ports() {
    let cfg = AcpiConfig {
        smi_cmd_port: 0x00B3,
        pm1a_evt_blk: 0x0500,
        pm1a_cnt_blk: 0x0504,
        pm_tmr_blk: 0x0508,
        gpe0_blk: 0x0520,
        gpe0_blk_len: 0x10,
        ..Default::default()
    };
    let tables = AcpiTables::build(&cfg, AcpiPlacement::default());
    let aml = &tables.dsdt[36..];

    let sys0 = find_device_body(aml, b"SYS0").expect("expected DSDT to contain _SB_.SYS0");

    let pnp0c02 = eisa_id_to_u32("PNP0C02").unwrap().to_le_bytes();
    let hid_pnp0c02 = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0c02[..]].concat();
    assert!(
        sys0.windows(hid_pnp0c02.len()).any(|w| w == hid_pnp0c02),
        "expected SYS0._HID to be PNP0C02"
    );

    let crs = parse_named_buffer(sys0, b"_CRS").expect("expected SYS0 to have a _CRS buffer");
    assert!(
        crs.ends_with(&[0x79, 0x00]),
        "expected SYS0._CRS ResourceTemplate to end with EndTag"
    );

    let smi_cmd = io_port_descriptor(cfg.smi_cmd_port, cfg.smi_cmd_port, 1, 1);
    assert!(
        crs.windows(smi_cmd.len()).any(|w| w == smi_cmd),
        "expected SYS0._CRS to reserve the SMI_CMD port"
    );

    let pm1a_evt = io_port_descriptor(cfg.pm1a_evt_blk, cfg.pm1a_evt_blk, 1, 4);
    assert!(
        crs.windows(pm1a_evt.len()).any(|w| w == pm1a_evt),
        "expected SYS0._CRS to reserve the PM1a_EVT_BLK I/O range"
    );

    let pm1a_cnt = io_port_descriptor(cfg.pm1a_cnt_blk, cfg.pm1a_cnt_blk, 1, 2);
    assert!(
        crs.windows(pm1a_cnt.len()).any(|w| w == pm1a_cnt),
        "expected SYS0._CRS to reserve the PM1a_CNT_BLK I/O range"
    );

    let pm_tmr = io_port_descriptor(cfg.pm_tmr_blk, cfg.pm_tmr_blk, 1, 4);
    assert!(
        crs.windows(pm_tmr.len()).any(|w| w == pm_tmr),
        "expected SYS0._CRS to reserve the PM_TMR_BLK I/O range"
    );

    let gpe0 = io_port_descriptor(cfg.gpe0_blk, cfg.gpe0_blk, 1, cfg.gpe0_blk_len);
    assert!(
        crs.windows(gpe0.len()).any(|w| w == gpe0),
        "expected SYS0._CRS to reserve the GPE0_BLK I/O range"
    );

    // Legacy platform-owned ports which must not be used for PCI I/O BAR allocation.
    let imcr = io_port_descriptor(0x0022, 0x0022, 1, 2);
    assert!(
        crs.windows(imcr.len()).any(|w| w == imcr),
        "expected SYS0._CRS to reserve IMCR ports 0x22..0x23"
    );

    let a20 = io_port_descriptor(0x0092, 0x0092, 1, 1);
    assert!(
        crs.windows(a20.len()).any(|w| w == a20),
        "expected SYS0._CRS to reserve A20 gate port 0x92"
    );

    let i8042 = io_port_descriptor(0x0060, 0x0060, 1, 5);
    assert!(
        crs.windows(i8042.len()).any(|w| w == i8042),
        "expected SYS0._CRS to reserve i8042 keyboard controller ports 0x60..0x64"
    );

    let reset = io_port_descriptor(0x0CF9, 0x0CF9, 1, 1);
    assert!(
        crs.windows(reset.len()).any(|w| w == reset),
        "expected SYS0._CRS to reserve the reset port (0xCF9)"
    );
}

#[test]
fn dsdt_exposes_standard_acpi_power_and_sleep_buttons() {
    let cfg = AcpiConfig::default();
    let tables = AcpiTables::build(&cfg, AcpiPlacement::default());
    let aml = &tables.dsdt[36..];

    let pwrb = find_device_body(aml, b"PWRB").expect("expected DSDT to contain _SB_.PWRB");
    let slpb = find_device_body(aml, b"SLPB").expect("expected DSDT to contain _SB_.SLPB");

    // Known EISA ID encodings (little endian bytes of EisaId("PNP0C0C") / EisaId("PNP0C0E")).
    // This avoids relying on the local `eisa_id_to_u32` helper for correctness.
    let pnp0c0c = 0x0C0C_D041u32.to_le_bytes();
    let hid_pnp0c0c = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0c0c[..]].concat();
    assert!(
        pwrb.windows(hid_pnp0c0c.len()).any(|w| w == hid_pnp0c0c),
        "expected PWRB._HID to be PNP0C0C (ACPI power button)"
    );

    let pnp0c0e = 0x0E0C_D041u32.to_le_bytes();
    let hid_pnp0c0e = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0c0e[..]].concat();
    assert!(
        slpb.windows(hid_pnp0c0e.len()).any(|w| w == hid_pnp0c0e),
        "expected SLPB._HID to be PNP0C0E (ACPI sleep button)"
    );

    // Both devices are modeled as always-present singletons.
    let uid0 = [&[0x08][..], &b"_UID"[..], &[0x00][..]].concat(); // Name (_UID, Zero)
    let sta0f = [&[0x08][..], &b"_STA"[..], &[0x0A, 0x0F][..]].concat(); // Name (_STA, 0x0F)
    assert!(
        pwrb.windows(uid0.len()).any(|w| w == uid0),
        "expected PWRB._UID to be 0"
    );
    assert!(
        pwrb.windows(sta0f.len()).any(|w| w == sta0f),
        "expected PWRB._STA to be 0x0F (present/enabled/show in UI/functioning)"
    );
    assert!(
        slpb.windows(uid0.len()).any(|w| w == uid0),
        "expected SLPB._UID to be 0"
    );
    assert!(
        slpb.windows(sta0f.len()).any(|w| w == sta0f),
        "expected SLPB._STA to be 0x0F (present/enabled/show in UI/functioning)"
    );
}

#[test]
fn mcfg_is_emitted_and_describes_the_ecam_window_when_enabled() {
    let cfg = AcpiConfig {
        pcie_ecam_base: 0xC000_0000,
        pcie_segment: 0,
        pcie_start_bus: 0,
        pcie_end_bus: 0,
        ..Default::default()
    };

    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let mcfg_addr = tables.addresses.mcfg.expect("MCFG should be present");

    // Allocate enough physical memory to cover all the written tables.
    let max_end = [
        tables.addresses.rsdp + tables.rsdp.len() as u64,
        tables.addresses.xsdt + tables.xsdt.len() as u64,
        tables.addresses.facs + tables.facs.len() as u64,
        mcfg_addr + tables.mcfg.as_ref().unwrap().len() as u64,
    ]
    .into_iter()
    .max()
    .unwrap();
    let mem_size = (max_end + 0x1000) as usize;
    let mut mem = TestMemory::new(mem_size);
    tables.write_to(&mut mem);

    // Ensure RSDT/XSDT include the MCFG entry.
    let rsdp = mem.read(tables.addresses.rsdp, 36);
    let rsdt_addr = read_u32_le(rsdp, 16) as u64;
    let xsdt_addr = read_u64_le(rsdp, 24);

    let rsdt_hdr_raw = mem.read(rsdt_addr, 36);
    let rsdt_hdr = parse_sdt_header(rsdt_hdr_raw);
    let rsdt_blob = mem.read(rsdt_addr, rsdt_hdr.length as usize);
    let rsdt_entries = (rsdt_hdr.length as usize - 36) / 4;
    let mut rsdt_ptrs = Vec::new();
    for i in 0..rsdt_entries {
        rsdt_ptrs.push(read_u32_le(rsdt_blob, 36 + i * 4) as u64);
    }
    assert!(
        rsdt_ptrs.contains(&mcfg_addr),
        "RSDT must include MCFG entry"
    );

    let xsdt_hdr_raw = mem.read(xsdt_addr, 36);
    let xsdt_hdr = parse_sdt_header(xsdt_hdr_raw);
    let xsdt_blob = mem.read(xsdt_addr, xsdt_hdr.length as usize);
    let xsdt_entries = (xsdt_hdr.length as usize - 36) / 8;
    let mut xsdt_ptrs = Vec::new();
    for i in 0..xsdt_entries {
        xsdt_ptrs.push(read_u64_le(xsdt_blob, 36 + i * 8));
    }
    assert!(
        xsdt_ptrs.contains(&mcfg_addr),
        "XSDT must include MCFG entry"
    );

    // Validate MCFG checksum and allocation structure.
    assert_eq!(read_sdt_signature(&mem, mcfg_addr), *b"MCFG");
    let mcfg_hdr = parse_sdt_header(mem.read(mcfg_addr, 36));
    assert_eq!(mcfg_hdr.revision, 1);
    let mcfg = mem.read(mcfg_addr, mcfg_hdr.length as usize);
    assert_eq!(checksum(mcfg), 0);
    assert!(mcfg_hdr.length >= 36 + 8 + 16);

    // First allocation structure begins at offset 44.
    let ecam_base = read_u64_le(mcfg, 44);
    let seg = read_u16_le(mcfg, 52);
    let start_bus = mcfg[54];
    let end_bus = mcfg[55];

    assert_eq!(ecam_base, cfg.pcie_ecam_base);
    assert_eq!(seg, cfg.pcie_segment);
    assert_eq!(start_bus, cfg.pcie_start_bus);
    assert_eq!(end_bus, cfg.pcie_end_bus);
}
