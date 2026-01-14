use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_bank_switch_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // cld (ensure `stosb` increments DI).
    sector[i] = 0xFC;
    i += 1;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0x4F05 (VBE Display Window Control / bank switching)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x05, 0x4F]);
    i += 3;
    // xor bx, bx (BH=0x00 "window A", BL=0x00 "set")
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // mov dx, 0x0001 (bank 1)
    sector[i..i + 3].copy_from_slice(&[0xBA, 0x01, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;

    // Write a red pixel to ES:DI = A000:0000 (B, G, R, X).
    // xor al, al ; B=0
    sector[i..i + 2].copy_from_slice(&[0x30, 0xC0]);
    i += 2;
    // stosb
    sector[i] = 0xAA;
    i += 1;
    // stosb (G=0)
    sector[i] = 0xAA;
    i += 1;
    // mov al, 0xFF ; R=255
    sector[i..i + 2].copy_from_slice(&[0xB0, 0xFF]);
    i += 2;
    // stosb
    sector[i] = 0xAA;
    i += 1;
    // xor al, al ; X=0
    sector[i..i + 2].copy_from_slice(&[0x30, 0xC0]);
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

#[test]
fn boot_int10_vbe_bank_switch_maps_a0000_window_beyond_64k() {
    let boot = build_int10_vbe_bank_switch_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
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

    // For 1024x768x32, the VRAM byte offset 0x1_0000 corresponds to:
    //   pixel_index = 0x1_0000 / 4 = 0x4000 = 16384
    //   (x, y) = (0, 16) since stride is 1024 pixels.
    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[16 * 1024], 0xFF00_00FF);
}
