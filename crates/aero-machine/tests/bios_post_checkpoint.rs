use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_serial_boot_sector(message: &[u8]) -> [u8; 512] {
    let mut sector = [0u8; 512];
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

    // hlt
    sector[i] = 0xF4;

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
