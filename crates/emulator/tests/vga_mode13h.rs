use std::path::PathBuf;

use emulator::devices::vga::Vga;
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

fn program_mode13h_registers(vga: &mut Vga) {
    // Sequencer Memory Mode (index 0x04):
    // - bit 3: chain-4 enable
    // - bit 2: odd/even disable (so `odd_even` becomes false in derived state)
    vga.port_write(0x3C4, 1, 0x04);
    vga.port_write(0x3C5, 1, 0x0C);

    // Graphics Controller Misc register (index 0x06) bit 0 = graphics mode.
    vga.port_write(0x3CE, 1, 0x06);
    vga.port_write(0x3CF, 1, 0x01);
}

fn program_test_palette(vga: &mut Vga) {
    // Reset write index to 0 and program a 3-3-2 color cube (256 unique colors),
    // expressed in VGA 6-bit DAC space.
    vga.port_write(0x3C8, 1, 0);

    // 3-3-2 color cube (256 unique colors), expressed in VGA 6-bit DAC space.
    for i in 0u16..256 {
        let r6 = ((i & 0b0000_0111) as u8) * 9;
        let g6 = (((i >> 3) & 0b0000_0111) as u8) * 9;
        let b6 = (((i >> 6) & 0b0000_0011) as u8) * 21;

        vga.port_write(0x3C9, 1, u32::from(r6));
        vga.port_write(0x3C9, 1, u32::from(g6));
        vga.port_write(0x3C9, 1, u32::from(b6));
    }

    // PEL mask defaults to 0xFF on real hardware; explicitly set it so tests
    // don't depend on initialization.
    vga.port_write(0x3C6, 1, 0xFF);
}

fn program_color_bar_vram(vga: &mut Vga) {
    let mut vram_buf = vec![0u8; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            // Spread indices evenly across the scanline without wrapping.
            let index = ((x * 256) / WIDTH) as u8;
            vram_buf[y * WIDTH + x] = index;
        }
    }
    vga.write_vram(0, &vram_buf);
}

#[test]
fn vga_mode13h_renders_color_bars_golden() {
    let mut vga = Vga::new();

    program_mode13h_registers(&mut vga);
    program_test_palette(&mut vga);
    program_color_bar_vram(&mut vga);

    let (width, height, framebuffer) = vga
        .render()
        .expect("expected mode detection to select Mode 13h");
    assert_eq!((width, height), (WIDTH, HEIGHT));
    assert_eq!(framebuffer.len(), WIDTH * HEIGHT);

    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(&golden_path("color_bars.png"), &rgba, WIDTH, HEIGHT);
}

#[test]
fn vga_mode13h_palette_change_repaints_pixels() {
    let mut vga = Vga::new();

    program_mode13h_registers(&mut vga);
    program_test_palette(&mut vga);
    program_color_bar_vram(&mut vga);

    let sample_offset = (10 * WIDTH) + 10;
    let pixel_before = {
        let (_, _, framebuffer) = vga.render().unwrap();
        framebuffer[sample_offset]
    };

    // Update the DAC entry that corresponds to the sample location.
    let sample_index = ((10 * 256) / WIDTH) as u8;
    vga.port_write(0x3C8, 1, u32::from(sample_index));
    vga.port_write(0x3C9, 1, 63);
    vga.port_write(0x3C9, 1, 0);
    vga.port_write(0x3C9, 1, 0);

    let pixel_after = {
        let (_, _, framebuffer) = vga.render().unwrap();
        framebuffer[sample_offset]
    };

    assert_ne!(pixel_before, pixel_after);
}

#[test]
fn vga_mode13h_applies_pel_mask() {
    let mut vga = Vga::new();

    program_mode13h_registers(&mut vga);
    program_test_palette(&mut vga);

    // Write a single pixel with high bits set.
    let offset = 0;
    vga.write_vram_u8(offset, 0xAB);

    let full_mask_pixel = {
        let (_, _, framebuffer) = vga.render().unwrap();
        framebuffer[offset]
    };

    vga.port_write(0x3C6, 1, 0x0F);
    let masked_pixel = {
        let (_, _, framebuffer) = vga.render().unwrap();
        framebuffer[offset]
    };

    assert_ne!(full_mask_pixel, masked_pixel);

    // With PEL mask 0x0F, index 0xAB becomes 0x0B.
    let expected_index = 0xAB & 0x0F;
    let expected_color = vga.dac().palette_rgba()[expected_index as usize];
    assert_eq!(masked_pixel, expected_color);
}
