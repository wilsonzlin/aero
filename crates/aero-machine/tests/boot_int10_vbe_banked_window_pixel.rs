use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_banked_window_pixel_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // ---------------------------------------------------------------------
    // Set VBE mode 0x118 with the LFB requested (BX bit14), as real boot code
    // would typically do when available.
    //
    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // ---------------------------------------------------------------------
    // Set bank 0 explicitly via VBE window control (AX=4F05). This ensures
    // deterministic bank state even if firmware defaults change.
    //
    // mov ax, 0x4F05
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x05, 0x4F]);
    i += 3;
    // xor bx, bx  ; BH=0 (window A), BL=0 (set)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // xor dx, dx  ; DX=0 (bank 0)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xD2]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // ---------------------------------------------------------------------
    // Write one packed 32bpp pixel via the banked 0xA0000 legacy window.
    // Real-mode code cannot directly address the high linear framebuffer.
    //
    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;
    // cld  ; ensure stosb increments
    sector[i] = 0xFC;
    i += 1;

    // mov al, 0x00 ; B
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0x00 ; G
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0xFF ; R
    sector[i..i + 2].copy_from_slice(&[0xB0, 0xFF]);
    i += 2;
    // stosb
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0x00 ; X
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]);
    i += 2;
    // stosb
    sector[i] = 0xAA;
    i += 1;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

fn run_banked_window_pixel_test(enable_pc_platform: bool) {
    let boot = build_int10_vbe_banked_window_pixel_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}

#[test]
fn boot_int10_vbe_banked_window_pixel_pc_platform() {
    run_banked_window_pixel_test(true);
}

#[test]
fn boot_int10_vbe_banked_window_pixel_no_pc_platform() {
    run_banked_window_pixel_test(false);
}
