use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn base_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test deterministic/minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    }
}

#[test]
fn aerogpu_bochs_vbe_dispi_16bpp_display_present_respects_offsets_and_stride() {
    let mut m = Machine::new(base_cfg()).unwrap();
    m.reset();

    // 2x2 visible, 4x4 virtual, 16bpp RGB565 with a (1,1) visible offset.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 2); // xres
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 2); // yres
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 16); // bpp
    m.io_write(0x01CE, 2, 0x0006);
    m.io_write(0x01CF, 2, 4); // virt_width
    m.io_write(0x01CE, 2, 0x0007);
    m.io_write(0x01CF, 2, 4); // virt_height
    m.io_write(0x01CE, 2, 0x0008);
    m.io_write(0x01CF, 2, 1); // x_offset
    m.io_write(0x01CE, 2, 0x0009);
    m.io_write(0x01CF, 2, 1); // y_offset
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041); // enable + lfb

    let base = m.vbe_lfb_base();

    // If the stride is computed incorrectly from xres instead of virt_width, the base offset
    // would be 6 (1 scanline * 2px/line * 2 bytes/px + 1px * 2 bytes/px).
    let wrong_base_off = 6u64;
    m.write_physical_u16(base + wrong_base_off, 0xF800); // red

    // Correct base offset uses virt_width (4) for stride: (1*4 + 1)*2 = 10.
    let correct_base_off = 10u64;
    m.write_physical_u16(base + correct_base_off, 0x8543);

    let reads_before = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");

    m.display_present();

    assert_eq!(m.display_resolution(), (2, 2));
    // For 0x8543 (RGB565): r=0b10000 -> 0x84, g=0b101010 -> 0xAA, b=0b00011 -> 0x18.
    // Expected RGBA8888: 0xFF18AA84.
    assert_eq!(m.display_framebuffer()[0], 0xFF18_AA84);

    // Present should use the direct VRAM fast-path when the Bochs VBE_DISPI framebuffer is backed
    // by BAR1 VRAM (avoid routing per-pixel reads through the PCI MMIO router).
    let reads_after = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");
    assert_eq!(reads_before, reads_after);
}

#[test]
fn aerogpu_bochs_vbe_dispi_oversized_dimensions_do_not_oom_or_panic() {
    let mut m = Machine::new(base_cfg()).unwrap();
    m.reset();

    // Program an intentionally absurd Bochs VBE_DISPI mode. These registers are guest-controlled
    // and must not cause the host to allocate an unbounded framebuffer.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 0xFFFF); // xres
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 0xFFFF); // yres
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32); // bpp
    m.io_write(0x01CE, 2, 0x0006);
    m.io_write(0x01CF, 2, 0xFFFF); // virt_width
    m.io_write(0x01CE, 2, 0x0007);
    m.io_write(0x01CF, 2, 0xFFFF); // virt_height
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041); // enable + lfb

    m.display_present();

    // If the VBE mode is rejected, the machine falls back to BIOS text mode (80x25 -> 720x400).
    assert_eq!(m.display_resolution(), (720, 400));
}
