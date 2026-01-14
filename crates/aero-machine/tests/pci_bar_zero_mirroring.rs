#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciDevice};
use aero_devices::usb::ehci::EhciPciDevice;
use aero_devices::usb::uhci::UhciPciDevice;
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn machine_process_ahci_mirrors_bar5_when_guest_clears_it_to_zero() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let ahci = vm.ahci().expect("ahci enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::SATA_AHCI_ICH9.bdf;

    // BIOS POST must assign a non-zero base address to BAR5 (ABAR).
    let abar_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("AHCI config function must exist");
        cfg.bar_range(profile::AHCI_ABAR_BAR_INDEX)
            .expect("AHCI BAR5 must exist")
            .base
    };
    assert_ne!(abar_base, 0, "expected AHCI BAR5 base to be assigned");

    // Sync once so the device model observes the assigned BAR5 base.
    vm.process_ahci();
    {
        let dev = ahci.borrow();
        assert_eq!(
            dev.config()
                .bar_range(profile::AHCI_ABAR_BAR_INDEX)
                .unwrap()
                .base,
            abar_base
        );
    }

    // Now simulate a guest unassigning BAR5 by programming it to 0.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .write_config(bdf, u16::from(profile::AHCI_ABAR_CFG_OFFSET), 4, 0);
    }

    // Re-sync: BAR5 base=0 must still be mirrored into the device model (BAR-present-with-base=0).
    vm.process_ahci();
    {
        let dev = ahci.borrow();
        assert_eq!(
            dev.config()
                .bar_range(profile::AHCI_ABAR_BAR_INDEX)
                .unwrap()
                .base,
            0
        );
    }
}

#[test]
fn machine_tick_platform_mirrors_uhci_bar4_when_guest_clears_it_to_zero() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let uhci = vm.uhci().expect("uhci enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::USB_UHCI_PIIX3.bdf;
    let bar = UhciPciDevice::IO_BAR_INDEX;
    let bar_cfg_offset = 0x10u16 + u16::from(bar) * 4;

    // BIOS POST must assign a non-zero base address to BAR4 (UHCI I/O window).
    let bar4_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("UHCI config function must exist");
        cfg.bar_range(bar).expect("UHCI BAR4 must exist").base
    };
    assert_ne!(bar4_base, 0, "expected UHCI BAR4 base to be assigned");

    // Sync once so the device model observes the assigned BAR4 base.
    vm.tick_platform(1);
    {
        let dev = uhci.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar4_base);
    }

    // Now simulate a guest unassigning BAR4 by programming it to 0.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, bar_cfg_offset, 4, 0);
    }

    // Re-sync: BAR4 base=0 must still be mirrored into the device model (BAR-present-with-base=0).
    vm.tick_platform(1);
    {
        let mut dev = uhci.borrow_mut();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, 0);
        // I/O BARs must still expose bit0=1 to indicate an I/O BAR even when the base is 0.
        assert_eq!(dev.config_mut().read(bar_cfg_offset, 4), 0x1);
    }
}

#[test]
fn machine_tick_platform_mirrors_ehci_bar0_when_guest_clears_it_to_zero() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let ehci = vm.ehci().expect("ehci enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::USB_EHCI_ICH9.bdf;
    let bar = EhciPciDevice::MMIO_BAR_INDEX;
    let bar_cfg_offset = 0x10u16 + u16::from(bar) * 4;

    // BIOS POST must assign a non-zero base address to BAR0 (EHCI MMIO window).
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("EHCI config function must exist");
        cfg.bar_range(bar).expect("EHCI BAR0 must exist").base
    };
    assert_ne!(bar0_base, 0, "expected EHCI BAR0 base to be assigned");

    // Sync once so the device model observes the assigned BAR0 base.
    vm.tick_platform(1);
    {
        let dev = ehci.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar0_base);
    }

    // Now simulate a guest unassigning BAR0 by programming it to 0.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, bar_cfg_offset, 4, 0);
    }

    // Re-sync: BAR0 base=0 must still be mirrored into the device model (BAR-present-with-base=0).
    vm.tick_platform(1);
    {
        let dev = ehci.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, 0);
    }
}

#[test]
fn machine_process_ide_mirrors_bar4_when_guest_clears_it_to_zero() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let ide = vm.ide().expect("ide enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::IDE_PIIX3.bdf;

    let bar = 4u8;
    let bar_cfg_offset = 0x10u16 + u16::from(bar) * 4;

    // BIOS POST must assign a non-zero base address to BAR4 (Bus Master IDE I/O window).
    let bar4_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("IDE config function must exist");
        cfg.bar_range(bar).expect("IDE BAR4 must exist").base
    };
    assert_ne!(bar4_base, 0, "expected IDE BAR4 base to be assigned");

    // Sync once so the device model observes the assigned BAR4 base.
    vm.process_ide();
    {
        let dev = ide.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar4_base);
    }

    // Now simulate a guest unassigning BAR4 by programming it to 0.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, bar_cfg_offset, 4, 0);
    }

    // Re-sync: BAR4 base=0 must still be mirrored into the device model (BAR-present-with-base=0).
    vm.process_ide();
    {
        let mut dev = ide.borrow_mut();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, 0);
        // I/O BARs must still expose bit0=1 to indicate an I/O BAR even when the base is 0.
        assert_eq!(dev.config_mut().read(bar_cfg_offset, 4), 0x1);
    }
}

#[test]
fn snapshot_device_states_mirrors_virtio_input_keyboard_bar0_when_guest_clears_it_to_zero() {
    let vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let keyboard = vm.virtio_input_keyboard().expect("virtio input enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;

    let bar = 0u8;
    let bar_cfg_offset = 0x10u16 + u16::from(bar) * 4;

    // BIOS POST must assign a non-zero base address to BAR0 (virtio-pci MMIO window).
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("virtio-input keyboard config function must exist");
        cfg.bar_range(bar)
            .expect("virtio-input keyboard BAR0 must exist")
            .base
    };
    assert_ne!(
        bar0_base, 0,
        "expected virtio-input keyboard BAR0 base to be assigned"
    );

    // Ensure the device model observes the assigned BAR0 base before we clear it.
    {
        let dev = keyboard.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar0_base);
    }

    // Simulate a guest unassigning BAR0 by programming it to 0. Virtio BAR0 is 64-bit, so clear
    // both halves.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, bar_cfg_offset, 4, 0);
        pci_cfg
            .bus_mut()
            .write_config(bdf, bar_cfg_offset + 4, 4, 0);
    }

    // Snapshotting calls `SnapshotSource::device_states`, which must mirror BAR0 base=0 into the
    // virtio-input device model so snapshots don't preserve stale BAR bases.
    let _ = <Machine as aero_snapshot::SnapshotSource>::device_states(&vm);
    {
        let dev = keyboard.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, 0);
    }
}

#[test]
fn snapshot_device_states_mirrors_virtio_net_bar0_when_guest_clears_it_to_zero() {
    let vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let virtio_net = vm.virtio_net().expect("virtio net enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::VIRTIO_NET.bdf;

    let bar = 0u8;
    let bar_cfg_offset = 0x10u16 + u16::from(bar) * 4;

    // BIOS POST must assign a non-zero base address to BAR0 (virtio-pci MMIO window).
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("virtio-net config function must exist");
        cfg.bar_range(bar).expect("virtio-net BAR0 must exist").base
    };
    assert_ne!(bar0_base, 0, "expected virtio-net BAR0 base to be assigned");

    // Simulate the device model having a stale, non-zero BAR0 base (e.g. carried over from a
    // previous platform sync) so we can assert snapshotting overwrites it with the canonical PCI
    // config view.
    {
        let mut dev = virtio_net.borrow_mut();
        dev.config_mut().set_bar_base(bar, bar0_base);
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar0_base);
    }

    // Simulate a guest unassigning BAR0 by programming it to 0. Virtio BAR0 is 64-bit, so clear
    // both halves.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, bar_cfg_offset, 4, 0);
        pci_cfg
            .bus_mut()
            .write_config(bdf, bar_cfg_offset + 4, 4, 0);
    }

    // Snapshotting calls `SnapshotSource::device_states`, which must mirror BAR0 base=0 into the
    // virtio-net device model so snapshots don't preserve stale BAR bases.
    let _ = <Machine as aero_snapshot::SnapshotSource>::device_states(&vm);
    {
        let dev = virtio_net.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, 0);
    }
}

#[test]
fn snapshot_device_states_mirrors_e1000_bar0_when_guest_clears_it_to_zero() {
    let vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_e1000: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let e1000 = vm.e1000().expect("e1000 enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::NIC_E1000_82540EM.bdf;

    // BIOS POST must assign non-zero base addresses to BAR0 (MMIO) + BAR1 (I/O).
    let (bar0_base, bar1_base) = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("E1000 config function must exist");
        (
            cfg.bar_range(0).expect("E1000 BAR0 must exist").base,
            cfg.bar_range(1).expect("E1000 BAR1 must exist").base,
        )
    };
    assert_ne!(bar0_base, 0, "expected E1000 BAR0 base to be assigned");
    assert_ne!(bar1_base, 0, "expected E1000 BAR1 base to be assigned");

    // Ensure the device model observes the assigned BAR0 base before we clear it.
    {
        let dev = e1000.borrow();
        assert_eq!(dev.pci_config_read(0x10, 4), u32::try_from(bar0_base).unwrap());
        assert_eq!(
            dev.pci_config_read(0x14, 4),
            u32::try_from(bar1_base).unwrap() | 0x1
        );
    }

    // Simulate a guest unassigning BAR0/BAR1 by programming them to 0.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, 0x10, 4, 0);
        pci_cfg.bus_mut().write_config(bdf, 0x14, 4, 0);
    }

    // The E1000 device model maintains its own PCI config image; ensure it is still stale until we
    // take a snapshot (which should perform mirroring).
    {
        let dev = e1000.borrow();
        assert_eq!(dev.pci_config_read(0x10, 4), u32::try_from(bar0_base).unwrap());
        assert_eq!(
            dev.pci_config_read(0x14, 4),
            u32::try_from(bar1_base).unwrap() | 0x1
        );
    }

    // Snapshotting calls `SnapshotSource::device_states`, which must mirror BAR0 base=0 into the
    // E1000 device model so snapshots don't preserve stale BAR bases.
    let _ = <Machine as aero_snapshot::SnapshotSource>::device_states(&vm);
    {
        let dev = e1000.borrow();
        assert_eq!(dev.pci_config_read(0x10, 4), 0);
        // BAR1 is an I/O BAR, so bit0 must remain set even when the base is 0.
        assert_eq!(dev.pci_config_read(0x14, 4), 0x1);
    }
}

#[test]
fn snapshot_device_states_mirrors_nvme_bar0_when_guest_clears_it_to_zero() {
    let vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let nvme = vm.nvme().expect("nvme enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::NVME_CONTROLLER.bdf;

    let bar = 0u8;
    let bar_cfg_offset = 0x10u16 + u16::from(bar) * 4;

    // BIOS POST must assign a non-zero base address to BAR0 (NVMe MMIO window).
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("NVMe config function must exist");
        cfg.bar_range(bar).expect("NVMe BAR0 must exist").base
    };
    assert_ne!(bar0_base, 0, "expected NVMe BAR0 base to be assigned");

    // Ensure the device model observes the assigned BAR0 base before we clear it.
    {
        let dev = nvme.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar0_base);
    }

    // Simulate a guest unassigning BAR0 by programming it to 0. NVMe BAR0 is 64-bit, so clear
    // both halves.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, bar_cfg_offset, 4, 0);
        pci_cfg
            .bus_mut()
            .write_config(bdf, bar_cfg_offset + 4, 4, 0);
    }

    // Ensure the device model is still stale until snapshot.
    {
        let dev = nvme.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar0_base);
    }

    // Snapshotting must mirror BAR0 base=0 into the NVMe device model.
    let _ = <Machine as aero_snapshot::SnapshotSource>::device_states(&vm);
    {
        let dev = nvme.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, 0);
    }
}

#[test]
fn snapshot_device_states_mirrors_virtio_blk_bar0_when_guest_clears_it_to_zero() {
    let vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep this test focused on PCI config <-> device model mirroring.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let virtio_blk = vm.virtio_blk().expect("virtio-blk enabled");
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let bdf = profile::VIRTIO_BLK.bdf;

    let bar = 0u8;
    let bar_cfg_offset = 0x10u16 + u16::from(bar) * 4;

    // BIOS POST must assign a non-zero base address to BAR0 (virtio-pci MMIO window).
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("virtio-blk config function must exist");
        cfg.bar_range(bar).expect("virtio-blk BAR0 must exist").base
    };
    assert_ne!(bar0_base, 0, "expected virtio-blk BAR0 base to be assigned");

    // Ensure the device model observes the assigned BAR0 base before we clear it.
    {
        let dev = virtio_blk.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar0_base);
    }

    // Simulate a guest unassigning BAR0 by programming it to 0. Virtio BAR0 is 64-bit, so clear
    // both halves.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, bar_cfg_offset, 4, 0);
        pci_cfg
            .bus_mut()
            .write_config(bdf, bar_cfg_offset + 4, 4, 0);
    }

    // Ensure the device model is still stale until snapshot.
    {
        let dev = virtio_blk.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, bar0_base);
    }

    // Snapshotting must mirror BAR0 base=0 into the virtio-blk device model.
    let _ = <Machine as aero_snapshot::SnapshotSource>::device_states(&vm);
    {
        let dev = virtio_blk.borrow();
        assert_eq!(dev.config().bar_range(bar).unwrap().base, 0);
    }
}
