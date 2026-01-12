use aero_devices::a20_gate::{A20Gate, A20_GATE_PORT};
use aero_platform::reset::{ResetKind, ResetLatch};
use aero_platform::Platform;

#[test]
fn a20_disabled_wraps_at_1mib() {
    let mut platform = Platform::new(2 * 1024 * 1024);

    platform.memory.write_u8(0x0, 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xAA);
}

#[test]
fn a20_enabled_makes_1mib_distinct() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    platform.io.register(
        A20_GATE_PORT,
        Box::new(A20Gate::new(platform.chipset.a20())),
    );

    platform.memory.write_u8(0x0, 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xAA);

    platform.io.write_u8(A20_GATE_PORT, 0x02);
    platform.memory.write_u8(0x1_00000, 0xBB);

    assert_eq!(platform.memory.read_u8(0x0), 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xBB);
}

#[test]
fn port_92h_toggles_a20_state() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    platform.io.register(
        A20_GATE_PORT,
        Box::new(A20Gate::new(platform.chipset.a20())),
    );

    platform.memory.write_u8(0x0, 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    platform.io.write_u8(A20_GATE_PORT, 0xA2);
    assert_eq!(platform.io.read_u8(A20_GATE_PORT), 0xA2);

    platform.memory.write_u8(0x1_00000, 0x22);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    platform.io.write_u8(A20_GATE_PORT, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);
}

#[test]
fn port_92h_reset_bit_invokes_callback_and_self_clears() {
    let mut platform = Platform::new(2 * 1024 * 1024);

    let reset_latch = ResetLatch::new();
    let reset_sink = reset_latch.clone();

    platform.io.register(
        A20_GATE_PORT,
        Box::new(A20Gate::with_reset_sink(platform.chipset.a20(), reset_sink)),
    );

    platform.io.write_u8(A20_GATE_PORT, 0x03);
    assert_eq!(reset_latch.take(), Some(ResetKind::System));

    let value = platform.io.read_u8(A20_GATE_PORT);
    assert_eq!(value & 0x01, 0);
    assert_eq!(value & 0x02, 0x02);
}

#[test]
fn a20_masking_applies_across_multi_byte_accesses() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    platform.io.register(
        A20_GATE_PORT,
        Box::new(A20Gate::new(platform.chipset.a20())),
    );

    // A 2-byte access starting at 0x0F_FFFF crosses the 1MiB boundary.
    // With A20 disabled, the second byte (0x1_00000) must alias to 0x0.
    platform.memory.write_u8(0x0F_FFFF, 0xAA);
    platform.memory.write_u8(0x0, 0xBB);

    let mut buf = [0u8; 2];
    platform.memory.read_physical(0x0F_FFFF, &mut buf);
    assert_eq!(buf, [0xAA, 0xBB]);

    // The same rule must apply on writes.
    platform.memory.write_physical(0x0F_FFFF, &[0x11, 0x22]);
    assert_eq!(platform.memory.read_u8(0x0F_FFFF), 0x11);
    assert_eq!(platform.memory.read_u8(0x0), 0x22);

    // Enable A20: now the second byte should land at 0x1_00000 instead of wrapping.
    let value = platform.io.read_u8(A20_GATE_PORT);
    platform.io.write_u8(A20_GATE_PORT, value | 0x02);
    platform.memory.write_u8(0x0F_FFFF, 0xCC);
    platform.memory.write_u8(0x1_00000, 0xDD);
    platform.memory.read_physical(0x0F_FFFF, &mut buf);
    assert_eq!(buf, [0xCC, 0xDD]);
}
