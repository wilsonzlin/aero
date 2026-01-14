use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bda::{BDA_CURSOR_SHAPE_ADDR, BDA_VIDEO_PAGE_OFFSET_ADDR};

fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    if hash == 0 {
        hash = FNV_OFFSET;
    }
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn framebuffer_hash(fb: &[u32]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for px in fb {
        hash = fnv1a64(hash, &px.to_ne_bytes());
    }
    hash
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected run exit: {other:?}"),
        }
    }
    panic!("guest never reached HLT");
}

fn new_deterministic_aerogpu_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for these tests.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        // Keep the machine minimal.
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn aerogpu_text_mode_scanout_renders_b8000_cell() {
    // Configure a machine with the canonical "AeroGPU (no VGA)" display mode:
    // - enable_pc_platform=true
    // - enable_aerogpu=true
    // - enable_vga=false
    //
    // In this mode, the machine should still be able to present BIOS/boot text output by scanning
    // the legacy text buffer at 0xB8000.
    let mut m = new_deterministic_aerogpu_machine();

    // Force deterministic baseline: clear the full 32KiB legacy text window (0xB8000..0xC0000).
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);

    // Ensure the active page offset is 0 so cell (0,0) maps to 0xB8000.
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);

    // Disable cursor for deterministic output (cursor start CH bit5 = 1).
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);

    // Write "A" at the top-left cell with white-on-blue attributes.
    m.write_physical_u8(0xB8000, b'A');
    m.write_physical_u8(0xB8001, 0x1F);

    m.display_present();

    let (w, h) = m.display_resolution();
    assert_ne!((w, h), (0, 0), "expected non-zero scanout resolution");
    assert_eq!(
        m.display_framebuffer().len(),
        (w as usize).saturating_mul(h as usize),
        "framebuffer length should match resolution"
    );

    // If the implementation matches the canonical VGA text renderer (9x16 cells), lock in the
    // full framebuffer hash as a regression check.
    if (w, h) == (720, 400) {
        assert_eq!(
            framebuffer_hash(m.display_framebuffer()),
            0x5cfe440e33546065
        );
    } else {
        // Otherwise, at least validate expected foreground/background colors are present in the
        // top-left cell.
        let cell_w = (w / 80).max(1) as usize;
        let cell_h = (h / 25).max(1) as usize;
        let fb = m.display_framebuffer();
        let mut fg = 0usize;
        let mut bg = 0usize;
        for y in 0..cell_h.min(h as usize) {
            let row = y * w as usize;
            for x in 0..cell_w.min(w as usize) {
                match fb[row + x] {
                    0xFFFF_FFFF => fg += 1, // white
                    0xFFAA_0000 => bg += 1, // blue
                    other => panic!("unexpected pixel color in cell (0,0): 0x{other:08x}"),
                }
            }
        }
        assert!(fg > 0, "expected some foreground (white) pixels for 'A'");
        assert!(bg > 0, "expected some background (blue) pixels for 'A'");
    }
}

fn build_cursor_boot_sector(row: u8, col: u8, start: u8, end: u8) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ah, 0x02  ; INT 10h AH=02h Set Cursor Position
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x02]);
    i += 2;
    // mov bh, 0x00  ; page 0
    sector[i..i + 2].copy_from_slice(&[0xB7, 0x00]);
    i += 2;
    // mov dh, row
    sector[i..i + 2].copy_from_slice(&[0xB6, row]);
    i += 2;
    // mov dl, col
    sector[i..i + 2].copy_from_slice(&[0xB2, col]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ah, 0x01  ; INT 10h AH=01h Set Cursor Shape
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x01]);
    i += 2;
    // mov ch, start
    sector[i..i + 2].copy_from_slice(&[0xB5, start]);
    i += 2;
    // mov cl, end
    sector[i..i + 2].copy_from_slice(&[0xB1, end]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn aerogpu_text_mode_scanout_renders_cursor_overlay() {
    // Program cursor state via BIOS INT 10h, then validate the presented scanout includes the
    // cursor inversion overlay.
    let boot = build_cursor_boot_sector(0, 0, 0x00, 0x00);
    let mut m = new_deterministic_aerogpu_machine();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Clear the legacy text window and write a space cell with white-on-blue attributes.
    // Space glyph is all background pixels, so the cursor inversion is easy to detect.
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    m.write_physical_u8(0xB8000, b' ');
    m.write_physical_u8(0xB8001, 0x1F);

    m.display_present();
    let (w, h) = m.display_resolution();
    assert_ne!((w, h), (0, 0), "expected non-zero scanout resolution");

    let fb = m.display_framebuffer();
    assert!(
        fb.len() >= w as usize,
        "expected at least one full scanline"
    );

    // First scanline of cell (0,0) should be cursor-inverted to the foreground color (white).
    assert_eq!(fb[0], 0xFFFF_FFFF);
    // Second scanline should remain the background color (EGA blue).
    assert_eq!(fb[w as usize], 0xFFAA_0000);
}
