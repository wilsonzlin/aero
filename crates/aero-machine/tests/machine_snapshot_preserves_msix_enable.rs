#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::{profile, MsixCapability, PciDevice as _};
use aero_machine::{Machine, MachineConfig};

#[test]
fn machine_snapshot_preserves_virtio_blk_msix_enable_and_mask() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the machine minimal and deterministic for a focused snapshot regression test.
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_uhci: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Sanity: runtime virtio-blk MSI-X starts disabled.
    let src_virtio_blk = src.virtio_blk().expect("virtio-blk should be enabled");
    {
        let src_msix_enabled = src_virtio_blk
            .borrow()
            .config()
            .capability::<MsixCapability>()
            .expect("virtio-blk should have MSI-X capability")
            .enabled();
        assert!(
            !src_msix_enabled,
            "test setup: expected runtime virtio-blk MSI-X to be disabled before programming canonical config"
        );
    }

    // Enable MSI-X (and function mask) in the *canonical* PCI config space owned by the machine.
    let bdf = profile::VIRTIO_BLK.bdf;
    let pci_cfg = src
        .pci_config_ports()
        .expect("pc platform should provide PCI config ports");
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("virtio-blk should exist on PCI bus");

        // Some older topologies may not expose MSI-X in the canonical config device. If missing,
        // add it so we can validate the snapshot mirroring logic for the split canonical/runtime
        // config case.
        let msix_offset = cfg.find_capability(PCI_CAP_ID_MSIX).unwrap_or_else(|| {
            // Virtio-pci MSI-X table+PBA are located in BAR0 after the device-specific config
            // window. Match the runtime virtio-pci layout (`aero_virtio::pci`).
            cfg.add_capability(Box::new(MsixCapability::new(
                /* table_size */ 2, /* table_bir */ 0, /* table_offset */ 0x3100,
                /* pba_bir */ 0, /* pba_offset */ 0x3120,
            )))
        });

        let ctrl_off = u16::from(msix_offset) + 0x02;
        let ctrl = cfg.read(ctrl_off, 2) as u16;
        cfg.write(ctrl_off, 2, u32::from(ctrl | (1 << 15) | (1 << 14)));
    }

    let snapshot = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snapshot).unwrap();

    let virtio_blk = restored.virtio_blk().expect("virtio-blk should be enabled");
    let (enabled, function_masked) = {
        let dev = virtio_blk.borrow();
        let msix = dev
            .config()
            .capability::<MsixCapability>()
            .expect("restored runtime virtio-blk should have MSI-X capability");
        (msix.enabled(), msix.function_masked())
    };

    assert!(enabled, "expected restored virtio-blk MSI-X to be enabled");
    assert!(
        function_masked,
        "expected restored virtio-blk MSI-X function mask bit to be preserved"
    );
}
