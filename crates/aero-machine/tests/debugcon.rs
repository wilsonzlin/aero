use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_debugcon_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov dx, 0x00E9
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xE9, 0x00]);
    i += 3;

    // outb: 'A'
    // mov al, 'A'
    sector[i..i + 2].copy_from_slice(&[0xB0, b'A']);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // outw: 'B', 'C' (little-endian)
    // mov ax, 0x4342
    sector[i..i + 3].copy_from_slice(&[0xB8, b'B', b'C']);
    i += 3;
    // out dx, ax
    sector[i] = 0xEF;
    i += 1;

    // outl: 'D', 'E', 'F', 'G' (little-endian)
    // mov eax, 0x47464544 (operand-size override in 16-bit mode)
    sector[i..i + 6].copy_from_slice(&[0x66, 0xB8, b'D', b'E', b'F', b'G']);
    i += 6;
    // out dx, eax (0x66 prefix)
    sector[i..i + 2].copy_from_slice(&[0x66, 0xEF]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    let mut halted = false;
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => {
                halted = true;
                break;
            }
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");
}

#[test]
fn debugcon_captures_port_e9_output() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_debugcon: true,
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        ..Default::default()
    })
    .unwrap();

    let boot = build_debugcon_boot_sector();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    assert_eq!(m.take_debugcon_output(), b"ABCDEFG");
}
