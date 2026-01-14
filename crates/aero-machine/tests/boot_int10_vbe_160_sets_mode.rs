use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_160_set_mode_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4160 (mode 0x160 + linear framebuffer requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x60, 0x41]);
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
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn boot_int10_vbe_160_sets_mode_and_lfb_is_visible() {
    let boot = build_int10_vbe_160_set_mode_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for these tests.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    m.display_present();
    assert_eq!(m.display_resolution(), (1280, 720));

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    let base = m.vbe_lfb_base();
    m.write_physical_u32(base, 0x00FF_0000);
    m.display_present();
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
