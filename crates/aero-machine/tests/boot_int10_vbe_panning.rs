use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("machine did not halt within budget");
}

fn build_vbe_mode_118_with_stride_and_display_start_boot_sector(
    bytes_per_scan_line: u16,
    x_off: u16,
    y_off: u16,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // INT 10h AX=4F02: Set VBE mode 0x118 (1024x768x32bpp) with LFB requested.
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]); // mov ax, 0x4F02
    i += 3;
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]); // mov bx, 0x4118
    i += 3;
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]); // int 0x10
    i += 2;

    // INT 10h AX=4F06: Set logical scan line length in bytes (BL=0x02).
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x06, 0x4F]); // mov ax, 0x4F06
    i += 3;
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x02, 0x00]); // mov bx, 0x0002
    i += 3;
    sector[i] = 0xB9; // mov cx, imm16
    sector[i + 1..i + 3].copy_from_slice(&bytes_per_scan_line.to_le_bytes());
    i += 3;
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]); // int 0x10
    i += 2;

    // INT 10h AX=4F07: Set display start (panning).
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x07, 0x4F]); // mov ax, 0x4F07
    i += 3;
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x00]); // mov bx, 0x0000
    i += 3;
    sector[i] = 0xB9; // mov cx, imm16
    sector[i + 1..i + 3].copy_from_slice(&x_off.to_le_bytes());
    i += 3;
    sector[i] = 0xBA; // mov dx, imm16
    sector[i + 1..i + 3].copy_from_slice(&y_off.to_le_bytes());
    i += 3;
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]); // int 0x10
    i += 2;

    sector[i] = 0xF4; // hlt

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn boot_int10_vbe_scanline_bytes_and_display_start_affect_scanout_base() {
    // Pick a scanline length that differs from the mode's default pitch (1024*4) so we can
    // observe that the scanout path respects `INT 10h AX=4F06` rather than assuming a tightly
    // packed `width * bytes_per_pixel` stride.
    let bytes_per_scan_line = 4101u16;
    let x_off = 1u16;
    let y_off = 4u16; // ensure stride mismatch changes base by >=4 bytes (no overlap between pixels)

    let boot = build_vbe_mode_118_with_stride_and_display_start_boot_sector(
        bytes_per_scan_line,
        x_off,
        y_off,
    );

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    let bytes_per_pixel = 4u64;
    let base = m.vbe_lfb_base();

    // INT 10h AX=4F06 BL=2 sets the logical scan line length in bytes. The BIOS preserves
    // byte-granular pitches but clamps them to at least the mode's natural pitch (1024*4).
    let effective_bytes_per_scan_line =
        u64::from(bytes_per_scan_line).max(1024u64 * bytes_per_pixel);

    // Correct mapping per VBE contract:
    //   base = lfb_base + y_off * bytes_per_scan_line + x_off * bytes_per_pixel
    let correct_off =
        u64::from(y_off) * effective_bytes_per_scan_line + u64::from(x_off) * bytes_per_pixel;

    // Incorrect mapping if the scanout path ignores the logical scanline override and assumes the
    // mode's default pitch (1024*4).
    let wrong_stride_bytes = 1024u64 * bytes_per_pixel;
    let wrong_off = u64::from(y_off) * wrong_stride_bytes + u64::from(x_off) * bytes_per_pixel;

    assert!(
        correct_off >= wrong_off + 4,
        "test requires non-overlapping pixel writes (correct_off={correct_off}, wrong_off={wrong_off})"
    );

    // Seed different colors at the two candidate base addresses. The scanout renderer must pick
    // the `correct_off` pixel.
    m.write_physical_u32(base + wrong_off, 0x00FF_0000); // red (BGRX)
    m.write_physical_u32(base + correct_off, 0x0000_FF00); // green (BGRX)

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_FF00);
}
