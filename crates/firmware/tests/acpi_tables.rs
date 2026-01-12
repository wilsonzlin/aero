use aero_devices::acpi_pm::{
    DEFAULT_ACPI_DISABLE, DEFAULT_ACPI_ENABLE, DEFAULT_GPE0_BLK, DEFAULT_GPE0_BLK_LEN,
    DEFAULT_PM1A_CNT_BLK, DEFAULT_PM1A_EVT_BLK, DEFAULT_PM_TMR_BLK, DEFAULT_SMI_CMD_PORT,
};
use aero_devices::apic::{IOAPIC_MMIO_BASE, LAPIC_MMIO_BASE};
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::pci::PciIntxRouterConfig;
use aero_devices::reset_ctrl::{RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE};
use aero_pc_constants::PCIE_ECAM_BASE;
use firmware::acpi::{
    checksum8, find_rsdp_in_memory, parse_header, parse_rsdp_v2, parse_rsdt_entries,
    parse_xsdt_entries, validate_table_checksum, AcpiConfig, AcpiTables, DEFAULT_EBDA_BASE,
    DEFAULT_PCI_MMIO_START, RSDP_CHECKSUM_LEN_V1, RSDP_V2_SIZE,
};
use memory::{DenseMemory, GuestMemory};
use std::path::PathBuf;

fn build_tables(cpu_count: u8) -> AcpiTables {
    let config = AcpiConfig::new(cpu_count, 0x1_0000_0000); // 4GiB
    AcpiTables::build(&config).expect("ACPI tables should build")
}

fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
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

#[test]
fn checksums_sum_to_zero() {
    let tables = build_tables(2);

    // RSDP: first 20 bytes and full length must checksum to 0.
    assert_eq!(checksum8(&tables.rsdp[..RSDP_CHECKSUM_LEN_V1]), 0);
    assert_eq!(checksum8(&tables.rsdp[..RSDP_V2_SIZE]), 0);

    for (name, bytes) in [
        ("DSDT", tables.dsdt.as_slice()),
        ("FADT", tables.fadt.as_slice()),
        ("MADT", tables.madt.as_slice()),
        ("HPET", tables.hpet.as_slice()),
        ("RSDT", tables.rsdt.as_slice()),
        ("XSDT", tables.xsdt.as_slice()),
    ] {
        assert!(
            validate_table_checksum(bytes),
            "{name} checksum did not sum to zero"
        );
        let hdr = parse_header(bytes).expect("table header should parse");
        assert_eq!(hdr.length as usize, bytes.len(), "{name} length mismatch");
    }
}

#[test]
fn fadt_exposes_acpi_pm_blocks_and_reset_register() {
    let tables = build_tables(2);
    let fadt = tables.fadt.as_slice();

    // --- ACPI PM blocks + enable handshake ---
    assert_eq!(read_u16_le(fadt, 46), 9, "SCI IRQ should be 9");

    assert_eq!(
        read_u32_le(fadt, 48) as u16,
        DEFAULT_SMI_CMD_PORT,
        "SMI_CMD must be 0xB2"
    );
    assert_eq!(fadt[52], DEFAULT_ACPI_ENABLE, "ACPI_ENABLE must be 0xA0");
    assert_eq!(fadt[53], DEFAULT_ACPI_DISABLE, "ACPI_DISABLE must be 0xA1");

    assert_eq!(
        read_u32_le(fadt, 56) as u16,
        DEFAULT_PM1A_EVT_BLK,
        "PM1a_EVT must be 0x400"
    );
    assert_eq!(
        read_u32_le(fadt, 64) as u16,
        DEFAULT_PM1A_CNT_BLK,
        "PM1a_CNT must be 0x404"
    );
    assert_eq!(
        read_u32_le(fadt, 76) as u16,
        DEFAULT_PM_TMR_BLK,
        "PM_TMR must be 0x408"
    );
    assert_eq!(
        read_u32_le(fadt, 80) as u16,
        DEFAULT_GPE0_BLK,
        "GPE0 must be 0x420"
    );
    assert_eq!(fadt[91], 4, "PM_TMR_LEN must be 4");
    assert_eq!(
        fadt[92],
        DEFAULT_GPE0_BLK_LEN,
        "GPE0_BLK_LEN must be 0x08"
    );

    // Offsets per ACPI 2.0+ FADT layout (see `acpi::structures::Fadt`).
    const FLAGS_OFFSET: usize = 112;
    const RESET_REG_OFFSET: usize = 116;

    let flags = u32::from_le_bytes(fadt[FLAGS_OFFSET..FLAGS_OFFSET + 4].try_into().unwrap());
    assert_ne!(flags & (1 << 10), 0, "RESET_REG_SUP flag must be set");

    // ResetReg is a Generic Address Structure (GAS).
    assert_eq!(fadt[RESET_REG_OFFSET], 0x01, "ResetReg must be System I/O");
    assert_eq!(
        fadt[RESET_REG_OFFSET + 1],
        8,
        "ResetReg width must be 8 bits"
    );
    assert_eq!(
        fadt[RESET_REG_OFFSET + 3],
        1,
        "ResetReg access size must be byte"
    );

    let addr = u64::from_le_bytes(
        fadt[RESET_REG_OFFSET + 4..RESET_REG_OFFSET + 12]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        addr,
        u64::from(RESET_CTRL_PORT),
        "ResetReg address must be port 0xCF9"
    );
    assert_eq!(
        fadt[RESET_REG_OFFSET + 12],
        RESET_CTRL_RESET_VALUE,
        "ResetValue must be 0x06"
    );
}

#[test]
fn placement_is_aligned_and_non_overlapping() {
    let config = AcpiConfig::new(2, 0x1_0000_0000);
    let tables = AcpiTables::build(&config).expect("ACPI tables should build");

    for (name, addr) in [
        ("RSDP", tables.rsdp_addr),
        ("DSDT", tables.dsdt_addr),
        ("FADT", tables.fadt_addr),
        ("MADT", tables.madt_addr),
        ("HPET", tables.hpet_addr),
        ("RSDT", tables.rsdt_addr),
        ("XSDT", tables.xsdt_addr),
        ("FACS", tables.facs_addr),
    ] {
        assert_eq!(addr % 16, 0, "{name} not 16-byte aligned");
    }
    assert!(tables.rsdp_addr < 0x0010_0000, "RSDP must live below 1MiB");

    let low_ram_top = config
        .guest_memory_size
        .min(config.pci_mmio_start)
        .min(PCIE_ECAM_BASE);
    let nvs_end = tables.nvs_base + tables.nvs_size;
    assert!(
        nvs_end <= low_ram_top,
        "ACPI windows must fit below top-of-low-ram (nvs_end=0x{nvs_end:x} low_ram_top=0x{low_ram_top:x})"
    );

    // Tables must fit within their windows.
    let reclaim_end = tables.reclaim_base + tables.reclaim_size;
    for (name, addr, len) in [
        ("DSDT", tables.dsdt_addr, tables.dsdt.len()),
        ("FADT", tables.fadt_addr, tables.fadt.len()),
        ("MADT", tables.madt_addr, tables.madt.len()),
        ("HPET", tables.hpet_addr, tables.hpet.len()),
        ("RSDT", tables.rsdt_addr, tables.rsdt.len()),
        ("XSDT", tables.xsdt_addr, tables.xsdt.len()),
    ] {
        let end = addr + len as u64;
        assert!(
            addr >= tables.reclaim_base && end <= reclaim_end,
            "{name} out of reclaim window"
        );
    }
    let facs_end = tables.facs_addr + tables.facs.len() as u64;
    assert!(
        tables.facs_addr >= tables.nvs_base && facs_end <= nvs_end,
        "FACS out of NVS window"
    );

    // Ensure the reclaimable tables don't overlap each other.
    let mut ranges = [
        ("DSDT", tables.dsdt_addr, tables.dsdt.len() as u64),
        ("FADT", tables.fadt_addr, tables.fadt.len() as u64),
        ("MADT", tables.madt_addr, tables.madt.len() as u64),
        ("HPET", tables.hpet_addr, tables.hpet.len() as u64),
        ("RSDT", tables.rsdt_addr, tables.rsdt.len() as u64),
        ("XSDT", tables.xsdt_addr, tables.xsdt.len() as u64),
    ];
    ranges.sort_by_key(|(_, start, _)| *start);
    for win in ranges.windows(2) {
        let (left_name, left_start, left_len) = win[0];
        let (right_name, right_start, _) = win[1];
        let left_end = left_start + left_len;
        assert!(left_end <= right_start, "{left_name} overlaps {right_name}");
    }
}

#[test]
fn rsdp_rsdt_xsdt_pointers_are_consistent() {
    let tables = build_tables(2);
    let parsed = parse_rsdp_v2(&tables.rsdp).expect("RSDP should parse");
    assert_eq!(parsed.revision, 2);
    assert_eq!(parsed.length as usize, RSDP_V2_SIZE);
    assert_eq!(parsed.rsdt_address as u64, tables.rsdt_addr);
    assert_eq!(parsed.xsdt_address, tables.xsdt_addr);

    let rsdt_entries = parse_rsdt_entries(&tables.rsdt).expect("RSDT should parse");
    assert_eq!(
        rsdt_entries,
        vec![
            tables.fadt_addr as u32,
            tables.madt_addr as u32,
            tables.hpet_addr as u32
        ]
    );
    let xsdt_entries = parse_xsdt_entries(&tables.xsdt).expect("XSDT should parse");
    assert_eq!(
        xsdt_entries,
        vec![tables.fadt_addr, tables.madt_addr, tables.hpet_addr]
    );

    // FADT pointers to DSDT and FACS.
    assert_eq!(&tables.fadt[0..4], b"FACP");
    let dsdt_32 = u32::from_le_bytes(tables.fadt[40..44].try_into().unwrap());
    let facs_32 = u32::from_le_bytes(tables.fadt[36..40].try_into().unwrap());
    assert_eq!(dsdt_32 as u64, tables.dsdt_addr);
    assert_eq!(facs_32 as u64, tables.facs_addr);
}

#[test]
fn madt_contains_apic_entries_and_interrupt_overrides() {
    let cpu_count = 4;
    let tables = build_tables(cpu_count);
    let madt = tables.madt.as_slice();
    assert_eq!(&madt[0..4], b"APIC");
    assert!(validate_table_checksum(madt));

    // MADT header: Local APIC address and flags follow the SDT header.
    assert_eq!(
        read_u32_le(madt, 36),
        LAPIC_MMIO_BASE as u32,
        "LAPIC base mismatch"
    );

    let mut lapic_ids = Vec::new();
    let mut found_ioapic = false;
    let mut found_irq0_iso = false;
    let mut found_sci_iso = false;

    let mut off = 44;
    while off < madt.len() {
        let entry_type = madt[off];
        let entry_len = madt[off + 1] as usize;
        assert!(entry_len >= 2);
        match entry_type {
            0 => {
                // Processor Local APIC.
                let acpi_id = madt[off + 2];
                let apic_id = madt[off + 3];
                assert_eq!(acpi_id, apic_id);
                lapic_ids.push(acpi_id);
            }
            1 => {
                // I/O APIC.
                let addr = read_u32_le(madt, off + 4);
                assert_eq!(addr, IOAPIC_MMIO_BASE as u32);
                found_ioapic = true;
            }
            2 => {
                // Interrupt Source Override.
                let bus = madt[off + 2];
                let src = madt[off + 3];
                let gsi = read_u32_le(madt, off + 4);
                let flags = read_u16_le(madt, off + 8);
                if bus == 0 && src == 0 && gsi == 2 {
                    found_irq0_iso = true;
                }
                if bus == 0 && src == 9 && gsi == 9 {
                    found_sci_iso = true;
                    assert_eq!(flags, 0x000F);
                }
            }
            _ => {}
        }
        off += entry_len;
    }

    lapic_ids.sort_unstable();
    assert_eq!(lapic_ids, (0..cpu_count).collect::<Vec<u8>>());
    assert!(found_ioapic);
    assert!(found_irq0_iso);
    assert!(found_sci_iso);
}

#[test]
fn dsdt_contains_pci_routing_and_resources() {
    let tables = build_tables(2);
    let dsdt = tables.dsdt.as_slice();
    assert_eq!(&dsdt[0..4], b"DSDT");
    assert!(validate_table_checksum(dsdt));

    let aml = &dsdt[36..];
    for name in [b"PCI0", b"_CRS", b"_PRT", b"_PIC", b"_S5_"] {
        assert!(
            aml.windows(name.len()).any(|w| w == name),
            "DSDT AML missing {:?}",
            core::str::from_utf8(name).unwrap()
        );
    }

    // `_PIC` should program the IMCR (ports 0x22/0x23) so the platform switches
    // between legacy PIC routing and APIC/IOAPIC routing.
    let imcr_opregion = [
        &[0x5B, 0x80][..], // OperationRegionOp (ExtOpPrefix + Op)
        &b"IMCR"[..],      // NameSeg
        &[0x01, 0x0A, 0x22, 0x0A, 0x02][..], // SystemIO, base 0x22, length 2
    ]
    .concat();
    assert!(
        find_subslice(aml, &imcr_opregion).is_some(),
        "DSDT AML missing IMCR SystemIO OperationRegion for ports 0x22..0x23"
    );

    let imcr_field = [
        &[0x5B, 0x81, 0x0F][..], // FieldOp + pkglen
        &b"IMCR"[..],
        &[0x01][..], // ByteAcc + NoLock + Preserve
        &b"IMCS"[..],
        &[0x08][..],
        &b"IMCD"[..],
        &[0x08][..],
    ]
    .concat();
    assert!(
        find_subslice(aml, &imcr_field).is_some(),
        "DSDT AML missing IMCR Field (IMCS/IMCD)"
    );

    let pic_body = [
        &b"_PIC"[..],
        &[0x01][..], // 1 arg
        &[0x70, 0x68][..],
        &b"PICM"[..],
        &[0x70, 0x0A, 0x70][..],
        &b"IMCS"[..],
        &[0x7B, 0x68, 0x01][..],
        &b"IMCD"[..],
    ]
    .concat();
    assert!(
        find_subslice(aml, &pic_body).is_some(),
        "DSDT AML missing _PIC body that programs IMCR select/data"
    );

    // Validate the `_PRT` mapping matches the default PCI INTx router config.
    let prt = parse_prt_entries(aml).expect("_PRT package should parse");
    assert_eq!(prt.len(), 31 * 4);

    let pirq_to_gsi = PciIntxRouterConfig::default().pirq_to_gsi;
    let mut expected = Vec::new();
    for dev in 1u32..=31 {
        let addr = (dev << 16) | 0xFFFF;
        for pin in 0u8..=3 {
            let pirq = ((dev as usize) + (pin as usize)) & 3;
            expected.push((addr, pin, pirq_to_gsi[pirq]));
        }
    }
    assert_eq!(prt, expected);

    // `_CRS` should expose the PCI config I/O ports (0xCF8..0xCFF).
    let cfg_ports = [0x47, 0x01, 0xF8, 0x0C, 0xF8, 0x0C, 0x01, 0x08];
    assert!(
        find_subslice(aml, &cfg_ports).is_some(),
        "PCI0._CRS missing PCI config I/O port descriptor"
    );

    // `_CRS` should include the PCI MMIO window (DWord address descriptor).
    let expected_size = (IOAPIC_MMIO_BASE as u32) - (DEFAULT_PCI_MMIO_START as u32);
    let expected_start = DEFAULT_PCI_MMIO_START as u32;
    let expected_end = expected_start + expected_size - 1;
    let desc_prefix = [0x87, 0x17, 0x00, 0x00, 0x0D, 0x06];
    let Some(off) = find_subslice(aml, &desc_prefix) else {
        panic!("PCI0._CRS missing PCI MMIO DWord address descriptor");
    };
    assert_eq!(read_u32_le(aml, off + 10), expected_start);
    assert_eq!(read_u32_le(aml, off + 14), expected_end);
    assert_eq!(read_u32_le(aml, off + 22), expected_size);
}

#[test]
fn shipped_dsdt_aml_matches_aero_acpi_generator() {
    let cfg = aero_acpi::AcpiConfig::default();
    let placement = aero_acpi::AcpiPlacement::default();
    let generated = aero_acpi::AcpiTables::build(&cfg, placement).dsdt;

    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("acpi");
    path.push("dsdt.aml");
    let on_disk = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

    assert_eq!(
        on_disk, generated,
        "crates/firmware/acpi/dsdt.aml is out of date; regenerate it with: cargo run -p firmware --bin gen_dsdt"
    );
}

fn read_table_from_mem<M: GuestMemory>(mem: &M, addr: u64) -> Vec<u8> {
    let mut header_buf = [0u8; 36];
    mem.read_into(addr, &mut header_buf)
        .expect("header read should succeed");
    let hdr = parse_header(&header_buf).expect("header parse should succeed");
    let mut buf = vec![0u8; hdr.length as usize];
    mem.read_into(addr, &mut buf)
        .expect("table read should succeed");
    buf
}

#[test]
fn memory_scan_finds_rsdp_and_tables() {
    let mut mem = DenseMemory::new(32 * 1024 * 1024).expect("allocate guest memory");
    let config = AcpiConfig::new(4, mem.size());
    let rsdp_addr = AcpiTables::build_and_write(&config, &mut mem).expect("write should succeed");

    let found = find_rsdp_in_memory(&mem, DEFAULT_EBDA_BASE, 0xA0000)
        .or_else(|| find_rsdp_in_memory(&mem, 0xE0000, 0x100000))
        .expect("RSDP should be discoverable by scan");
    assert_eq!(found, rsdp_addr);

    let mut rsdp_buf = [0u8; RSDP_V2_SIZE];
    mem.read_into(rsdp_addr, &mut rsdp_buf).expect("rsdp read");
    let parsed = parse_rsdp_v2(&rsdp_buf).expect("RSDP should parse");

    let rsdt = read_table_from_mem(&mem, parsed.rsdt_address as u64);
    let xsdt = read_table_from_mem(&mem, parsed.xsdt_address);
    assert!(validate_table_checksum(&rsdt));
    assert!(validate_table_checksum(&xsdt));

    let rsdt_entries = parse_rsdt_entries(&rsdt).expect("RSDT entries");
    let xsdt_entries = parse_xsdt_entries(&xsdt).expect("XSDT entries");
    assert_eq!(rsdt_entries.len(), 3);
    assert_eq!(xsdt_entries.len(), 3);

    // Follow pointers and ensure we can read/validate the referenced tables.
    for addr in xsdt_entries {
        let table = read_table_from_mem(&mem, addr);
        assert!(validate_table_checksum(&table));
        let hdr = parse_header(&table).unwrap();
        assert!(
            matches!(&hdr.signature, b"FACP" | b"APIC" | b"HPET"),
            "unexpected table signature {:?}",
            core::str::from_utf8(&hdr.signature).ok()
        );
    }
}

#[test]
fn hpet_table_matches_device_model_base() {
    let tables = build_tables(2);
    let hpet = tables.hpet.as_slice();
    assert_eq!(&hpet[0..4], b"HPET");
    assert!(validate_table_checksum(hpet));

    // HPET GAS address is at offset 44 in the table (see `acpi::structures::Hpet`).
    let addr = u64::from_le_bytes(hpet[44..52].try_into().unwrap());
    assert_eq!(addr, HPET_MMIO_BASE);
}
