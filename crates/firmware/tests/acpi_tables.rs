use firmware::acpi::{
    checksum8, find_rsdp_in_memory, parse_header, parse_rsdp_v2, parse_rsdt_entries,
    parse_xsdt_entries, validate_table_checksum, AcpiConfig, AcpiTables, DEFAULT_EBDA_BASE,
    RSDP_CHECKSUM_LEN_V1, RSDP_V2_SIZE,
};
use memory::{DenseMemory, GuestMemory};

mod golden;

fn build_golden_tables() -> AcpiTables {
    let config = AcpiConfig {
        cpu_count: 2,
        guest_memory_size: 0x1_0000_0000, // 4GiB
        ..AcpiConfig::new(2, 0x1_0000_0000)
    };
    AcpiTables::build(&config).expect("ACPI tables should build")
}

#[test]
fn checksums_sum_to_zero() {
    let tables = build_golden_tables();

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
fn fadt_exposes_acpi_reset_register() {
    let tables = build_golden_tables();
    let fadt = tables.fadt.as_slice();

    // Offsets per ACPI 2.0+ FADT layout (see `acpi::structures::Fadt`).
    const FLAGS_OFFSET: usize = 112;
    const RESET_REG_OFFSET: usize = 116;

    let flags = u32::from_le_bytes(fadt[FLAGS_OFFSET..FLAGS_OFFSET + 4].try_into().unwrap());
    assert_ne!(flags & (1 << 10), 0, "RESET_REG_SUP flag must be set");

    // ResetReg is a Generic Address Structure (GAS).
    assert_eq!(fadt[RESET_REG_OFFSET + 0], 0x01, "ResetReg must be System I/O");
    assert_eq!(fadt[RESET_REG_OFFSET + 1], 8, "ResetReg width must be 8 bits");
    assert_eq!(fadt[RESET_REG_OFFSET + 3], 1, "ResetReg access size must be byte");

    let addr = u64::from_le_bytes(
        fadt[RESET_REG_OFFSET + 4..RESET_REG_OFFSET + 12]
            .try_into()
            .unwrap(),
    );
    assert_eq!(addr, 0x0CF9, "ResetReg address must be port 0xCF9");
    assert_eq!(fadt[RESET_REG_OFFSET + 12], 0x06, "ResetValue must be 0x06");
}

#[test]
fn placement_is_aligned_and_non_overlapping() {
    let tables = build_golden_tables();

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
    let nvs_end = tables.nvs_base + tables.nvs_size;
    let facs_end = tables.facs_addr + tables.facs.len() as u64;
    assert!(
        tables.facs_addr >= tables.nvs_base && facs_end <= nvs_end,
        "FACS out of NVS window"
    );

    // Ensure the reclaimable tables don't overlap each other.
    let mut ranges = vec![
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
        assert!(
            left_end <= right_start,
            "{left_name} overlaps {right_name}"
        );
    }
}

#[test]
fn rsdp_rsdt_xsdt_pointers_are_consistent() {
    let tables = build_golden_tables();
    let parsed = parse_rsdp_v2(&tables.rsdp).expect("RSDP should parse");
    assert_eq!(parsed.revision, 2);
    assert_eq!(parsed.length as usize, RSDP_V2_SIZE);
    assert_eq!(parsed.rsdt_address as u64, tables.rsdt_addr);
    assert_eq!(parsed.xsdt_address, tables.xsdt_addr);

    let rsdt_entries = parse_rsdt_entries(&tables.rsdt).expect("RSDT should parse");
    assert_eq!(
        rsdt_entries,
        vec![tables.fadt_addr as u32, tables.madt_addr as u32, tables.hpet_addr as u32]
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
    mem.read_into(rsdp_addr, &mut rsdp_buf)
        .expect("rsdp read");
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
            matches!(
                &hdr.signature,
                b"FACP" | b"APIC" | b"HPET"
            ),
            "unexpected table signature {:?}",
            core::str::from_utf8(&hdr.signature).ok()
        );
    }
}

#[test]
fn golden_tables_match_expected_bytes() {
    let tables = build_golden_tables();

    assert_eq!(&tables.rsdp[..], golden::EXPECTED_RSDP);
    assert_eq!(tables.dsdt.as_slice(), firmware::acpi::dsdt::DSDT_AML);
    assert_eq!(tables.fadt.as_slice(), golden::EXPECTED_FADT);
    assert_eq!(tables.madt.as_slice(), golden::EXPECTED_MADT);
    assert_eq!(tables.hpet.as_slice(), golden::EXPECTED_HPET);
    assert_eq!(tables.rsdt.as_slice(), golden::EXPECTED_RSDT);
    assert_eq!(tables.xsdt.as_slice(), golden::EXPECTED_XSDT);
    assert_eq!(tables.facs.as_slice(), golden::EXPECTED_FACS);
}
