use firmware::bios::{Bios, BiosConfig};
use machine::{CpuExit, InMemoryDisk, MemoryAccess};
use vm::Vm;

#[test]
fn boot_sector_int_sanity_exercises_int10_13_15_16_and_reaches_signature() {
    let boot: &[u8] = include_bytes!("../../../test_images/boot_sectors/int_sanity.bin");
    assert_eq!(boot.len(), 512, "boot sector must be exactly 512 bytes");
    assert_eq!(
        &boot[510..512],
        &[0x55, 0xAA],
        "boot sector missing 0x55AA signature"
    );

    // Two sectors: boot sector + data sector.
    let mut disk_bytes = vec![0u8; 2 * 512];
    disk_bytes[..512].copy_from_slice(boot);
    disk_bytes[512] = 0x99;
    let disk = InMemoryDisk::new(disk_bytes);

    let bios = Bios::new(BiosConfig {
        memory_size_bytes: 64 * 1024 * 1024,
        ..BiosConfig::default()
    });

    let mut vm = Vm::new(64 * 1024 * 1024, bios, disk);
    vm.reset();
    vm.bios.push_key(0x2C5A); // scan=0x2C, ascii='Z'

    for _ in 0..10_000 {
        if vm.step() == CpuExit::Halt {
            break;
        }
    }

    assert!(vm.cpu.halted, "boot sector did not halt");
    assert_eq!(vm.mem.read_u16(0x0530), 0x4B4F, "missing OK signature");
    assert_eq!(vm.bios.tty_output(), b"A", "INT10 output mismatch");
    assert_eq!(vm.mem.read_u16(0x0510), 0x2C5A, "INT16 key mismatch");
    assert_eq!(vm.mem.read_u8(0x0520), 0x99, "INT13 disk read mismatch");
}

#[test]
fn real_mode_payload_enables_a20_and_stops_aliasing() {
    // This payload:
    // - disables A20 via INT 15h AX=2400 and demonstrates wraparound at 1MiB
    // - enables A20 via INT 15h AX=2401 and demonstrates distinct addressing
    //
    // The payload stores:
    //   [0x0002..0x0003] = value observed at 0x0000 after the "disabled A20" write
    //   [0x0004..0x0005] = value observed at 0x0000 after enabling A20 and rewriting
    let payload: &[u8] = &[
        0xB8, 0x00, 0x24, // mov ax, 0x2400 (disable A20)
        0xCD, 0x15, // int 15h
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x00, 0x11, 0x11, // mov word [0], 0x1111
        0xB8, 0xFF, 0xFF, // mov ax, 0xFFFF
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x10, 0x00, 0x22, 0x22, // mov word [0x0010], 0x2222 (phys 1MiB)
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xA0, 0x00, 0x00, // mov al, [0x0000]
        0xA2, 0x02, 0x00, // mov [0x0002], al
        0xA0, 0x01, 0x00, // mov al, [0x0001]
        0xA2, 0x03, 0x00, // mov [0x0003], al
        0xB8, 0x01, 0x24, // mov ax, 0x2401 (enable A20)
        0xCD, 0x15, // int 15h
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x00, 0x33, 0x33, // mov word [0], 0x3333
        0xB8, 0xFF, 0xFF, // mov ax, 0xFFFF
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x10, 0x00, 0x44, 0x44, // mov word [0x0010], 0x4444 (phys 1MiB)
        0xB8, 0x00, 0x00, // mov ax, 0
        0x8E, 0xD8, // mov ds, ax
        0xA0, 0x00, 0x00, // mov al, [0x0000]
        0xA2, 0x04, 0x00, // mov [0x0004], al
        0xA0, 0x01, 0x00, // mov al, [0x0001]
        0xA2, 0x05, 0x00, // mov [0x0005], al
        0xF4, // hlt
    ];

    let mut sector = [0u8; 512];
    sector[..payload.len()].copy_from_slice(payload);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    let disk = InMemoryDisk::from_boot_sector(sector);

    let bios = Bios::new(BiosConfig {
        enable_acpi: false,
        memory_size_bytes: 2 * 1024 * 1024,
        ..BiosConfig::default()
    });

    let mut vm = Vm::new(2 * 1024 * 1024, bios, disk);
    vm.reset();

    for _ in 0..2_000 {
        if vm.step() == CpuExit::Halt {
            break;
        }
    }

    assert!(vm.cpu.halted, "payload should terminate with HLT");
    assert_eq!(
        vm.mem.read_u16(0x0002),
        0x2222,
        "expected aliasing while A20 disabled"
    );
    assert_eq!(
        vm.mem.read_u16(0x0004),
        0x3333,
        "expected 0x0000 to remain 0x3333 after re-enabling A20"
    );
    assert_eq!(
        vm.mem.read_u16(0x1_00000),
        0x4444,
        "expected 0x1_00000 to contain 0x4444 after enabling A20"
    );
}

