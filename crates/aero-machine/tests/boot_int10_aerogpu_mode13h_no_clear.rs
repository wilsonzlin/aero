use aero_machine::{Machine, MachineConfig, RunExit, ScanoutSource};
use pretty_assertions::assert_eq;

fn build_mode13h_no_clear_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // Write a known palette index into the VGA graphics window *before* setting mode 13h.
    //
    // This exercises the "no clear" semantics of INT 10h AH=00h when bit 7 of AL is set.
    //
    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov byte ptr es:[0x0000], 4  (palette index 4 = EGA red in the default VGA palette)
    sector[i..i + 6].copy_from_slice(&[0x26, 0xC6, 0x06, 0x00, 0x00, 0x04]);
    i += 6;

    // mov ax, 0x0093 (set video mode 13h with "no clear" flag)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x93, 0x00]);
    i += 3;
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
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn boot_int10_aerogpu_mode13h_no_clear_preserves_vram() {
    let boot = build_mode13h_no_clear_boot_sector();

    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this compatibility test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyVga);

    m.display_present();
    assert_eq!(m.display_resolution(), (320, 200));
    // Default VGA palette entry 4 is EGA red (0xAA,0x00,0x00).
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00AA);
}
