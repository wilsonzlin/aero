use firmware::bios::{
    Bios, BiosConfig, BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR, TICKS_PER_DAY,
};
use firmware::rtc::DateTime;
use machine::{CpuExit, InMemoryDisk, MemoryAccess};
use std::time::Duration;
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

#[test]
fn int1a_get_system_time_returns_bda_ticks() {
    let bios = Bios::new(firmware::rtc::CmosRtc::new(DateTime::new(
        2026, 1, 1, 0, 0, 0,
    )));
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // BDA tick count is initialized during POST.
    assert_eq!(vm.mem.read_u32(BDA_TICK_COUNT_ADDR), 0);
    assert_eq!(vm.mem.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);
    vm.cpu.rax = 0x0000; // AH=00h Get System Time

    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x1A));
    assert_eq!(vm.step(), CpuExit::Continue);

    let ticks = (((vm.cpu.rcx & 0xFFFF) as u32) << 16) | ((vm.cpu.rdx & 0xFFFF) as u32);
    assert_eq!(ticks, vm.mem.read_u32(BDA_TICK_COUNT_ADDR));
    assert_eq!((vm.cpu.rax & 0xFF) as u8, 0);
}

#[test]
fn int1a_midnight_flag_is_reported_and_cleared() {
    let bios = Bios::new(firmware::rtc::CmosRtc::new(DateTime::new(
        2026, 1, 1, 23, 59, 59,
    )));
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Cross midnight by advancing 2 seconds.
    vm.bios.advance_time(&mut vm.mem, Duration::from_secs(2));
    assert_eq!(vm.mem.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 1);

    // Program: INT 1Ah; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x1A, 0xF4]);
    vm.cpu.rax = 0x0000; // AH=00h Get System Time

    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x1A));
    assert_eq!(vm.step(), CpuExit::Continue);

    let ticks = (((vm.cpu.rcx & 0xFFFF) as u32) << 16) | ((vm.cpu.rdx & 0xFFFF) as u32);
    assert_eq!(ticks, vm.mem.read_u32(BDA_TICK_COUNT_ADDR));
    assert_eq!(ticks, (u64::from(TICKS_PER_DAY) * 1 / 86_400) as u32);
    assert_eq!((vm.cpu.rax & 0xFF) as u8, 1);
    assert_eq!(vm.mem.read_u8(BDA_MIDNIGHT_FLAG_ADDR), 0);
}
