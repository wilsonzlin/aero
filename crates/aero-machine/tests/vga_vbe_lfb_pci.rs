use aero_gpu_vga::{DisplayOutput, SVGA_LFB_BASE};
use aero_machine::{Machine, MachineConfig};

#[test]
fn vga_vbe_lfb_is_reachable_via_pci_mmio_router() {
    let mut cfg = MachineConfig::default();
    cfg.enable_pc_platform = true;
    cfg.enable_vga = true;

    let mut m = Machine::new(cfg).unwrap();
    let vga = m.vga().unwrap();

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

    let base = u64::from(SVGA_LFB_BASE);
    // Write a red pixel at (0,0) in BGRX format via *machine memory*.
    m.write_physical_u8(base, 0x00); // B
    m.write_physical_u8(base + 1, 0x00); // G
    m.write_physical_u8(base + 2, 0xFF); // R
    m.write_physical_u8(base + 3, 0x00); // X

    let mut vga = vga.borrow_mut();
    vga.present();
    assert_eq!(vga.get_resolution(), (64, 64));
    assert_eq!(vga.get_framebuffer()[0], 0xFF00_00FF);
}
