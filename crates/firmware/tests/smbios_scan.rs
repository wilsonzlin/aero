use firmware::memory::{MemoryBus, VecMemory};
use firmware::smbios::{
    find_eps, parse_eps_table_info, parse_structure_types, validate_eps_checksum, SmbiosConfig,
    SmbiosTables,
};

#[test]
fn host_memory_scan_finds_eps_and_parses_table() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    mem.write_physical(0x40E, &0x9FC0u16.to_le_bytes());

    let config = SmbiosConfig {
        ram_bytes: 512 * 1024 * 1024,
        ..Default::default()
    };
    let eps_addr = SmbiosTables::build_and_write(&config, &mut mem);

    let scanned = find_eps(&mut mem).expect("EPS not found by scan") as u32;
    assert_eq!(scanned, eps_addr);

    // Parse EPS enough to sanity-check the table is reachable and ends with Type 127.
    let mut eps = [0u8; 0x1F];
    mem.read_physical(eps_addr as u64, &mut eps);
    assert!(validate_eps_checksum(&eps));
    let table_info = parse_eps_table_info(&eps).expect("invalid SMBIOS EPS");

    let mut table = vec![0u8; table_info.table_len];
    mem.read_physical(table_info.table_addr, &mut table);

    let types = parse_structure_types(&table);
    assert_eq!(
        types.last().copied(),
        Some(127),
        "SMBIOS table did not contain Type 127 end-of-table"
    );
}

#[test]
fn cpu_count_emits_one_type4_per_cpu() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    mem.write_physical(0x40E, &0x9FC0u16.to_le_bytes());

    let config = SmbiosConfig {
        cpu_count: 4,
        ..Default::default()
    };
    let eps_addr = SmbiosTables::build_and_write(&config, &mut mem);

    let scanned = find_eps(&mut mem).expect("EPS not found by scan") as u32;
    assert_eq!(scanned, eps_addr);

    // Parse EPS enough to find the structure table.
    let mut eps = [0u8; 0x1F];
    mem.read_physical(eps_addr as u64, &mut eps);
    assert!(validate_eps_checksum(&eps));
    let table_info = parse_eps_table_info(&eps).expect("invalid SMBIOS EPS");

    let mut table = vec![0u8; table_info.table_len];
    mem.read_physical(table_info.table_addr, &mut table);

    // Walk structures until end-of-table, counting Type 4 (Processor Information).
    let types = parse_structure_types(&table);
    let type4_count = types.iter().filter(|&&ty| ty == 4).count();
    assert_eq!(
        type4_count,
        usize::from(config.cpu_count),
        "SMBIOS table should contain one Type 4 per CPU"
    );
}
