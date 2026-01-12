#![cfg(target_arch = "wasm32")]

use aero_usb::passthrough::UsbHostAction;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};
use aero_wasm::UhciControllerBridge;
use aero_wasm::WebUsbUhciPassthroughHarness;
use aero_wasm::{UsbHidPassthroughBridge, WebUsbUhciBridge};
use aero_wasm::{add, demo_render_rgba8888};
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

// UHCI register offsets / bits (mirrors `crates/aero-usb/src/uhci/regs.rs`).
const REG_USBCMD: u16 = 0x00;
const REG_USBSTS: u16 = 0x02;
const REG_USBINTR: u16 = 0x04;
const REG_FRBASEADD: u16 = 0x08;
const REG_PORTSC1: u16 = 0x10;

const USBCMD_RUN: u16 = 1 << 0;
const USBSTS_USBINT: u16 = 1 << 0;
const USBINTR_IOC: u16 = 1 << 2;
const PORTSC_PED: u16 = 1 << 2;

// UHCI link pointer bits.
const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

// UHCI TD control/token fields.
const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_ACTLEN_MASK: u32 = 0x7FF;

const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

// PCI command register bit: Bus Master Enable (BME).
//
// The WASM UHCI bridges gate DMA/ticking on BME so that devices can't read/write guest RAM unless
// the guest has explicitly enabled bus mastering via PCI config space.
const PCI_COMMAND_BME: u32 = 1 << 2;

fn write_u32(mem: &mut [u8], addr: u32, value: u32) {
    let addr = addr as usize;
    mem[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(mem: &[u8], addr: u32) -> u32 {
    let addr = addr as usize;
    u32::from_le_bytes(mem[addr..addr + 4].try_into().unwrap())
}

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
fn module_loads_and_exports_work() {
    assert_eq!(add(40, 2), 42);

    let mut buf = vec![0u8; 8 * 8 * 4];
    let offset = buf.as_mut_ptr() as u32;
    let written = demo_render_rgba8888(offset, 8, 8, 8 * 4, 1000.0);
    assert_eq!(written, 64);
    assert_eq!(&buf[0..4], &[60, 35, 20, 255]);
}

#[wasm_bindgen_test]
fn webusb_uhci_harness_queues_actions_without_host() {
    let mut harness = WebUsbUhciPassthroughHarness::new();

    // Tick long enough to finish the UHCI port reset window and reach the first
    // control transfer (GET_DESCRIPTOR device 8 bytes).
    for _ in 0..60 {
        harness.tick();
    }

    let drained = harness.drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");

    assert!(
        !actions.is_empty(),
        "expected at least one queued UsbHostAction"
    );
    // We should start enumeration with a control IN descriptor request.
    assert!(matches!(actions[0], UsbHostAction::ControlIn { .. }));
}

#[wasm_bindgen_test]
fn uhci_controller_bridge_can_step_guest_memory_and_toggle_irq() {
    // Synthetic guest memory region: allocate outside the wasm heap to keep wasm-pack tests from
    // exhausting the bounded `runtime_alloc` heap.
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x9000);
    // Safety: `alloc_guest_region_bytes` reserves `guest_size` bytes in linear memory starting at
    // `guest_base` and the region is private to this test.
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    let mut ctrl =
        UhciControllerBridge::new(guest_base, guest_size).expect("new UhciControllerBridge");
    // PCI Bus Master Enable gates UHCI DMA. When exercising the bridge directly (without the JS
    // PCI bus), explicitly enable bus mastering so frame stepping can access guest memory.
    ctrl.set_pci_command(PCI_COMMAND_BME);

    // Attach a trivial device at root port 0 so we can complete a single IN TD.
    ctrl.connect_device_for_test(0, Box::new(SimpleInDevice::new(b"ABCD")));

    // Enable the root port + IOC interrupts and point the controller at the frame list.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PED as u32);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);
    ctrl.io_write(REG_FRBASEADD, 4, 0x1000);

    // Frame list: terminate everything, then set frame 0 to point at our QH.
    for i in 0..1024u32 {
        write_u32(guest, 0x1000 + i * 4, LINK_PTR_T);
    }
    write_u32(guest, 0x1000, 0x2000 | LINK_PTR_Q);

    // Queue head -> TD.
    write_u32(guest, 0x2000, LINK_PTR_T);
    write_u32(guest, 0x2004, 0x3000);

    // TD: IN to addr0/ep1, 4 bytes.
    let maxlen_field = (4u32 - 1) << TD_TOKEN_MAXLEN_SHIFT;
    let token = 0x69u32 | maxlen_field | (1 << TD_TOKEN_ENDPT_SHIFT); // IN, addr0/ep1
    write_u32(guest, 0x3000, LINK_PTR_T);
    write_u32(guest, 0x3004, TD_CTRL_ACTIVE | TD_CTRL_IOC | 0x7FF);
    write_u32(guest, 0x3008, token);
    write_u32(guest, 0x300C, 0x4000);

    // Verify register read/write wiring.
    let usbcmd_before = ctrl.io_read(REG_USBCMD, 2) as u16;
    assert_eq!(usbcmd_before & USBCMD_RUN, 0);

    ctrl.io_write(REG_USBCMD, 2, USBCMD_RUN as u32);
    let usbcmd_after = ctrl.io_read(REG_USBCMD, 2) as u16;
    assert_ne!(usbcmd_after & USBCMD_RUN, 0);

    assert!(
        !ctrl.irq_asserted(),
        "irq should be low before any IOC completion"
    );

    ctrl.step_frames(1);

    assert_eq!(&guest[0x4000..0x4004], b"ABCD");
    // Hardware should advance the QH element pointer as TDs complete.
    assert_eq!(read_u32(guest, 0x2004), LINK_PTR_T);

    let ctrl_sts = read_u32(guest, 0x3004);
    assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    assert_eq!(ctrl_sts & TD_CTRL_ACTLEN_MASK, 3);

    assert!(
        ctrl.irq_asserted(),
        "irq should assert after IOC completion"
    );

    // Clear the USBINT status bit (write-1-to-clear) and ensure the irq deasserts.
    ctrl.io_write(REG_USBSTS, 2, USBSTS_USBINT as u32);
    assert!(
        !ctrl.irq_asserted(),
        "irq should deassert after clearing USBSTS.USBINT"
    );
}

#[wasm_bindgen_test]
fn webusb_uhci_bridge_can_attach_and_detach_usb_hid_passthrough_device() {
    // Minimal (but valid) USB HID report descriptor: a single 8-bit input.
    // Ref: HID 1.11 spec, Example "Vendor-defined" input report.
    let report_descriptor = vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (0x01)
        0xa1, 0x01, // Collection (Application)
        0x09, 0x02, //   Usage (0x02)
        0x15, 0x00, //   Logical Minimum (0)
        0x26, 0xff, 0x00, //   Logical Maximum (255)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x01, //   Report Count (1)
        0x81, 0x02, //   Input (Data,Var,Abs)
        0xc0, // End Collection
    ];

    let dev = UsbHidPassthroughBridge::new(
        0x1234,
        0x5678,
        None,
        Some("Test HID".to_string()),
        None,
        report_descriptor,
        false,
        None,
        None,
    );

    let mut bridge = WebUsbUhciBridge::new(0);

    // Hub ports 1..=3 are reserved for synthetic HID devices, so use port 4+ for arbitrary tests.
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("path to_value");
    bridge
        .attach_usb_hid_passthrough_device(path, &dev)
        .expect("attach_usb_hid_passthrough_device ok");

    let path = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("path to_value");
    bridge.detach_at_path(path).expect("detach_at_path ok");
}
