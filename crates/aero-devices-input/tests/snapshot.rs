use aero_devices_input::I8042Controller;
use aero_io_snapshot::io::state::IoSnapshot;

#[test]
fn i8042_snapshot_roundtrip_preserves_pending_bytes() {
    let mut dev = I8042Controller::new();
    dev.inject_browser_key("KeyA", true);
    dev.inject_browser_key("KeyA", false);

    let snap = dev.save_state();

    let mut restored = I8042Controller::new();
    restored.load_state(&snap).unwrap();

    assert_eq!(restored.read_port(0x60), 0x1e);
    assert_eq!(restored.read_port(0x60), 0x9e);
    assert_eq!(restored.read_port(0x60), 0x00);
}

#[test]
fn i8042_snapshot_roundtrip_preserves_output_port_and_pending_write() {
    let mut dev = I8042Controller::new();

    // Set an initial output-port value.
    dev.write_port(0x64, 0xD1);
    dev.write_port(0x60, 0x03);

    // Leave an in-flight "write output port" pending write.
    dev.write_port(0x64, 0xD1);

    let snap = dev.save_state();

    let mut restored = I8042Controller::new();
    restored.load_state(&snap).unwrap();

    // Verify output port preserved.
    restored.write_port(0x64, 0xD0);
    assert_eq!(restored.read_port(0x60), 0x03);

    // Verify pending write preserved and targets the output port.
    restored.write_port(0x60, 0x01);
    restored.write_port(0x64, 0xD0);
    assert_eq!(restored.read_port(0x60), 0x01);
}
