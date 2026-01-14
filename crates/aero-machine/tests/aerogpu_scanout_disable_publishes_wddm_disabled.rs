#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use std::sync::Arc;

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_shared::scanout_state::{
    ScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_VBE_LFB, SCANOUT_SOURCE_WDDM,
};
use pretty_assertions::assert_eq;

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

#[test]
fn aerogpu_scanout_disable_publishes_wddm_disabled_even_with_legacy_vbe_panning_and_stride() {
    // Use a scanline length that differs from the mode's default pitch (1024*4) so we can observe
    // that the BIOS publishes a legacy scanout descriptor that reflects `INT 10h AX=4F06`.
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
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();
    m.set_scanout_state(Some(scanout_state.clone()));
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // The BIOS should have published a legacy VBE scanout descriptor that includes the display
    // start offsets and the scanline length requested via AX=4F06 (clamped to at least the mode's
    // natural pitch).
    let bytes_per_pixel = 4u64;
    // INT 10h AX=4F06 BL=2 sets the logical scan line length in bytes. The BIOS preserves
    // byte-granular pitches but clamps them to at least the mode's natural pitch (1024*4).
    let expected_pitch = u64::from(bytes_per_scan_line).max(1024u64 * bytes_per_pixel);
    let expected_legacy_base =
        m.vbe_lfb_base() + u64::from(y_off) * expected_pitch + u64::from(x_off) * bytes_per_pixel;
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.width, 1024);
    assert_eq!(snap.height, 768);
    assert_eq!(snap.pitch_bytes, expected_pitch as u32);
    assert_eq!(snap.base_paddr(), expected_legacy_base);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);

    // Enable A20 so we can place the WDDM scanout buffer above 1MiB without aliasing.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let (bar0_base, command) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        (
            cfg.bar_range(profile::AEROGPU_BAR0_INDEX)
                .expect("AeroGPU BAR0 missing")
                .base,
            cfg.command(),
        )
    };

    // Enable PCI bus mastering (required for device-initiated scanout reads).
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(
            profile::AEROGPU.bdf,
            0x04,
            2,
            u32::from(command | (1 << 2)),
        );
    }

    // Claim WDDM scanout.
    let fb_gpa = 0x0020_0000u64;
    let width = 8u32;
    let height = 8u32;
    let pitch = width * 4;
    // Seed a single pixel (B,G,R,X = AA,BB,CC,00) so the config is non-trivial.
    m.write_physical_u32(fb_gpa, 0x00CC_BBAA);

    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        width,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        height,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    m.process_aerogpu();
    let snap_wddm = scanout_state.snapshot();
    assert_eq!(snap_wddm.source, SCANOUT_SOURCE_WDDM);
    let gen_wddm = snap_wddm.generation;

    // Explicitly disable WDDM scanout. This is treated as a visibility toggle: the shared scanout
    // descriptor remains WDDM but is marked disabled (width/height/pitch/base=0) so legacy VBE does
    // not steal scanout back.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        0,
    );
    m.process_aerogpu();

    let snap = scanout_state.snapshot();
    assert_ne!(snap.generation, gen_wddm);
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), 0);
    assert_eq!(snap.width, 0);
    assert_eq!(snap.height, 0);
    assert_eq!(snap.pitch_bytes, 0);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
}
