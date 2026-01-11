mod common;

use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_x86::Register;
use common::machine::{TestBus, Tier0Machine};

#[test]
fn boot_sector_hello_via_int10() {
    // A tiny boot sector that prints "Hello" using INT 10h.
    //
    // We install an IVT handler for INT 10h that outputs AL to port 0xE9 and IRET's.
    // This exercises:
    // - real-mode segmentation (CS/DS/SS base handling)
    // - string op LODSB
    // - INT/IRET delivery via the IVT
    // - port I/O (debugcon 0xE9)
    // - HLT

    let mut bus = TestBus::new(1024 * 1024);

    let boot_addr = 0x7C00u64;
    let msg_off = boot_addr as u16 + 0x1B; // after the code below
    let boot = [
        0x31,
        0xC0, // xor ax,ax
        0x8E,
        0xD8, // mov ds,ax
        0x8E,
        0xC0, // mov es,ax
        0x8E,
        0xD0, // mov ss,ax
        0xBC,
        0x00,
        0x7C, // mov sp,0x7c00
        0xBE,
        (msg_off & 0xFF) as u8,
        (msg_off >> 8) as u8, // mov si,msg
        0xFC,                 // cld
        0xAC,                 // lodsb
        0x0A,
        0xC0, // or al,al
        0x74,
        0x06, // jz done
        0xB4,
        0x0E, // mov ah,0x0e
        0xCD,
        0x10, // int 0x10
        0xEB,
        0xF5, // jmp loop (back 11 bytes)
        0xF4, // done: hlt
        b'H',
        b'e',
        b'l',
        b'l',
        b'o',
        0,
    ];
    bus.load(boot_addr, &boot);

    // BIOS handler at F000:0100 => physical 0xF0100.
    let bios_seg = 0xF000u16;
    let bios_off = 0x0100u16;
    let bios_addr = ((bios_seg as u64) << 4) + bios_off as u64;
    let bios = [
        0x50, // push ax
        0xE6, 0xE9, // out 0xE9, al
        0x58, // pop ax
        0xCF, // iret
    ];
    bus.load(bios_addr, &bios);

    // IVT[0x10] = bios handler.
    let ivt = 0x10u64 * 4;
    CpuBus::write_u16(&mut bus, ivt, bios_off).unwrap();
    CpuBus::write_u16(&mut bus, ivt + 2, bios_seg).unwrap();

    let mut cpu = CpuState::new(CpuMode::Bit16);
    cpu.write_reg(Register::CS, 0);
    cpu.set_rip(boot_addr);

    let mut machine = Tier0Machine::new(cpu, bus);
    machine.run(10_000);

    assert_eq!(
        std::str::from_utf8(machine.bus.debugcon()).unwrap(),
        "Hello"
    );
}
