mod common;

use firmware::bus::Bus;
use firmware::vm::RealModeVm;

use common::TestMachine;

#[test]
fn real_mode_payload_enables_a20_and_stops_aliasing() {
    // This test executes a tiny real-mode payload (in the RealModeVm instruction subset)
    // which disables A20 via INT 15h AX=2400, demonstrates wraparound at 1MiB, then
    // enables A20 via INT 15h AX=2401 and demonstrates distinct addressing.
    //
    // The payload stores:
    //   [0x0002..0x0003] = value observed at 0x0000 after the "disabled A20" write
    //   [0x0004..0x0005] = value observed at 0x0000 after enabling A20 and rewriting
    //
    // The test reads those bytes from RAM to validate behaviour.
    let mut m = TestMachine::new();

    // Real-mode payload loaded at 0000:7C00.
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

    let mut vm = RealModeVm::new(&mut m.bus, &mut m.bios);
    vm.load(0x7C00, payload);
    vm.run_until(2_000, |vm| vm.halted).unwrap();

    assert!(vm.halted, "payload should terminate with HLT");

    assert_eq!(
        m.bus.read_u16(0x0002),
        0x2222,
        "expected aliasing while A20 disabled"
    );
    assert_eq!(
        m.bus.read_u16(0x0004),
        0x3333,
        "expected 0x0000 to remain 0x3333 after re-enabling A20"
    );
    assert_eq!(
        m.bus.read_u16(0x1_00000),
        0x4444,
        "expected 0x1_00000 to contain 0x4444 after enabling A20"
    );
}

