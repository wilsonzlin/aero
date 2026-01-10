use std::path::{Path, PathBuf};

use emulator::devices::vga::{
    Mode12hRenderer, Mode13hRenderer, TextModeRenderer, VBE_LFB_BASE, VgaDac, VgaDevice, VgaMemory,
    VramPlane,
};
use emulator::io::PortIO;
use image::RgbaImage;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/vga")
        .join(name)
}

fn rgba_bytes_from_framebuffer(framebuffer: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(framebuffer.len() * 4);
    for &pixel in framebuffer {
        out.extend_from_slice(&pixel.to_le_bytes());
    }
    out
}

fn assert_png_matches(path: &Path, rgba: &[u8], width: usize, height: usize) {
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

fn program_ega_palette(dac: &mut VgaDac) {
    // Standard-ish EGA palette in VGA 6-bit space.
    const EGA: [[u8; 3]; 16] = [
        [0x00, 0x00, 0x00],
        [0x00, 0x00, 0x2A],
        [0x00, 0x2A, 0x00],
        [0x00, 0x2A, 0x2A],
        [0x2A, 0x00, 0x00],
        [0x2A, 0x00, 0x2A],
        [0x2A, 0x15, 0x00],
        [0x2A, 0x2A, 0x2A],
        [0x15, 0x15, 0x15],
        [0x15, 0x15, 0x3F],
        [0x15, 0x3F, 0x15],
        [0x15, 0x3F, 0x3F],
        [0x3F, 0x15, 0x15],
        [0x3F, 0x15, 0x3F],
        [0x3F, 0x3F, 0x15],
        [0x3F, 0x3F, 0x3F],
    ];

    for (i, [r, g, b]) in EGA.iter().copied().enumerate() {
        dac.set_entry_6bit(i as u8, r, g, b);
    }

    // VGA text modes program the Attribute Controller palette registers to point at the first
    // 64 DAC entries (the "EGA" palette), rather than using indices 0..=15 directly. Mirror the
    // standard mode 03h mapping so text rendering that honours the AC palette sees the expected
    // 16-colour values.
    const MODE03_DAC_MAP: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
    ];
    for (i, &dac_idx) in MODE03_DAC_MAP.iter().enumerate() {
        let [r, g, b] = EGA[i];
        dac.set_entry_6bit(dac_idx, r, g, b);
    }

    dac.set_pel_mask(0xFF);
}

#[test]
fn golden_text_mode_glyphs_colors_and_cursor() {
    let mut vga = VgaDevice::new();
    vga.set_legacy_mode(0x03, true);

    let mut vram = VgaMemory::new();
    // Mirror the BIOS-style clear performed by `VgaDevice::set_legacy_mode` so the golden image
    // matches typical 80x25 text mode state (space characters with light-grey attributes).
    vram.plane_mut(VramPlane(0))[..0x4000].fill(0x20);
    vram.plane_mut(VramPlane(1))[..0x4000].fill(0x07);
    vram.mark_all_dirty();

    let mut dac = VgaDac::new();
    program_ega_palette(&mut dac);

    // Write some text with varying colors on the first row.
    let msg = b"AERO VGA TEST 1234";
    for (i, &ch) in msg.iter().enumerate() {
        vram.write_plane_byte(0, i, ch);
        let fg = (i as u8) & 0x0F;
        let bg = 0x01;
        vram.write_plane_byte(1, i, (bg << 4) | fg);
    }

    // Cursor at cell 5 with a mid-height block.
    vga.port_write(0x3D4, 1, 0x0A);
    vga.port_write(0x3D5, 1, 0x08);
    vga.port_write(0x3D4, 1, 0x0B);
    vga.port_write(0x3D5, 1, 0x0F);
    vga.port_write(0x3D4, 1, 0x0E);
    vga.port_write(0x3D5, 1, 0x00);
    vga.port_write(0x3D4, 1, 0x0F);
    vga.port_write(0x3D5, 1, 0x05);

    let mut renderer = TextModeRenderer::new();
    let framebuffer = renderer.render(&vga, &mut vram, &mut dac);
    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(
        &golden_path("text_mode.png"),
        &rgba,
        emulator::devices::vga::TEXT_MODE_WIDTH,
        emulator::devices::vga::TEXT_MODE_HEIGHT,
    );
}

#[test]
fn golden_mode_12h_planar_checkerboard() {
    let mut vga = VgaDevice::new();
    vga.set_legacy_mode(0x12, true);

    let mut vram = VgaMemory::new();

    let mut dac = VgaDac::new();
    program_ega_palette(&mut dac);

    // 16x16 pixel checkerboard, colors 0x0 and 0xF.
    let bytes_per_row = 80usize;
    for y in 0..emulator::devices::vga::MODE12H_HEIGHT {
        let checker_y = (y / 16) & 1;
        for byte_x in 0..bytes_per_row {
            let checker_x = ((byte_x * 8) / 16) & 1;
            let checker = checker_x ^ checker_y;
            let byte_val = if checker == 0 { 0x00 } else { 0xFF };
            let byte_off = y * bytes_per_row + byte_x;
            for plane in 0..4 {
                vram.write_plane_byte(plane, byte_off, byte_val);
            }
        }
    }

    let mut renderer = Mode12hRenderer::new();
    let framebuffer = renderer.render(&vga, &mut vram, &mut dac);
    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(
        &golden_path("mode12h_checkerboard.png"),
        &rgba,
        emulator::devices::vga::MODE12H_WIDTH,
        emulator::devices::vga::MODE12H_HEIGHT,
    );
}

#[test]
fn golden_mode_13h_color_bars() {
    let mut _vga = VgaDevice::new();
    _vga.set_legacy_mode(0x13, true);

    let mut vram = VgaMemory::new();

    let mut dac = VgaDac::new();
    program_ega_palette(&mut dac);

    let mut buf = vec![0u8; emulator::devices::vga::MODE13H_VRAM_SIZE];
    for y in 0..emulator::devices::vga::MODE13H_HEIGHT {
        for x in 0..emulator::devices::vga::MODE13H_WIDTH {
            let bar = x / 20; // 320 / 16 = 20 px per bar.
            buf[y * emulator::devices::vga::MODE13H_WIDTH + x] = bar as u8;
        }
    }
    vram.write(0, &buf);

    let mut renderer = Mode13hRenderer::new();
    let framebuffer = renderer.render(&mut vram, &mut dac);
    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(
        &golden_path("mode13h_color_bars.png"),
        &rgba,
        emulator::devices::vga::MODE13H_WIDTH,
        emulator::devices::vga::MODE13H_HEIGHT,
    );
}

#[test]
fn golden_vbe_1024x768x32_gradient() {
    let mut vga = VgaDevice::new();
    vga.set_mode(0x4118).expect("set VBE mode 1024x768x32 with LFB");
    assert!(vga.is_lfb_enabled());
    assert_eq!(vga.resolution(), Some((1024, 768)));
    assert_eq!(vga.mode_info(0x118).unwrap().phys_base_ptr(), VBE_LFB_BASE);

    let width = 1024u32;
    let height = 768u32;
    let mut lfb = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let r = (x * 255 / (width - 1)) as u8;
            let g = (y * 255 / (height - 1)) as u8;
            let b = 0x80;
            let idx = ((y * width + x) * 4) as usize;
            lfb[idx] = r;
            lfb[idx + 1] = g;
            lfb[idx + 2] = b;
            lfb[idx + 3] = 0xFF;
        }
    }
    vga.lfb_write(0, &lfb);

    let (w, h) = vga.resolution().unwrap();
    let framebuffer = vga.render();
    let rgba = rgba_bytes_from_framebuffer(framebuffer);
    assert_png_matches(
        &golden_path("vbe_1024x768x32_gradient.png"),
        &rgba,
        w as usize,
        h as usize,
    );
}
