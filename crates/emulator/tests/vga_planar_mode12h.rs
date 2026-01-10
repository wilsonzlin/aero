use std::path::PathBuf;

use emulator::devices::vga::{VgaDac, VgaDevice, VgaMemory, VgaRenderer, VRAM_BASE};
use emulator::io::PortIO;
use image::RgbaImage;

const WIDTH: usize = 640;
const HEIGHT: usize = 480;
const BYTES_PER_SCANLINE: usize = WIDTH / 8;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("vga_planar")
        .join(name)
}

fn rgba_bytes_from_framebuffer(framebuffer: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(framebuffer.len() * 4);
    for &pixel in framebuffer {
        out.extend_from_slice(&pixel.to_le_bytes());
    }
    out
}

fn assert_png_matches(path: &PathBuf, rgba: &[u8], width: usize, height: usize) {
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden file has no parent directory"))
            .expect("create golden directory");
        let image = RgbaImage::from_raw(width as u32, height as u32, rgba.to_vec())
            .expect("RGBA buffer length must match image dimensions");
        image.save(path).expect("write golden png");
        return;
    }

    let expected = image::open(path)
        .unwrap_or_else(|err| panic!("failed to open golden image {path:?}: {err}"))
        .to_rgba8();

    assert_eq!(expected.width(), width as u32);
    assert_eq!(expected.height(), height as u32);
    assert_eq!(expected.into_raw(), rgba);
}

fn program_test_palette(dac: &mut VgaDac) {
    for idx in 0u16..256 {
        let i = idx as u8;
        let r6 = i & 0x3F;
        let g6 = (i >> 2) & 0x3F;
        let b6 = (i >> 4) & 0x3F;
        dac.set_entry_6bit(i, r6, g6, b6);
    }
    dac.set_pel_mask(0xFF);
}

fn write_ac(regs: &mut VgaDevice, index: u8, value: u8) {
    // Reset flip-flop.
    regs.port_read(0x3DA, 1);
    regs.port_write(0x3C0, 1, u32::from(index));
    regs.port_write(0x3C0, 1, u32::from(value));
}

fn program_attribute_palette(regs: &mut VgaDevice) {
    for i in 0..16u8 {
        write_ac(regs, i, i * 3);
    }
    write_ac(regs, 0x12, 0x0F); // Color Plane Enable.
    write_ac(regs, 0x14, 0x02); // Color Select: adds 0x80 to the DAC index.
    write_ac(regs, 0x10, 0x00); // Mode Control (P54S disabled).
}

fn stripes_color_index(x: usize) -> u8 {
    (x / (WIDTH / 16)) as u8
}

fn program_stripes_vram(vram: &mut VgaMemory) {
    for y in 0..HEIGHT {
        for byte_x in 0..BYTES_PER_SCANLINE {
            let offset = y * BYTES_PER_SCANLINE + byte_x;
            let mut plane_bytes = [0u8; 4];
            for bit in 0..8 {
                let x = byte_x * 8 + bit;
                let idx = stripes_color_index(x);
                let mask = 1u8 << (7 - bit);
                for plane in 0..4 {
                    if ((idx >> plane) & 1) != 0 {
                        plane_bytes[plane] |= mask;
                    }
                }
            }
            for plane in 0..4 {
                vram.write_plane_byte(plane, offset, plane_bytes[plane]);
            }
        }
    }
}

fn seq_write(regs: &mut VgaDevice, index: u8, value: u8) {
    regs.port_write(0x3C4, 1, u32::from(index));
    regs.port_write(0x3C5, 1, u32::from(value));
}

fn gc_write(regs: &mut VgaDevice, index: u8, value: u8) {
    regs.port_write(0x3CE, 1, u32::from(index));
    regs.port_write(0x3CF, 1, u32::from(value));
}

fn vga_set_write_mode(regs: &mut VgaDevice, write_mode: u8) {
    gc_write(regs, 0x05, write_mode & 0x03);
}

fn vga_fill_rect_set_reset(
    regs: &mut VgaDevice,
    vram: &mut VgaMemory,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    color: u8,
) {
    assert!(x1 <= WIDTH && y1 <= HEIGHT && x0 < x1 && y0 < y1);

    seq_write(regs, 0x02, 0x0F); // Map mask.
    gc_write(regs, 0x03, 0x00); // Data rotate: replace, rotate 0.
    gc_write(regs, 0x00, color & 0x0F); // Set/reset.
    gc_write(regs, 0x01, 0x0F); // Enable set/reset.
    vga_set_write_mode(regs, 0);

    let start_byte = x0 / 8;
    let end_byte = (x1 - 1) / 8;
    let start_bit = x0 & 7;
    let end_bit = (x1 - 1) & 7;

    let start_mask = 0xFFu8 >> start_bit;
    let end_mask = 0xFFu8 << (7 - end_bit);

    for y in y0..y1 {
        for byte_x in start_byte..=end_byte {
            let mask = if start_byte == end_byte {
                start_mask & end_mask
            } else if byte_x == start_byte {
                start_mask
            } else if byte_x == end_byte {
                end_mask
            } else {
                0xFF
            };

            gc_write(regs, 0x08, mask);
            let addr = VRAM_BASE + (y * BYTES_PER_SCANLINE + byte_x) as u32;
            vram.write_u8_planar(regs, addr, 0);
        }
    }
}

fn vga_draw_diag_line(regs: &mut VgaDevice, vram: &mut VgaMemory, color: u8, len: usize) {
    seq_write(regs, 0x02, 0x0F);
    gc_write(regs, 0x03, 0x00);
    gc_write(regs, 0x00, color & 0x0F);
    gc_write(regs, 0x01, 0x0F);
    vga_set_write_mode(regs, 0);

    for i in 0..len {
        let x = i;
        let y = i;
        let mask = 0x80u8 >> (x & 7);
        gc_write(regs, 0x08, mask);
        let addr = VRAM_BASE + (y * BYTES_PER_SCANLINE + (x / 8)) as u32;
        vram.write_u8_planar(regs, addr, 0);
    }
}

fn vga_stamp_glyph_mode3(
    regs: &mut VgaDevice,
    vram: &mut VgaMemory,
    x0: usize,
    y0: usize,
    color: u8,
    rows: &[u8],
) {
    assert_eq!(rows.len(), 8, "expected 8 glyph rows");
    assert_eq!(x0 & 7, 0, "glyph stamp must be byte-aligned");

    seq_write(regs, 0x02, 0x0F);
    gc_write(regs, 0x03, 0x00);
    gc_write(regs, 0x00, color & 0x0F);
    gc_write(regs, 0x01, 0x0F);
    gc_write(regs, 0x08, 0xFF);
    vga_set_write_mode(regs, 3);

    let byte_x = x0 / 8;
    for (row, &pattern) in rows.iter().enumerate() {
        let addr = VRAM_BASE + ((y0 + row) * BYTES_PER_SCANLINE + byte_x) as u32;
        vram.write_u8_planar(regs, addr, pattern);
    }
}

fn vga_copy_glyph_mode1(
    regs: &mut VgaDevice,
    vram: &mut VgaMemory,
    src_x: usize,
    dst_x: usize,
    y0: usize,
) {
    assert_eq!(src_x & 7, 0);
    assert_eq!(dst_x & 7, 0);

    seq_write(regs, 0x02, 0x0F);
    vga_set_write_mode(regs, 1);

    let src_byte = src_x / 8;
    let dst_byte = dst_x / 8;
    for row in 0..8 {
        let src_addr = VRAM_BASE + ((y0 + row) * BYTES_PER_SCANLINE + src_byte) as u32;
        let dst_addr = VRAM_BASE + ((y0 + row) * BYTES_PER_SCANLINE + dst_byte) as u32;
        vram.read_u8_planar(regs, src_addr);
        vram.write_u8_planar(regs, dst_addr, 0);
    }
}

fn draw_shapes_via_vga_pipeline(regs: &mut VgaDevice, vram: &mut VgaMemory) {
    vga_fill_rect_set_reset(regs, vram, 13, 21, 213, 111, 0x0A);
    vga_fill_rect_set_reset(regs, vram, 220, 50, 400, 150, 0x03);
    vga_draw_diag_line(regs, vram, 0x0F, HEIGHT.min(WIDTH));

    let glyph: [u8; 8] = [
        0b0011_1100,
        0b0110_0110,
        0b0110_0110,
        0b0111_1110,
        0b0110_0110,
        0b0110_0110,
        0b0110_0110,
        0b0000_0000,
    ];

    let glyph_x = 504;
    let glyph_y = 300;
    vga_stamp_glyph_mode3(regs, vram, glyph_x, glyph_y, 0x06, &glyph);
    vga_copy_glyph_mode1(regs, vram, glyph_x, glyph_x + 16, glyph_y);
}

#[test]
fn vga_mode12h_renders_planar_stripes_golden() {
    let mut regs = VgaDevice::new();
    regs.set_legacy_mode(0x12, false);

    let mut vram = VgaMemory::new();
    let mut dac = VgaDac::new();
    let mut renderer = VgaRenderer::new();

    program_test_palette(&mut dac);
    program_attribute_palette(&mut regs);
    program_stripes_vram(&mut vram);

    let (width, height, framebuffer) = renderer
        .render(&regs, &mut vram, &mut dac)
        .expect("expected mode detection to select Mode 12h");

    assert_eq!((width, height), (WIDTH, HEIGHT));
    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(&golden_path("mode12h_stripes.png"), &rgba, WIDTH, HEIGHT);
}

#[test]
fn vga_mode12h_setreset_pipeline_golden() {
    let mut regs = VgaDevice::new();
    regs.set_legacy_mode(0x12, false);

    let mut vram = VgaMemory::new();
    let mut dac = VgaDac::new();
    let mut renderer = VgaRenderer::new();

    program_test_palette(&mut dac);
    program_attribute_palette(&mut regs);
    draw_shapes_via_vga_pipeline(&mut regs, &mut vram);

    let (width, height, framebuffer) = renderer
        .render(&regs, &mut vram, &mut dac)
        .expect("expected mode detection to select Mode 12h");

    assert_eq!((width, height), (WIDTH, HEIGHT));
    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(
        &golden_path("mode12h_setreset_shapes.png"),
        &rgba,
        WIDTH,
        HEIGHT,
    );
}
