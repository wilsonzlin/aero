use std::cell::Cell;
use std::rc::Rc;

use aero_devices::i8042::{I8042Ports, PlatformSystemControlSink};
use aero_platform::Platform;

#[test]
fn i8042_output_port_toggles_a20_gate_in_platform_memory() {
    let mut platform = Platform::new(2 * 1024 * 1024);

    let reset_count = Rc::new(Cell::new(0u32));
    let i8042 = I8042Ports::new();
    let controller = i8042.controller();

    let reset_handle = reset_count.clone();
    controller.borrow_mut().set_system_control_sink(Box::new(
        PlatformSystemControlSink::with_reset_callback(
            platform.chipset.a20(),
            Box::new(move || reset_handle.set(reset_handle.get() + 1)),
        ),
    ));

    platform.io.register(0x60, Box::new(i8042.port60()));
    platform.io.register(0x64, Box::new(i8042.port64()));

    // Before enabling A20, 0x1_00000 aliases 0x0.
    platform.memory.write_u8(0x0, 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xAA);

    // Enable A20 via i8042 output port write: set bit 1 while keeping reset
    // deasserted (bit 0 = 1).
    platform.io.write_u8(0x64, 0xD1);
    platform.io.write_u8(0x60, 0x03);

    platform.memory.write_u8(0x1_00000, 0xBB);
    assert_eq!(platform.memory.read_u8(0x0), 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xBB);

    assert_eq!(reset_count.get(), 0);
}
