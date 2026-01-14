#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::USB_EHCI_ICH9;
use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_devices::usb::ehci::regs as ehci_regs;
use aero_devices::usb::uhci::{regs, UhciPciDevice};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};
use core::any::Any;

#[test]
fn uhci_tick_increments_frnum_when_running() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let bar4_base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(
        bar4_base, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Start the controller (USBCMD.RS).
    m.io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

    let before = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    m.tick_platform(1_000_000);
    let after = m.io_read(base + regs::REG_FRNUM, 2) as u16;

    assert_eq!(after, (before.wrapping_add(1)) & 0x07ff);
}

#[test]
fn uhci_portsc_reflects_device_attach_and_detach() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    let base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        let bar4_base = cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
        u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
    };

    let portsc_before = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_eq!(
        portsc_before & 0x0003,
        0,
        "PORTSC1 should start disconnected"
    );

    // Attach a built-in USB HID keyboard directly to UHCI root port 0.
    let keyboard = UsbHidKeyboardHandle::new();
    m.usb_attach_root(0, Box::new(keyboard))
        .expect("attach should succeed");

    let portsc_attached = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_ne!(
        portsc_attached & 0x0001,
        0,
        "CCS should be set after attach"
    );
    assert_ne!(
        portsc_attached & 0x0002,
        0,
        "CSC should be set after attach"
    );

    // Detach and ensure connect status clears but change bit remains latched.
    m.usb_detach_root(0).expect("detach should succeed");
    let portsc_detached = m.io_read(base + regs::REG_PORTSC1, 2) as u16;
    assert_eq!(portsc_detached & 0x0001, 0, "CCS should clear after detach");
    assert_ne!(portsc_detached & 0x0002, 0, "CSC should latch after detach");
}

#[test]
fn machine_synthetic_usb_hid_mouse_hwheel_injection_produces_report() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for this device-model test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic USB HID mouse handle should be present");

    assert_eq!(
        mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack,
        "mouse should accept SET_CONFIGURATION"
    );

    m.inject_usb_hid_mouse_hwheel(7);

    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0, 0, 0, 0, 7]);
}

#[test]
fn machine_synthetic_usb_hid_mouse_wheel_injection_produces_report() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for this device-model test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic USB HID mouse handle should be present");

    assert_eq!(
        mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack,
        "mouse should accept SET_CONFIGURATION"
    );

    m.inject_usb_hid_mouse_wheel(5);

    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0, 0, 0, 5, 0]);
}

#[test]
fn machine_synthetic_usb_hid_mouse_move_injection_produces_report() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for this device-model test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let mut mouse = m
        .usb_hid_mouse_handle()
        .expect("synthetic USB HID mouse handle should be present");

    assert_eq!(
        mouse.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack,
        "mouse should accept SET_CONFIGURATION"
    );

    m.inject_usb_hid_mouse_move(10, 5);

    let report = match mouse.handle_in_transfer(0x81, 5) {
        UsbInResult::Data(data) => data,
        other => panic!("expected mouse report data, got {other:?}"),
    };
    assert_eq!(report, vec![0, 10, 5, 0, 0]);
}

#[test]
fn machine_synthetic_usb_hid_consumer_injection_produces_report() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for this device-model test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let mut consumer = m
        .usb_hid_consumer_control_handle()
        .expect("synthetic USB HID consumer control handle should be present");

    assert_eq!(
        consumer.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack,
        "consumer control device should accept SET_CONFIGURATION"
    );

    // Usage Page 0x0C: Consumer. 0x00E9 = Volume Up.
    m.inject_usb_hid_consumer_usage(0x00e9, true);

    let report = match consumer.handle_in_transfer(0x81, 2) {
        UsbInResult::Data(data) => data,
        other => panic!("expected consumer report data, got {other:?}"),
    };
    assert_eq!(report, vec![0xe9, 0x00]);
}

#[test]
fn machine_synthetic_usb_hid_does_not_overwrite_root_port0_when_occupied() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_synthetic_usb_hid: true,
        // Keep the machine minimal/deterministic for this topology test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Replace the synthetic external hub (normally on root port 0) with a host-attached device.
    let uhci = m.uhci().expect("UHCI device should exist");
    uhci.borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));

    // Reset should not clobber host-attached devices on root port 0.
    m.reset();

    let uhci = m.uhci().expect("UHCI device should exist");
    let uhci_ref = uhci.borrow();
    let dev0 = uhci_ref
        .controller()
        .hub()
        .port_device(0)
        .expect("root port 0 should remain occupied across reset");

    assert!(
        (dev0.model() as &dyn Any).is::<UsbHidKeyboardHandle>(),
        "expected root port 0 to retain the host-attached device"
    );
    assert!(
        dev0.as_hub().is_none(),
        "expected root port 0 to not be replaced by the synthetic external hub"
    );
}

#[test]
fn uhci_tick_remainder_roundtrips_through_snapshot_restore() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    let bar4_base = {
        let pci_cfg = src
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(
        bar4_base, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Start the controller (USBCMD.RS).
    src.io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

    let before = src.io_read(base + regs::REG_FRNUM, 2) as u16;
    // Advance by half a frame; the controller should not increment FRNUM yet.
    src.tick_platform(500_000);
    let mid = src.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(mid, before);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let bar4_base_restored = {
        let pci_cfg = restored
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(
        bar4_base_restored, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base_restored =
        u16::try_from(bar4_base_restored).expect("UHCI BAR4 base should fit in u16");

    let after_restore = restored.io_read(base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(after_restore, before);

    // Advance by the remaining half-frame; if the machine's UHCI tick remainder is included in
    // snapshots, this should now increment FRNUM by 1.
    restored.tick_platform(500_000);
    let after_tick = restored.io_read(base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(after_tick, (before.wrapping_add(1)) & 0x07ff);
}

#[test]
fn usb_tick_remainders_roundtrip_with_uhci_and_ehci_enabled() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        enable_ehci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_ahci: false,
        enable_ide: false,
        enable_nvme: false,
        enable_virtio_blk: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    let pci_cfg = src
        .pci_config_ports()
        .expect("pc platform should expose pci_cfg");

    let (uhci_base, ehci_base) = {
        let mut pci_cfg = pci_cfg.borrow_mut();

        let bus = pci_cfg.bus_mut();

        let uhci_cfg = bus
            .device_config_mut(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        // Ensure I/O decoding is enabled so `io_read/io_write` reach the UHCI BAR.
        uhci_cfg.set_command(uhci_cfg.command() | 0x1);

        let uhci_bar4_base = uhci_cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
        assert_ne!(
            uhci_bar4_base, 0,
            "UHCI BAR4 base should be assigned by BIOS POST"
        );

        let ehci_cfg = bus
            .device_config_mut(USB_EHCI_ICH9.bdf)
            .expect("EHCI PCI function should exist");
        // Ensure MMIO decoding is enabled so `read_physical_u32/write_physical_u32` reach EHCI.
        ehci_cfg.set_command(ehci_cfg.command() | 0x2);
        let ehci_bar0_base = ehci_cfg.bar_range(0).map(|range| range.base).unwrap_or(0);
        assert_ne!(
            ehci_bar0_base, 0,
            "EHCI BAR0 base should be assigned by BIOS POST"
        );

        (
            u16::try_from(uhci_bar4_base).expect("UHCI BAR4 base should fit in u16"),
            ehci_bar0_base,
        )
    };

    // Start both controllers.
    src.io_write(uhci_base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));
    src.write_physical_u32(ehci_base + ehci_regs::REG_USBCMD, ehci_regs::USBCMD_RS);

    let uhci_frnum_before = src.io_read(uhci_base + regs::REG_FRNUM, 2) as u16;
    let ehci_frindex_before = src.read_physical_u32(ehci_base + ehci_regs::REG_FRINDEX);

    // Advance by half a frame; neither controller should advance its frame counter yet, but the
    // machine should retain the fractional remainder across snapshot restore.
    src.tick_platform(500_000);

    let uhci_frnum_mid = src.io_read(uhci_base + regs::REG_FRNUM, 2) as u16;
    let ehci_frindex_mid = src.read_physical_u32(ehci_base + ehci_regs::REG_FRINDEX);
    assert_eq!(uhci_frnum_mid, uhci_frnum_before);
    assert_eq!(ehci_frindex_mid, ehci_frindex_before);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let pci_cfg_restored = restored
        .pci_config_ports()
        .expect("pc platform should expose pci_cfg");
    let (uhci_base_restored, ehci_base_restored) = {
        let mut pci_cfg = pci_cfg_restored.borrow_mut();

        let uhci_cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        let uhci_bar4_base = uhci_cfg.bar_range(4).map(|range| range.base).unwrap_or(0);

        let ehci_cfg = pci_cfg
            .bus_mut()
            .device_config(USB_EHCI_ICH9.bdf)
            .expect("EHCI PCI function should exist");
        let ehci_bar0_base = ehci_cfg.bar_range(0).map(|range| range.base).unwrap_or(0);

        (
            u16::try_from(uhci_bar4_base).expect("UHCI BAR4 base should fit in u16"),
            ehci_bar0_base,
        )
    };

    let uhci_frnum_after_restore = restored.io_read(uhci_base_restored + regs::REG_FRNUM, 2) as u16;
    let ehci_frindex_after_restore =
        restored.read_physical_u32(ehci_base_restored + ehci_regs::REG_FRINDEX);
    assert_eq!(uhci_frnum_after_restore, uhci_frnum_before);
    assert_eq!(ehci_frindex_after_restore, ehci_frindex_before);

    // Advance by the remaining half-frame; both controllers should now advance by one 1ms tick.
    restored.tick_platform(500_000);

    let uhci_frnum_after_tick = restored.io_read(uhci_base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(
        uhci_frnum_after_tick,
        (uhci_frnum_before.wrapping_add(1)) & 0x07ff
    );

    let ehci_frindex_after_tick =
        restored.read_physical_u32(ehci_base_restored + ehci_regs::REG_FRINDEX);
    assert_eq!(
        ehci_frindex_after_tick,
        ehci_frindex_before.wrapping_add(8) & ehci_regs::FRINDEX_MASK
    );
}

struct LegacyUsbSnapshotSource {
    machine: Machine,
}

impl snapshot::SnapshotSource for LegacyUsbSnapshotSource {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotSource::snapshot_meta(&mut self.machine)
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::SnapshotSource::cpu_state(&self.machine)
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::SnapshotSource::mmu_state(&self.machine)
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        let mut devices = snapshot::SnapshotSource::device_states(&self.machine);
        let Some(pos) = devices
            .iter()
            .position(|device| device.id == snapshot::DeviceId::USB)
        else {
            return devices;
        };
        let Some(uhci) = self.machine.uhci() else {
            return devices;
        };

        let version = <UhciPciDevice as IoSnapshot>::DEVICE_VERSION;
        devices[pos] = snapshot::DeviceState {
            id: snapshot::DeviceId::USB,
            version: version.major,
            flags: version.minor,
            data: uhci.borrow().save_state(),
        };
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::SnapshotSource::disk_overlays(&self.machine)
    }

    fn ram_len(&self) -> usize {
        snapshot::SnapshotSource::ram_len(&self.machine)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        snapshot::SnapshotSource::read_ram(&self.machine, offset, buf)
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        snapshot::SnapshotSource::take_dirty_pages(&mut self.machine)
    }
}

#[test]
fn uhci_restore_accepts_legacy_deviceid_usb_payload_without_machine_remainder() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this IO/timer test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = LegacyUsbSnapshotSource {
        machine: Machine::new(cfg.clone()).unwrap(),
    };

    let bar4_base = {
        let pci_cfg = src
            .machine
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    assert_ne!(
        bar4_base, 0,
        "UHCI BAR4 base should be assigned by BIOS POST"
    );
    let base = u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16");

    // Start the controller (USBCMD.RS).
    src.machine
        .io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

    let before = src.machine.io_read(base + regs::REG_FRNUM, 2) as u16;
    // Advance by half a frame so the machine has a sub-ms UHCI remainder at snapshot time.
    src.machine.tick_platform(500_000);

    // Ensure our snapshot source really emitted the legacy payload (UHCP), not the canonical `USBC`
    // wrapper.
    let usb_state = snapshot::SnapshotSource::device_states(&src)
        .into_iter()
        .find(|d| d.id == snapshot::DeviceId::USB)
        .expect("USB device state should exist");
    assert_eq!(
        usb_state.data.get(8..12),
        Some(b"UHCP".as_slice()),
        "legacy snapshot should store UHCI PCI snapshot directly under DeviceId::USB"
    );

    let mut bytes = std::io::Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut bytes, &mut src, snapshot::SaveOptions::default()).unwrap();
    let bytes = bytes.into_inner();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&bytes).unwrap();

    let bar4_base_restored = {
        let pci_cfg = restored
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        cfg.bar_range(4).map(|range| range.base).unwrap_or(0)
    };
    let base_restored =
        u16::try_from(bar4_base_restored).expect("UHCI BAR4 base should fit in u16");

    let after_restore = restored.io_read(base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(after_restore, before);

    // Legacy snapshots did not include the machine's sub-ms tick remainder; we should start from a
    // deterministic default of 0 on restore.
    restored.tick_platform(500_000);
    let after_half_ms = restored.io_read(base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(after_half_ms, before);

    restored.tick_platform(500_000);
    let after_full_ms = restored.io_read(base_restored + regs::REG_FRNUM, 2) as u16;
    assert_eq!(after_full_ms, (before.wrapping_add(1)) & 0x07ff);
}

#[test]
fn uhci_snapshot_restore_roundtrips_controller_state() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_uhci: true,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_UHCI_PIIX3.bdf)
            .expect("UHCI PCI function should exist");
        let bar4_base = cfg.bar_range(4).map(|range| range.base).unwrap_or(0);
        u16::try_from(bar4_base).expect("UHCI BAR4 base should fit in u16")
    };

    // Start the controller and advance a handful of frames.
    m.io_write(base + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));
    let frnum_start = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    for _ in 0..5 {
        m.tick_platform(1_000_000);
    }
    let frnum_before = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_before, (frnum_start.wrapping_add(5)) & 0x07ff);

    let snap = m.take_snapshot_full().expect("take_snapshot_full");

    // Mutate state after snapshot so we can observe restoration. Tick 1.5ms so we advance a full
    // frame and leave a 0.5ms remainder in the machine.
    m.tick_platform(1_500_000);
    let frnum_mutated = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_mutated, (frnum_before.wrapping_add(1)) & 0x07ff);

    m.restore_snapshot_bytes(&snap).expect("restore_snapshot");

    let frnum_restored = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_restored, frnum_before);

    // Ensure snapshot restore does not preserve stale host-side sub-ms UHCI tick remainder.
    m.tick_platform(500_000);
    let frnum_after_half_ms = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_after_half_ms, frnum_before);

    // Ticking should continue from the restored value.
    m.tick_platform(500_000);
    let frnum_after = m.io_read(base + regs::REG_FRNUM, 2) as u16;
    assert_eq!(frnum_after, frnum_mutated);
}
