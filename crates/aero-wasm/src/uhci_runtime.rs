use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

use aero_usb::GuestMemory;
use aero_usb::hid::passthrough::{UsbHidPassthrough, UsbHidPassthroughOutputReport};
use aero_usb::hid::webhid;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::usb::{UsbDevice, UsbSpeed};

const DEFAULT_IO_BASE: u16 = 0x5000;
const DEFAULT_IRQ_LINE: u8 = 11;
const PORT_COUNT: usize = 2;

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

#[derive(Default)]
struct RuntimeIrq {
    level: bool,
}

impl InterruptController for RuntimeIrq {
    fn raise_irq(&mut self, _irq: u8) {
        self.level = true;
    }

    fn lower_irq(&mut self, _irq: u8) {
        self.level = false;
    }
}

struct LinearGuestMemory {
    guest_base: u32,
    guest_size: u32,
}

impl LinearGuestMemory {
    fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        let pages = core::arch::wasm32::memory_size(0) as u64;
        let mem_bytes = pages.saturating_mul(64 * 1024);

        let end = guest_base as u64 + guest_size as u64;
        if end > mem_bytes {
            return Err(js_error(&format!(
                "Guest RAM region out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size:x} end=0x{end:x} wasm_mem_bytes=0x{mem_bytes:x}"
            )));
        }

        Ok(Self {
            guest_base,
            guest_size,
        })
    }

    fn translate(&self, addr: u32) -> Option<u32> {
        if addr >= self.guest_size {
            return None;
        }
        self.guest_base.checked_add(addr)
    }
}

impl GuestMemory for LinearGuestMemory {
    fn read(&self, addr: u32, buf: &mut [u8]) {
        let guest_size = self.guest_size as u64;
        let addr_u64 = addr as u64;
        if addr_u64 >= guest_size {
            buf.fill(0);
            return;
        }

        let max_len = (guest_size - addr_u64)
            .min(buf.len() as u64)
            .min(usize::MAX as u64) as usize;

        let Some(linear) = self.translate(addr) else {
            buf.fill(0);
            return;
        };

        unsafe {
            let src = core::slice::from_raw_parts(linear as *const u8, max_len);
            buf[..max_len].copy_from_slice(src);
        }

        if max_len < buf.len() {
            buf[max_len..].fill(0);
        }
    }

    fn write(&mut self, addr: u32, buf: &[u8]) {
        let guest_size = self.guest_size as u64;
        let addr_u64 = addr as u64;
        if addr_u64 >= guest_size {
            return;
        }

        let max_len = (guest_size - addr_u64)
            .min(buf.len() as u64)
            .min(usize::MAX as u64) as usize;

        let Some(linear) = self.translate(addr) else {
            return;
        };

        unsafe {
            let dst = core::slice::from_raw_parts_mut(linear as *mut u8, max_len);
            dst.copy_from_slice(&buf[..max_len]);
        }
    }
}

fn collections_have_output_reports(collections: &[webhid::HidCollectionInfo]) -> bool {
    fn walk(col: &webhid::HidCollectionInfo) -> bool {
        if !col.output_reports.is_empty() {
            return true;
        }
        col.children.iter().any(walk)
    }

    collections.iter().any(walk)
}

#[derive(Clone)]
struct RcWebHidDevice(Rc<RefCell<UsbHidPassthrough>>);

impl UsbDevice for RcWebHidDevice {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn address(&self) -> u8 {
        self.0.borrow().address()
    }

    fn handle_setup(&mut self, setup: aero_usb::usb::SetupPacket) {
        self.0.borrow_mut().handle_setup(setup);
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> aero_usb::usb::UsbHandshake {
        self.0.borrow_mut().handle_out(ep, data)
    }

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> aero_usb::usb::UsbHandshake {
        self.0.borrow_mut().handle_in(ep, buf)
    }
}

#[derive(Clone)]
struct RcWebUsbDevice(Rc<RefCell<aero_usb::UsbWebUsbPassthroughDevice>>);

impl UsbDevice for RcWebUsbDevice {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn address(&self) -> u8 {
        self.0.borrow().address()
    }

    fn handle_setup(&mut self, setup: aero_usb::usb::SetupPacket) {
        self.0.borrow_mut().handle_setup(setup);
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> aero_usb::usb::UsbHandshake {
        self.0.borrow_mut().handle_out(ep, data)
    }

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> aero_usb::usb::UsbHandshake {
        self.0.borrow_mut().handle_in(ep, buf)
    }
}

struct WebHidDeviceState {
    port: usize,
    dev: Rc<RefCell<UsbHidPassthrough>>,
}

struct WebUsbDeviceState {
    port: usize,
    dev: Rc<RefCell<aero_usb::UsbWebUsbPassthroughDevice>>,
}

#[wasm_bindgen]
pub struct UhciRuntime {
    ctrl: UhciController,
    mem: LinearGuestMemory,
    irq: RuntimeIrq,

    webhid_devices: HashMap<u32, WebHidDeviceState>,
    webhid_ports: [Option<u32>; PORT_COUNT],

    webusb: Option<WebUsbDeviceState>,
}

#[wasm_bindgen]
impl UhciRuntime {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        let mem = LinearGuestMemory::new(guest_base, guest_size)?;
        Ok(Self {
            ctrl: UhciController::new(DEFAULT_IO_BASE, DEFAULT_IRQ_LINE),
            mem,
            irq: RuntimeIrq::default(),
            webhid_devices: HashMap::new(),
            webhid_ports: [None, None],
            webusb: None,
        })
    }

    pub fn io_base(&self) -> u16 {
        self.ctrl.io_base()
    }

    pub fn irq_line(&self) -> u8 {
        self.ctrl.irq_line()
    }

    pub fn irq_level(&self) -> bool {
        self.irq.level
    }

    pub fn port_read(&mut self, offset: u16, size: u8) -> u32 {
        let Some(port) = self.ctrl.io_base().checked_add(offset) else {
            return 0xFFFF_FFFF;
        };
        self.ctrl.port_read(port, size as usize)
    }

    pub fn port_write(&mut self, offset: u16, size: u8, value: u32) {
        let Some(port) = self.ctrl.io_base().checked_add(offset) else {
            return;
        };
        self.ctrl
            .port_write(port, size as usize, value, &mut self.irq);
    }

    pub fn tick_1ms(&mut self) {
        self.step_frame();
    }

    pub fn step_frame(&mut self) {
        self.ctrl.step_frame(&mut self.mem, &mut self.irq);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn webhid_attach(
        &mut self,
        device_id: u32,
        vendor_id: u16,
        product_id: u16,
        product_name: Option<String>,
        collections_json: JsValue,
        preferred_port: Option<u8>,
    ) -> Result<u32, JsValue> {
        self.webhid_detach(device_id);

        let port = self.alloc_port(preferred_port)?;

        let collections: Vec<webhid::HidCollectionInfo> =
            serde_wasm_bindgen::from_value(collections_json)
                .map_err(|err| js_error(&format!("Invalid WebHID collection schema: {err}")))?;

        let report_descriptor =
            webhid::synthesize_report_descriptor(&collections).map_err(|err| {
                js_error(&format!(
                    "Failed to synthesize HID report descriptor: {err}"
                ))
            })?;

        let has_interrupt_out = collections_have_output_reports(&collections);

        let device = UsbHidPassthrough::new(
            vendor_id,
            product_id,
            "WebHID".to_string(),
            product_name.unwrap_or_else(|| "WebHID HID Device".to_string()),
            None,
            report_descriptor,
            has_interrupt_out,
            None,
            None,
            None,
        );

        let dev = Rc::new(RefCell::new(device));
        self.ctrl
            .connect_device(port, Box::new(RcWebHidDevice(dev.clone())));

        self.webhid_ports[port] = Some(device_id);
        self.webhid_devices
            .insert(device_id, WebHidDeviceState { port, dev });

        Ok(port as u32)
    }

    pub fn webhid_detach(&mut self, device_id: u32) {
        let Some(state) = self.webhid_devices.remove(&device_id) else {
            return;
        };

        self.ctrl.disconnect_device(state.port);
        if self.webhid_ports[state.port] == Some(device_id) {
            self.webhid_ports[state.port] = None;
        }
    }

    pub fn webhid_push_input_report(
        &mut self,
        device_id: u32,
        report_id: u32,
        data: &[u8],
    ) -> Result<(), JsValue> {
        let Some(state) = self.webhid_devices.get(&device_id) else {
            return Ok(());
        };
        let report_id = u8::try_from(report_id)
            .map_err(|_| js_error("reportId is out of range (expected 0..=255)"))?;

        state.dev.borrow_mut().push_input_report(report_id, data);
        Ok(())
    }

    pub fn webhid_drain_output_reports(&mut self) -> JsValue {
        let out = Array::new();
        for (&device_id, state) in self.webhid_devices.iter_mut() {
            loop {
                let report = state.dev.borrow_mut().pop_output_report();
                let Some(report) = report else { break };
                out.push(&webhid_output_report_to_js(device_id, report));
            }
        }
        out.into()
    }

    pub fn webusb_attach(&mut self, preferred_port: Option<u8>) -> Result<u32, JsValue> {
        self.webusb_detach();

        let port = if let Some(p) = preferred_port {
            self.alloc_port(Some(p))?
        } else if self.port_is_free(1) {
            1
        } else {
            self.alloc_port(None)?
        };

        let dev = Rc::new(RefCell::new(aero_usb::UsbWebUsbPassthroughDevice::new()));
        self.ctrl
            .connect_device(port, Box::new(RcWebUsbDevice(dev.clone())));
        self.webusb = Some(WebUsbDeviceState { port, dev });
        Ok(port as u32)
    }

    pub fn webusb_detach(&mut self) {
        let Some(state) = self.webusb.take() else {
            return;
        };
        self.ctrl.disconnect_device(state.port);
    }

    pub fn webusb_drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let actions: Vec<UsbHostAction> = if let Some(state) = self.webusb.as_ref() {
            state.dev.borrow_mut().drain_actions()
        } else {
            Vec::new()
        };
        serde_wasm_bindgen::to_value(&actions).map_err(|e| js_error(&e.to_string()))
    }

    pub fn webusb_push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let Some(state) = self.webusb.as_ref() else {
            return Ok(());
        };
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| js_error(&format!("Invalid UsbHostCompletion: {e}")))?;
        state.dev.borrow_mut().push_completion(completion);
        Ok(())
    }
}

impl UhciRuntime {
    fn port_is_free(&self, port: usize) -> bool {
        if port >= PORT_COUNT {
            return false;
        }
        if self.webhid_ports[port].is_some() {
            return false;
        }
        if let Some(webusb) = self.webusb.as_ref() {
            if webusb.port == port {
                return false;
            }
        }
        true
    }

    fn alloc_port(&self, preferred: Option<u8>) -> Result<usize, JsValue> {
        if let Some(p) = preferred {
            let idx = p as usize;
            if idx >= PORT_COUNT {
                return Err(js_error(&format!(
                    "Invalid preferredPort {p} (expected 0..{})",
                    PORT_COUNT - 1
                )));
            }
            if self.port_is_free(idx) {
                return Ok(idx);
            }
        }

        for idx in 0..PORT_COUNT {
            if self.port_is_free(idx) {
                return Ok(idx);
            }
        }

        Err(js_error("No free UHCI root hub ports available."))
    }
}

fn webhid_output_report_to_js(device_id: u32, report: UsbHidPassthroughOutputReport) -> JsValue {
    let report_type = match report.report_type {
        2 => "output",
        3 => "feature",
        _ => "output",
    };

    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("deviceId"),
        &JsValue::from_f64(f64::from(device_id)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("reportType"),
        &JsValue::from_str(report_type),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("reportId"),
        &JsValue::from_f64(f64::from(report.report_id)),
    );
    let data = Uint8Array::from(report.data.as_slice());
    let _ = Reflect::set(&obj, &JsValue::from_str("data"), data.as_ref());
    obj.into()
}
