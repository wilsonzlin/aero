use firmware::bios::{Bios, BiosConfig};
use machine::{CpuExit, InMemoryDisk, MemoryAccess};
use vm::Vm;

fn boot_sector_with(bytes: &[u8]) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let len = bytes.len().min(510);
    sector[..len].copy_from_slice(&bytes[..len]);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn vm_resets_to_0x7c00_with_dl_set() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
    };
    let bios = Bios::new(cfg);
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[0x90, 0x90, 0x90]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    assert_eq!(vm.cpu.cs.selector, 0x0000);
    assert_eq!(vm.cpu.rip, 0x7C00);
    assert_eq!(vm.cpu.rsp, 0x7C00);
    assert_eq!(vm.cpu.rdx as u8, 0x80);

    // Execute a couple of NOPs so we know we started at the boot sector.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.cpu.rip, 0x7C01);
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.cpu.rip, 0x7C02);
}

#[test]
fn int10_tty_hypercall_roundtrip() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
    };
    let bios = Bios::new(cfg);
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 10h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x10, 0xF4]);
    vm.cpu.rax = 0x0E00 | (b'A' as u64);

    // Step INT: jumps to ROM stub.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.cpu.cs.selector, 0xF000);

    // Step HLT in ROM stub: VM dispatches the BIOS interrupt.
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x10));

    // Step IRET: returns to caller (after INT instruction).
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.cpu.cs.selector, 0x0000);
    assert_eq!(vm.cpu.rip, 0x7C02);

    // Step final HLT: stops.
    assert_eq!(vm.step(), CpuExit::Halt);
    assert_eq!(vm.bios.tty_output(), b"A");
}
