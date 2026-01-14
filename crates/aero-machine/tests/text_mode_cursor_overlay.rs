use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest never reached HLT");
}

fn build_cursor_boot_sector(row: u8, col: u8, start: u8, end: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
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
fn text_mode_cursor_overlay_renders_from_crtc_regs() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    // Cursor at (0,0) with a 1-scanline cursor at scanline 0.
    let boot = build_cursor_boot_sector(0, 0, 0x00, 0x00);
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Write a space with white-on-blue attributes to the first text cell.
    // Space glyph is all background pixels, so the cursor inversion is easy to detect.
    m.write_physical_u8(0xB8000, b' ');
    m.write_physical_u8(0xB8001, 0x1F);

    m.display_present();
    let (width, _height) = m.display_resolution();
    let fb = m.display_framebuffer();
    let px0 = fb[0];
    let px1 = fb[width as usize];

    // Text mode uses fixed 80x25 cells of 9x16 pixels.
    assert_eq!(width, 80 * 9);

    // First scanline of cell (0,0) should be cursor-inverted to the foreground color (white).
    assert_eq!(px0, 0xFFFF_FFFF);
    // Second scanline should remain the background color (EGA blue).
    assert_eq!(px1, 0xFFAA_0000);
}

#[test]
fn text_mode_cursor_overlay_respects_disable_bit() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    // Cursor at (0,0), but disabled via CH bit5.
    let boot = build_cursor_boot_sector(0, 0, 0x20, 0x00);
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Space glyph is all background pixels, so if the cursor is disabled we should see only the
    // background color.
    m.write_physical_u8(0xB8000, b' ');
    m.write_physical_u8(0xB8001, 0x1F);

    m.display_present();
    let pixel0 = m.display_framebuffer()[0];

    assert_eq!(pixel0, 0xFFAA_0000);
}

#[test]
fn text_mode_cursor_overlay_respects_crtc_start_address() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    // Force deterministic baseline: clear the full 32KiB text window.
    {
        let mut addr = 0xB8000u64;
        let mut remaining = 0x8000usize;
        const ZERO: [u8; 4096] = [0; 4096];
        while remaining != 0 {
            let len = remaining.min(ZERO.len());
            m.write_physical(addr, &ZERO[..len]);
            addr = addr.saturating_add(len as u64);
            remaining -= len;
        }
    }

    // Display page starting at cell 0x0800 (offset 0x1000 bytes into B8000).
    let start_addr_cells = 0x0800u16;
    m.io_write(0x3D4, 1, 0x0C);
    m.io_write(0x3D5, 1, u32::from((start_addr_cells >> 8) as u8));
    m.io_write(0x3D4, 1, 0x0D);
    m.io_write(0x3D5, 1, u32::from((start_addr_cells & 0x00FF) as u8));

    // Cursor at the top-left of the displayed page.
    m.io_write(0x3D4, 1, 0x0A);
    m.io_write(0x3D5, 1, 0x00); // enable, scanline 0
    m.io_write(0x3D4, 1, 0x0B);
    m.io_write(0x3D5, 1, 0x00); // 1 scanline tall
    m.io_write(0x3D4, 1, 0x0E);
    m.io_write(0x3D5, 1, u32::from((start_addr_cells >> 8) as u8));
    m.io_write(0x3D4, 1, 0x0F);
    m.io_write(0x3D5, 1, u32::from((start_addr_cells & 0x00FF) as u8));

    // Write a space cell (white-on-blue) into the first cell of the displayed page so the cursor
    // inversion is easy to detect.
    let base = 0xB8000u64 + u64::from(start_addr_cells) * 2;
    m.write_physical_u8(base, b' ');
    m.write_physical_u8(base + 1, 0x1F);

    m.display_present();
    let (width, _height) = m.display_resolution();
    let fb = m.display_framebuffer();
    let px0 = fb[0];
    let px1 = fb[width as usize];
    assert_eq!(width, 80 * 9);
    assert_eq!(px0, 0xFFFF_FFFF);
    assert_eq!(px1, 0xFFAA_0000);
}
