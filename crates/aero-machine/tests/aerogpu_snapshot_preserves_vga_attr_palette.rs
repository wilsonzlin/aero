use aero_machine::{Machine, MachineConfig};
use firmware::bda::{BDA_CURSOR_SHAPE_ADDR, BDA_VIDEO_PAGE_OFFSET_ADDR};
use pretty_assertions::assert_eq;

fn new_deterministic_aerogpu_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for these tests.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        // Keep the machine minimal.
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn aerogpu_snapshot_preserves_vga_attribute_controller_palette_mapping() {
    let mut m = new_deterministic_aerogpu_machine();

    // Force deterministic baseline: clear the full 32KiB legacy text window (0xB8000..0xC0000).
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);

    // Ensure the active page offset is 0 so cell (0,0) maps to 0xB8000.
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);

    // Disable cursor for deterministic output (cursor start CH bit5 = 1).
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);

    // Unmask all palette bits.
    m.io_write(0x3C6, 1, 0xFF);

    // Program DAC entry 2 to pure red (6-bit components).
    m.io_write(0x3C8, 1, 0x02);
    m.io_write(0x3C9, 1, 63); // R
    m.io_write(0x3C9, 1, 0); // G
    m.io_write(0x3C9, 1, 0); // B

    // Map attribute color index 1 -> DAC index 2 via the Attribute Controller palette register.
    // Reading input status 1 resets the flip-flop so the next 0x3C0 write is treated as an index.
    let _ = m.io_read(0x3DA, 1);
    m.io_write(0x3C0, 1, 0x21); // palette register 1 (bit 5 set to keep display enabled)
    m.io_write(0x3C0, 1, 0x02); // map to PEL=2

    // Put a blank cell with background color 1 at the top-left.
    m.write_physical_u8(0xB8000, b' ');
    m.write_physical_u8(0xB8001, 0x10); // bg=1, fg=0

    m.display_present();
    assert_eq!(m.display_resolution(), (720, 400));
    // RGBA8888 little-endian u32: [R, G, B, A].
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);

    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = new_deterministic_aerogpu_machine();
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    // The palette mapping should survive snapshot/restore (text scanout uses Attribute Controller
    // regs).
    m2.display_present();
    assert_eq!(m2.display_resolution(), (720, 400));
    assert_eq!(m2.display_framebuffer()[0], 0xFF00_00FF);

    // Also validate port-level readback.
    let _ = m2.io_read(0x3DA, 1);
    m2.io_write(0x3C0, 1, 0x21);
    assert_eq!(m2.io_read(0x3C1, 1) as u8, 0x02);
}

