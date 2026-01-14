use aero_machine::{Machine, MachineConfig, VBE_LFB_OFFSET};

#[test]
fn aerogpu_legacy_vbe_lfb_is_reachable_via_pci_mmio_router() {
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

    // Match the programming sequence used by `aero-gpu-vga`'s
    // `vbe_linear_framebuffer_write_shows_up_in_output` test.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    let base = bar1_base + VBE_LFB_OFFSET as u64;
    assert_eq!(m.vbe_lfb_base(), base);

    // Write a red pixel at (0,0) in packed 32bpp BGRX via *machine memory*.
    m.write_physical_u32(base, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (64, 64));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
