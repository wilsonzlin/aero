use aero_devices::a20_gate::A20Gate as Port92A20Gate;
use aero_devices::i8042::{I8042Ports, PlatformSystemControlSink};
use aero_platform::Platform;

#[test]
fn a20_state_is_shared_between_devices_and_platform_memory() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    let a20 = platform.chipset.a20();

    // 1) Register the fast A20 gate latch (port 0x92).
    platform
        .io
        .register(0x92, Box::new(Port92A20Gate::new(a20.clone())));

    // 2) Register the i8042 controller, wiring the output port callbacks to the same A20 handle.
    let i8042 = I8042Ports::new();
    let controller = i8042.controller();
    controller
        .borrow_mut()
        .set_system_control_sink(Box::new(PlatformSystemControlSink::new(a20.clone())));
    platform.io.register(0x60, Box::new(i8042.port60()));
    platform.io.register(0x64, Box::new(i8042.port64()));

    // Write distinct bytes while A20 is enabled.
    platform.io.write_u8(0x92, 0x02);
    platform.memory.write_u8(0x0, 0x11);
    platform.memory.write_u8(0x1_00000, 0x22);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    // Disable A20 via port 0x92 and verify aliasing.
    platform.io.write_u8(0x92, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    // i8042 output port reads should observe the same line state.
    platform.io.write_u8(0x64, 0xD0);
    assert_eq!(platform.io.read_u8(0x60) & 0x02, 0x00);

    // Enable A20 via the i8042 output port path and verify separation again.
    platform.io.write_u8(0x64, 0xD1);
    platform.io.write_u8(0x60, 0x03);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    platform.io.write_u8(0x64, 0xD0);
    assert_eq!(platform.io.read_u8(0x60) & 0x02, 0x02);

    // Disable A20 again and verify aliasing.
    platform.io.write_u8(0x92, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    platform.io.write_u8(0x64, 0xD0);
    assert_eq!(platform.io.read_u8(0x60) & 0x02, 0x00);
}
