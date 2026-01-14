use std::sync::Arc;

use aero_gpu_vga::SVGA_LFB_BASE;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_shared::scanout_state::{
    ScanoutState, ScanoutStateUpdate, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_VBE_LFB,
    SCANOUT_SOURCE_WDDM,
};
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
fn boot_sector_int10_vbe_sets_scanout_state_to_legacy_vbe_lfb() {
    let boot = build_int10_vbe_set_mode_boot_sector();

    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_scanout_state(Some(scanout_state.clone()));

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.base_paddr(), u64::from(SVGA_LFB_BASE));
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    assert_eq!(snap.pitch_bytes, 1024 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}

#[test]
fn boot_sector_int10_vbe_does_not_override_wddm_scanout_state() {
    let boot = build_int10_vbe_set_mode_boot_sector();

    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_scanout_state(Some(scanout_state.clone()));
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Simulate WDDM claiming scanout after the machine has reset.
    scanout_state.publish(ScanoutStateUpdate {
        source: SCANOUT_SOURCE_WDDM,
        base_paddr_lo: 0x1234,
        base_paddr_hi: 0,
        width: 800,
        height: 600,
        pitch_bytes: 800 * 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
    });
    let generation_before = scanout_state.snapshot().generation;

    run_until_halt(&mut m);

    // INT 10h legacy mode sets must not steal scanout after WDDM has taken ownership.
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.generation, generation_before);
}
