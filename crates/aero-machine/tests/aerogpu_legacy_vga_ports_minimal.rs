use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_legacy_vga_ports_minimal_semantics() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this port-semantics test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // ---------------------------------------------------------------------
    // Misc output
    // ---------------------------------------------------------------------
    m.io_write(0x3C2, 1, 0x67);
    assert_eq!(m.io_read(0x3C2, 1) as u8, 0x67);
    assert_eq!(m.io_read(0x3CC, 1) as u8, 0x67);

    // ---------------------------------------------------------------------
    // Sequencer index/data
    // ---------------------------------------------------------------------
    m.io_write(0x3C4, 1, 0x02);
    m.io_write(0x3C5, 1, 0xBE);
    m.io_write(0x3C4, 1, 0x02);
    assert_eq!(m.io_read(0x3C5, 1) as u8, 0xBE);

    // Support 16-bit "index+data" word writes to the index port.
    m.io_write(0x3C4, 2, 0xAD03); // idx=0x03, data=0xAD
    m.io_write(0x3C4, 1, 0x03);
    assert_eq!(m.io_read(0x3C5, 1) as u8, 0xAD);

    // ---------------------------------------------------------------------
    // Graphics controller index/data
    // ---------------------------------------------------------------------
    m.io_write(0x3CE, 1, 0x06);
    m.io_write(0x3CF, 1, 0x4F);
    m.io_write(0x3CE, 1, 0x06);
    assert_eq!(m.io_read(0x3CF, 1) as u8, 0x4F);

    m.io_write(0x3CE, 2, 0x5507); // idx=0x07, data=0x55
    m.io_write(0x3CE, 1, 0x07);
    assert_eq!(m.io_read(0x3CF, 1) as u8, 0x55);

    // ---------------------------------------------------------------------
    // CRTC index/data + mono/color aliasing
    // ---------------------------------------------------------------------
    m.io_write(0x3D4, 1, 0x0E);
    m.io_write(0x3D5, 1, 0x12);
    m.io_write(0x3B4, 1, 0x0E);
    assert_eq!(m.io_read(0x3B5, 1) as u8, 0x12);

    m.io_write(0x3B4, 1, 0x0F);
    m.io_write(0x3B5, 1, 0x34);
    m.io_write(0x3D4, 1, 0x0F);
    assert_eq!(m.io_read(0x3D5, 1) as u8, 0x34);

    // ---------------------------------------------------------------------
    // Attribute controller flip-flop reset via Input Status 1 (0x3DA/0x3BA)
    // ---------------------------------------------------------------------
    // Program attribute controller register 0x11 to a known value.
    m.io_read(0x3DA, 1);
    m.io_write(0x3C0, 1, 0x11);
    m.io_write(0x3C0, 1, 0xAA);

    // Select index 0x11 again, which puts the attribute controller into the "data" state.
    m.io_write(0x3C0, 1, 0x11);

    // Reading input status should reset the attribute flip-flop back to the "index" state.
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

    // ---------------------------------------------------------------------
    // Input Status 1 vertical retrace bit should toggle with deterministic time.
    // ---------------------------------------------------------------------
    const STEP_NS: u64 = 100_000;
    const SAMPLES: usize = 500;

    let mut any_high = false;
    let mut any_low = false;
    for _ in 0..SAMPLES {
        let color = m.io_read(0x3DA, 1) as u8;
        let mono = m.io_read(0x3BA, 1) as u8;
        assert_eq!(
            (color & 0x08) != 0,
            (mono & 0x08) != 0,
            "mono alias (0x3BA) should match 0x3DA vblank bit"
        );

        any_high |= (color & 0x08) != 0;
        any_low |= (color & 0x08) == 0;
        m.tick_platform(STEP_NS);
    }
    assert!(any_high, "expected to observe vblank bit set at least once");
    assert!(
        any_low,
        "expected to observe vblank bit clear at least once"
    );

    // ---------------------------------------------------------------------
    // Unimplemented reads should float high.
    // ---------------------------------------------------------------------
    assert_eq!(m.io_read(0x3C3, 1) as u8, 0xFF);
}
