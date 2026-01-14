#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use std::sync::Arc;

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_shared::scanout_state::{ScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM};
use pretty_assertions::assert_eq;

fn build_vbe_mode_118_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // INT 10h AX=4F02: Set VBE mode 0x118 (1024x768x32bpp) with LFB requested.
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]); // mov ax, 0x4F02
    i += 3;
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]); // mov bx, 0x4118
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
fn aerogpu_snapshot_preserves_wddm_scanout_when_disabled() {
    // Use a real INT 10h path so the BIOS VBE state is non-trivial and the legacy scanout descriptor
    // is meaningful if WDDM ownership is lost.
    let boot = build_vbe_mode_118_boot_sector();

    let cfg = MachineConfig {
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
    };

    // Reuse the same shared scanout header across the snapshot restore to simulate the browser
    // runtime (host-owned shared memory persists while the VM device state is restored).
    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(cfg.clone()).unwrap();
    m.set_scanout_state(Some(scanout_state.clone()));
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Enable A20 so both PCI MMIO addresses and the WDDM scanout GPA do not alias below 1MiB.
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

    // Enable PCI bus mastering (required for scanout DMA semantics in the machine).
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

    // Claim WDDM scanout ownership, then disable scanout (visibility toggle).
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
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);

    // Disable scanout; ownership must remain WDDM and the shared scanout descriptor must remain in
    // the WDDM category (but marked disabled by zeroed geometry/base).
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        0,
    );
    m.process_aerogpu();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);

    let before = scanout_state.snapshot();
    assert_eq!(before.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(before.base_paddr(), 0);
    assert_eq!(before.width, 0);
    assert_eq!(before.height, 0);
    assert_eq!(before.pitch_bytes, 0);
    assert_eq!(before.format, SCANOUT_FORMAT_B8G8R8X8);

    let snap = m.take_snapshot_full().unwrap();

    // Restore into a fresh machine, but keep the shared scanout header alive across the restore to
    // catch regressions where `SCANOUT0_ENABLE=0` incorrectly clears the WDDM ownership latch.
    let mut m2 = Machine::new(cfg).unwrap();
    m2.set_scanout_state(Some(scanout_state.clone()));
    m2.set_disk_image(boot.to_vec()).unwrap();
    m2.restore_snapshot_bytes(&snap).unwrap();
    m2.process_aerogpu();

    assert_eq!(
        m2.active_scanout_source(),
        ScanoutSource::Wddm,
        "snapshot restore must preserve the WDDM ownership latch even when scanout is disabled"
    );

    let after = scanout_state.snapshot();
    assert_eq!(after.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(after.base_paddr(), 0);
    assert_eq!(after.width, 0);
    assert_eq!(after.height, 0);
    assert_eq!(after.pitch_bytes, 0);
    assert_eq!(after.format, SCANOUT_FORMAT_B8G8R8X8);
    assert_ne!(
        after.generation, before.generation,
        "restoring a snapshot should republish scanout state into the shared descriptor"
    );
}
