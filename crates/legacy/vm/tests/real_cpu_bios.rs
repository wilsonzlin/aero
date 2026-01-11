use aero_cpu_core::state::{gpr, FLAG_CF, RFLAGS_IF};
use firmware::bda::BiosDataArea;
use firmware::bios::{Bios, BiosConfig, InMemoryDisk, BIOS_SEGMENT};
use memory::MemoryBus as _;
use vm::{CpuExit, Vm};

fn boot_sector_with(bytes: &[u8]) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let len = bytes.len().min(510);
    sector[..len].copy_from_slice(&bytes[..len]);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn int10_tty_hypercall_roundtrip_updates_text_mode_state() {
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
    vm.cpu.gpr[gpr::RAX] = 0x0E00 | (b'A' as u64);

    // INT: transfers to ROM stub.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.cpu.segments.cs.selector, BIOS_SEGMENT);

    // ROM stub `HLT`: triggers BIOS dispatch.
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x10));

    // ROM stub `IRET`: returns to caller.
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.cpu.segments.cs.selector, 0x0000);
    assert_eq!(vm.cpu.rip(), 0x7C02);

    // Final HLT halts the CPU.
    assert_eq!(vm.step(), CpuExit::Halt);

    assert_eq!(vm.bios.tty_output(), b"A");
    assert_eq!(vm.mem.read_u8(0xB8000), b'A');
    assert_eq!(vm.mem.read_u8(0xB8001), 0x07);
    assert_eq!(
        BiosDataArea::read_cursor_pos_page0(&mut vm.mem),
        (0, 1),
        "INT 10h teletype should advance the cursor"
    );
}

#[test]
fn bios_interrupt_preserves_interrupt_flag_from_caller() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg);
    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: CLI; STI; INT 10h; HLT
    vm.mem
        .write_physical(0x7C00, &[0xFA, 0xFB, 0xCD, 0x10, 0xF4]);
    vm.cpu.gpr[gpr::RAX] = 0x0E00 | (b'X' as u64);

    assert!(vm.cpu.get_flag(RFLAGS_IF), "POST should leave IF enabled");

    assert_eq!(vm.step(), CpuExit::Continue); // CLI
    assert!(!vm.cpu.get_flag(RFLAGS_IF));

    assert_eq!(vm.step(), CpuExit::Continue); // STI
    assert!(vm.cpu.get_flag(RFLAGS_IF));

    assert_eq!(vm.step(), CpuExit::Continue); // INT
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x10)); // HLT in ROM stub
    assert_eq!(vm.step(), CpuExit::Continue); // IRET

    // BIOS dispatch must preserve IF from the original interrupt frame, despite the CPU clearing
    // IF while delivering the software interrupt.
    assert!(vm.cpu.get_flag(RFLAGS_IF));
    assert_eq!(vm.cpu.segments.cs.selector, 0x0000);
    assert_eq!(vm.cpu.rip(), 0x7C04);

    assert_eq!(vm.step(), CpuExit::Halt);
    assert_eq!(vm.bios.tty_output(), b"X");
}

#[test]
fn int13_ext_read_reads_lba_into_memory() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg);

    let boot_sector = boot_sector_with(&[]);
    let mut disk_bytes = vec![0u8; 512 * 2];
    disk_bytes[..512].copy_from_slice(&boot_sector);
    disk_bytes[512..].fill(0xAA);
    let disk = InMemoryDisk::new(disk_bytes);

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // Disk Address Packet at 0000:0500.
    let dap = 0x0500u64;
    vm.mem.write_u8(dap + 0, 0x10);
    vm.mem.write_u8(dap + 1, 0x00);
    vm.mem.write_u16(dap + 2, 1); // sector count
    vm.mem.write_u16(dap + 4, 0x1000); // offset
    vm.mem.write_u16(dap + 6, 0x0000); // segment
    vm.mem.write_u64(dap + 8, 1); // LBA

    vm.cpu.gpr[gpr::RAX] = 0x4200; // AH=42h
    vm.cpu.gpr[gpr::RDX] = 0x0080; // DL=0x80
    vm.cpu.gpr[gpr::RSI] = 0x0500;

    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x13));
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::Halt);

    assert!(!vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0);
    assert_eq!(vm.mem.read_bytes(0x1000, 512), vec![0xAA; 512]);
}

#[test]
fn int13_ext_read_rejects_invalid_dap_size() {
    let cfg = BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        ..BiosConfig::default()
    };
    let bios = Bios::new(cfg);

    let disk = InMemoryDisk::from_boot_sector(boot_sector_with(&[]));
    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    // Program: INT 13h; HLT
    vm.mem.write_physical(0x7C00, &[0xCD, 0x13, 0xF4]);

    // Bogus DAP size (must be 0x10 or 0x18).
    let dap = 0x0500u64;
    vm.mem.write_u8(dap + 0, 0x11);
    vm.mem.write_u8(dap + 1, 0x00);

    vm.cpu.gpr[gpr::RAX] = 0x4200;
    vm.cpu.gpr[gpr::RDX] = 0x0080;
    vm.cpu.gpr[gpr::RSI] = 0x0500;

    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::BiosInterrupt(0x13));
    assert_eq!(vm.step(), CpuExit::Continue);
    assert_eq!(vm.step(), CpuExit::Halt);

    assert!(vm.cpu.get_flag(FLAG_CF));
    assert_eq!(((vm.cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0x01);
}
