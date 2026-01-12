#![cfg(target_arch = "wasm32")]

use aero_usb::passthrough::UsbHostAction;
use aero_wasm::UhciControllerBridge;
use aero_wasm::WebUsbUhciBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

const REG_USBCMD: u32 = 0x00;
const REG_USBINTR: u32 = 0x04;
const REG_FRBASEADD: u32 = 0x08;
const REG_PORTSC1: u32 = 0x10;

const USBCMD_RUN: u32 = 1 << 0;
const USBINTR_IOC: u32 = 1 << 2;
const PORTSC_PR: u32 = 1 << 9;

// PCI command register bit: Bus Master Enable (BME).
//
// The WASM UHCI bridges gate DMA/ticking on BME so that devices can't read/write guest RAM unless
// the guest has explicitly enabled bus mastering via PCI config space.
const PCI_COMMAND_BME: u32 = 1 << 2;

const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;

const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_D: u32 = 1 << 19;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

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
        | (max_len_field << TD_TOKEN_MAXLEN_SHIFT)
}

fn td_ctrl(active: bool, ioc: bool) -> u32 {
    let mut v = 0x7FF;
    if active {
        v |= TD_CTRL_ACTIVE;
    }
    if ioc {
        v |= TD_CTRL_IOC;
    }
    v
}

#[wasm_bindgen_test]
fn bridge_emits_host_actions_from_guest_frame_list() {
    let (guest_base, _guest_size) = common::alloc_guest_region_bytes(0x8000);

    // Layout (all 16-byte aligned).
    let fl_base = 0x1000u32;
    let qh_addr = fl_base + 0x1000;
    let setup_td = qh_addr + 0x20;
    let data_td = setup_td + 0x20;
    let status_td = data_td + 0x20;
    let setup_buf = status_td + 0x20;
    let data_buf = setup_buf + 0x10;

    // Install frame list pointing at our single QH.
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
        common::write_u32(guest_base + data_td + 0x0C, data_buf);

        // STATUS OUT TD (0-length, IOC).
        common::write_u32(guest_base + status_td + 0x00, LINK_PTR_T);
        common::write_u32(guest_base + status_td + 0x04, td_ctrl(true, true));
        common::write_u32(guest_base + status_td + 0x08, td_token(0xE1, 0, 0, true, 0));
        common::write_u32(guest_base + status_td + 0x0C, 0);
    }

    let mut bridge = WebUsbUhciBridge::new(guest_base);
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest frame list.
    bridge.set_pci_command(PCI_COMMAND_BME);
    bridge.set_connected(true);

    bridge.io_write(REG_FRBASEADD, 4, fl_base);
    // Some UHCI drivers use 32-bit I/O to program paired 16-bit registers (USBINTR+FRNUM).
    bridge.io_write(REG_USBINTR, 4, USBINTR_IOC);

    // Reset + enable port 2 (root port 1).
    // Similarly, allow 32-bit I/O at PORTSC1 to reach both PORTSC registers.
    bridge.io_write(REG_PORTSC1, 4, PORTSC_PR << 16);
    bridge.step_frames(50);

    // Some drivers use 32-bit I/O at 0x00 to update USBCMD+USBSTS simultaneously.
    bridge.io_write(REG_USBCMD, 4, USBCMD_RUN);
    bridge.step_frames(1);

    let drained = bridge.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");

    assert!(
        !actions.is_empty(),
        "expected at least one queued UsbHostAction"
    );
    assert!(matches!(actions[0], UsbHostAction::ControlIn { .. }));
}

#[wasm_bindgen_test]
fn uhci_controller_bridge_emits_host_actions_on_webusb_port() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let fl_guest = 0x1000u32;
    let fl_linear = guest_base + fl_guest;

    // Layout (guest physical addresses, all 16-byte aligned).
    let qh_guest = fl_guest + 0x1000;
    let setup_td = qh_guest + 0x20;
    let data_td = setup_td + 0x20;
    let status_td = data_td + 0x20;
    let setup_buf = status_td + 0x20;
    let data_buf = setup_buf + 0x10;

    let qh_linear = guest_base + qh_guest;
    let setup_td_linear = guest_base + setup_td;
    let data_td_linear = guest_base + data_td;
    let status_td_linear = guest_base + status_td;
    let setup_buf_linear = guest_base + setup_buf;

    unsafe {
        for i in 0..1024u32 {
            common::write_u32(fl_linear + i * 4, qh_guest | LINK_PTR_Q);
        }

        // QH: head=terminate, element=SETUP TD.
        common::write_u32(qh_linear + 0x00, LINK_PTR_T);
        common::write_u32(qh_linear + 0x04, setup_td);

        // Setup packet: GET_DESCRIPTOR (device), 8 bytes.
        let setup_packet = [
            0x80, // bmRequestType: device-to-host | standard | device
            0x06, // bRequest: GET_DESCRIPTOR
            0x00, 0x01, // wValue: (DEVICE=1)<<8 | index 0
            0x00, 0x00, // wIndex
            0x08, 0x00, // wLength: 8
        ];
        common::write_bytes(setup_buf_linear, &setup_packet);

        // SETUP TD.
        common::write_u32(setup_td_linear + 0x00, data_td);
        common::write_u32(setup_td_linear + 0x04, td_ctrl(true, false));
        common::write_u32(setup_td_linear + 0x08, td_token(0x2D, 0, 0, false, 8));
        common::write_u32(setup_td_linear + 0x0C, setup_buf);

        // DATA IN TD (will NAK until host completion is pushed).
        common::write_u32(data_td_linear + 0x00, status_td);
        common::write_u32(data_td_linear + 0x04, td_ctrl(true, false));
        common::write_u32(data_td_linear + 0x08, td_token(0x69, 0, 0, true, 8));
        common::write_u32(data_td_linear + 0x0C, data_buf);

        // STATUS OUT TD (0-length, IOC).
        common::write_u32(status_td_linear + 0x00, LINK_PTR_T);
        common::write_u32(status_td_linear + 0x04, td_ctrl(true, true));
        common::write_u32(status_td_linear + 0x08, td_token(0xE1, 0, 0, true, 0));
        common::write_u32(status_td_linear + 0x0C, 0);
    }

    let mut bridge =
        UhciControllerBridge::new(guest_base, guest_size).expect("UhciControllerBridge::new ok");
    // Enable PCI bus mastering so the bridge is allowed to DMA into the guest frame list.
    bridge.set_pci_command(PCI_COMMAND_BME);
    bridge.set_connected(true);

    bridge.io_write(REG_FRBASEADD as u16, 4, fl_guest);
    bridge.io_write(REG_USBINTR as u16, 4, USBINTR_IOC);

    // Reset the second root port (PORTSC2 lives in the upper 16-bits of a 32-bit write to PORTSC1).
    bridge.io_write(REG_PORTSC1 as u16, 4, PORTSC_PR << 16);
    bridge.step_frames(50);

    bridge.io_write(REG_USBCMD as u16, 4, USBCMD_RUN);
    bridge.step_frames(1);

    let drained = bridge.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");

    assert!(
        !actions.is_empty(),
        "expected at least one queued UsbHostAction"
    );
    assert!(matches!(actions[0], UsbHostAction::ControlIn { .. }));
}
