use aero_machine::{Machine, MachineConfig, RunExit, ScanoutSource};
use pretty_assertions::assert_eq;

fn build_int10_mode13h_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ax, 0x0013  ; INT 10h AH=00h Set Video Mode (mode 13h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x13, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write a palette index to the first pixel at A000:0000.
    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // xor di, di
    sector[i..i + 2].copy_from_slice(&[0x31, 0xFF]);
    i += 2;
    // mov al, 4  ; palette index 4 (EGA red in default VGA palette)
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x04]);
    i += 2;
    // cld
    sector[i] = 0xFC;
    i += 1;
    // stosb  ; [es:di] = al
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
fn boot_sector_int10_mode13h_sets_live_vga_mode_and_pixel_is_visible() {
    let boot = build_int10_mode13h_boot_sector();

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

    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyVga);
    m.display_present();
    assert_eq!(m.display_resolution(), (320, 200));
    // Palette index 4 in the default VGA palette is EGA red: RGB(0xAA, 0x00, 0x00).
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00AA);
}
