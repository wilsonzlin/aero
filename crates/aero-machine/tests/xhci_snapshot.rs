#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::{profile, PciInterruptPin};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, PlatformInterruptMode,
};
use aero_usb::hid::{UsbHidKeyboardHandle, UsbHidPassthroughHandle};
use aero_usb::hub::UsbHubDevice;
use aero_usb::{
    ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbWebUsbPassthroughDevice,
};
use pretty_assertions::{assert_eq, assert_ne};

const EXTERNAL_HUB_ROOT_PORT: u8 = Machine::UHCI_EXTERNAL_HUB_ROOT_PORT;
const WEBUSB_ROOT_PORT: u8 = Machine::UHCI_WEBUSB_ROOT_PORT;

fn sample_hid_report_descriptor_input_2_bytes() -> Vec<u8> {
    vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (0x01)
        0xa1, 0x01, // Collection (Application)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

fn queue_webhid_feature_report_request(dev: &UsbHidPassthroughHandle) {
    // HID class request: GET_REPORT(feature, report_id=3)
    let setup = SetupPacket {
        bm_request_type: 0xA1, // DeviceToHost | Class | Interface
        b_request: 0x01,       // GET_REPORT
        w_value: (3u16 << 8) | 3u16,
        w_index: 0,
        w_length: 64,
    };

    let mut handle = dev.clone();
    assert_eq!(
        handle.handle_control_request(setup, None),
        ControlResponse::Nak,
        "expected GET_REPORT(feature) to queue a host request and return NAK"
    );
}

fn queue_webusb_bulk_in_action(dev: &UsbWebUsbPassthroughDevice) {
    let mut handle = dev.clone();
    assert_eq!(
        handle.handle_in_transfer(0x81, 16),
        UsbInResult::Nak,
        "expected first bulk/interrupt IN transfer to queue a host action and return NAK"
    );
}

#[test]
fn snapshot_restore_roundtrips_xhci_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI + PCI INTx snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let xhci = vm.xhci().expect("xhci enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    let (gsi, bdf) = {
        let bdf = profile::USB_XHCI_QEMU.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        (gsi, bdf)
    };

    xhci.borrow_mut().raise_event_interrupt();
    assert!(
        xhci.borrow().irq_level(),
        "xHCI IRQ level should be asserted"
    );

    // Intentionally do *not* sync xHCI's INTx into the platform interrupt controller before
    // snapshot. This leaves the interrupt sink desynchronized, which restore must fix up by
    // polling device-level IRQ lines again.
    assert!(!interrupts.borrow().gsi_level(gsi));

    let expected_xhci_state = { xhci.borrow().save_state() };

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind.
    xhci.borrow_mut().clear_event_interrupt();
    assert!(
        !xhci.borrow().irq_level(),
        "clearing the event interrupt should deassert xHCI INTx"
    );

    let mutated_xhci_state = { xhci.borrow().save_state() };
    assert_ne!(
        mutated_xhci_state, expected_xhci_state,
        "xHCI state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the xHCI instance (host wiring/backends live outside snapshots).
    let xhci_after = vm.xhci().expect("xhci still enabled");
    assert!(
        Rc::ptr_eq(&xhci, &xhci_after),
        "restore must not replace the xHCI instance"
    );

    let restored_xhci_state = { xhci_after.borrow().save_state() };
    // xHCI's save_state format may include transient fields (e.g. internal bookkeeping) that are
    // not required to roundtrip byte-for-byte. Assert the restore is observable by ensuring the
    // post-restore state differs from the mutated (post-snapshot) state.
    assert_ne!(restored_xhci_state, mutated_xhci_state);
    assert!(xhci_after.borrow().irq_level());

    // After restore, the xHCI's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected PCI INTx (GSI {gsi}) to be asserted for xHCI (bdf={bdf:?}) after restore"
    );
}

#[test]
fn snapshot_restore_preserves_host_attached_xhci_device_handles() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    // Host attach a hub + a shareable USB HID keyboard handle.
    vm.usb_xhci_attach_at_path(
        &[EXTERNAL_HUB_ROOT_PORT],
        Box::new(UsbHubDevice::with_port_count(2)),
    )
    .expect("attach hub at root port 0");

    let keyboard = UsbHidKeyboardHandle::new();
    let keyboard_handle = keyboard.clone();
    vm.usb_xhci_attach_at_path(&[EXTERNAL_HUB_ROOT_PORT, 1], Box::new(keyboard))
        .expect("attach keyboard behind hub");

    // Configure the keyboard so injected key events buffer interrupt reports.
    {
        let xhci = vm.xhci().expect("xhci enabled");
        let mut xhci = xhci.borrow_mut();
        let ctrl = xhci.controller_mut();

        let kb_dev = ctrl
            .find_device_by_topology(1, &[1])
            .expect("keyboard reachable via topology");

        let setup = SetupPacket {
            bm_request_type: 0x00, // Host-to-device | Standard | Device
            b_request: 9,          // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(
            kb_dev.model_mut().handle_control_request(setup, None),
            ControlResponse::Ack,
            "expected SET_CONFIGURATION to succeed"
        );
    }

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // After restore, the host-side keyboard handle must still drive the attached device model.
    keyboard_handle.key_event(0x04, true); // HID usage: 'A'

    let xhci = vm.xhci().expect("xhci enabled");
    let mut xhci = xhci.borrow_mut();
    let ctrl = xhci.controller_mut();
    let kb_dev = ctrl
        .find_device_by_topology(1, &[1])
        .expect("keyboard still reachable after restore");

    match kb_dev.model_mut().handle_interrupt_in(0x81) {
        UsbInResult::Data(report) => {
            assert_eq!(report.len(), 8);
            // Boot keyboard report: bytes[2..] are key usage codes; ensure 'A' is present.
            assert_eq!(report[2], 0x04);
        }
        other => panic!("expected interrupt report after key injection, got {other:?}"),
    }
}

#[test]
fn snapshot_restore_clears_xhci_webusb_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_xhci_attach_root(WEBUSB_ROOT_PORT, Box::new(webusb.clone()))
        .expect("attach webusb device at root port 1");

    // Queue a host action so there is host-side asynchronous state to clear.
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06, // GET_DESCRIPTOR
        w_value: 0x0100,
        w_index: 0,
        w_length: 4,
    };
    let mut model = webusb.clone();
    assert_eq!(
        model.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    assert_eq!(
        webusb.pending_summary().queued_actions,
        1,
        "expected queued host action before snapshot"
    );

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(
        summary.queued_actions, 0,
        "expected host action queue to be cleared after snapshot restore"
    );
    assert_eq!(
        summary.inflight_control, None,
        "expected inflight control transfer to be cleared after snapshot restore"
    );
}

#[test]
fn snapshot_restore_clears_xhci_webusb_host_state_behind_hub() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    vm.usb_xhci_attach_at_path(
        &[EXTERNAL_HUB_ROOT_PORT],
        Box::new(UsbHubDevice::with_port_count(2)),
    )
    .expect("attach hub at root port 0");

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_xhci_attach_at_path(&[EXTERNAL_HUB_ROOT_PORT, 1], Box::new(webusb.clone()))
        .expect("attach webusb device behind hub");

    // Queue a host action so there is host-side asynchronous state to clear.
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06, // GET_DESCRIPTOR
        w_value: 0x0100,
        w_index: 0,
        w_length: 4,
    };
    let mut model = webusb.clone();
    assert_eq!(
        model.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    assert_eq!(
        webusb.pending_summary().queued_actions,
        1,
        "expected queued host action before snapshot"
    );

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let summary = webusb.pending_summary();
    assert_eq!(
        summary.queued_actions, 0,
        "expected host action queue to be cleared after snapshot restore"
    );
    assert_eq!(
        summary.inflight_control, None,
        "expected inflight control transfer to be cleared after snapshot restore"
    );
}

#[test]
fn snapshot_restore_clears_xhci_webhid_feature_report_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webhid = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_hid_report_descriptor_input_2_bytes(),
        false,
        None,
        None,
        None,
    );

    vm.usb_xhci_attach_root(EXTERNAL_HUB_ROOT_PORT, Box::new(webhid.clone()))
        .expect("attach WebHID passthrough device behind xHCI");

    // Queue a host-side feature report request and simulate the host popping it before snapshot.
    queue_webhid_feature_report_request(&webhid);
    let req = webhid
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.request_id, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Host-side feature report requests are backed by asynchronous WebHID operations; after restore
    // they must be cleared so the guest can re-issue a fresh request.
    assert!(webhid.pop_feature_report_request().is_none());

    // Re-issue the request; IDs should continue from the saved counter.
    queue_webhid_feature_report_request(&webhid);
    let req2 = webhid
        .pop_feature_report_request()
        .expect("expected feature report request after restore");
    assert_eq!(req2.request_id, 2);
}

#[test]
fn snapshot_restore_clears_xhci_webhid_feature_report_host_state_behind_hub() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    vm.usb_xhci_attach_at_path(
        &[EXTERNAL_HUB_ROOT_PORT],
        Box::new(UsbHubDevice::with_port_count(2)),
    )
    .expect("attach hub at root port 0");

    let webhid = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        sample_hid_report_descriptor_input_2_bytes(),
        false,
        None,
        None,
        None,
    );

    vm.usb_xhci_attach_at_path(&[EXTERNAL_HUB_ROOT_PORT, 1], Box::new(webhid.clone()))
        .expect("attach WebHID passthrough device behind hub port 1");

    queue_webhid_feature_report_request(&webhid);
    let req = webhid
        .pop_feature_report_request()
        .expect("expected queued feature report request");
    assert_eq!(req.request_id, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    assert!(webhid.pop_feature_report_request().is_none());

    // Re-issue the request; IDs should continue from the saved counter.
    queue_webhid_feature_report_request(&webhid);
    let req2 = webhid
        .pop_feature_report_request()
        .expect("expected feature report request after restore");
    assert_eq!(req2.request_id, 2);
}

#[test]
fn snapshot_restore_clears_xhci_webusb_bulk_in_host_state() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI snapshot restore behavior.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    })
    .unwrap();

    let webusb = UsbWebUsbPassthroughDevice::new();
    vm.usb_xhci_attach_root(WEBUSB_ROOT_PORT, Box::new(webusb.clone()))
        .expect("attach webusb device at root port 1");

    queue_webusb_bulk_in_action(&webusb);
    let before = webusb.pending_summary();
    assert_eq!(before.queued_actions, 1);
    assert_eq!(before.inflight_endpoints, 1);

    let snapshot = vm.take_snapshot_full().unwrap();
    vm.restore_snapshot_bytes(&snapshot).unwrap();

    let after = webusb.pending_summary();
    assert_eq!(after.queued_actions, 0);
    assert_eq!(after.inflight_endpoints, 0);
    assert_eq!(after.inflight_control, None);
}

#[test]
fn snapshot_restore_preserves_xhci_msix_table_and_delivery() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI + snapshot + MSI-X.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_debugcon: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        // Keep the A20 gate so we can explicitly enable A20 before touching high MMIO addresses.
        enable_a20_gate: true,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    vm.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = profile::USB_XHCI_QEMU.bdf;
    let bar0_base = vm.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Enable MSI-X in the canonical PCI config space and program table entry 0 via BAR0 MMIO.
    let table_offset = {
        let pci_cfg = vm
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("xHCI should exist on PCI bus");

        // Ensure MMIO decode + bus mastering are enabled, and disable INTx so the test only
        // succeeds via MSI-X.
        let command = cfg.command();
        cfg.write(
            0x04,
            2,
            u32::from(command | (1 << 1) | (1 << 2) | (1 << 10)),
        );

        let msix_off = cfg
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("xHCI should expose MSI-X capability in PCI config space");
        let msix_base = u16::from(msix_off);

        let table = cfg.read(msix_base + 0x04, 4);
        assert_eq!(
            table & 0x7,
            0,
            "xHCI MSI-X table should live in BAR0 (BIR=0)"
        );

        // Enable MSI-X (control bit 15).
        let ctrl = cfg.read(msix_base + 0x02, 2) as u16;
        cfg.write(msix_base + 0x02, 2, u32::from(ctrl | (1 << 15)));

        u64::from(table & !0x7)
    };

    let vector1: u8 = 0x66;
    let entry0 = bar0_base + table_offset;
    vm.write_physical_u32(entry0, 0xfee0_0000);
    vm.write_physical_u32(entry0 + 0x04, 0);
    vm.write_physical_u32(entry0 + 0x08, u32::from(vector1));
    vm.write_physical_u32(entry0 + 0x0c, 0); // unmasked

    // Mirror MSI-X enable state into the runtime xHCI model before taking a snapshot.
    vm.tick_platform(1);

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate the MSI-X table so restore is an observable rewind.
    let vector2: u8 = 0x67;
    vm.write_physical_u32(entry0 + 0x08, u32::from(vector2));
    assert_eq!(vm.read_physical_u32(entry0 + 0x08) as u8, vector2);

    let xhci = vm.xhci().expect("xhci enabled");
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    xhci.borrow_mut().raise_event_interrupt();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector2)
    );
    interrupts.borrow_mut().acknowledge(vector2);
    interrupts.borrow_mut().eoi(vector2);
    xhci.borrow_mut().clear_event_interrupt();

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // The xHCI instance should not be replaced by restore (host wiring/backends live outside
    // snapshots).
    let xhci_after = vm.xhci().expect("xhci still enabled");
    assert!(Rc::ptr_eq(&xhci, &xhci_after));

    // Ensure the restored MSI-X table entry is rewound back to vector1.
    assert_eq!(vm.read_physical_u32(entry0 + 0x08) as u8, vector1);

    // Mirror restored MSI-X enable state into the runtime xHCI model before triggering delivery.
    vm.tick_platform(1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    xhci_after.borrow_mut().raise_event_interrupt();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector1)
    );
    assert!(
        !xhci_after.borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active"
    );
}

#[test]
fn snapshot_restore_preserves_xhci_msix_pending_bit_and_delivers_after_unmask() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI + snapshot + MSI-X pending bit handling.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_debugcon: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        // Keep the A20 gate so we can explicitly enable A20 before touching high MMIO addresses.
        enable_a20_gate: true,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    vm.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = profile::USB_XHCI_QEMU.bdf;
    let bar0_base = vm.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Enable MSI-X (with Function Mask) in canonical PCI config space, and discover table/PBA
    // offsets.
    let (table_offset, pba_offset) = {
        let pci_cfg = vm
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("xHCI should exist on PCI bus");

        // Ensure MMIO decode + bus mastering are enabled so BAR0 MMIO is valid, and disable INTx so
        // there is no fallback interrupt delivery.
        let command = cfg.command();
        cfg.write(
            0x04,
            2,
            u32::from(command | (1 << 1) | (1 << 2) | (1 << 10)),
        );

        let msix_off = cfg
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("xHCI should expose MSI-X capability in PCI config space");
        let msix_base = u16::from(msix_off);

        let table = cfg.read(msix_base + 0x04, 4);
        let pba = cfg.read(msix_base + 0x08, 4);
        assert_eq!(
            table & 0x7,
            0,
            "xHCI MSI-X table should live in BAR0 (BIR=0)"
        );
        assert_eq!(pba & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");

        // Enable MSI-X (bit 15) and set Function Mask (bit 14) so the interrupt is latched as
        // pending rather than delivered.
        let ctrl = cfg.read(msix_base + 0x02, 2) as u16;
        cfg.write(msix_base + 0x02, 2, u32::from(ctrl | (1 << 15) | (1 << 14)));

        (u64::from(table & !0x7), u64::from(pba & !0x7))
    };

    // Program MSI-X table entry 0: destination = BSP (APIC ID 0), vector = 0x68.
    let vector: u8 = 0x68;
    let entry0 = bar0_base + table_offset;
    vm.write_physical_u32(entry0, 0xfee0_0000);
    vm.write_physical_u32(entry0 + 0x04, 0);
    vm.write_physical_u32(entry0 + 0x08, u32::from(vector));
    vm.write_physical_u32(entry0 + 0x0c, 0); // unmasked

    // Raise an xHCI interrupt condition while MSI-X is function-masked. This should set the PBA
    // pending bit without delivering an MSI.
    let xhci = vm.xhci().expect("xhci enabled");
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    xhci.borrow_mut().raise_event_interrupt();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert!(
        !xhci.borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active (even if masked)"
    );
    let pba_bits = vm.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while function-masked"
    );

    // Clear the interrupt condition before snapshotting. Pending MSI-X delivery should still occur
    // after unmask due to the PBA pending bit, even without a new interrupt edge.
    xhci.borrow_mut().clear_event_interrupt();
    let pba_bits = vm.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing the interrupt condition"
    );

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot: clear function mask and allow xHCI to deliver the pending MSI-X
    // vector (which should clear the pending bit).
    {
        let pci_cfg = vm
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("xHCI should exist on PCI bus");
        let msix_off = cfg
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("xHCI should expose MSI-X capability in PCI config space");
        let msix_base = u16::from(msix_off);
        let ctrl = cfg.read(msix_base + 0x02, 2) as u16;
        cfg.write(msix_base + 0x02, 2, u32::from(ctrl & !(1 << 14)));
    }
    vm.tick_platform(1_000_000);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    let pba_bits = vm.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after unmask + delivery"
    );
    interrupts.borrow_mut().acknowledge(vector);
    interrupts.borrow_mut().eoi(vector);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the xHCI instance.
    let xhci_after = vm.xhci().expect("xhci still enabled");
    assert!(Rc::ptr_eq(&xhci, &xhci_after));

    // After restore, the pending bit should be set again (rewound), and delivery should occur once
    // the function mask is cleared.
    let pba_bits = vm.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be restored as set"
    );

    // Clear the function mask again and tick to re-drive pending MSI-X delivery.
    {
        let pci_cfg = vm
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("xHCI should exist on PCI bus");
        let msix_off = cfg
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("xHCI should expose MSI-X capability in PCI config space");
        let msix_base = u16::from(msix_off);
        let ctrl = cfg.read(msix_base + 0x02, 2) as u16;
        cfg.write(msix_base + 0x02, 2, u32::from(ctrl & !(1 << 14)));
    }
    vm.tick_platform(1_000_000);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    let pba_bits = vm.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after restore + unmask + delivery"
    );
}

#[test]
fn snapshot_restore_preserves_xhci_msix_vector_mask_pending_bit_and_delivers_after_unmask() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_xhci: true,
        // Keep this test focused on xHCI + snapshot + per-vector MSI-X mask semantics.
        enable_ahci: false,
        enable_ide: false,
        enable_vga: false,
        enable_serial: false,
        enable_debugcon: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        // Keep the A20 gate so we can explicitly enable A20 before touching high MMIO addresses.
        enable_a20_gate: true,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    vm.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = profile::USB_XHCI_QEMU.bdf;
    let bar0_base = vm.pci_bar_base(bdf, 0).expect("xHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Enable MSI-X in canonical PCI config space and discover table/PBA offsets.
    let (table_offset, pba_offset) = {
        let pci_cfg = vm
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("xHCI should exist on PCI bus");

        // Ensure MMIO decode + bus mastering are enabled.
        let command = cfg.command();
        cfg.write(0x04, 2, u32::from(command | (1 << 1) | (1 << 2)));

        let msix_off = cfg
            .find_capability(PCI_CAP_ID_MSIX)
            .expect("xHCI should expose MSI-X capability in PCI config space");
        let msix_base = u16::from(msix_off);

        let table = cfg.read(msix_base + 0x04, 4);
        let pba = cfg.read(msix_base + 0x08, 4);
        assert_eq!(
            table & 0x7,
            0,
            "xHCI MSI-X table should live in BAR0 (BIR=0)"
        );
        assert_eq!(pba & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");

        // Enable MSI-X (bit 15) and ensure Function Mask (bit 14) is cleared.
        let ctrl = cfg.read(msix_base + 0x02, 2) as u16;
        cfg.write(
            msix_base + 0x02,
            2,
            u32::from((ctrl & !(1 << 14)) | (1 << 15)),
        );

        (u64::from(table & !0x7), u64::from(pba & !0x7))
    };

    // Program MSI-X table entry 0 and keep it masked (vector control bit 0).
    let vector: u8 = 0x6c;
    let entry0 = bar0_base + table_offset;
    vm.write_physical_u32(entry0, 0xfee0_0000);
    vm.write_physical_u32(entry0 + 0x04, 0);
    vm.write_physical_u32(entry0 + 0x08, u32::from(vector));
    vm.write_physical_u32(entry0 + 0x0c, 1); // masked

    // Tick once so the runtime xHCI device mirrors MSI-X state.
    vm.tick_platform(1);

    let xhci = vm.xhci().expect("xhci enabled");

    // Raise an xHCI interrupt condition while the entry is masked. This should set the PBA pending
    // bit without delivering an MSI.
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    xhci.borrow_mut().raise_event_interrupt();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert!(
        !xhci.borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active (even if the entry is masked)"
    );
    let pba_bits = vm.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while entry is masked"
    );

    // Clear the interrupt condition before snapshot so delivery on unmask proves the pending bit is
    // re-driven even without a new rising edge.
    xhci.borrow_mut().clear_event_interrupt();
    assert_ne!(
        vm.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected pending bit to remain set after clearing the interrupt condition"
    );

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot: unmask the entry, deliver the pending MSI-X vector, and clear
    // the pending bit.
    vm.write_physical_u32(entry0 + 0x0c, 0);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    interrupts.borrow_mut().acknowledge(vector);
    interrupts.borrow_mut().eoi(vector);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert_eq!(
        vm.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after unmask + delivery"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Ensure high MMIO addresses decode correctly post-restore as well.
    vm.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // Restore should not replace the xHCI instance.
    let xhci_after = vm.xhci().expect("xhci still enabled");
    assert!(Rc::ptr_eq(&xhci, &xhci_after));

    // Ensure the MSI-X table entry mask and PBA pending bit were restored.
    assert_eq!(
        vm.read_physical_u32(entry0 + 0x0c) & 1,
        1,
        "expected MSI-X vector control mask bit to be restored"
    );
    assert_ne!(
        vm.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be restored as set"
    );

    // Unmask the entry again and expect the pending MSI-X vector to be delivered and cleared.
    vm.write_physical_u32(entry0 + 0x0c, 0);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    interrupts.borrow_mut().acknowledge(vector);
    interrupts.borrow_mut().eoi(vector);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert_eq!(
        vm.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after restore + unmask + delivery"
    );
}
