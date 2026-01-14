use aero_machine::{Machine, MachineConfig, RunExit};
use aero_storage::SECTOR_SIZE;

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn bios_panic_is_mirrored_to_com1_when_serial_enabled() {
    // Keep the machine minimal so no platform timer interrupts can wake HLT unexpectedly.
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_vga: false,
        enable_serial: true,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).expect("machine should construct");

    // Deliberately invalid boot sector: missing the 0x55AA signature.
    m.set_disk_image(vec![0u8; SECTOR_SIZE])
        .expect("disk size should be accepted");
    m.reset();

    assert!(
        m.cpu().halted,
        "CPU should be halted after BIOS boot failure"
    );
    assert!(
        matches!(m.run_slice(1), RunExit::Halted { .. }),
        "expected halted run exit"
    );

    let serial = m.serial_output_bytes();
    assert!(
        contains_bytes(&serial, b"Invalid boot signature")
            || contains_bytes(&serial, b"Disk read error"),
        "unexpected serial output: {}",
        String::from_utf8_lossy(&serial)
    );
}
