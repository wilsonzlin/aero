use aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX;
use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};
use pretty_assertions::assert_eq;

fn build_int10_vbe_115_set_mode_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4115 (mode 0x115 + linear framebuffer requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x15, 0x41]);
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
fn boot_int10_aerogpu_vbe_115_sets_mode_and_lfb_is_visible_via_bar1() {
    let boot = build_int10_vbe_115_set_mode_boot_sector();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for these tests.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    let bdf = m.aerogpu_bdf().expect("AeroGPU should be present");
    let bar1_base = m.pci_bar_base(bdf, AEROGPU_BAR1_VRAM_INDEX).unwrap_or(0);
    assert_ne!(
        bar1_base, 0,
        "AeroGPU BAR1 base should be assigned by BIOS POST"
    );

    run_until_halt(&mut m);

    // The BIOS should report the LFB base as BAR1_BASE + VBE_LFB_OFFSET (currently 0x40000 /
    // AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES; after the legacy VGA backing reservation).
    let lfb_base = m.vbe_lfb_base();
    assert_eq!(lfb_base, bar1_base + VBE_LFB_OFFSET as u64);

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    m.write_physical_u32(lfb_base, 0x00FF_0000);

    m.display_present();
    assert_eq!(m.display_resolution(), (800, 600));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
