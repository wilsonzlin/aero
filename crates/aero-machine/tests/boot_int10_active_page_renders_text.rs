use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("machine did not halt within budget");
}

fn build_boot_sector_select_page1_write_space_disable_cursor() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x0501  ; AH=05 Select Active Display Page, AL=1
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x05]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0x0E20  ; AH=0Eh Teletype output, AL=' '
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x20, 0x0E]);
    i += 3;
    // mov bx, 0x011F  ; BH=1 page, BL=0x1F attribute (white on blue)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x1F, 0x01]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ah, 0x01  ; AH=01 Set Cursor Shape
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x01]);
    i += 2;
    // mov ch, 0x20  ; disable cursor (bit5)
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
fn boot_int10_active_page_select_makes_page_visible_via_crtc_start_address() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
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
    m.set_disk_image(build_boot_sector_select_page1_write_space_disable_cursor().to_vec())
        .unwrap();
    m.reset();
    run_until_halt(&mut m);

    m.display_present();
    let pixel0 = m.display_framebuffer()[0];

    // The guest wrote a space with a blue background into the top-left cell of page 1. Without
    // correctly mirroring AH=05 active page -> CRTC start address, we'd still be displaying page 0
    // and this pixel would be black.
    assert_eq!(pixel0, 0xFFAA_0000);
}
