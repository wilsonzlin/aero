#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use std::sync::Arc;

use aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_shared::scanout_state::{
    ScanoutState, ScanoutStateUpdate, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_TEXT,
    SCANOUT_SOURCE_LEGACY_VBE_LFB, SCANOUT_SOURCE_WDDM,
};
use pretty_assertions::assert_eq;

fn build_int10_vbe_set_mode_boot_sector_for_mode(mode: u16) -> [u8; 512] {
    let mut sector = [0u8; 512];
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

fn build_int10_vbe_set_mode_boot_sector() -> [u8; 512] {
    build_int10_vbe_set_mode_boot_sector_for_mode(0x118)
}

fn build_int10_vbe_set_mode_then_text_mode_boot_sector() -> [u8; 512] {
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

    // mov ax, 0x0003 (INT 10h AH=00h Set Video Mode, AL=03h 80x25 text)
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

fn build_int10_vbe_set_mode_and_display_start_boot_sector(x_off: u16, y_off: u16) -> [u8; 512] {
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

    // mov ax, 0x4F07 (VBE Set/Get Display Start)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x07, 0x4F]);
    i += 3;

    // xor bx, bx (BL=0 set)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;

    // mov cx, x_off
    sector[i] = 0xB9;
    sector[i + 1..i + 3].copy_from_slice(&x_off.to_le_bytes());
    i += 3;

    // mov dx, y_off
    sector[i] = 0xBA;
    sector[i + 1..i + 3].copy_from_slice(&y_off.to_le_bytes());
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

fn build_int10_vbe_set_mode_stride_bytes_and_display_start_boot_sector(
    bytes_per_scan_line: u16,
    x_off: u16,
    y_off: u16,
) -> [u8; 512] {
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

    // mov ax, 0x4F06 (VBE Set/Get Logical Scan Line Length)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x06, 0x4F]);
    i += 3;

    // mov bx, 0x0002 (BL=2 set in bytes)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x02, 0x00]);
    i += 3;

    // mov cx, bytes_per_scan_line
    sector[i] = 0xB9;
    sector[i + 1..i + 3].copy_from_slice(&bytes_per_scan_line.to_le_bytes());
    i += 3;

    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0x4F07 (VBE Set/Get Display Start)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x07, 0x4F]);
    i += 3;

    // xor bx, bx (BL=0 set)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;

    // mov cx, x_off
    sector[i] = 0xB9;
    sector[i + 1..i + 3].copy_from_slice(&x_off.to_le_bytes());
    i += 3;

    // mov dx, y_off
    sector[i] = 0xBA;
    sector[i + 1..i + 3].copy_from_slice(&y_off.to_le_bytes());
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
    let lfb_base: u32 = 0xD000_0000;

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(lfb_base),
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

    assert_eq!(m.vbe_lfb_base(), u64::from(lfb_base));

    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.base_paddr(), m.vbe_lfb_base());
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    assert_eq!(snap.pitch_bytes, 1024 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}

#[test]
fn boot_sector_int10_vbe_sets_scanout_state_to_legacy_vbe_lfb_with_derived_base() {
    // Like `boot_sector_int10_vbe_sets_scanout_state_to_legacy_vbe_lfb`, but configure the legacy
    // LFB base via the derived VRAM layout knobs:
    //   lfb_base = vga_vram_bar_base + vga_lfb_offset
    //
    // Use an *unaligned* derived base so the machine must mask it down to the PCI BAR-sized
    // alignment (and the scanout state must reflect the aligned base).
    let lfb_offset: u32 = VBE_FRAMEBUFFER_OFFSET as u32; // 256KiB
    let requested_vram_bar_base: u32 = 0xCFFC_1000;
    let derived_lfb_base = requested_vram_bar_base.wrapping_add(lfb_offset);
    assert_eq!(derived_lfb_base, 0xD000_1000);

    let boot = build_int10_vbe_set_mode_boot_sector();
    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_vram_bar_base: Some(requested_vram_bar_base),
        vga_lfb_offset: Some(lfb_offset),
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

    let vga = m.vga().expect("machine should have a VGA device");
    let bar_size_bytes: u32 = vga
        .borrow()
        .vram_size()
        .try_into()
        .expect("VRAM size fits in u32");
    assert!(bar_size_bytes.is_power_of_two());
    let expected_aligned_base = derived_lfb_base & !(bar_size_bytes - 1);

    assert_eq!(m.vbe_lfb_base(), u64::from(expected_aligned_base));

    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.base_paddr(), m.vbe_lfb_base());
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    assert_eq!(snap.pitch_bytes, 1024 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}

#[test]
fn boot_sector_int10_vbe_8bpp_mode_falls_back_to_legacy_text_scanout() {
    // The shared scanout descriptor/presentation path only supports 32bpp packed-pixel scanout
    // surfaces today. For palettized VBE modes (8bpp), the legacy VGA renderer path must be used
    // instead.
    let boot = build_int10_vbe_set_mode_boot_sector_for_mode(0x105);

    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_scanout_state(Some(scanout_state.clone()));

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    let generation_before = scanout_state.snapshot().generation;

    run_until_halt(&mut m);

    let snap = scanout_state.snapshot();
    assert_ne!(snap.generation, generation_before);
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
    assert_eq!(snap.base_paddr(), 0);
    assert_eq!(snap.width, 0);
    assert_eq!(snap.height, 0);
    assert_eq!(snap.pitch_bytes, 0);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}

#[test]
fn boot_sector_int10_vbe_then_text_mode_sets_scanout_state_to_legacy_text() {
    let boot = build_int10_vbe_set_mode_then_text_mode_boot_sector();

    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
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
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
    assert_eq!(snap.base_paddr(), 0);
    assert_eq!(snap.width, 0);
    assert_eq!(snap.height, 0);
    assert_eq!(snap.pitch_bytes, 0);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}

#[test]
fn boot_sector_int10_vbe_does_not_override_wddm_scanout_state() {
    let boot = build_int10_vbe_set_mode_boot_sector();

    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
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

#[test]
fn boot_sector_int10_vbe_display_start_updates_scanout_state_base() {
    let boot = build_int10_vbe_set_mode_and_display_start_boot_sector(1, 0);

    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
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
    assert_eq!(snap.base_paddr(), m.vbe_lfb_base() + 4);
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    assert_eq!(snap.pitch_bytes, 1024 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}

#[test]
fn boot_sector_int10_vbe_scanline_bytes_and_display_start_update_scanout_state() {
    let bytes_per_scan_line = 4101u16;
    let x_off = 1u16;
    let y_off = 4u16;
    let boot = build_int10_vbe_set_mode_stride_bytes_and_display_start_boot_sector(
        bytes_per_scan_line,
        x_off,
        y_off,
    );

    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
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
    let bytes_per_pixel = 4u64;
    // INT 10h AX=4F06 BL=2 sets the logical scan line length in bytes. The BIOS preserves
    // byte-granular pitches but clamps them to at least the mode's natural pitch (1024*4).
    let expected_pitch = u64::from(bytes_per_scan_line).max(1024u64 * bytes_per_pixel);

    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(
        snap.base_paddr(),
        m.vbe_lfb_base() + u64::from(y_off) * expected_pitch + u64::from(x_off) * bytes_per_pixel
    );
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    assert_eq!(snap.pitch_bytes, expected_pitch as u32);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}
