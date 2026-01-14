use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_debugcon_boot_sector(message: &[u8]) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    for &b in message {
        // mov al, imm8
        sector[i..i + 2].copy_from_slice(&[0xB0, b]);
        i += 2;
        // out 0xE9, al
        sector[i..i + 2].copy_from_slice(&[0xE6, 0xE9]);
        i += 2;
    }

    // hlt
    sector[i] = 0xF4;

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
fn debugcon_port_0xe9_captures_output_bytes() {
    let mut m = Machine::new(MachineConfig {
        // Use a realistic RAM size so BIOS POST can place ACPI/SMBIOS structures when the PC
        // platform is enabled.
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_debugcon: true,
        ..Default::default()
    })
    .unwrap();

    let boot = build_debugcon_boot_sector(b"OK\n");
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    assert_eq!(m.debugcon_output_len(), 3);
    assert_eq!(m.take_debugcon_output(), b"OK\n");
    assert_eq!(m.debugcon_output_len(), 0);
}
