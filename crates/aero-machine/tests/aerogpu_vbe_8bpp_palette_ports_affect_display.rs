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
fn aerogpu_vbe_8bpp_render_uses_vga_dac_palette() {
    let boot = build_int10_vbe_set_mode_105_boot_sector();

    let mut m = Machine::new(MachineConfig {
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
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Program palette entry 1 to pure red via VGA DAC ports (6-bit values).
    m.io_write(0x3C6, 1, 0xFF); // PEL mask
    m.io_write(0x3C8, 1, 0x01); // DAC write index
    m.io_write(0x3C9, 1, 63); // R
    m.io_write(0x3C9, 1, 0); // G
    m.io_write(0x3C9, 1, 0); // B

    // Write palette index 1 to the first pixel in the 8bpp framebuffer.
    let lfb_base = m.vbe_lfb_base();
    m.write_physical_u8(lfb_base, 1);

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
