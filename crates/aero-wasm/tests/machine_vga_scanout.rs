#![cfg(not(target_arch = "wasm32"))]

use aero_wasm::{Machine, RunExitKind};

fn boot_sector_write_a_to_b8000() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov ax, 0xB800
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xB8]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;
    // mov ax, 0x0020  (' ' with attr 0x00 => black)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x20, 0x00]);
    i += 3;
    // mov cx, 2000  (80*25)
    sector[i..i + 3].copy_from_slice(&[0xB9, 0xD0, 0x07]);
    i += 3;
    // rep stosw
    sector[i..i + 2].copy_from_slice(&[0xF3, 0xAB]);
    i += 2;

    // Disable the hardware text cursor (CRTC cursor start register bit5).
    // mov dx, 0x3D4
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xD4, 0x03]);
    i += 3;
    // mov al, 0x0A
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x0A]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // inc dx
    sector[i] = 0x42;
    i += 1;
    // mov al, 0x20 (cursor disable)
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x20]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // Write 'A' with attr 0x1F (white on blue) at the top-left cell.
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;
    // mov ax, 0x1F41
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x41, 0x1F]);
    i += 3;
    // stosw
    sector[i] = 0xAB;
    i += 1;

    // cli; hlt; jmp $
    sector[i] = 0xFA;
    i += 1;
    sector[i] = 0xF4;
    i += 1;
    sector[i..i + 2].copy_from_slice(&[0xEB, 0xFE]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn fnv1a(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn fnv1a_blank_rgba8(len: usize) -> u64 {
    // Blank framebuffer is fully black with alpha=255: [0,0,0,255] repeating.
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for i in 0..len {
        let b = if (i & 3) == 3 { 0xFF } else { 0x00 };
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[test]
fn machine_vga_scanout_exports_non_empty_rgba8888_framebuffer() {
    // Keep the RAM size small-ish for a fast smoke test while still being large enough for the
    // canonical PC machine configuration.
    let boot = boot_sector_write_a_to_b8000();
    let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");
    m.set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");
    m.reset();

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = m.run_slice(50_000);
        match exit.kind() {
            RunExitKind::Completed => {}
            RunExitKind::Halted => {
                halted = true;
                break;
            }
            other => panic!("unexpected RunExitKind: {other:?}"),
        }
    }
    assert!(halted, "guest never reached HLT");

    // Ensure the front buffer is up to date (no-op if nothing is dirty).
    m.vga_present();

    let width = m.vga_width();
    let height = m.vga_height();
    assert!(width > 0, "vga_width must be non-zero when VGA is present");
    assert!(height > 0, "vga_height must be non-zero when VGA is present");

    assert_eq!(
        m.vga_stride_bytes(),
        width.saturating_mul(4),
        "stride must be width * 4 for RGBA8888"
    );

    let len_bytes = m.vga_framebuffer_len_bytes();
    let expected_len_bytes = (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(4);
    assert!(
        expected_len_bytes <= u64::from(u32::MAX),
        "framebuffer byte length should fit in u32 for test mode"
    );
    assert_eq!(
        len_bytes as u64, expected_len_bytes,
        "len_bytes must equal width * height * 4"
    );

    let copy = m.vga_framebuffer_copy_rgba8888();
    assert!(!copy.is_empty(), "copied framebuffer should be non-empty");
    assert_eq!(copy.len() as u32, len_bytes, "copy length should match len_bytes");

    let blank = fnv1a_blank_rgba8(copy.len());
    let hash = fnv1a(&copy);
    assert_ne!(
        hash, blank,
        "expected VGA framebuffer hash to differ from blank screen"
    );

    // The raw pointer view is only meaningful for wasm32 builds; native builds return 0.
    assert_eq!(m.vga_framebuffer_ptr(), 0);
}
