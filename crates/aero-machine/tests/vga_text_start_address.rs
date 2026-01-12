use aero_gpu_vga::DisplayOutput;
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn vga_text_mode_crtc_start_address_offsets_rendered_text() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
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
    let vga = m.vga().expect("VGA enabled");

    // Force deterministic baseline.
    {
        let mut vga = vga.borrow_mut();
        vga.set_text_mode_80x25();
        vga.vram_mut().fill(0);
    }

    // Disable the cursor for deterministic pixels.
    m.io_write(0x3D4, 1, 0x0A);
    m.io_write(0x3D5, 1, 0x20);

    // Page 0 cell 0: space with light-grey-on-black (0x07). Background is black.
    m.write_physical_u8(0xB8000, b' ');
    m.write_physical_u8(0xB8001, 0x07);

    // Page 1 cell 0: space with white-on-blue (0x1F). Background is blue.
    //
    // A common BIOS text page size is 0x1000 bytes (BDA page size), which corresponds to 0x0800
    // character cells (CRTC start address units).
    const PAGE1_OFFSET_BYTES: u64 = 0x1000;
    m.write_physical_u8(0xB8000 + PAGE1_OFFSET_BYTES, b' ');
    m.write_physical_u8(0xB8001 + PAGE1_OFFSET_BYTES, 0x1F);

    // With start address 0, we should see the page 0 cell.
    let pixel_page0 = {
        let mut vga = vga.borrow_mut();
        vga.present();
        vga.get_framebuffer()[0]
    };
    assert_eq!(pixel_page0, 0xFF00_0000);

    // Set CRTC start address to page 1 (0x0800 cells).
    m.io_write(0x3D4, 1, 0x0C);
    m.io_write(0x3D5, 1, 0x08);
    m.io_write(0x3D4, 1, 0x0D);
    m.io_write(0x3D5, 1, 0x00);

    let pixel_page1 = {
        let mut vga = vga.borrow_mut();
        vga.present();
        vga.get_framebuffer()[0]
    };
    assert_eq!(pixel_page1, 0xFFAA_0000);
}
