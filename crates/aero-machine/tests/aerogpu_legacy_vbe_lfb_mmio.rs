use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_legacy_vbe_lfb_banked_window_maps_into_vram() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for this test.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        // Keep the machine minimal.
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();

    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let bar1_base = m
        .pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
        .expect("expected AeroGPU BAR1 to be present");
    assert_ne!(
        bar1_base, 0,
        "AeroGPU BAR1 base should be assigned by BIOS POST"
    );

    // Program a mode that crosses the 64KiB bank boundary so we can validate the legacy
    // `0xA0000..0xAFFFF` window maps into `VBE_LFB_OFFSET + bank*64KiB`.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 300);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);
    assert_eq!(
        m.vbe_lfb_base(),
        bar1_base + aero_machine::VBE_LFB_OFFSET as u64
    );

    // Pixel (0,260) lands in bank 1 for a 64x300x32bpp framebuffer:
    //   pitch = 64*4 = 256 bytes
    //   offset = 260*pitch = 66560 bytes = 1*65536 + 1024
    let x = 0u64;
    let y = 260u64;
    let pitch = 64u64 * 4;
    let pixel_off = y * pitch + x * 4;
    let bank = (pixel_off / 65_536) as u16;
    let bank_off = pixel_off % 65_536;

    // Select bank 1 via Bochs VBE_DISPI.
    m.io_write(0x01CE, 2, 0x0005);
    m.io_write(0x01CF, 2, u32::from(bank));

    // Write a red pixel through the legacy `0xA0000` banked window.
    let banked_window = 0xA0000u64 + bank_off;
    m.write_physical_u32(banked_window, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 300));
    assert_eq!(m.display_framebuffer()[(y as usize) * 64], 0xFF00_00FF);
}
