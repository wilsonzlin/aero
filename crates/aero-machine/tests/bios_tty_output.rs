use aero_machine::{Machine, MachineConfig, RunExit};
use aero_storage::SECTOR_SIZE;

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn boot_sector_int10_teletype(message: &[u8]) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];
    let mut i = 0usize;
    for &b in message {
        // mov ax, 0x0E00 | b
        sector[i] = 0xB8;
        sector[i + 1] = b;
        sector[i + 2] = 0x0E;
        i += 3;
        // int 0x10
        sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
        i += 2;
    }
    // hlt
    sector[i] = 0xF4;

    sector[SECTOR_SIZE - 2] = 0x55;
    sector[SECTOR_SIZE - 1] = 0xAA;
    sector
}

#[test]
fn bios_tty_output_exposes_boot_panic_message() {
    // Keep the machine minimal so no platform timer interrupts can wake HLT unexpectedly.
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).expect("machine should construct");

    // Deliberately invalid boot sector: missing the 0x55AA signature.
    let bad_disk = vec![0u8; SECTOR_SIZE];
    m.set_disk_image(bad_disk)
        .expect("disk size should be accepted");
    m.reset();

    // BIOS should panic and halt the CPU.
    assert!(
        m.cpu().halted,
        "CPU should be halted after BIOS boot failure"
    );
    let exit = m.run_slice(1);
    assert!(
        matches!(exit, RunExit::Halted { .. }),
        "expected halted run exit, got {exit:?}"
    );

    // BIOS should have recorded a panic message in its TTY output buffer.
    let tty = m.bios_tty_output();
    assert!(
        contains_bytes(tty, b"Invalid boot signature") || contains_bytes(tty, b"Disk read error"),
        "unexpected BIOS TTY output: {}",
        String::from_utf8_lossy(tty)
    );

    // Convenience APIs: cloning + clearing.
    let tty_cloned = m.bios_tty_output_bytes();
    assert_eq!(tty_cloned, tty);
    m.clear_bios_tty_output();
    assert!(m.bios_tty_output().is_empty());
}

#[test]
fn bios_tty_output_captures_int10_teletype_output() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).expect("machine should construct");
    m.set_disk_image(boot_sector_int10_teletype(b"Hi").to_vec())
        .expect("boot sector should be valid");
    m.reset();

    // Boot sector should run and halt.
    loop {
        match m.run_slice(1000) {
            RunExit::Halted { .. } => break,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected run exit: {other:?}"),
        }
    }

    assert!(
        contains_bytes(m.bios_tty_output(), b"Hi"),
        "unexpected BIOS TTY output: {}",
        String::from_utf8_lossy(m.bios_tty_output())
    );
}
