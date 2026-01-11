#![cfg(target_arch = "wasm32")]

use aero_usb::passthrough::UsbHostAction;
use aero_wasm::WebUsbUhciBridge;
use wasm_bindgen_test::wasm_bindgen_test;

const REG_USBCMD: u32 = 0x00;
const REG_USBINTR: u32 = 0x04;
const REG_FRBASEADD: u32 = 0x08;
const REG_PORTSC1: u32 = 0x10;

const USBCMD_RUN: u32 = 1 << 0;
const USBINTR_IOC: u32 = 1 << 2;
const PORTSC_PR: u32 = 1 << 9;

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

unsafe fn write_u32(addr: u32, value: u32) {
    core::ptr::write_unaligned(addr as *mut u32, value);
}

unsafe fn write_bytes(addr: u32, bytes: &[u8]) {
    core::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
}

#[wasm_bindgen_test]
fn bridge_emits_host_actions_from_guest_frame_list() {
    // Allocate a chunk of linear memory that we treat as guest RAM.
    // `guest_base=0` means guest physical addresses map directly to linear offsets.
    let mut backing = vec![0u8; 0x50_000];
    let base = backing.as_mut_ptr() as u32;
    let fl_base = (base + 0x0fff) & !0x0fff; // 4KiB-align for FRBASEADD.

    // Layout (all 16-byte aligned).
    let qh_addr = fl_base + 0x1000;
    let setup_td = qh_addr + 0x20;
    let data_td = setup_td + 0x20;
    let status_td = data_td + 0x20;
    let setup_buf = status_td + 0x20;
    let data_buf = setup_buf + 0x10;

    // Install frame list pointing at our single QH.
    unsafe {
        for i in 0..1024u32 {
            write_u32(fl_base + i * 4, qh_addr | LINK_PTR_Q);
        }

        // QH: head=terminate, element=SETUP TD.
        write_u32(qh_addr + 0x00, LINK_PTR_T);
        write_u32(qh_addr + 0x04, setup_td);

        // Setup packet: GET_DESCRIPTOR (device), 8 bytes.
        let setup_packet = [
            0x80, // bmRequestType: device-to-host | standard | device
            0x06, // bRequest: GET_DESCRIPTOR
            0x00, 0x01, // wValue: (DEVICE=1)<<8 | index 0
            0x00, 0x00, // wIndex
            0x08, 0x00, // wLength: 8
        ];
        write_bytes(setup_buf, &setup_packet);

        // SETUP TD.
        write_u32(setup_td + 0x00, data_td);
        write_u32(setup_td + 0x04, td_ctrl(true, false));
        write_u32(setup_td + 0x08, td_token(0x2D, 0, 0, false, 8));
        write_u32(setup_td + 0x0C, setup_buf);

        // DATA IN TD (will NAK until host completion is pushed).
        write_u32(data_td + 0x00, status_td);
        write_u32(data_td + 0x04, td_ctrl(true, false));
        write_u32(data_td + 0x08, td_token(0x69, 0, 0, true, 8));
        write_u32(data_td + 0x0C, data_buf);

        // STATUS OUT TD (0-length, IOC).
        write_u32(status_td + 0x00, LINK_PTR_T);
        write_u32(status_td + 0x04, td_ctrl(true, true));
        write_u32(status_td + 0x08, td_token(0xE1, 0, 0, true, 0));
        write_u32(status_td + 0x0C, 0);
    }

    let mut bridge = WebUsbUhciBridge::new(0);
    bridge.set_connected(true);

    bridge.io_write(REG_FRBASEADD, 4, fl_base);
    bridge.io_write(REG_USBINTR, 2, USBINTR_IOC);

    // Reset + enable port 1.
    bridge.io_write(REG_PORTSC1, 2, PORTSC_PR);
    bridge.step_frames(50);

    bridge.io_write(REG_USBCMD, 2, USBCMD_RUN);
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
