use aero_devices::pci::profile::AEROGPU_VRAM_SIZE;
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_get_controller_info_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x2000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0x20]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov di, 0x0100
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x01]);
    i += 3;

    // mov ax, 0x4F00 (VBE Get Controller Info)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0x4F]);
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
fn vbe_total_memory_matches_aerogpu_vram_aperture_when_enabled() {
    let boot = build_int10_vbe_get_controller_info_boot_sector();

    let base_cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    };

    // Baseline: VGA/VBE path reports 16MiB (256 * 64KiB blocks).
    let mut legacy = Machine::new(base_cfg.clone()).unwrap();
    legacy.set_disk_image(boot.to_vec()).unwrap();
    legacy.reset();
    run_until_halt(&mut legacy);

    let info_addr = 0x2000u64 * 16 + 0x0100;
    let info = legacy.read_physical_bytes(info_addr, 512);
    assert_eq!(&info[0..4], b"VESA");
    let legacy_blocks = u16::from_le_bytes([info[18], info[19]]);
    assert_eq!(legacy_blocks, 256);

    // AeroGPU-enabled path should report the BAR1 VRAM aperture size instead of the legacy 16MiB.
    let mut aerogpu_cfg = base_cfg;
    // AeroGPU and the standalone VGA/VBE device are mutually exclusive in the canonical machine.
    aerogpu_cfg.enable_vga = false;
    aerogpu_cfg.enable_aerogpu = true;
    let mut aerogpu = Machine::new(aerogpu_cfg).unwrap();
    aerogpu.set_disk_image(boot.to_vec()).unwrap();
    aerogpu.reset();
    run_until_halt(&mut aerogpu);

    let info2 = aerogpu.read_physical_bytes(info_addr, 512);
    assert_eq!(&info2[0..4], b"VESA");
    let aerogpu_blocks = u16::from_le_bytes([info2[18], info2[19]]);
    let expected_blocks = AEROGPU_VRAM_SIZE
        .div_ceil(64 * 1024)
        .min(u64::from(u16::MAX)) as u16;
    assert_eq!(aerogpu_blocks, expected_blocks);
    assert!(aerogpu_blocks > legacy_blocks);
}
