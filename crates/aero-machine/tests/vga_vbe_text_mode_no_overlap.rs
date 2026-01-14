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
    panic!("guest did not halt within budget");
}

fn build_boot_sector_vbe_then_return_to_text_no_clear() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // Write 'A' with attribute 0x1F (white on blue) to the top-left text cell (B800:0000).
    //
    // mov ax, 0xB800
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xB8]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;
    // mov ax, 0x1F41
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x41, 0x1F]);
    i += 3;
    // mov es:[di], ax
    sector[i..i + 3].copy_from_slice(&[0x26, 0x89, 0x05]);
    i += 3;

    // Switch to a VBE mode (0x118) with LFB + no-clear.
    //
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0xC118 (mode 0x118 + LFB requested + no-clear)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0xC1]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write one red pixel (packed B,G,R,X) via the banked window at A000:0000.
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

    // mov byte es:[di], 0x00  ; B
    sector[i..i + 4].copy_from_slice(&[0x26, 0xC6, 0x05, 0x00]);
    i += 4;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte es:[di], 0x00  ; G
    sector[i..i + 4].copy_from_slice(&[0x26, 0xC6, 0x05, 0x00]);
    i += 4;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte es:[di], 0xFF  ; R
    sector[i..i + 4].copy_from_slice(&[0x26, 0xC6, 0x05, 0xFF]);
    i += 4;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte es:[di], 0x00  ; X/reserved
    sector[i..i + 4].copy_from_slice(&[0x26, 0xC6, 0x05, 0x00]);
    i += 4;

    // Return to 80x25 text mode without clearing the text buffer.
    //
    // mov ax, 0x0083 (AH=00 Set Video Mode, AL=0x03 | 0x80 no-clear)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x83, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Disable the cursor to keep the rendered framebuffer deterministic.
    //
    // mov ah, 0x01 (Set Cursor Shape)
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x01]);
    i += 2;
    // mov ch, 0x20 (disable cursor bit)
    sector[i..i + 2].copy_from_slice(&[0xB5, 0x20]);
    i += 2;
    // mov cl, 0x00
    sector[i..i + 2].copy_from_slice(&[0xB1, 0x00]);
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
fn vbe_framebuffer_does_not_clobber_legacy_text_memory() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test environment deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(build_boot_sector_vbe_then_return_to_text_no_clear().to_vec())
        .unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Verify the original text cell bytes were preserved.
    assert_eq!(m.read_physical_u8(0xB8000), b'A');
    assert_eq!(m.read_physical_u8(0xB8001), 0x1F);

    // And ensure the rendered output matches the expected glyph (pixel (2,0) is a foreground pixel
    // for 'A' in the built-in font).
    m.display_present();
    assert_eq!(m.display_resolution(), (720, 400));
    assert_eq!(m.display_framebuffer()[2], 0xFFFF_FFFF);
}
