use std::cell::RefCell;
use std::rc::Rc;

use emulator::devices::vga::{
    VgaDevice, VgaMmio, VgaMmioRegion, VBE_BIOS_DATA_PADDR, VBE_LFB_BASE, VBE_LFB_SIZE,
    VGA_BANK_WINDOW_PADDR, VGA_BANK_WINDOW_SIZE,
};
use emulator::memory_bus::MemoryBus;
use memory::bus::MemoryBus as _;
use memory::DenseMemory;

fn new_bus_with_vga() -> (MemoryBus, Rc<RefCell<VgaDevice>>) {
    let ram = DenseMemory::new(2 * 1024 * 1024).unwrap();
    let mut bus = MemoryBus::new(Box::new(ram));

    let vga = Rc::new(RefCell::new(VgaDevice::new()));

    // Map the banked window and LFB apertures as device-backed memory.
    bus.add_mmio_region(
        VGA_BANK_WINDOW_PADDR,
        VGA_BANK_WINDOW_SIZE,
        Box::new(VgaMmio::new(vga.clone(), VgaMmioRegion::BankedWindowA)),
    )
    .unwrap();
    bus.add_mmio_region(
        VBE_LFB_BASE as u64,
        VBE_LFB_SIZE as u64,
        Box::new(VgaMmio::new(vga.clone(), VgaMmioRegion::Lfb)),
    )
    .unwrap();

    // Populate the conventional-memory blob referenced by `VbeControllerInfo`.
    vga.borrow_mut()
        .vbe_mut()
        .install_bios_data(bus.ram_mut())
        .unwrap();

    (bus, vga)
}

#[test]
fn controller_info_basic_fields() {
    let (_bus, vga) = new_bus_with_vga();

    let info = vga.borrow().controller_info();
    assert_eq!(info.signature(), *b"VESA");
    assert_eq!(info.version(), 0x0300);

    // We place the mode list at the start of our BIOS data blob.
    let expected_mode_list_ptr = ((VBE_BIOS_DATA_PADDR >> 4) << 16) | 0x0000;
    assert_eq!(info.video_mode_ptr(), expected_mode_list_ptr);
}

#[test]
fn mode_info_1024x768x32() {
    let (_bus, vga) = new_bus_with_vga();

    let mode = 0x118;
    let mi = vga.borrow().mode_info(mode).expect("mode info");

    assert_eq!(mi.bits_per_pixel(), 32);
    assert_eq!(mi.bytes_per_scan_line(), 1024 * 4);
    assert_eq!(mi.phys_base_ptr(), VBE_LFB_BASE);

    assert_eq!(mi.red_mask_size(), 8);
    assert_eq!(mi.red_field_position(), 16);
    assert_eq!(mi.green_mask_size(), 8);
    assert_eq!(mi.green_field_position(), 8);
    assert_eq!(mi.blue_mask_size(), 8);
    assert_eq!(mi.blue_field_position(), 0);
    assert_eq!(mi.reserved_mask_size(), 8);
    assert_eq!(mi.reserved_field_position(), 24);
}

#[test]
fn setting_mode_enables_lfb_mapping_and_updates_resolution() {
    let (mut bus, vga) = new_bus_with_vga();

    // Write before LFB enabled should be ignored.
    bus.write_u32(VBE_LFB_BASE as u64, 0x11223344);
    assert_eq!(bus.read_u32(VBE_LFB_BASE as u64), 0);

    vga.borrow_mut().set_mode(0x118 | 0x4000).unwrap();
    assert!(vga.borrow().is_lfb_enabled());
    assert_eq!(vga.borrow().resolution(), Some((1024, 768)));

    bus.write_u32(VBE_LFB_BASE as u64, 0x11223344);
    assert_eq!(bus.read_u32(VBE_LFB_BASE as u64), 0x11223344);
}

#[test]
fn lfb_pixel_write_visible_after_render() {
    let (mut bus, vga) = new_bus_with_vga();
    vga.borrow_mut().set_mode(0x118 | 0x4000).unwrap();

    let pitch = vga.borrow().pitch_bytes().unwrap() as u64;
    let x = 10u64;
    let y = 20u64;
    let addr = VBE_LFB_BASE as u64 + y * pitch + x * 4;
    let pixel = 0x00AA_BBCCu32;
    bus.write_u32(addr, pixel);

    let mut vga_mut = vga.borrow_mut();
    let fb = vga_mut.render();
    let w = 1024usize;
    assert_eq!(fb[(y as usize) * w + (x as usize)], pixel);
}

#[test]
fn boot_logo_gradient_matches_expected() {
    let (mut bus, vga) = new_bus_with_vga();
    vga.borrow_mut().set_mode(0x118 | 0x4000).unwrap();

    let (w, h) = vga.borrow().resolution().unwrap();
    let pitch = vga.borrow().pitch_bytes().unwrap() as usize;

    // Build a synthetic gradient (packed pixel 0x00RRGGBB).
    let mut frame_bytes = vec![0u8; pitch * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let r = (x * 255 / (w as usize - 1)) as u8;
            let g = (y * 255 / (h as usize - 1)) as u8;
            let b = (((x + y) * 255) / ((w as usize - 1) + (h as usize - 1))) as u8;
            let px = u32::from_le_bytes([b, g, r, 0]);
            let off = y * pitch + x * 4;
            frame_bytes[off..off + 4].copy_from_slice(&px.to_le_bytes());
        }
    }

    bus.write_physical(VBE_LFB_BASE as u64, &frame_bytes);

    // Verify a handful of sample points across the frame.
    let mut vga_mut = vga.borrow_mut();
    let fb = vga_mut.render();
    let sample_points = [
        (0usize, 0usize),
        (w as usize / 2, h as usize / 2),
        (w as usize - 1, 0),
        (0, h as usize - 1),
        (w as usize - 1, h as usize - 1),
    ];
    for (x, y) in sample_points {
        let r = (x * 255 / (w as usize - 1)) as u8;
        let g = (y * 255 / (h as usize - 1)) as u8;
        let b = (((x + y) * 255) / ((w as usize - 1) + (h as usize - 1))) as u8;
        let expected = u32::from_le_bytes([b, g, r, 0]);
        assert_eq!(fb[y * w as usize + x], expected);
    }
}
