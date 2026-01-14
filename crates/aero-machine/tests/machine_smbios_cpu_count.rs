use std::collections::BTreeSet;

use aero_machine::{Machine, MachineConfig};
use firmware::smbios::{
    find_eps, parse_eps_table_info, parse_structure_headers, validate_eps_checksum,
};

#[test]
fn machine_smbios_exposes_configured_cpu_count() {
    let cpu_count = 4u8;

    let mut machine = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_acpi: true,
        cpu_count,
        ..Default::default()
    })
    .expect("Machine::new should succeed");

    // Discover the SMBIOS EPS using the spec-defined scan rules rather than relying on the
    // convenience accessor.
    let eps_addr =
        find_eps(&mut machine).expect("SMBIOS EPS not found in EBDA or BIOS scan region");

    // Parse the SMBIOS 2.x Entry Point Structure to locate the structure table.
    let eps = machine.read_physical_bytes(eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(validate_eps_checksum(&eps), "SMBIOS EPS checksum failed");

    let table_info = parse_eps_table_info(&eps).expect("invalid SMBIOS EPS");
    let table = machine.read_physical_bytes(table_info.table_addr, table_info.table_len);

    let structures = parse_structure_headers(&table);
    let cpu_structs: Vec<_> = structures.iter().filter(|s| s.ty == 4).collect();
    assert_eq!(
        cpu_structs.len(),
        cpu_count as usize,
        "SMBIOS must expose one Type 4 (Processor Information) structure per configured CPU"
    );

    let handles: BTreeSet<u16> = cpu_structs.iter().map(|s| s.handle).collect();
    assert_eq!(
        handles.len(),
        cpu_count as usize,
        "SMBIOS Type 4 handles must be unique"
    );
}
