use std::path::PathBuf;

use emulator::devices::vga::{Mode13hRenderer, VgaDac, VgaDevice, VgaMemory, VgaRenderer};
use emulator::io::PortIO;
use image::RgbaImage;

const WIDTH: usize = 320;
const HEIGHT: usize = 200;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("vga_mode13h")
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
    // 3-3-2 color cube (256 unique colors), expressed in VGA 6-bit DAC space.
    for i in 0u16..256 {
        let index = i as u8;
        let r6 = ((i & 0b0000_0111) as u8) * 9;
        let g6 = (((i >> 3) & 0b0000_0111) as u8) * 9;
        let b6 = (((i >> 6) & 0b0000_0011) as u8) * 21;
        dac.set_entry_6bit(index, r6, g6, b6);
    }
    dac.set_pel_mask(0xFF);
}

fn program_color_bar_vram(vram: &mut VgaMemory) {
    let mut vram_buf = vec![0u8; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            // Spread indices evenly across the scanline without wrapping.
            let index = ((x * 256) / WIDTH) as u8;
            vram_buf[y * WIDTH + x] = index;
        }
    }
    vram.write(0, &vram_buf);
}

#[test]
fn vga_mode13h_renders_color_bars_golden() {
    let mut vram = VgaMemory::new();
    let mut dac = VgaDac::new();
    let mut renderer = Mode13hRenderer::new();

    program_test_palette(&mut dac);
    program_color_bar_vram(&mut vram);

    let framebuffer = renderer.render(&mut vram, &mut dac);
    assert_eq!(framebuffer.len(), WIDTH * HEIGHT);

    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(&golden_path("color_bars.png"), &rgba, WIDTH, HEIGHT);
}

#[test]
fn vga_mode13h_palette_change_repaints_pixels() {
    let mut vram = VgaMemory::new();
    let mut dac = VgaDac::new();
    let mut renderer = Mode13hRenderer::new();

    program_test_palette(&mut dac);
    program_color_bar_vram(&mut vram);

    let sample_offset = (10 * WIDTH) + 10;
    let pixel_before = {
        let framebuffer = renderer.render(&mut vram, &mut dac);
        framebuffer[sample_offset]
    };

    // Update the DAC entry that corresponds to the sample location.
    let sample_index = ((10 * 256) / WIDTH) as u8;
    dac.set_entry_6bit(sample_index, 63, 0, 0);

    let pixel_after = {
        let framebuffer = renderer.render(&mut vram, &mut dac);
        framebuffer[sample_offset]
    };

    assert_ne!(pixel_before, pixel_after);
}

#[test]
fn vga_mode13h_applies_pel_mask() {
    let mut vram = VgaMemory::new();
    let mut dac = VgaDac::new();
    let mut renderer = Mode13hRenderer::new();

    program_test_palette(&mut dac);

    // Write a single pixel with high bits set.
    let offset = 0;
    vram.write(offset, &[0xAB]);

    let full_mask_pixel = {
        let framebuffer = renderer.render(&mut vram, &mut dac);
        framebuffer[offset]
    };

    dac.set_pel_mask(0x0F);
    let masked_pixel = {
        let framebuffer = renderer.render(&mut vram, &mut dac);
        framebuffer[offset]
    };

    assert_ne!(full_mask_pixel, masked_pixel);

    // With PEL mask 0x0F, index 0xAB becomes 0x0B.
    let expected_index = 0xAB & 0x0F;
    let expected_color = dac.palette_rgba()[expected_index as usize];
    assert_eq!(masked_pixel, expected_color);
}

#[test]
fn vga_mode13h_detects_chain4_packed_mode_from_registers() {
    let mut regs = VgaDevice::new();

    // Sequencer Memory Mode (index 0x04):
    // - bit 3: chain-4 enable
    // - bit 2: odd/even disable (so `odd_even` becomes false in derived state)
    regs.port_write(0x3C4, 1, 0x04);
    regs.port_write(0x3C5, 1, 0x0C);

    // Graphics Controller Misc register (index 0x06) bit 0 = graphics mode.
    regs.port_write(0x3CE, 1, 0x06);
    regs.port_write(0x3CF, 1, 0x01);

    let mut vram = VgaMemory::new();
    let mut dac = VgaDac::new();
    let mut renderer = VgaRenderer::new();

    program_test_palette(&mut dac);
    program_color_bar_vram(&mut vram);

    let (width, height, framebuffer) = renderer
        .render(&regs, &mut vram, &mut dac)
        .expect("expected mode detection to select Mode 13h");

    assert_eq!((width, height), (WIDTH, HEIGHT));
    assert_eq!(framebuffer.len(), WIDTH * HEIGHT);
}
