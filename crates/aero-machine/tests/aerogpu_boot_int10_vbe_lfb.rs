use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

const VBE_LFB_OFFSET: u64 = aero_machine::VBE_LFB_OFFSET as u64;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

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
fn aerogpu_boot_sector_int10_vbe_mode_shows_vram_lfb() {
    let boot = build_int10_vbe_set_mode_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    enable_a20(&mut m);

    let bar1_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(profile::AEROGPU.bdf)
            .expect("AeroGPU device missing from PCI bus");
        cfg.bar_range(1).expect("AeroGPU BAR1 must exist").base
    };
    assert_ne!(bar1_base, 0);
    let lfb_base = bar1_base + VBE_LFB_OFFSET;

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    m.write_physical_u8(lfb_base, 0x00); // B
    m.write_physical_u8(lfb_base + 1, 0x00); // G
    m.write_physical_u8(lfb_base + 2, 0xFF); // R
    m.write_physical_u8(lfb_base + 3, 0x00); // X

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
