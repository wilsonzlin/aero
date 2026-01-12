use aero_devices::a20_gate::{A20Gate, A20_GATE_PORT};
use aero_platform::Platform;

fn boot_payload_check_a20(platform: &mut Platform) {
    // Legacy wraparound check: before enabling A20, 0x1_00000 aliases 0x0.
    platform.memory.write_u8(0x0, 0x5A);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x5A);

    // Typical "fast A20" enabling sequence: read-modify-write port 0x92.
    let mut sys_ctrl = platform.io.read_u8(A20_GATE_PORT);
    sys_ctrl |= 0x02;
    sys_ctrl &= !0x01;
    platform.io.write_u8(A20_GATE_PORT, sys_ctrl);

    // After enabling A20, 0x1_00000 should be distinct from 0x0.
    platform.memory.write_u8(0x1_00000, 0xC3);
    assert_eq!(platform.memory.read_u8(0x0), 0x5A);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xC3);
}

#[test]
fn boot_payload_smoke() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    platform.io.register(
        A20_GATE_PORT,
        Box::new(A20Gate::new(platform.chipset.a20())),
    );

    boot_payload_check_a20(&mut platform);
}
