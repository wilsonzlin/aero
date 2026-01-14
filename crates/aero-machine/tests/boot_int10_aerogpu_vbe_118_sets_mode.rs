use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};
use pretty_assertions::assert_eq;

fn build_int10_vbe_set_mode_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
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
fn boot_int10_aerogpu_vbe_118_sets_mode_and_lfb_is_visible_via_bar1() {
    let boot = build_int10_vbe_set_mode_boot_sector();

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

    let bar1_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(profile::AEROGPU.bdf)
            .expect("AeroGPU device missing from PCI bus");
        cfg.bar_range(profile::AEROGPU_BAR1_VRAM_INDEX)
            .expect("AeroGPU BAR1 must exist")
            .base
    };
    assert_ne!(bar1_base, 0);
    let lfb_base = bar1_base + VBE_LFB_OFFSET as u64;
    assert_eq!(m.vbe_lfb_base(), lfb_base);

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    m.write_physical_u32(lfb_base, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
