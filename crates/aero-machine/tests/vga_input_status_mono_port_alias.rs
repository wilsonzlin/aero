use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn vga_input_status_mono_port_alias_resets_attribute_flip_flop() {
    for enable_pc_platform in [false, true] {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform,
            enable_vga: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: false,
            enable_virtio_net: false,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();

        // Program attribute controller register 0x11 to a known value.
        m.io_write(0x3C0, 1, 0x11);
        m.io_write(0x3C0, 1, 0xAA);

        // Select index 0x11 again, which puts the attribute controller into the "data" state.
        m.io_write(0x3C0, 1, 0x11);

        // Reading input status should reset the attribute flip-flop back to the "index" state.
        //
        // Ensure the mono alias port works too (0x3BA vs 0x3DA).
        m.io_read(0x3BA, 1);

        // If the flip-flop was reset, this write should be treated as an *index* write, not data.
        // If 0x3BA isn't decoded or doesn't reset the flip-flop, this will overwrite attribute[0x11]
        // instead.
        m.io_write(0x3C0, 1, 0x12);

        // Ensure we're back in the "index" state before selecting an index for readback.
        m.io_read(0x3DA, 1);

        // Read back register 0x11 and verify it wasn't clobbered by the write above.
        m.io_write(0x3C0, 1, 0x11);
        let v = m.io_read(0x3C1, 1) as u8;
        assert_eq!(v, 0xAA);
    }
}
