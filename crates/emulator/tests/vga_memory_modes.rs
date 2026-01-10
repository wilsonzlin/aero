use emulator::devices::vga::{VgaDevice, VgaMemory, VramPlane, VGA_PLANE_SIZE};
use emulator::io::PortIO;

const A0000: u64 = 0xA0000;

fn write_seq(vga: &mut VgaDevice, index: u8, value: u8) {
    vga.port_write(0x3C4, 1, index.into());
    vga.port_write(0x3C5, 1, value.into());
}

fn write_gc(vga: &mut VgaDevice, index: u8, value: u8) {
    vga.port_write(0x3CE, 1, index.into());
    vga.port_write(0x3CF, 1, value.into());
}

fn setup_planar(vga: &mut VgaDevice) {
    // A0000-AFFFF, planar addressing (no chain-4, no odd/even).
    write_gc(vga, 0x06, 0x04); // memory map select = 01b => A0000-AFFFF
    write_seq(vga, 0x04, 0x04); // odd/even disable = 1, chain-4 = 0

    write_seq(vga, 0x02, 0x0f); // map mask
    write_gc(vga, 0x05, 0x00); // write mode 0, read mode 0
    write_gc(vga, 0x08, 0xff); // bit mask
    write_gc(vga, 0x03, 0x00); // data rotate
    write_gc(vga, 0x01, 0x00); // enable set/reset
    write_gc(vga, 0x00, 0x00); // set/reset
}

#[test]
fn write_mode_0_bitmask_only_affects_masked_bits() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar(&mut vga);

    // Pre-fill all planes at offset 0 with 0xAA so latch background is known.
    for plane in 0..4 {
        vram.plane_mut(VramPlane(plane))[0] = 0xaa;
    }

    write_gc(&mut vga, 0x08, 0x0f);
    vga.mem_write_u8(&mut vram, A0000, 0x55);

    for plane in 0..4 {
        assert_eq!(vram.plane(VramPlane(plane))[0], 0xa5);
    }
}

#[test]
fn set_reset_with_enable_set_reset_writes_per_plane_patterns() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar(&mut vga);

    write_gc(&mut vga, 0x01, 0x0f); // enable set/reset
    write_gc(&mut vga, 0x00, 0b0101); // set/reset

    vga.mem_write_u8(&mut vram, A0000 + 0x1234, 0x00);

    let off = 0x1234usize;
    assert_eq!(vram.plane(VramPlane(0))[off], 0xff);
    assert_eq!(vram.plane(VramPlane(1))[off], 0x00);
    assert_eq!(vram.plane(VramPlane(2))[off], 0xff);
    assert_eq!(vram.plane(VramPlane(3))[off], 0x00);
}

#[test]
fn write_mode_2_color_expand_uses_low_nibble_as_plane_select() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar(&mut vga);

    write_gc(&mut vga, 0x05, 0x02); // write mode 2

    vga.mem_write_u8(&mut vram, A0000 + 7, 0x0a);

    let off = 7usize;
    assert_eq!(vram.plane(VramPlane(0))[off], 0x00);
    assert_eq!(vram.plane(VramPlane(1))[off], 0xff);
    assert_eq!(vram.plane(VramPlane(2))[off], 0x00);
    assert_eq!(vram.plane(VramPlane(3))[off], 0xff);
}

#[test]
fn write_mode_1_copies_latches_from_previous_read() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar(&mut vga);

    // Fill a source byte with distinct per-plane values.
    let src_off = 0usize;
    vram.plane_mut(VramPlane(0))[src_off] = 0x11;
    vram.plane_mut(VramPlane(1))[src_off] = 0x22;
    vram.plane_mut(VramPlane(2))[src_off] = 0x33;
    vram.plane_mut(VramPlane(3))[src_off] = 0x44;

    // Read to load latches.
    write_gc(&mut vga, 0x04, 0x00); // read map select = plane 0
    assert_eq!(vga.mem_read_u8(&mut vram, A0000).unwrap(), 0x11);

    // Destination is initially different.
    let dst_off = 1usize;
    for plane in 0..4 {
        vram.plane_mut(VramPlane(plane))[dst_off] = 0x00;
    }

    write_gc(&mut vga, 0x05, 0x01); // write mode 1
    vga.mem_write_u8(&mut vram, A0000 + 1, 0xff);

    assert_eq!(vram.plane(VramPlane(0))[dst_off], 0x11);
    assert_eq!(vram.plane(VramPlane(1))[dst_off], 0x22);
    assert_eq!(vram.plane(VramPlane(2))[dst_off], 0x33);
    assert_eq!(vram.plane(VramPlane(3))[dst_off], 0x44);
}

#[test]
fn read_mode_1_color_compare_matches_expected_bits() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar(&mut vga);

    // Arrange a latch pattern where bits 7-4 represent color 0b0011 and bits 3-0 represent 0.
    vram.plane_mut(VramPlane(0))[0] = 0xf0;
    vram.plane_mut(VramPlane(1))[0] = 0xf0;
    vram.plane_mut(VramPlane(2))[0] = 0x00;
    vram.plane_mut(VramPlane(3))[0] = 0x00;

    write_gc(&mut vga, 0x02, 0b0011); // color compare
    write_gc(&mut vga, 0x07, 0x0f); // color don't care (compare all planes)
    write_gc(&mut vga, 0x05, 0x08); // read mode 1, write mode 0

    assert_eq!(vga.mem_read_u8(&mut vram, A0000).unwrap(), 0xf0);
}

#[test]
fn chain4_addressing_interleaves_planes() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();

    write_gc(&mut vga, 0x06, 0x04); // A0000-AFFFF
    write_seq(&mut vga, 0x04, 0x08); // chain-4 enabled
    write_seq(&mut vga, 0x02, 0x0f); // map mask
    write_gc(&mut vga, 0x05, 0x00); // write mode 0
    write_gc(&mut vga, 0x08, 0xff); // bit mask

    vga.mem_write_u8(&mut vram, A0000 + 0, 0xaa);
    vga.mem_write_u8(&mut vram, A0000 + 1, 0xbb);
    vga.mem_write_u8(&mut vram, A0000 + 2, 0xcc);
    vga.mem_write_u8(&mut vram, A0000 + 3, 0xdd);
    vga.mem_write_u8(&mut vram, A0000 + 4, 0xee);

    assert_eq!(vram.plane(VramPlane(0))[0], 0xaa);
    assert_eq!(vram.plane(VramPlane(1))[0], 0xbb);
    assert_eq!(vram.plane(VramPlane(2))[0], 0xcc);
    assert_eq!(vram.plane(VramPlane(3))[0], 0xdd);
    assert_eq!(vram.plane(VramPlane(0))[1], 0xee);
}

#[test]
fn planar_vertical_line_like_mode12h_set_reset_bitmask() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar(&mut vga);

    // 640px / 8 = 80 bytes per scanline in planar 16-color modes like 12h.
    let bytes_per_scanline = 80usize;
    let x = 3usize;
    let byte_x = x / 8;
    let bit = 0x80u8 >> (x & 7);

    // Color 0b1010.
    write_gc(&mut vga, 0x01, 0x0f); // enable set/reset
    write_gc(&mut vga, 0x00, 0b1010); // set/reset
    write_gc(&mut vga, 0x08, bit); // bit mask selects the pixel column within each byte

    // Draw y=0..4.
    for y in 0..5usize {
        let off = y * bytes_per_scanline + byte_x;
        assert!(off < VGA_PLANE_SIZE);
        vga.mem_write_u8(&mut vram, A0000 + off as u64, 0x00);
    }

    for y in 0..5usize {
        let off = y * bytes_per_scanline + byte_x;
        assert_eq!(vram.plane(VramPlane(0))[off], 0x00);
        assert_eq!(vram.plane(VramPlane(1))[off], bit);
        assert_eq!(vram.plane(VramPlane(2))[off], 0x00);
        assert_eq!(vram.plane(VramPlane(3))[off], bit);
    }
}
