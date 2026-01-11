use aero_cpu_core::state::gpr;
use aero_snapshot::RamMode;
use firmware::bios::{Bios, BiosConfig};
use firmware::bios::InMemoryDisk;
use memory::MemoryBus as _;
use vm::{CpuExit, SnapshotError, SnapshotOptions, Vm};

fn boot_sector_with(bytes: &[u8]) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let len = bytes.len().min(510);
    sector[..len].copy_from_slice(&bytes[..len]);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn snapshot_round_trip_preserves_pending_bios_int() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 10h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x10, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0E00 | (b'A' as u64);

    // Execute INT: sets up pending BIOS interrupt + jumps into ROM stub.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert!(vm.cpu.pending_bios_int_valid);
    assert_eq!(vm.cpu.pending_bios_int, 0x10);

    let snapshot = vm.save_snapshot(SnapshotOptions::default()).unwrap();

    // Baseline completion.
    let mut baseline = vm;
    assert_eq!(baseline.step(), CpuExit::BiosInterrupt(0x10));
    assert_eq!(baseline.step(), CpuExit::Continue); // IRET
    assert_eq!(baseline.step(), CpuExit::Halt);
    let expected = baseline.bios.tty_output().to_vec();

    // Restore into a fresh VM and continue.
    let bios2 = Bios::new(cfg);
    let disk2 = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut restored = Vm::new(16 * 1024 * 1024, bios2, disk2);
    restored.reset();
    restored.restore_snapshot(&snapshot).unwrap();

    assert_eq!(restored.step(), CpuExit::BiosInterrupt(0x10));
    assert_eq!(restored.step(), CpuExit::Continue);
    assert_eq!(restored.step(), CpuExit::Halt);

    assert_eq!(restored.bios.tty_output(), expected);
}

#[test]
fn snapshot_round_trip_preserves_bios_output_buffer() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 10h; INT 10h; HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x10, 0xCD, 0x10, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0E00 | (b'A' as u64);

    // Run first INT to completion (ROM stub prints once).
    assert_eq!(vm.step(), CpuExit::Continue); // INT
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x10)); // HLT in ROM stub -> dispatch
    assert_eq!(vm.step(), CpuExit::Continue); // IRET back to boot code
    assert_eq!(vm.bios.tty_output(), b"A");

    let snapshot = vm.save_snapshot(SnapshotOptions::default()).unwrap();

    // Finish baseline.
    let mut baseline = vm;
    assert_eq!(baseline.step(), CpuExit::Continue); // second INT
    assert_eq!(baseline.step(), CpuExit::BiosInterrupt(0x10));
    assert_eq!(baseline.step(), CpuExit::Continue);
    assert_eq!(baseline.step(), CpuExit::Halt);
    let expected = baseline.bios.tty_output().to_vec();

    // Restore snapshot and finish.
    let bios2 = Bios::new(cfg);
    let disk2 = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut restored = Vm::new(16 * 1024 * 1024, bios2, disk2);
    restored.reset();
    restored.restore_snapshot(&snapshot).unwrap();

    assert_eq!(restored.step(), CpuExit::Continue);
    assert_eq!(restored.step(), CpuExit::BiosInterrupt(0x10));
    assert_eq!(restored.step(), CpuExit::Continue);
    assert_eq!(restored.step(), CpuExit::Halt);

    assert_eq!(restored.bios.tty_output(), expected);
}

#[test]
fn snapshot_round_trip_dirty_chain_preserves_stack_writes() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 10h; INT 10h; HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x10, 0xCD, 0x10, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0E00 | (b'A' as u64);

    // First INT to completion.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x10));
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.bios.tty_output(), b"A");

    // Stack frame written by INT: return IP (0x7C02) is stored at 0x7BFA.
    assert_eq!(vm.mem.read_u8(0x7BFA), 0x02);

    let base = vm.save_snapshot(SnapshotOptions::default()).unwrap();

    // Second INT to completion; pushes return IP (0x7C04), overwriting the same stack slot.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x10));
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.bios.tty_output(), b"AA");
    assert_eq!(vm.mem.read_u8(0x7BFA), 0x04);

    let diff = vm
        .save_snapshot(SnapshotOptions {
            ram_mode: RamMode::Dirty,
        })
        .unwrap();

    let mut baseline = vm;
    assert_eq!(baseline.step(), CpuExit::Halt);
    let expected_output = baseline.bios.tty_output().to_vec();
    let expected_stack_byte = baseline.mem.read_u8(0x7BFA);

    // Restoring a dirty snapshot without first restoring its full parent should fail fast.
    let bios2 = Bios::new(cfg.clone());
    let disk2 = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut wrong_base = Vm::new(16 * 1024 * 1024, bios2, disk2);
    wrong_base.reset();
    let err = wrong_base.restore_snapshot(&diff).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("snapshot parent mismatch")
    ));

    // Restore base + diff into a fresh VM.
    let bios2 = Bios::new(cfg);
    let disk2 = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut restored = Vm::new(16 * 1024 * 1024, bios2, disk2);
    restored.reset();
    restored.restore_snapshot(&base).unwrap();
    restored.restore_snapshot(&diff).unwrap();

    assert_eq!(restored.mem.read_u8(0x7BFA), expected_stack_byte);
    assert_eq!(restored.step(), CpuExit::Halt);
    assert_eq!(restored.bios.tty_output(), expected_output);
}

#[test]
fn snapshot_restore_rejects_dirty_snapshot_with_wrong_parent() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    let base = vm.save_snapshot(SnapshotOptions::default()).unwrap();

    // Dirty some RAM so the next snapshot is a diff with a non-null parent id.
    vm.mem.write_physical(0x1234, &[0xAA]);
    let diff = vm
        .save_snapshot(SnapshotOptions {
            ram_mode: RamMode::Dirty,
        })
        .unwrap();

    // Restoring a dirty diff without having applied its base snapshot should fail fast.
    let bios2 = Bios::new(cfg);
    let disk2 = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut restored = Vm::new(16 * 1024 * 1024, bios2, disk2);
    restored.reset();

    let err = restored.restore_snapshot(&diff).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("snapshot parent mismatch")
    ));

    // Full snapshots ignore expected-parent validation and must still restore fine.
    restored.restore_snapshot(&base).unwrap();
}

#[test]
fn snapshot_restore_requires_full_dirty_parent_chain() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Base full snapshot A.
    let base = vm.save_snapshot(SnapshotOptions::default()).unwrap();

    // Dirty diff snapshot B (parent A).
    vm.mem.write_physical(0x1111, &[0xAA]);
    let diff1 = vm
        .save_snapshot(SnapshotOptions {
            ram_mode: RamMode::Dirty,
        })
        .unwrap();

    // Dirty diff snapshot C (parent B).
    vm.mem.write_physical(0x2222, &[0xBB]);
    let diff2 = vm
        .save_snapshot(SnapshotOptions {
            ram_mode: RamMode::Dirty,
        })
        .unwrap();

    let bios2 = Bios::new(cfg);
    let disk2 = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut restored = Vm::new(16 * 1024 * 1024, bios2, disk2);
    restored.reset();

    // Cannot apply C onto a fresh VM (no parent restored).
    let err = restored.restore_snapshot(&diff2).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("snapshot parent mismatch")));

    // Restoring A then skipping B and applying C must also fail (wrong parent id).
    restored.restore_snapshot(&base).unwrap();
    let err = restored.restore_snapshot(&diff2).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("snapshot parent mismatch")));

    // Restoring the full chain A -> B -> C should succeed.
    restored.restore_snapshot(&diff1).unwrap();
    restored.restore_snapshot(&diff2).unwrap();
    assert_eq!(restored.mem.read_u8(0x1111), 0xAA);
    assert_eq!(restored.mem.read_u8(0x2222), 0xBB);
}

#[test]
fn snapshot_restore_full_snapshot_ignores_existing_last_snapshot_id() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    let base = vm.save_snapshot(SnapshotOptions::default()).unwrap();
    let expected_rip = vm.cpu.rip();
    let expected_cs = vm.cpu.segments.cs.selector;
    let expected_boot_opcode = vm.mem.read_u8(0x7C00);

    // Create a VM that already has a non-None last_snapshot_id, then restore a full snapshot into it.
    // Full snapshots are standalone and must ignore parent validation.
    let bios2 = Bios::new(cfg);
    let disk2 = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut restored = Vm::new(16 * 1024 * 1024, bios2, disk2);
    restored.reset();

    // Take any snapshot to set last_snapshot_id, then perturb state so restore definitely does work.
    let _ = restored.save_snapshot(SnapshotOptions::default()).unwrap();
    restored.cpu.set_rip(0);

    restored.restore_snapshot(&base).unwrap();
    assert_eq!(restored.cpu.rip(), expected_rip);
    assert_eq!(restored.cpu.segments.cs.selector, expected_cs);
    assert_eq!(restored.mem.read_u8(0x7C00), expected_boot_opcode);
}
