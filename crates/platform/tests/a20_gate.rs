use aero_devices::a20_gate::A20Gate;
use aero_platform::Platform;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn a20_disabled_wraps_at_1mib() {
    let mut platform = Platform::new(2 * 1024 * 1024);

    platform.memory.write_u8(0x0, 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xAA);
}

#[test]
fn a20_enabled_makes_1mib_distinct() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    platform
        .io
        .register(0x92, Box::new(A20Gate::new(platform.chipset.a20())));

    platform.memory.write_u8(0x0, 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xAA);

    platform.io.write_u8(0x92, 0x02);
    platform.memory.write_u8(0x1_00000, 0xBB);

    assert_eq!(platform.memory.read_u8(0x0), 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xBB);
}

#[test]
fn port_92h_toggles_a20_state() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    platform
        .io
        .register(0x92, Box::new(A20Gate::new(platform.chipset.a20())));

    platform.memory.write_u8(0x0, 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    platform.io.write_u8(0x92, 0xA2);
    assert_eq!(platform.io.read_u8(0x92), 0xA2);

    platform.memory.write_u8(0x1_00000, 0x22);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    platform.io.write_u8(0x92, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);
}

#[test]
fn port_92h_reset_bit_invokes_callback_and_self_clears() {
    let mut platform = Platform::new(2 * 1024 * 1024);

    let reset_count = Rc::new(Cell::new(0u32));
    let reset_count_handle = reset_count.clone();

    platform.io.register(
        0x92,
        Box::new(A20Gate::with_reset_callback(
            platform.chipset.a20(),
            Box::new(move || reset_count_handle.set(reset_count_handle.get() + 1)),
        )),
    );

    platform.io.write_u8(0x92, 0x03);
    assert_eq!(reset_count.get(), 1);

    let value = platform.io.read_u8(0x92);
    assert_eq!(value & 0x01, 0);
    assert_eq!(value & 0x02, 0x02);
}
