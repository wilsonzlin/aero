use aero_devices::i8042::{I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn drain_i8042_output(m: &mut Machine) -> Vec<u8> {
    let mut out = Vec::new();
    while (m.io_read(I8042_STATUS_PORT, 1) as u8) & 0x01 != 0 {
        out.push(m.io_read(I8042_DATA_PORT, 1) as u8);
    }
    out
}

#[test]
fn machine_snapshot_roundtrip_preserves_pending_i8042_output_bytes() {
    // Keep the machine minimal: this test is specifically validating that i8042 pending output
    // bytes survive full-machine snapshot/restore deterministically.
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: true,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    assert_eq!(
        src.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "sanity: i8042 output buffer should start empty"
    );

    // Inject a make+break sequence without draining port 0x60. This leaves:
    // - the make byte in the i8042 output buffer, and
    // - the remaining break bytes queued internally.
    src.inject_browser_key("KeyA", true);
    src.inject_browser_key("KeyA", false);

    assert!(
        (src.io_read(I8042_STATUS_PORT, 1) as u8) & 0x01 != 0,
        "sanity: expected i8042 output buffer to become non-empty after key injection"
    );

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    assert!(
        (restored.io_read(I8042_STATUS_PORT, 1) as u8) & 0x01 != 0,
        "i8042 output buffer should remain non-empty after snapshot restore"
    );

    // Default i8042 translation is Set-2 -> Set-1, so "A" make/break is `0x1E, 0x9E`.
    let drained = drain_i8042_output(&mut restored);
    assert_eq!(drained, vec![0x1E, 0x9E]);

    // Ensure the i8042 output buffer is fully drained and does not replay bytes.
    assert_eq!(
        restored.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "i8042 output buffer should be empty after draining expected bytes"
    );
    assert_eq!(
        restored.io_read(I8042_DATA_PORT, 1) as u8,
        0x00,
        "reading port 0x60 with no pending data should return 0"
    );
    assert_eq!(
        restored.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "i8042 output buffer should remain empty after extra reads"
    );
}
