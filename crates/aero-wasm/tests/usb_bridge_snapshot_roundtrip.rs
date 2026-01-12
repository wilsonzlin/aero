#![cfg(target_arch = "wasm32")]

use aero_usb::passthrough::{
    PendingSummary, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};
use aero_wasm::{UhciControllerBridge, WebUsbUhciBridge};
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

// UHCI register offsets / bits (mirrors `crates/aero-usb/src/uhci/regs.rs`).
const REG_USBCMD: u16 = 0x00;
const REG_USBSTS: u16 = 0x02;
const REG_USBINTR: u16 = 0x04;
const REG_FRNUM: u16 = 0x06;
const REG_FRBASEADD: u16 = 0x08;
const REG_PORTSC1: u16 = 0x10;

const USBCMD_RUN: u16 = 1 << 0;
const USBSTS_USBINT: u16 = 1 << 0;
const USBINTR_IOC: u16 = 1 << 2;
const PORTSC_PED: u16 = 1 << 2;
const PORTSC_PR: u16 = 1 << 9;

// UHCI link pointer bits.
const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

// UHCI TD control/token fields.
const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_ACTLEN_MASK: u32 = 0x7FF;

const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

// PCI command register bit: Bus Master Enable (BME).
//
// The WASM UHCI bridges gate DMA/ticking on BME so that devices can't read/write guest RAM unless
// the guest has explicitly enabled bus mastering via PCI config space.
const PCI_COMMAND_BME: u32 = 1 << 2;

struct SimpleInDevice {
    payload: Vec<u8>,
}

impl SimpleInDevice {
    fn new(payload: &[u8]) -> Self {
        Self {
            payload: payload.to_vec(),
        }
    }
}

impl UsbDeviceModel for SimpleInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        if ep != 0x81 {
            return UsbInResult::Nak;
        }
        let mut data = self.payload.clone();
        if data.len() > max_len {
            data.truncate(max_len);
        }
        UsbInResult::Data(data)
    }
}

#[wasm_bindgen_test]
fn uhci_controller_bridge_snapshot_roundtrip_preserves_irq_and_registers() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);

    let mut ctrl = UhciControllerBridge::new(guest_base, guest_size).unwrap();
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    ctrl.set_pci_command(PCI_COMMAND_BME);
    ctrl.connect_device_for_test(0, Box::new(SimpleInDevice::new(b"ABCD")));

    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PED as u32);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);
    ctrl.io_write(REG_FRBASEADD, 4, 0x1000);

    unsafe {
        for i in 0..1024u32 {
            common::write_u32(guest_base + 0x1000 + i * 4, LINK_PTR_T);
        }
        common::write_u32(guest_base + 0x1000, 0x2000 | LINK_PTR_Q);

        common::write_u32(guest_base + 0x2000, LINK_PTR_T);
        common::write_u32(guest_base + 0x2004, 0x3000);

        let maxlen_field = (4u32 - 1) << TD_TOKEN_MAXLEN_SHIFT;
        let token = 0x69u32 | maxlen_field | (1 << TD_TOKEN_ENDPT_SHIFT); // IN, addr0/ep1
        common::write_u32(guest_base + 0x3000, LINK_PTR_T);
        common::write_u32(guest_base + 0x3004, TD_CTRL_ACTIVE | TD_CTRL_IOC | 0x7FF);
        common::write_u32(guest_base + 0x3008, token);
        common::write_u32(guest_base + 0x300C, 0x4000);
    }

    ctrl.io_write(REG_USBCMD, 2, USBCMD_RUN as u32);
    ctrl.step_frame();

    assert!(ctrl.irq_asserted());
    unsafe {
        let bytes = core::slice::from_raw_parts((guest_base + 0x4000) as *const u8, 4);
        assert_eq!(bytes, b"ABCD");
    }

    let usbcmd = ctrl.io_read(REG_USBCMD, 2);
    let usbsts = ctrl.io_read(REG_USBSTS, 2);
    let usbintr = ctrl.io_read(REG_USBINTR, 2);
    let frnum = ctrl.io_read(REG_FRNUM, 2);
    let frbaseadd = ctrl.io_read(REG_FRBASEADD, 4);
    let qh_element = unsafe { common::read_u32(guest_base + 0x2004) };
    let td_ctrl = unsafe { common::read_u32(guest_base + 0x3004) };

    let snapshot = ctrl.save_state();

    let mut ctrl2 = UhciControllerBridge::new(guest_base, guest_size).unwrap();
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    ctrl2.set_pci_command(PCI_COMMAND_BME);
    ctrl2.connect_device_for_test(0, Box::new(SimpleInDevice::new(b"ABCD")));
    ctrl2.load_state(&snapshot).unwrap();

    assert_eq!(ctrl2.io_read(REG_USBCMD, 2), usbcmd);
    assert_eq!(ctrl2.io_read(REG_USBSTS, 2), usbsts);
    assert_eq!(ctrl2.io_read(REG_USBINTR, 2), usbintr);
    assert_eq!(ctrl2.io_read(REG_FRNUM, 2), frnum);
    assert_eq!(ctrl2.io_read(REG_FRBASEADD, 4), frbaseadd);
    assert!(ctrl2.irq_asserted());

    // Controller progress lives in guest RAM (frame list/QH/TD), which is snapshotted separately
    // as part of the VM memory image; ensure the test still observes the updated pointers.
    assert_eq!(unsafe { common::read_u32(guest_base + 0x2004) }, qh_element);
    assert_eq!(unsafe { common::read_u32(guest_base + 0x3004) }, td_ctrl);
    assert_eq!(td_ctrl & TD_CTRL_ACTIVE, 0);
    assert_eq!(td_ctrl & TD_CTRL_ACTLEN_MASK, 3);

    // Clearing USBSTS.USBINT should deassert the IRQ line.
    ctrl2.io_write(REG_USBSTS, 2, USBSTS_USBINT as u32);
    assert!(!ctrl2.irq_asserted());
}

#[wasm_bindgen_test]
fn uhci_controller_bridge_restore_clears_webusb_host_state_and_allows_retry() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let fl_base = setup_webusb_control_in_frame_list(guest_base);

    let mut ctrl = UhciControllerBridge::new(guest_base, guest_size).unwrap();
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    ctrl.set_pci_command(PCI_COMMAND_BME);
    ctrl.set_connected(true);
    ctrl.io_write(REG_FRBASEADD, 4, fl_base);

    // Reset + enable the WebUSB root port (PORTSC2) by pulsing PR and waiting 50 frames.
    ctrl.io_write(REG_PORTSC1, 4, (u32::from(PORTSC_PR)) << 16);
    ctrl.step_frames(50);

    ctrl.io_write(REG_USBCMD, 2, USBCMD_RUN as u32);

    // First attempt should emit a host action.
    ctrl.step_frame();
    let drained = ctrl.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    let first_id = match actions.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action, got {other:?}"),
    };

    // With the action drained but no completion, the device is still inflight and should not emit
    // duplicates while the TD retries.
    ctrl.step_frame();
    let drained_again = ctrl.drain_actions().expect("drain_actions ok");
    let actions_again: Vec<UsbHostAction> = if drained_again.is_null() {
        Vec::new()
    } else {
        serde_wasm_bindgen::from_value(drained_again).expect("deserialize UsbHostAction[]")
    };
    assert!(
        actions_again.is_empty(),
        "expected inflight WebUSB transfer to suppress duplicate actions"
    );

    let snapshot = ctrl.snapshot_state().to_vec();

    let mut ctrl2 = UhciControllerBridge::new(guest_base, guest_size).unwrap();
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    ctrl2.set_pci_command(PCI_COMMAND_BME);
    ctrl2.restore_state(&snapshot).expect("restore_state ok");

    let drained_after_restore = ctrl2.drain_actions().expect("drain_actions ok");
    assert!(
        drained_after_restore.is_null(),
        "expected WebUSB host queues to be cleared on restore"
    );

    // The guest TD remains active and is retried; with host state cleared, the next retry should
    // re-emit host actions.
    ctrl2.step_frame();
    let drained_retry = ctrl2.drain_actions().expect("drain_actions ok");
    let actions_retry: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained_retry).expect("deserialize UsbHostAction[]");
    let retry_id = match actions_retry.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action after restore, got {other:?}"),
    };
    assert_ne!(
        retry_id, first_id,
        "expected re-emitted host action to allocate a new id after restore"
    );
}

// WebUSB UHCI bridge register constants (u32 offsets).
const WREG_USBCMD: u32 = 0x00;
const WREG_USBSTS: u32 = 0x02;
const WREG_USBINTR: u32 = 0x04;
const WREG_FRBASEADD: u32 = 0x08;
const WREG_PORTSC1: u32 = 0x10;

const WUSBCMD_RUN: u32 = 1 << 0;
const WUSBINTR_IOC: u32 = 1 << 2;
const WUSBSTS_USBINT: u32 = 1 << 0;
const WPORTSC_PR: u32 = 1 << 9;

const TD_CTRL_IOC_W: u32 = 1 << 24;

const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_D: u32 = 1 << 19;
const TD_TOKEN_MAXLEN_SHIFT_W: u32 = 21;

fn td_token(pid: u8, addr: u8, ep: u8, toggle: bool, max_len: usize) -> u32 {
    let max_len_field = if max_len == 0 {
        0x7FFu32
    } else {
        (max_len as u32).saturating_sub(1)
    };
    (pid as u32)
        | ((addr as u32) << TD_TOKEN_DEVADDR_SHIFT)
        | ((ep as u32) << TD_TOKEN_ENDPT_SHIFT)
        | (if toggle { TD_TOKEN_D } else { 0 })
        | (max_len_field << TD_TOKEN_MAXLEN_SHIFT_W)
}

fn td_ctrl(active: bool, ioc: bool) -> u32 {
    let mut v = 0x7FF;
    if active {
        v |= TD_CTRL_ACTIVE;
    }
    if ioc {
        v |= TD_CTRL_IOC_W;
    }
    v
}

fn setup_webusb_control_in_frame_list(guest_base: u32) -> u32 {
    // Layout (all 16-byte aligned).
    let fl_base = 0x1000;
    let qh_addr = 0x2000;
    let setup_td = qh_addr + 0x20;
    let data_td = setup_td + 0x20;
    let status_td = data_td + 0x20;
    let setup_buf = status_td + 0x20;
    let _data_buf = setup_buf + 0x10;

    unsafe {
        for i in 0..1024u32 {
            common::write_u32(guest_base + fl_base + i * 4, qh_addr | LINK_PTR_Q);
        }

        // QH: head=terminate, element=SETUP TD.
        common::write_u32(guest_base + qh_addr + 0x00, LINK_PTR_T);
        common::write_u32(guest_base + qh_addr + 0x04, setup_td);

        // Setup packet: GET_DESCRIPTOR (device), 8 bytes.
        let setup_packet = [
            0x80, // bmRequestType: device-to-host | standard | device
            0x06, // bRequest: GET_DESCRIPTOR
            0x00, 0x01, // wValue: (DEVICE=1)<<8 | index 0
            0x00, 0x00, // wIndex
            0x08, 0x00, // wLength: 8
        ];
        common::write_bytes(guest_base + setup_buf, &setup_packet);

        // SETUP TD.
        common::write_u32(guest_base + setup_td + 0x00, data_td);
        common::write_u32(guest_base + setup_td + 0x04, td_ctrl(true, false));
        common::write_u32(guest_base + setup_td + 0x08, td_token(0x2D, 0, 0, false, 8));
        common::write_u32(guest_base + setup_td + 0x0C, setup_buf);

        // DATA IN TD (will NAK until host completion is pushed).
        common::write_u32(guest_base + data_td + 0x00, status_td);
        common::write_u32(guest_base + data_td + 0x04, td_ctrl(true, false));
        common::write_u32(guest_base + data_td + 0x08, td_token(0x69, 0, 0, true, 8));
        common::write_u32(guest_base + data_td + 0x0C, setup_buf + 0x10);

        // STATUS OUT TD (0-length, IOC).
        common::write_u32(guest_base + status_td + 0x00, LINK_PTR_T);
        common::write_u32(guest_base + status_td + 0x04, td_ctrl(true, true));
        common::write_u32(guest_base + status_td + 0x08, td_token(0xE1, 0, 0, true, 0));
        common::write_u32(guest_base + status_td + 0x0C, 0);
    }

    fl_base
}

#[wasm_bindgen_test]
fn webusb_uhci_bridge_snapshot_roundtrip_preserves_irq_and_registers() {
    let (guest_base, _guest_size) = common::alloc_guest_region_bytes(0x8000);
    let fl_base = setup_webusb_control_in_frame_list(guest_base);

    let mut bridge = WebUsbUhciBridge::new(guest_base);
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    bridge.set_pci_command(PCI_COMMAND_BME);
    bridge.set_connected(true);

    bridge.io_write(WREG_FRBASEADD, 4, fl_base);
    bridge.io_write(WREG_USBINTR, 4, WUSBINTR_IOC);

    // WebUSB passthrough lives on root port 1 (PORTSC2); root port 0 is reserved for the
    // external hub used by WebHID passthrough.
    bridge.io_write(WREG_PORTSC1, 4, WPORTSC_PR << 16);
    bridge.step_frames(50);

    bridge.io_write(WREG_USBCMD, 4, WUSBCMD_RUN);
    bridge.step_frames(1);

    let drained = bridge.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    let id = match actions.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action, got {other:?}"),
    };

    let completion = UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success { data: vec![0u8; 8] },
    };
    bridge
        .push_completion(serde_wasm_bindgen::to_value(&completion).unwrap())
        .unwrap();
    bridge.step_frames(1);

    assert!(
        bridge.irq_level(),
        "expected IOC completion to assert irq_level"
    );

    let usbcmd = bridge.io_read(WREG_USBCMD, 2);
    let usbsts = bridge.io_read(WREG_USBSTS, 2);
    let usbintr = bridge.io_read(WREG_USBINTR, 2);
    let frbaseadd = bridge.io_read(WREG_FRBASEADD, 4);

    let snapshot = bridge.save_state();

    let mut bridge2 = WebUsbUhciBridge::new(guest_base);
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    bridge2.set_pci_command(PCI_COMMAND_BME);
    bridge2.set_connected(true);
    bridge2.load_state(&snapshot).unwrap();

    assert_eq!(bridge2.io_read(WREG_USBCMD, 2), usbcmd);
    assert_eq!(bridge2.io_read(WREG_USBSTS, 2), usbsts);
    assert_eq!(bridge2.io_read(WREG_USBINTR, 2), usbintr);
    assert_eq!(bridge2.io_read(WREG_FRBASEADD, 4), frbaseadd);
    assert!(bridge2.irq_level());

    bridge2.io_write(WREG_USBSTS, 2, WUSBSTS_USBINT);
    assert!(!bridge2.irq_level());
}

#[wasm_bindgen_test]
fn webusb_uhci_bridge_restore_clears_host_actions_and_allows_retry() {
    let (guest_base, _guest_size) = common::alloc_guest_region_bytes(0x8000);
    let fl_base = setup_webusb_control_in_frame_list(guest_base);

    let mut bridge = WebUsbUhciBridge::new(guest_base);
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    bridge.set_pci_command(PCI_COMMAND_BME);
    bridge.set_connected(true);

    bridge.io_write(WREG_FRBASEADD, 4, fl_base);
    bridge.io_write(WREG_USBINTR, 4, WUSBINTR_IOC);
    bridge.io_write(WREG_PORTSC1, 4, WPORTSC_PR << 16);
    bridge.step_frames(50);
    bridge.io_write(WREG_USBCMD, 4, WUSBCMD_RUN);
    bridge.step_frames(1);

    let summary: PendingSummary =
        serde_wasm_bindgen::from_value(bridge.pending_summary().unwrap()).unwrap();
    assert!(
        summary.queued_actions > 0,
        "expected at least one queued host action before snapshot"
    );

    let snapshot = bridge.save_state();

    let mut bridge2 = WebUsbUhciBridge::new(guest_base);
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest schedule.
    bridge2.set_pci_command(PCI_COMMAND_BME);
    bridge2.set_connected(true);
    bridge2.load_state(&snapshot).unwrap();

    // Host-action queues are intentionally cleared on restore (see `load_state` docs).
    let summary_after: PendingSummary =
        serde_wasm_bindgen::from_value(bridge2.pending_summary().unwrap()).unwrap();
    assert_eq!(summary_after.queued_actions, 0);

    // The guest TD remains active and is retried; with host state cleared, the next retry should
    // re-emit host actions.
    bridge2.step_frames(1);
    let summary_retry: PendingSummary =
        serde_wasm_bindgen::from_value(bridge2.pending_summary().unwrap()).unwrap();
    assert!(
        summary_retry.queued_actions > 0,
        "expected host actions to be re-emitted after restore"
    );
}
