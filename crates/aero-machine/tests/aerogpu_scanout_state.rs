#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use std::sync::Arc;

use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};
use aero_protocol::aerogpu::aerogpu_pci as agpu_pci;
use aero_shared::scanout_state::{
    ScanoutState, ScanoutStateUpdate, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_TEXT,
    SCANOUT_SOURCE_LEGACY_VBE_LFB, SCANOUT_SOURCE_WDDM,
};
use pretty_assertions::assert_eq;

fn build_int10_vbe_set_mode_then_wait_then_text_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
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

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;

    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // Wait loop until word at 0x0500 becomes 0x1234.
    let loop_start = i;
    // mov ax, [0x0500]
    sector[i..i + 3].copy_from_slice(&[0xA1, 0x00, 0x05]);
    i += 3;
    // cmp ax, 0x1234
    sector[i..i + 3].copy_from_slice(&[0x3D, 0x34, 0x12]);
    i += 3;
    // jne loop_start (short jump; patch rel8)
    let jne_pos = i;
    sector[i..i + 2].copy_from_slice(&[0x75, 0x00]);
    i += 2;
    let rel = (loop_start as i32) - ((jne_pos + 2) as i32);
    sector[jne_pos + 1] = i8::try_from(rel).unwrap() as u8;

    // mov ax, 0x0003 (80x25 text mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x03, 0x00]);
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

fn build_int10_vbe_set_mode_then_hlt_boot_sector_for_mode(
    mode: u16,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, (mode + LFB requested)
    let bx = mode | 0x4000;
    let [bx_lo, bx_hi] = bx.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xBB, bx_lo, bx_hi]);
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

fn run_until_halt(m: &mut Machine, max_slices: usize) {
    for _ in 0..max_slices {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn aerogpu_scanout_state_transitions_from_legacy_to_wddm_and_is_sticky() {
    let boot = build_int10_vbe_set_mode_then_wait_then_text_boot_sector();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test deterministic/minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let scanout_state = Arc::new(ScanoutState::new());
    m.set_scanout_state(Some(scanout_state.clone()));

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Reset should publish legacy text.
    let snap0 = scanout_state.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_LEGACY_TEXT);
    assert_ne!(snap0.generation, 0, "reset should publish a scanout update");

    // Run until the guest has executed the VBE mode set and entered the wait loop. The scanout
    // state update is published by the INT 10h handler.
    for _ in 0..200 {
        match m.run_slice(10_000) {
            RunExit::Completed { .. } => {}
            RunExit::Halted { .. } => panic!("guest halted before reaching VBE wait loop"),
            other => panic!("unexpected exit: {other:?}"),
        }
        if scanout_state.snapshot().source == SCANOUT_SOURCE_LEGACY_VBE_LFB {
            break;
        }
    }

    // After VBE set mode, scanout should be legacy VBE LFB with base = BAR1 + VBE_LFB_OFFSET.
    let bdf = profile::AEROGPU.bdf;
    let bar0_base = m
        .pci_bar_base(bdf, profile::AEROGPU_BAR0_INDEX)
        .expect("BAR0 should be assigned");
    let bar1_base = m
        .pci_bar_base(bdf, profile::AEROGPU_BAR1_VRAM_INDEX)
        .expect("BAR1 should be assigned");
    let expected_lfb_base = bar1_base + VBE_LFB_OFFSET as u64;

    let snap1 = scanout_state.snapshot();
    assert_eq!(snap1.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap1.base_paddr(), expected_lfb_base);
    assert_eq!(snap1.width, 1024);
    assert_eq!(snap1.height, 768);
    assert_eq!(snap1.pitch_bytes, 4096);
    assert_eq!(snap1.format, SCANOUT_FORMAT_B8G8R8X8);

    // BIOS-reported VBE LFB base should match the BAR1-backed VRAM layout.
    let expected_lfb_base_u32 =
        u32::try_from(expected_lfb_base).expect("expected LFB base should fit in u32");
    assert_eq!(m.vbe_lfb_base(), expected_lfb_base);
    assert_eq!(m.vbe_lfb_base_u32(), expected_lfb_base_u32);

    // Simulate WDDM scanout enable via BAR0 regs.
    m.write_physical_u32(
        bar0_base + u64::from(agpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        1024,
    );
    m.write_physical_u32(
        bar0_base + u64::from(agpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        768,
    );
    m.write_physical_u32(
        bar0_base + u64::from(agpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        agpu_pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(agpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        4096,
    );
    m.write_physical_u32(
        bar0_base + u64::from(agpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        expected_lfb_base as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(agpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (expected_lfb_base >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(agpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );
    m.process_aerogpu();

    let snap2 = scanout_state.snapshot();
    assert_eq!(snap2.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap2.base_paddr(), expected_lfb_base);
    assert_eq!(snap2.width, 1024);
    assert_eq!(snap2.height, 768);
    assert_eq!(snap2.pitch_bytes, 4096);
    assert_eq!(snap2.format, SCANOUT_FORMAT_B8G8R8X8);

    // Allow the guest to resume and attempt to switch back to text mode via INT 10h. Once WDDM has
    // claimed scanout, legacy BIOS mode sets must not steal it back.
    let gen2 = snap2.generation;
    m.write_physical_u16(0x0500, 0x1234);
    run_until_halt(&mut m, 200);

    let snap3 = scanout_state.snapshot();
    assert_eq!(snap3.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap3.generation, gen2);
}

#[test]
fn aerogpu_scanout_state_wddm_mismatch_falls_back_to_legacy_text_for_8bpp_vbe() {
    let boot = build_int10_vbe_set_mode_then_hlt_boot_sector_for_mode(0x105);

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test deterministic/minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let scanout_state = Arc::new(ScanoutState::new());
    m.set_scanout_state(Some(scanout_state.clone()));

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m, 200);

    // Ensure the BIOS VBE mode is set (so this is *not* the "None => legacy text" fallback).
    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));

    // Simulate a shared scanout descriptor stuck in WDDM mode (e.g. after a reset/restore mismatch)
    // while the AeroGPU device model has not claimed WDDM scanout ownership.
    scanout_state.publish(ScanoutStateUpdate {
        source: SCANOUT_SOURCE_WDDM,
        base_paddr_lo: 0,
        base_paddr_hi: 0,
        width: 1,
        height: 1,
        pitch_bytes: 4,
        format: SCANOUT_FORMAT_B8G8R8X8,
    });

    let gen_before = scanout_state.snapshot().generation;
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_WDDM);

    m.process_aerogpu();

    let snap = scanout_state.snapshot();
    assert_ne!(snap.generation, gen_before);
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
}
