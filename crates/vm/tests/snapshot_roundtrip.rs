use firmware::bios::{Bios, BiosConfig};
use machine::{CpuExit, InMemoryDisk, MemoryAccess};
use vm::{SnapshotOptions, Vm};

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
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 10h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x10, 0xF4]);
    vm.cpu.rax = 0x0E00 | (b'A' as u64);

    // Execute INT: sets up pending BIOS interrupt + jumps into ROM stub.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.cpu.pending_bios_int, Some(0x10));

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
    };
    let bios = Bios::new(cfg.clone());
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 10h; INT 10h; HLT
    vm.mem
        .write_physical(0x7C00, &[0xCD, 0x10, 0xCD, 0x10, 0xF4]);
    vm.cpu.rax = 0x0E00 | (b'A' as u64);

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
