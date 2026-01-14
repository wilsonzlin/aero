#![cfg(target_arch = "wasm32")]

use aero_wasm::UhciRuntime;
use js_sys::{Array, Reflect};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn make_runtime() -> UhciRuntime {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    UhciRuntime::new(guest_base, guest_size).expect("UhciRuntime::new")
}

// UHCI register offsets / bits (mirrors `crates/aero-usb/src/uhci.rs` tests).
const REG_USBCMD: u16 = 0x00;
const REG_FRBASEADD: u16 = 0x08;
const REG_PORTSC2: u16 = 0x12;
const USBCMD_RUN: u16 = 1 << 0;
const PORTSC_PED: u16 = 1 << 2;

// UHCI link pointer bits.
const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

// UHCI TD control/token fields.
const TD_CTRL_ACTIVE: u32 = 1 << 23;
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

fn td_ctrl(active: bool) -> u32 {
    let mut v = 0x7FF;
    if active {
        v |= TD_CTRL_ACTIVE;
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
        common::write_u32(guest_base + setup_td + 0x04, td_ctrl(true));
        common::write_u32(guest_base + setup_td + 0x08, td_token(0x2D, 0, 0, false, 8));
        common::write_u32(guest_base + setup_td + 0x0C, setup_buf);

        // DATA IN TD (will NAK until host completion is pushed).
        common::write_u32(guest_base + data_td + 0x00, status_td);
        common::write_u32(guest_base + data_td + 0x04, td_ctrl(true));
        common::write_u32(guest_base + data_td + 0x08, td_token(0x69, 0, 0, true, 8));
        common::write_u32(guest_base + data_td + 0x0C, setup_buf + 0x10);

        // STATUS OUT TD (0-length, IOC bit omitted here).
        common::write_u32(guest_base + status_td + 0x00, LINK_PTR_T);
        common::write_u32(guest_base + status_td + 0x04, td_ctrl(true));
        common::write_u32(guest_base + status_td + 0x08, td_token(0xE1, 0, 0, true, 0));
        common::write_u32(guest_base + status_td + 0x0C, 0);
    }

    fl_base
}

#[wasm_bindgen_test]
fn uhci_runtime_webusb_drain_actions_returns_null_when_not_attached() {
    let mut rt = make_runtime();
    let drained = rt.webusb_drain_actions().expect("webusb_drain_actions ok");
    assert!(
        drained.is_null(),
        "expected webusb_drain_actions to return null when WebUSB is not attached"
    );
}

#[wasm_bindgen_test]
fn uhci_runtime_webusb_drain_actions_returns_null_when_attached_but_idle() {
    let mut rt = make_runtime();
    rt.webusb_attach(Some(1)).expect("webusb_attach ok");
    let drained = rt.webusb_drain_actions().expect("webusb_drain_actions ok");
    assert!(
        drained.is_null(),
        "expected webusb_drain_actions to return null when there are no queued actions"
    );
}

#[wasm_bindgen_test]
fn uhci_runtime_webusb_drain_actions_returns_array_when_actions_queued() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    let fl_base = setup_webusb_control_in_frame_list(guest_base);

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("UhciRuntime::new");
    rt.webusb_attach(Some(1)).expect("webusb_attach ok");

    rt.port_write(REG_FRBASEADD, 4, fl_base);
    rt.port_write(REG_PORTSC2, 2, PORTSC_PED as u32);
    rt.port_write(REG_USBCMD, 2, USBCMD_RUN as u32);

    let mut drained = JsValue::NULL;
    for _ in 0..8 {
        rt.step_frame();
        drained = rt.webusb_drain_actions().expect("webusb_drain_actions ok");
        if !drained.is_null() {
            break;
        }
    }
    assert!(
        !drained.is_null(),
        "expected webusb_drain_actions to return actions after running the schedule"
    );
    assert!(
        Array::is_array(&drained),
        "expected webusb_drain_actions to return an Array when actions are queued"
    );

    let actions = Array::from(&drained);
    assert!(
        actions.length() > 0,
        "expected webusb_drain_actions Array to contain at least one action"
    );
    let action0 = actions.get(0);
    let kind = Reflect::get(&action0, &JsValue::from_str("kind"))
        .expect("action.kind exists")
        .as_string()
        .expect("action.kind is string");
    assert_eq!(kind, "controlIn");
}
