use aero_devices::pci::profile;
use aero_gpu_vga::SVGA_LFB_BASE;
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_set_mode_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;

    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Query VBE mode info (so we can verify PhysBasePtr in guest memory).
    //
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // mov ax, 0x4F01
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x4F]);
    i += 3;
    // mov cx, 0x0118 (mode id, without flags)
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x18, 0x01]);
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
fn boot_sector_int10_vbe_sets_mode_and_lfb_is_visible_at_non_default_base() {
    let boot = build_int10_vbe_set_mode_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Force the VGA PCI BAR allocation away from the historical hard-coded base.
        enable_e1000: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    let lfb_base = m
        .pci_bar_base(profile::VGA_TRANSITIONAL_STUB.bdf, 0)
        .and_then(|base| u32::try_from(base).ok())
        .expect("VGA PCI BAR should be assigned by BIOS POST");
    assert_ne!(lfb_base, 0);
    assert_ne!(
        lfb_base, SVGA_LFB_BASE,
        "expected VGA BAR base to differ from SVGA_LFB_BASE when another PCI MMIO device is enabled"
    );
    assert_eq!(m.vbe_lfb_base(), u64::from(lfb_base));

    let phys_base_ptr = m.read_physical_u32(0x0500 + 40);
    assert_eq!(
        phys_base_ptr, lfb_base,
        "INT 10h AX=4F01 mode info PhysBasePtr must match the VGA PCI BAR assignment"
    );

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));

    let base = u64::from(lfb_base);
    m.write_physical_u8(base, 0x00); // B
    m.write_physical_u8(base + 1, 0x00); // G
    m.write_physical_u8(base + 2, 0xFF); // R
    m.write_physical_u8(base + 3, 0x00); // X

    m.display_present();
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
