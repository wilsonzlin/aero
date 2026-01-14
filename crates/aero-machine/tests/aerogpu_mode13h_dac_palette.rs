use aero_machine::{Machine, MachineConfig, RunExit, ScanoutSource};
use pretty_assertions::assert_eq;

fn build_int10_set_mode13h_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x0013 (set video mode 13h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x13, 0x00]);
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
fn aerogpu_mode13h_render_uses_vga_dac_palette_and_pel_mask() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let boot = build_int10_set_mode13h_boot_sector();
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyVga);

    // Write palette index 1 to the first pixel in the mode 13h framebuffer.
    m.write_physical_u8(0xA0000, 1);

    // Program DAC entry 0 = green, entry 1 = red (6-bit values).
    m.io_write(0x3C8, 1, 0x00);
    // entry 0: green
    m.io_write(0x3C9, 1, 0); // R
    m.io_write(0x3C9, 1, 63); // G
    m.io_write(0x3C9, 1, 0); // B
                             // entry 1: red
    m.io_write(0x3C9, 1, 63); // R
    m.io_write(0x3C9, 1, 0); // G
    m.io_write(0x3C9, 1, 0); // B

    // With PEL mask=0xFF, index 1 should show as red.
    m.io_write(0x3C6, 1, 0xFF);
    m.display_present();
    assert_eq!(m.display_resolution(), (320, 200));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);

    // With PEL mask=0, index 1 should be masked to 0 and show as green.
    m.io_write(0x3C6, 1, 0x00);
    m.display_present();
    assert_eq!(m.display_framebuffer()[0], 0xFF00_FF00);
    assert_eq!(m.io_read(0x3C6, 1) as u8, 0x00);
}
