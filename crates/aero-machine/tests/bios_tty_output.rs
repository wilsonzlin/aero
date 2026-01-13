use aero_machine::{Machine, MachineConfig, RunExit};

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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
    let bad_disk = vec![0u8; 512];
    m.set_disk_image(bad_disk)
        .expect("disk size should be accepted");
    m.reset();

    // BIOS should panic and halt the CPU.
    assert!(m.cpu().halted, "CPU should be halted after BIOS boot failure");
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
