use emulator::devices::vga::{VgaDevice, VgaMemory, VramPlane};
use emulator::io::PortIO;

const GC_SET_RESET: u8 = 0x00;
const GC_ENABLE_SET_RESET: u8 = 0x01;
const GC_COLOR_COMPARE: u8 = 0x02;
const GC_DATA_ROTATE: u8 = 0x03;
const GC_READ_MAP_SELECT: u8 = 0x04;
const GC_MODE: u8 = 0x05;
const GC_MISC: u8 = 0x06;
const GC_COLOR_DONT_CARE: u8 = 0x07;
const GC_BIT_MASK: u8 = 0x08;

const SEQ_MAP_MASK: u8 = 0x02;
const SEQ_MEMORY_MODE: u8 = 0x04;

fn gc_write(vga: &mut VgaDevice, idx: u8, val: u8) {
    vga.port_write(0x3CE, 1, idx as u32);
    vga.port_write(0x3CF, 1, val as u32);
}

fn seq_write(vga: &mut VgaDevice, idx: u8, val: u8) {
    vga.port_write(0x3C4, 1, idx as u32);
    vga.port_write(0x3C5, 1, val as u32);
}

fn setup_planar_a0000(vga: &mut VgaDevice) {
    // Map A0000 64KiB window and disable odd/even for linear planar addressing.
    gc_write(vga, GC_MISC, 0x05);
    seq_write(vga, SEQ_MEMORY_MODE, 0x04);
}

fn plane_byte(vram: &VgaMemory, plane: usize, offset: usize) -> u8 {
    vram.plane(VramPlane(plane))[offset]
}

#[test]
fn write_mode_0_basic_write_all_planes() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    seq_write(&mut vga, SEQ_MAP_MASK, 0x0F);
    gc_write(&mut vga, GC_MODE, 0x00); // write mode 0
    gc_write(&mut vga, GC_ENABLE_SET_RESET, 0x00);
    gc_write(&mut vga, GC_DATA_ROTATE, 0x00); // rotate=0, func=replace
    gc_write(&mut vga, GC_BIT_MASK, 0xFF);

    assert!(vga.mem_write_u8(&mut vram, 0xA0000, 0xAA));

    for plane in 0..4 {
        assert_eq!(plane_byte(&vram, plane, 0), 0xAA);
    }
}

#[test]
fn write_mode_0_set_reset_overrides_cpu_data() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    seq_write(&mut vga, SEQ_MAP_MASK, 0x0F);
    gc_write(&mut vga, GC_MODE, 0x00);
    gc_write(&mut vga, GC_SET_RESET, 0b0101);
    gc_write(&mut vga, GC_ENABLE_SET_RESET, 0x0F);
    gc_write(&mut vga, GC_DATA_ROTATE, 0x00);
    gc_write(&mut vga, GC_BIT_MASK, 0xFF);

    assert!(vga.mem_write_u8(&mut vram, 0xA0000, 0x00));

    assert_eq!(plane_byte(&vram, 0, 0), 0xFF);
    assert_eq!(plane_byte(&vram, 1, 0), 0x00);
    assert_eq!(plane_byte(&vram, 2, 0), 0xFF);
    assert_eq!(plane_byte(&vram, 3, 0), 0x00);
}

#[test]
fn write_mode_0_bit_mask_preserves_unmasked_bits() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    seq_write(&mut vga, SEQ_MAP_MASK, 0x0F);
    gc_write(&mut vga, GC_MODE, 0x00);
    gc_write(&mut vga, GC_ENABLE_SET_RESET, 0x00);
    gc_write(&mut vga, GC_DATA_ROTATE, 0x00);
    gc_write(&mut vga, GC_BIT_MASK, 0x0F);

    // Seed memory and latches with 0xFF.
    for plane in 0..4 {
        vram.write_plane_byte(plane, 0, 0xFF);
    }
    let _ = vga.mem_read_u8(&mut vram, 0xA0000).unwrap();

    assert!(vga.mem_write_u8(&mut vram, 0xA0000, 0x00));

    for plane in 0..4 {
        assert_eq!(plane_byte(&vram, plane, 0), 0xF0);
    }
}

#[test]
fn write_mode_1_writes_latches() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    seq_write(&mut vga, SEQ_MAP_MASK, 0x0F);
    gc_write(&mut vga, GC_MODE, 0x01); // write mode 1

    // Address A at offset 0 contains unique bytes per plane.
    vram.write_plane_byte(0, 0, 0x11);
    vram.write_plane_byte(1, 0, 0x22);
    vram.write_plane_byte(2, 0, 0x33);
    vram.write_plane_byte(3, 0, 0x44);

    // Address B at offset 1 starts clear.
    for plane in 0..4 {
        vram.write_plane_byte(plane, 1, 0x00);
    }

    // Load latches from A, then write to B.
    let _ = vga.mem_read_u8(&mut vram, 0xA0000).unwrap();
    assert!(vga.mem_write_u8(&mut vram, 0xA0001, 0xFF));

    assert_eq!(plane_byte(&vram, 0, 1), 0x11);
    assert_eq!(plane_byte(&vram, 1, 1), 0x22);
    assert_eq!(plane_byte(&vram, 2, 1), 0x33);
    assert_eq!(plane_byte(&vram, 3, 1), 0x44);
}

#[test]
fn write_mode_2_expands_cpu_color_bits() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    seq_write(&mut vga, SEQ_MAP_MASK, 0x0F);
    gc_write(&mut vga, GC_MODE, 0x02); // write mode 2
    gc_write(&mut vga, GC_DATA_ROTATE, 0x00);
    gc_write(&mut vga, GC_BIT_MASK, 0xFF);

    assert!(vga.mem_write_u8(&mut vram, 0xA0000, 0x0A));

    assert_eq!(plane_byte(&vram, 0, 0), 0x00);
    assert_eq!(plane_byte(&vram, 1, 0), 0xFF);
    assert_eq!(plane_byte(&vram, 2, 0), 0x00);
    assert_eq!(plane_byte(&vram, 3, 0), 0xFF);
}

#[test]
fn write_mode_3_uses_cpu_data_as_bitmask() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    seq_write(&mut vga, SEQ_MAP_MASK, 0x0F);
    gc_write(&mut vga, GC_MODE, 0x03); // write mode 3
    gc_write(&mut vga, GC_SET_RESET, 0x01); // plane0 = 1, others 0
    gc_write(&mut vga, GC_DATA_ROTATE, 0x00); // rotate 0, func replace
    gc_write(&mut vga, GC_BIT_MASK, 0xFF);

    // Ensure latches are known (zero).
    for plane in 0..4 {
        vram.write_plane_byte(plane, 0, 0x00);
    }
    let _ = vga.mem_read_u8(&mut vram, 0xA0000).unwrap();

    // CPU data becomes the mask, so only low nibble bits are written.
    assert!(vga.mem_write_u8(&mut vram, 0xA0000, 0x0F));

    assert_eq!(plane_byte(&vram, 0, 0), 0x0F);
    assert_eq!(plane_byte(&vram, 1, 0), 0x00);
    assert_eq!(plane_byte(&vram, 2, 0), 0x00);
    assert_eq!(plane_byte(&vram, 3, 0), 0x00);
}

#[test]
fn read_mode_0_selects_plane() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    vram.write_plane_byte(0, 0, 0x10);
    vram.write_plane_byte(1, 0, 0x20);
    vram.write_plane_byte(2, 0, 0x30);
    vram.write_plane_byte(3, 0, 0x40);

    gc_write(&mut vga, GC_MODE, 0x00); // read mode 0
    gc_write(&mut vga, GC_READ_MAP_SELECT, 0x02);
    assert_eq!(vga.mem_read_u8(&mut vram, 0xA0000).unwrap(), 0x30);
}

#[test]
fn read_mode_1_color_compare() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    setup_planar_a0000(&mut vga);
    // Planes arranged so that bits 7-4 match color_compare and bits 3-0 don't.
    vram.write_plane_byte(0, 0, 0xF0);
    vram.write_plane_byte(1, 0, 0x00);
    vram.write_plane_byte(2, 0, 0xFF);
    vram.write_plane_byte(3, 0, 0x00);

    gc_write(&mut vga, GC_MODE, 0x18); // read mode 1 (bit3), odd/even disabled
    gc_write(&mut vga, GC_COLOR_COMPARE, 0x05);
    gc_write(&mut vga, GC_COLOR_DONT_CARE, 0x0F);

    assert_eq!(vga.mem_read_u8(&mut vram, 0xA0000).unwrap(), 0xF0);
}

#[test]
fn chain4_addressing_maps_bytes_to_planes() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    // Enable chain4.
    gc_write(&mut vga, GC_MISC, 0x05);
    seq_write(&mut vga, SEQ_MEMORY_MODE, 0x0C);
    seq_write(&mut vga, SEQ_MAP_MASK, 0x0F);
    gc_write(&mut vga, GC_MODE, 0x00);

    assert!(vga.mem_write_u8(&mut vram, 0xA0000, 0x11));
    assert!(vga.mem_write_u8(&mut vram, 0xA0001, 0x22));
    assert!(vga.mem_write_u8(&mut vram, 0xA0002, 0x33));
    assert!(vga.mem_write_u8(&mut vram, 0xA0003, 0x44));

    assert_eq!(plane_byte(&vram, 0, 0), 0x11);
    assert_eq!(plane_byte(&vram, 1, 0), 0x22);
    assert_eq!(plane_byte(&vram, 2, 0), 0x33);
    assert_eq!(plane_byte(&vram, 3, 0), 0x44);

    assert_eq!(vga.mem_read_u8(&mut vram, 0xA0002).unwrap(), 0x33);
}

#[test]
fn odd_even_addressing_separates_planes_0_and_1() {
    let mut vga = VgaDevice::new();
    let mut vram = VgaMemory::new();
    // Enable odd/even (odd/even disable bits must be clear).
    gc_write(&mut vga, GC_MISC, 0x05);
    seq_write(&mut vga, SEQ_MEMORY_MODE, 0x00);
    gc_write(&mut vga, GC_MODE, 0x10); // host odd/even enable
    seq_write(&mut vga, SEQ_MAP_MASK, 0x03);

    assert!(vga.mem_write_u8(&mut vram, 0xA0000, 0xAA)); // even -> plane 0, offset 0
    assert!(vga.mem_write_u8(&mut vram, 0xA0001, 0xBB)); // odd -> plane 1, offset 0

    assert_eq!(plane_byte(&vram, 0, 0), 0xAA);
    assert_eq!(plane_byte(&vram, 1, 0), 0xBB);
}
