use aero_devices::i8042::{I8042Ports, I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_platform::Platform;

#[test]
fn i8042_port_bus_delivers_translated_scancodes() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    let i8042 = I8042Ports::new();
    let controller = i8042.controller();

    platform.io.register(I8042_DATA_PORT, Box::new(i8042.port60()));
    platform.io.register(I8042_STATUS_PORT, Box::new(i8042.port64()));

    controller.borrow_mut().inject_browser_key("KeyA", true);

    let status = platform.io.read_u8(I8042_STATUS_PORT);
    assert_ne!(status & 0x01, 0, "output buffer should be full");

    // With i8042 translation enabled by default, Set-2 0x1C becomes Set-1 0x1E.
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT), 0x1E);

    let status = platform.io.read_u8(I8042_STATUS_PORT);
    assert_eq!(status & 0x01, 0, "output buffer should be empty after read");
}

#[test]
fn i8042_command_byte_read_write_roundtrip() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    let i8042 = I8042Ports::new();

    platform.io.register(I8042_DATA_PORT, Box::new(i8042.port60()));
    platform.io.register(I8042_STATUS_PORT, Box::new(i8042.port64()));

    // Read the default command byte.
    platform.io.write_u8(I8042_STATUS_PORT, 0x20);
    let cmd = platform.io.read_u8(I8042_DATA_PORT);
    assert_eq!(cmd, 0x45);

    // Update it and read it back.
    platform.io.write_u8(I8042_STATUS_PORT, 0x60);
    platform.io.write_u8(I8042_DATA_PORT, 0x47);

    platform.io.write_u8(I8042_STATUS_PORT, 0x20);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT), 0x47);
}
