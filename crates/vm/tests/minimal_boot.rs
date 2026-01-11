use firmware::bios::{
    Bios, BiosConfig, BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR, TICKS_PER_DAY,
};
use firmware::rtc::{CmosRtc, DateTime};
use machine::{CpuExit, InMemoryDisk, MemoryAccess, FLAG_CF, FLAG_ZF};
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
        ..BiosConfig::default()
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
        ..BiosConfig::default()
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
    let bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
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
    let bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 23, 59, 59)));
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

#[test]
fn int13_chs_read_reads_second_sector_into_memory() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg);

    // Two sectors: boot sector + one data sector.
    let mut disk_bytes = vec![0u8; 2 * 512];
    disk_bytes[510] = 0x55;
    disk_bytes[511] = 0xAA;
    disk_bytes[512] = 0x42;
    let disk = InMemoryDisk::new(disk_bytes);

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // CHS read 1 sector from cylinder 0, head 0, sector 2 into 0x0000:0x0500.
    vm.cpu.rax = 0x0201;
    vm.cpu.rcx = 0x0002; // CH=0, CL=2
    vm.cpu.rdx = 0x0080; // DH=0, DL=0x80
    vm.cpu.es.selector = 0x0000;
    vm.cpu.rbx = 0x0500;

    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x13));
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::Halt);

    assert_eq!(vm.mem.read_u8(0x0500), 0x42);
    assert_eq!(vm.cpu.rflags & FLAG_CF, 0);
}

#[test]
fn int15_e820_returns_extended_attributes_when_requested() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg);
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 15h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x15, 0xF4]);

    // E820 query for first entry into ES:DI=0x0000:0x0600.
    vm.cpu.rax = 0xE820;
    vm.cpu.rdx = 0x534D_4150; // 'SMAP'
    vm.cpu.rbx = 0;
    vm.cpu.rcx = 24;
    vm.cpu.es.selector = 0x0000;
    vm.cpu.rdi = 0x0600;

    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x15));
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::Halt);

    assert_eq!(vm.cpu.rflags & FLAG_CF, 0);
    assert_eq!(vm.cpu.rax as u32, 0x534D_4150);
    assert_eq!(vm.cpu.rcx as u32, 24);
    assert_eq!(vm.cpu.rbx as u32, 1);

    let base = vm.mem.read_u64(0x0600);
    let length = vm.mem.read_u64(0x0608);
    let kind = vm.mem.read_u32(0x0610);
    let attrs = vm.mem.read_u32(0x0614);
    assert_eq!(base, 0);
    assert_ne!(length, 0);
    assert_eq!(kind, 1);
    assert_eq!(attrs, 1);
}

#[test]
fn int16_read_key_returns_scancode_and_ascii() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let mut bios = Bios::new(cfg);
    bios.push_key(0x2C5A); // scan=0x2C, ascii='Z'
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 16h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x16, 0xF4]);
    vm.cpu.rax = 0x0000;

    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x16));
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::Halt);

    assert_eq!(vm.cpu.rax as u16, 0x2C5A);
    assert_eq!(vm.cpu.rflags & FLAG_CF, 0);
    assert_eq!(vm.cpu.rflags & FLAG_ZF, 0);
}
