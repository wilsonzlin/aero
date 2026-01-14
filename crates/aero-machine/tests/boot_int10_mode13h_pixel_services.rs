use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_mode13h_pixel_service_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x0013  ; INT 10h AH=00h Set Video Mode (mode 13h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x13, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // INT 10h AH=0Ch: Write Graphics Pixel
    //
    // Write palette index 4 at (0,0).
    //
    // xor bx, bx  ; BH=page 0
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // xor cx, cx  ; x=0
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC9]);
    i += 2;
    // xor dx, dx  ; y=0
    sector[i..i + 2].copy_from_slice(&[0x31, 0xD2]);
    i += 2;
    // mov ax, 0x0C04  ; AH=0x0C, AL=4 (palette index 4; EGA red in default VGA palette)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x04, 0x0C]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // INT 10h AH=0Dh: Read Graphics Pixel
    //
    // Read pixel at (0,0) back into AL (should be 4).
    // mov ax, 0x0D00
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0x0D]);
    i += 3;
    // (CX,DX already 0 from previous calls)
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Use the returned AL as the color for a second write at (1,0). If the read service is
    // broken and returns 0, this pixel will stay black and the host-side assertions will fail.
    //
    // inc cx  ; x=1
    sector[i] = 0x41;
    i += 1;
    // mov ah, 0x0C  ; preserve AL
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x0C]);
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
fn boot_int10_mode13h_write_pixel_service_is_visible() {
    let boot = build_int10_mode13h_pixel_service_boot_sector();

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

    m.display_present();
    assert_eq!(m.display_resolution(), (320, 200));

    // Palette index 4 in the default VGA palette is EGA red: RGB(0xAA, 0x00, 0x00).
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00AA);
    assert_eq!(m.display_framebuffer()[1], 0xFF00_00AA);
}
