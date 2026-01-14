#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::{AEROGPU, AEROGPU_BAR1_VRAM_INDEX};
use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};
use pretty_assertions::assert_eq;
use std::sync::Arc;

use aero_shared::scanout_state::{ScanoutState, SCANOUT_SOURCE_LEGACY_VBE_LFB};

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

fn build_int10_vbe_103_set_mode_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4103 (mode 0x103 + linear framebuffer requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x03, 0x41]);
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

fn build_vbe_mode_118_with_stride_and_display_start_boot_sector(
    bytes_per_scan_line: u16,
    x_off: u16,
    y_off: u16,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // INT 10h AX=4F02: Set VBE mode 0x118 (1024x768x32bpp) with LFB requested.
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]); // mov ax, 0x4F02
    i += 3;
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]); // mov bx, 0x4118
    i += 3;
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]); // int 0x10
    i += 2;

    // INT 10h AX=4F06: Set logical scan line length in bytes (BL=0x02).
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x06, 0x4F]); // mov ax, 0x4F06
    i += 3;
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x02, 0x00]); // mov bx, 0x0002
    i += 3;
    sector[i] = 0xB9; // mov cx, imm16
    sector[i + 1..i + 3].copy_from_slice(&bytes_per_scan_line.to_le_bytes());
    i += 3;
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]); // int 0x10
    i += 2;

    // INT 10h AX=4F07: Set display start (panning).
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x07, 0x4F]); // mov ax, 0x4F07
    i += 3;
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x00]); // mov bx, 0x0000 (BL=0 set)
    i += 3;
    sector[i] = 0xB9; // mov cx, imm16
    sector[i + 1..i + 3].copy_from_slice(&x_off.to_le_bytes());
    i += 3;
    sector[i] = 0xBA; // mov dx, imm16
    sector[i + 1..i + 3].copy_from_slice(&y_off.to_le_bytes());
    i += 3;
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]); // int 0x10
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
fn aerogpu_scanout_reads_from_bar1_vram_without_mmio_reads_during_present() {
    // Validate the AeroGPU VRAM scanout fast-path:
    // - BIOS sets a VBE mode with LFB at BAR1_BASE + VBE_LFB_OFFSET,
    // - guest writes a pixel into the LFB via BAR1,
    // - `display_present` reads directly from the device's VRAM backing store (no BAR1 MMIO reads).
    let boot = build_int10_vbe_115_set_mode_boot_sector();
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal and deterministic for a focused test.
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    let bar1_base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(AEROGPU.bdf)
            .expect("AeroGPU PCI function should exist");
        cfg.bar_range(AEROGPU_BAR1_VRAM_INDEX)
            .map(|range| range.base)
            .unwrap_or(0)
    };
    assert_ne!(
        bar1_base, 0,
        "AeroGPU BAR1 base should be assigned by BIOS POST"
    );

    run_until_halt(&mut m);

    // The AeroGPU VBE LFB begins at BAR1_BASE + VBE_LFB_OFFSET.
    let lfb_base = bar1_base + VBE_LFB_OFFSET as u64;

    // Write a red pixel at (0,0) in VBE packed-pixel B,G,R,X format.
    m.write_physical_u32(lfb_base, 0x00FF_0000);

    let reads_before = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");

    m.display_present();

    assert_eq!(m.display_resolution(), (800, 600));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);

    // Critical assertion: present should have used the direct VRAM fast-path (no BAR1 reads routed
    // through the MMIO bus).
    let reads_after = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");
    assert_eq!(reads_before, reads_after);
}

#[test]
fn aerogpu_vbe_lfb_fastpath_honors_panning_and_stride() {
    // Ensure the VRAM present fast-path honors both:
    // - the VBE scanline length override (AX=4F06, set in bytes), and
    // - VBE display start (panning) offsets.
    //
    // We write distinct sentinel pixels at the addresses corresponding to several common bugs:
    // - ignoring panning (base = lfb_base),
    // - ignoring x offset,
    // - ignoring y offset,
    // - ignoring stride (using row_bytes instead of bytes_per_scan_line).
    //
    // Then we assert the (0,0) output pixel matches only the correct base computation.
    let bytes_per_scan_line = 4101u16;
    let x_off = 1u16;
    let y_off = 4u16;
    let boot = build_vbe_mode_118_with_stride_and_display_start_boot_sector(
        bytes_per_scan_line,
        x_off,
        y_off,
    );

    let scanout_state = Arc::new(ScanoutState::new());
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal and deterministic for a focused test.
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();
    m.set_scanout_state(Some(scanout_state.clone()));

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Ensure physical addresses above 1MiB are not masked (BAR1 is typically above 1MiB).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let lfb_base = m.vbe_lfb_base();

    // Use the published legacy VBE scanout descriptor to determine the effective base/pitch after
    // the BIOS calls. This keeps the test robust even if VBE helpers clamp stride/panning.
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    let pitch = u64::from(snap.pitch_bytes);
    let row_bytes = u64::from(snap.width) * 4;
    assert_ne!(
        pitch, row_bytes,
        "test requires non-default stride to distinguish pitch vs row_bytes"
    );

    let base_correct = snap.base_paddr();
    let delta = base_correct
        .checked_sub(lfb_base)
        .expect("legacy base should be >= lfb_base");
    let y_off_eff = delta / pitch;
    let x_bytes = delta % pitch;
    assert_eq!(
        x_bytes % 4,
        0,
        "display start X offset must be 4-byte aligned"
    );
    let x_off_eff = x_bytes / 4;

    let base_no_y = lfb_base + x_off_eff * 4;
    let base_no_x = lfb_base + y_off_eff * pitch;
    let base_wrong_pitch = lfb_base + y_off_eff * row_bytes + x_off_eff * 4;

    // Write sentinel pixels (VBE packed-pixel B,G,R,X format).
    m.write_physical_u32(lfb_base, 0x0000_FF00); // green: ignore both offsets
    m.write_physical_u32(base_no_y, 0x00FF_FF00); // yellow: ignore y
    m.write_physical_u32(base_no_x, 0x00FF_00FF); // magenta: ignore x
    m.write_physical_u32(base_wrong_pitch, 0x0000_00FF); // blue: ignore stride
    m.write_physical_u32(base_correct, 0x00FF_0000); // red: correct base

    let reads_before = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");

    m.display_present();

    // The mode is 1024x768x32bpp (VBE 0x118).
    assert_eq!(m.display_resolution(), (1024, 768));
    // Top-left pixel must match the red sentinel written at the correctly computed base.
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);

    // Present must not have routed any BAR1 reads through the MMIO bus.
    let reads_after = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");
    assert_eq!(reads_before, reads_after);
}

#[test]
fn aerogpu_vbe_8bpp_vram_fastpath_uses_dac_palette_and_avoids_mmio_reads() {
    // Validate that the VRAM fast-path works for 8bpp VBE modes as well:
    // - set a VBE 8bpp mode with LFB in BAR1 VRAM
    // - program a known VGA DAC palette entry
    // - write a palette index into the LFB
    // - assert the rendered pixel matches the palette and no BAR1 MMIO reads occurred during
    //   present.
    let boot = build_int10_vbe_103_set_mode_boot_sector();
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal and deterministic for a focused test.
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    let bar1_base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(AEROGPU.bdf)
            .expect("AeroGPU PCI function should exist");
        cfg.bar_range(AEROGPU_BAR1_VRAM_INDEX)
            .map(|range| range.base)
            .unwrap_or(0)
    };
    assert_ne!(bar1_base, 0);

    run_until_halt(&mut m);

    // Program palette entry 0 = pure red (255,0,0) via VGA DAC ports.
    // DAC write index port: 0x3C8, DAC data: 0x3C9.
    m.io_write(0x3C8, 1, 0); // index 0
    m.io_write(0x3C9, 1, 0xFF); // R
    m.io_write(0x3C9, 1, 0x00); // G
    m.io_write(0x3C9, 1, 0x00); // B

    let lfb_base = bar1_base + VBE_LFB_OFFSET as u64;
    // Pixel (0,0) = palette index 0.
    m.write_physical_u8(lfb_base, 0);

    let reads_before = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");

    m.display_present();

    assert_eq!(m.display_resolution(), (800, 600));
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);

    let reads_after = m
        .aerogpu_bar1_mmio_read_count()
        .expect("AeroGPU should expose a BAR1 MMIO read counter");
    assert_eq!(reads_before, reads_after);
}
