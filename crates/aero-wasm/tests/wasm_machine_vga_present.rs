#![cfg(target_arch = "wasm32")]

use aero_wasm::{Machine, RunExitKind};
use wasm_bindgen_test::wasm_bindgen_test;

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

#[wasm_bindgen_test]
fn wasm_machine_vga_present_exposes_nonblank_framebuffer() {
    let boot = boot_sector_write_a_to_b8000();
    let mut machine = Machine::new(16 * 1024 * 1024).expect("Machine::new");
    machine
        .set_disk_image(&boot)
        .expect("set_disk_image should accept a 512-byte boot sector");
    machine.reset();

    let mut halted = false;
    for _ in 0..10_000 {
        let exit = machine.run_slice(50_000);
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

    // Ensure the VGA/SVGA front buffer is up to date before reading it via ptr/len.
    // (In the canonical machine configuration VGA is always present.)
    machine.vga_present();

    let width = machine.vga_width();
    let height = machine.vga_height();
    assert!(width > 0, "expected non-zero vga_width");
    assert!(height > 0, "expected non-zero vga_height");
    assert_eq!(machine.vga_stride_bytes(), width * 4);

    let ptr = machine.vga_framebuffer_ptr();
    let len = machine.vga_framebuffer_len_bytes();
    assert!(ptr != 0, "expected non-zero vga_framebuffer_ptr");
    assert!(len != 0, "expected non-zero vga_framebuffer_len_bytes");

    // Safety: ptr/len is a view into the module's own linear memory.
    let fb = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let hash = fnv1a(fb);
    let blank = fnv1a_blank_rgba8(len as usize);
    assert_ne!(
        hash, blank,
        "expected VGA framebuffer hash to differ from blank screen"
    );
}
