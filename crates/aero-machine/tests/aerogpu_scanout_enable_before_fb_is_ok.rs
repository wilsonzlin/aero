#![cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]

use std::sync::Arc;

use aero_machine::{Machine, MachineConfig};

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_protocol::aerogpu::aerogpu_pci;
use aero_shared::scanout_state::{ScanoutState, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_WDDM};

use aero_machine::RunExit;
use aero_shared::scanout_state::{SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_VBE_LFB};

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
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

fn build_vbe_mode_118_with_stride_and_display_start_boot_sector(
    bytes_per_scan_line: u16,
    x_off: u16,
    y_off: u16,
) -> [u8; 512] {
    let mut sector = [0u8; 512];
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
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x00]); // mov bx, 0x0000
    i += 3;
    sector[i] = 0xB9; // mov cx, imm16
    sector[i + 1..i + 3].copy_from_slice(&x_off.to_le_bytes());
    i += 3;
    sector[i] = 0xBA; // mov dx, imm16
    sector[i + 1..i + 3].copy_from_slice(&y_off.to_le_bytes());
    i += 3;
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]); // int 0x10
    i += 2;

    sector[i] = 0xF4; // hlt

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn aerogpu_scanout_enable_before_fb_is_ok() {
    // Keep the machine small and deterministic for a unit test while still including the PCI bus
    // and the canonical AeroGPU PCI identity.
    let cfg = MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).expect("Machine::new should succeed");
    let scanout_state = Arc::new(ScanoutState::new());
    m.set_scanout_state(Some(scanout_state.clone()));

    // Seed a visible legacy VGA text cell so we can detect accidental handoff to a blank WDDM
    // scanout.
    m.write_physical_u16(0xB8000, 0x1F41); // 'A' with bright attribute
    m.display_present();

    let legacy_res = m.display_resolution();
    assert_eq!(legacy_res, (720, 400));
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_LEGACY_TEXT);

    // Locate BAR0 and enable bus mastering (DMA) so scanout reads behave like a real PCI device.
    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        // WDDM scanout reads behave like device-initiated DMA; require PCI Bus Master Enable (BME)
        // so `display_present` can legally read the scanout buffer.
        cfg.set_command(cfg.command() | (1 << 2));
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };

    let w = 2u32;
    let h = 2u32;
    let pitch = w * 4;

    // ---------------------------------------------------------------------
    // 1) Win7 KMD behavior: enable scanout while FB_GPA=0 during early init.
    // ---------------------------------------------------------------------
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64,
        w,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64,
        h,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64,
        aerogpu_pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64,
        pitch,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64,
        0,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64,
        0,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64,
        1,
    );

    // Run the AeroGPU device tick so scanout state updates are published.
    m.process_aerogpu();

    // Scanout must *not* hand off to WDDM yet, since FB_GPA=0 is not a valid config.
    m.display_present();
    assert_eq!(m.display_resolution(), legacy_res);
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_LEGACY_TEXT);

    // ---------------------------------------------------------------------
    // 2) Later: driver programs a real framebuffer address while enable stays 1.
    // ---------------------------------------------------------------------
    let fb_gpa: u64 = 0x0008_0000;
    let fb_data: [u8; 16] = [
        // row 0: red, green
        0x00, 0x00, 0xFF, 0x00, // B,G,R,X
        0x00, 0xFF, 0x00, 0x00, //
        // row 1: blue, white
        0xFF, 0x00, 0x00, 0x00, //
        0xFF, 0xFF, 0xFF, 0x00, //
    ];
    m.write_physical(fb_gpa, &fb_data);

    // Update only the framebuffer pointer; `SCANOUT0_ENABLE` remains 1.
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64,
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64,
        (fb_gpa >> 32) as u32,
    );

    // Run the device tick to publish the now-valid scanout config.
    m.process_aerogpu();

    m.display_present();

    assert_eq!(m.display_resolution(), (w, h));
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_WDDM);

    let expected = [
        u32::from_le_bytes([0xFF, 0x00, 0x00, 0xFF]), // red
        u32::from_le_bytes([0x00, 0xFF, 0x00, 0xFF]), // green
        u32::from_le_bytes([0x00, 0x00, 0xFF, 0xFF]), // blue
        u32::from_le_bytes([0xFF, 0xFF, 0xFF, 0xFF]), // white
    ];
    assert_eq!(m.display_framebuffer(), expected.as_slice());

    // Once WDDM is active, legacy VGA writes must not steal scanout back.
    m.write_physical_u16(0xB8000, 0x2E42); // 'B'
    m.display_present();
    assert_eq!(m.display_resolution(), (w, h));
    assert_eq!(m.display_framebuffer(), expected.as_slice());

    // Explicit scanout disable should release WDDM scanout ownership and return us to legacy.
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64,
        0,
    );
    m.process_aerogpu();
    m.display_present();
    assert_eq!(m.display_resolution(), legacy_res);
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_LEGACY_TEXT);
}

#[test]
fn aerogpu_scanout_disable_reverts_to_legacy_vbe_scanout_with_panning_stride() {
    let bytes_per_scan_line = 4101u16;
    let x_off = 1u16;
    let y_off = 4u16;
    let boot = build_vbe_mode_118_with_stride_and_display_start_boot_sector(
        bytes_per_scan_line,
        x_off,
        y_off,
    );

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep output deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    let scanout_state = Arc::new(ScanoutState::new());
    m.set_scanout_state(Some(scanout_state.clone()));

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Sanity check: after the BIOS VBE calls, legacy scanout should already reflect panning/stride.
    let lfb_base = u64::from(m.vbe_lfb_base());
    let expected_base = lfb_base
        + u64::from(y_off) * u64::from(bytes_per_scan_line)
        + u64::from(x_off) * 4;

    let snap0 = scanout_state.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap0.base_paddr(), expected_base);
    assert_eq!(snap0.width, 1024);
    assert_eq!(snap0.height, 768);
    assert_eq!(snap0.pitch_bytes, u32::from(bytes_per_scan_line));
    assert_eq!(snap0.format, SCANOUT_FORMAT_B8G8R8X8);

    enable_a20(&mut m);

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };

    // Claim WDDM scanout, then explicitly disable it.
    let w = 64u32;
    let h = 64u32;
    let pitch = w * 4;
    let fb_gpa: u64 = 0x0010_0000;

    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64, w);
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64, h);
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64,
        pitch,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64,
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64,
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64,
        aerogpu_pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64, 1);
    m.process_aerogpu();
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_WDDM);

    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64, 0);
    m.process_aerogpu();

    let snap1 = scanout_state.snapshot();
    assert_eq!(snap1.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap1.base_paddr(), expected_base);
    assert_eq!(snap1.width, 1024);
    assert_eq!(snap1.height, 768);
    assert_eq!(snap1.pitch_bytes, u32::from(bytes_per_scan_line));
}
