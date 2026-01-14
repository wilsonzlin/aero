use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_set_mode_105_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4105 (mode 0x105 + LFB requested, 1024x768x8bpp)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x05, 0x41]);
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
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn aerogpu_snapshot_preserves_vga_dac_palette_and_pel_mask() {
    let boot = build_int10_vbe_set_mode_105_boot_sector();

    let cfg = MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Program a distinct palette and use a non-default PEL mask so we can validate these survive
    // snapshot/restore.
    //
    // - Set palette[0] = green
    // - Set palette[1] = red
    // - Set PEL mask = 0 so index 1 maps to palette[0]
    m.io_write(0x3C6, 1, 0x00); // PEL mask
    m.io_write(0x3C8, 1, 0x00); // DAC write index
                                // Entry 0: green.
    m.io_write(0x3C9, 1, 0); // R
    m.io_write(0x3C9, 1, 63); // G
    m.io_write(0x3C9, 1, 0); // B
                             // Entry 1: red.
    m.io_write(0x3C9, 1, 63); // R
    m.io_write(0x3C9, 1, 0); // G
    m.io_write(0x3C9, 1, 0); // B

    // Write palette index 1 to the first pixel in the 8bpp framebuffer. PEL mask forces it to use
    // palette entry 0 instead.
    let lfb_base = m.vbe_lfb_base();
    m.write_physical_u8(lfb_base, 1);

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_FF00);

    let snap = m.take_snapshot_full().unwrap();

    // Restore into a fresh machine and validate palette + PEL mask survive.
    let mut m2 = Machine::new(cfg).unwrap();
    m2.set_disk_image(boot.to_vec()).unwrap();
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    m2.display_present();
    assert_eq!(m2.display_resolution(), (1024, 768));
    assert_eq!(m2.display_framebuffer()[0], 0xFF00_FF00);
    assert_eq!(m2.io_read(0x3C6, 1) as u8, 0x00);
}
