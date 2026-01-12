use aero_gpu_vga::{DisplayOutput, SVGA_LFB_BASE};
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_set_mode_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
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
fn boot_sector_int10_vbe_sets_live_svga_mode_and_lfb_is_visible() {
    let boot = build_int10_vbe_set_mode_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    let vga = m.vga().expect("machine should have a VGA device");
    assert_eq!(vga.borrow().get_resolution(), (1024, 768));

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    let base = u64::from(SVGA_LFB_BASE);
    m.write_physical_u8(base, 0x00); // B
    m.write_physical_u8(base + 1, 0x00); // G
    m.write_physical_u8(base + 2, 0xFF); // R
    m.write_physical_u8(base + 3, 0x00); // X

    vga.borrow_mut().present();
    assert_eq!(vga.borrow().get_framebuffer()[0], 0xFF00_00FF);
}
