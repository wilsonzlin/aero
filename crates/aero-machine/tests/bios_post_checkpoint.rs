use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bios::EBDA_BASE;
use firmware::smbios::{
    find_eps, parse_eps_table_info, parse_structure_types, validate_eps_checksum,
};
use pretty_assertions::assert_eq;

fn build_serial_boot_sector(message: &[u8]) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov dx, 0x3f8
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
    i += 3;

    for &b in message {
        // mov al, imm8
        sector[i..i + 2].copy_from_slice(&[0xB0, b]);
        i += 2;
        // out dx, al
        sector[i] = 0xEE;
        i += 1;
    }

    // cli; hlt (avoid waking due to platform interrupts once timers are enabled)
    sector[i] = 0xFA;
    sector[i + 1] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => break,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
}

fn boot_sector(pattern: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [pattern; aero_storage::SECTOR_SIZE];
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)) == 0
}

#[test]
fn boots_mbr_and_writes_to_serial_integration() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();

    let boot = build_serial_boot_sector(b"OK\n");
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);
    assert_eq!(m.serial_output_len(), 3);
    assert_eq!(m.serial_output_bytes(), b"OK\n");
    assert_eq!(m.take_serial_output(), b"OK\n");
    assert_eq!(m.serial_output_len(), 0);
}

#[test]
fn bios_post_loads_boot_sector_and_publishes_acpi_and_smbios() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_acpi: true,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot_sector(0xAA).to_vec()).unwrap();
    m.reset();

    // BIOS must have loaded the boot sector.
    let loaded = m.read_physical_bytes(0x7C00, aero_storage::SECTOR_SIZE);
    assert_eq!(loaded[..510], vec![0xAA; 510]);
    assert_eq!(loaded[510], 0x55);
    assert_eq!(loaded[511], 0xAA);

    // ACPI RSDP should be written during POST when enabled.
    let rsdp_addr = m.acpi_rsdp_addr().expect("RSDP should be published");
    let rsdp = m.read_physical_bytes(rsdp_addr, 36);
    assert_eq!(&rsdp[0..8], b"RSD PTR ");
    assert!(checksum_ok(&rsdp[..20]));
    assert!(checksum_ok(&rsdp));

    // SMBIOS EPS should be discoverable by spec search rules.
    let eps_addr = find_eps(&mut m).expect("SMBIOS EPS not found after BIOS POST");
    assert!((EBDA_BASE..EBDA_BASE + 1024).contains(&eps_addr));

    let eps = m.read_physical_bytes(eps_addr, 0x1F);
    assert_eq!(&eps[0..4], b"_SM_");
    assert!(validate_eps_checksum(&eps));

    let table_info = parse_eps_table_info(&eps).expect("invalid SMBIOS EPS");
    let table = m.read_physical_bytes(table_info.table_addr, table_info.table_len);
    let types = parse_structure_types(&table);

    assert_eq!(types.last().copied(), Some(127), "missing Type 127");
    assert!(types.contains(&0), "missing Type 0");
    assert!(types.contains(&1), "missing Type 1");
    assert!(types.contains(&4), "missing Type 4");
    assert!(types.contains(&16), "missing Type 16");
    assert!(types.contains(&17), "missing Type 17");
    assert!(types.contains(&19), "missing Type 19");
}

#[test]
fn snapshot_round_trip_full_is_deterministic() {
    let boot = build_serial_boot_sector(b"OK\n");

    let mut baseline = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    baseline.set_disk_image(boot.to_vec()).unwrap();
    baseline.reset();
    run_until_halt(&mut baseline);
    let baseline_out = baseline.take_serial_output();

    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    vm.set_disk_image(boot.to_vec()).unwrap();
    vm.reset();

    // Run far enough to emit "OK" but stop before the final newline so the snapshot is a real
    // mid-execution checkpoint.
    assert!(matches!(vm.run_slice(5), RunExit::Completed { .. }));
    let snap = vm.take_snapshot_full().unwrap();

    let mut resumed = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    resumed.set_disk_image(boot.to_vec()).unwrap();
    resumed.reset();
    resumed.restore_snapshot_bytes(&snap).unwrap();
    run_until_halt(&mut resumed);

    assert_eq!(baseline_out, resumed.take_serial_output());
}

#[test]
fn snapshot_round_trip_dirty_chain_is_deterministic() {
    let boot = build_serial_boot_sector(b"OK\n");

    let mut baseline = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    baseline.set_disk_image(boot.to_vec()).unwrap();
    baseline.reset();
    run_until_halt(&mut baseline);
    let baseline_out = baseline.take_serial_output();

    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    vm.set_disk_image(boot.to_vec()).unwrap();
    vm.reset();

    assert!(matches!(vm.run_slice(3), RunExit::Completed { .. }));
    let base = vm.take_snapshot_full().unwrap();

    assert!(matches!(vm.run_slice(2), RunExit::Completed { .. }));
    let diff = vm.take_snapshot_dirty().unwrap();

    let mut resumed = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    resumed.set_disk_image(boot.to_vec()).unwrap();
    resumed.reset();
    resumed.restore_snapshot_bytes(&base).unwrap();
    resumed.restore_snapshot_bytes(&diff).unwrap();
    run_until_halt(&mut resumed);

    assert_eq!(baseline_out, resumed.take_serial_output());
}

#[test]
fn dirty_snapshot_is_rejected_without_matching_parent() {
    let boot = build_serial_boot_sector(b"OK\n");

    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    vm.set_disk_image(boot.to_vec()).unwrap();
    vm.reset();

    assert!(matches!(vm.run_slice(3), RunExit::Completed { .. }));
    let _base = vm.take_snapshot_full().unwrap();

    assert!(matches!(vm.run_slice(2), RunExit::Completed { .. }));
    let diff = vm.take_snapshot_dirty().unwrap();

    let mut resumed = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    resumed.set_disk_image(boot.to_vec()).unwrap();
    resumed.reset();

    let err = resumed.restore_snapshot_bytes(&diff).unwrap_err();
    assert!(
        err.to_string().contains("snapshot parent mismatch"),
        "unexpected error: {err}"
    );
}
