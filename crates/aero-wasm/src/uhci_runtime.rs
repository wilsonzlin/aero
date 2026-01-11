use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::GuestMemory;
use aero_usb::hid::passthrough::{UsbHidPassthrough, UsbHidPassthroughOutputReport};
use aero_usb::hid::webhid;
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::usb::{UsbDevice, UsbSpeed};

const DEFAULT_IO_BASE: u16 = 0x5000;
const DEFAULT_IRQ_LINE: u8 = 11;
const PORT_COUNT: usize = 2;
const EXTERNAL_HUB_ROOT_PORT: usize = 0;
const DEFAULT_EXTERNAL_HUB_PORT_COUNT: u8 = 16;
const WEBUSB_ROOT_PORT: usize = 1;
const MAX_USB_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

const UHCI_RUNTIME_DEVICE_ID: [u8; 4] = *b"UHRT";
const UHCI_RUNTIME_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

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
    location: WebHidDeviceLocation,
    dev: Rc<RefCell<UsbHidPassthrough>>,
    vendor_id: u16,
    product_id: u16,
    product: String,
    report_descriptor: Vec<u8>,
    has_interrupt_out: bool,
}

struct WebUsbDeviceState {
    port: usize,
    dev: Rc<RefCell<aero_usb::UsbWebUsbPassthroughDevice>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WebHidDeviceLocation {
    RootPort(usize),
    ExternalHubPort(u8),
}

#[derive(Clone, Copy, Debug)]
struct ExternalHubState {
    port_count: u8,
}

#[wasm_bindgen]
pub struct UhciRuntime {
    ctrl: UhciController,
    mem: LinearGuestMemory,
    irq: RuntimeIrq,

    webhid_devices: HashMap<u32, WebHidDeviceState>,
    webhid_ports: [Option<u32>; PORT_COUNT],
    webhid_hub_ports: HashMap<u8, u32>,

    external_hub: Option<ExternalHubState>,
    external_hub_port_count_hint: Option<u8>,

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
            webhid_hub_ports: HashMap::new(),
            external_hub: None,
            external_hub_port_count_hint: None,
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
        let product = product_name.unwrap_or_else(|| "WebHID HID Device".to_string());

        let device = UsbHidPassthrough::new(
            vendor_id,
            product_id,
            "WebHID".to_string(),
            product.clone(),
            None,
            report_descriptor.clone(),
            has_interrupt_out,
            None,
            None,
            None,
        );

        let dev = Rc::new(RefCell::new(device));
        self.ctrl
            .connect_device(port, Box::new(RcWebHidDevice(dev.clone())));

        self.webhid_ports[port] = Some(device_id);
        self.webhid_devices.insert(
            device_id,
            WebHidDeviceState {
                location: WebHidDeviceLocation::RootPort(port),
                dev,
                vendor_id,
                product_id,
                product,
                report_descriptor,
                has_interrupt_out,
            },
        );

        Ok(port as u32)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn webhid_attach_at_path(
        &mut self,
        device_id: u32,
        vendor_id: u16,
        product_id: u16,
        product_name: Option<String>,
        collections_json: JsValue,
        guest_path: JsValue,
    ) -> Result<(), JsValue> {
        let (root_port, hub_port) = parse_external_hub_guest_path(guest_path)?;
        if root_port != EXTERNAL_HUB_ROOT_PORT {
            return Err(js_error(&format!(
                "Unsupported guestPath root port {root_port} (expected {EXTERNAL_HUB_ROOT_PORT} for external hub)"
            )));
        }

        self.ensure_external_hub(hub_port)?;

        // Clear any existing device at this hub port so we do not silently stack devices.
        if let Some(prev_device_id) = self.webhid_hub_ports.get(&hub_port).copied() {
            if prev_device_id != device_id {
                self.webhid_detach(prev_device_id);
            }
        }

        self.webhid_detach(device_id);

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
        let product = product_name.unwrap_or_else(|| "WebHID HID Device".to_string());

        let device = UsbHidPassthrough::new(
            vendor_id,
            product_id,
            "WebHID".to_string(),
            product.clone(),
            None,
            report_descriptor.clone(),
            has_interrupt_out,
            None,
            None,
            None,
        );

        let dev = Rc::new(RefCell::new(device));
        {
            let hub = self.external_hub_mut().ok_or_else(|| {
                js_error("External hub is missing (expected to be attached at root port 0)")
            })?;
            hub.attach(hub_port, Box::new(RcWebHidDevice(dev.clone())));
        }

        self.webhid_hub_ports.insert(hub_port, device_id);
        self.webhid_devices.insert(
            device_id,
            WebHidDeviceState {
                location: WebHidDeviceLocation::ExternalHubPort(hub_port),
                dev,
                vendor_id,
                product_id,
                product,
                report_descriptor,
                has_interrupt_out,
            },
        );

        Ok(())
    }

    pub fn webhid_attach_hub(
        &mut self,
        guest_path: JsValue,
        port_count: Option<u32>,
    ) -> Result<(), JsValue> {
        let root_port = parse_root_port_guest_path(guest_path)?;
        if root_port != EXTERNAL_HUB_ROOT_PORT {
            return Err(js_error(&format!(
                "Unsupported hub guestPath root port {root_port} (expected {EXTERNAL_HUB_ROOT_PORT})"
            )));
        }

        let desired = if let Some(count) = port_count {
            let clamped = clamp_hub_port_count(count);
            self.external_hub_port_count_hint = Some(clamped);
            clamped
        } else {
            self.external_hub_port_count_hint
                .unwrap_or(DEFAULT_EXTERNAL_HUB_PORT_COUNT)
        };

        self.ensure_external_hub(desired)?;
        Ok(())
    }

    pub fn webhid_detach(&mut self, device_id: u32) {
        let Some(state) = self.webhid_devices.remove(&device_id) else {
            return;
        };

        match state.location {
            WebHidDeviceLocation::RootPort(port) => {
                self.ctrl.disconnect_device(port);
                if self.webhid_ports[port] == Some(device_id) {
                    self.webhid_ports[port] = None;
                }
            }
            WebHidDeviceLocation::ExternalHubPort(port) => {
                self.webhid_hub_ports.remove(&port);
                if let Some(hub) = self.external_hub_mut() {
                    hub.detach(port);
                }
            }
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

        if let Some(preferred) = preferred_port {
            if preferred as usize != WEBUSB_ROOT_PORT {
                return Err(js_error(&format!(
                    "Invalid preferredPort {preferred} for WebUSB (expected {WEBUSB_ROOT_PORT})"
                )));
            }
        }

        // Root port 1 is reserved for WebUSB. Detach any legacy root-port WebHID device
        // that may have been attached there (older clients may not use the external hub path).
        if let Some(device_id) = self.webhid_ports[WEBUSB_ROOT_PORT] {
            self.webhid_detach(device_id);
        }

        if !self.port_is_free(WEBUSB_ROOT_PORT) {
            return Err(js_error(&format!(
                "UHCI root port {WEBUSB_ROOT_PORT} is not available for WebUSB"
            )));
        }
        let port = WEBUSB_ROOT_PORT;

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

    /// Serialize the current runtime state into a deterministic snapshot blob.
    ///
    /// Format: top-level `aero-io-snapshot` TLV with:
    /// - tag 1: `aero_usb::uhci::UhciController` snapshot bytes
    /// - tag 2: IRQ latch level (`irq_level`)
    /// - tag 3: external hub port count (if present)
    /// - tag 4: external hub snapshot bytes (if present)
    /// - tag 5: external hub port-count hint (optional)
    /// - tag 6: WebHID passthrough devices (sorted list)
    /// - tag 7: WebUSB passthrough device snapshot bytes (if present)
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_LEVEL: u16 = 2;
        const TAG_EXTERNAL_HUB_PORT_COUNT: u16 = 3;
        const TAG_EXTERNAL_HUB_STATE: u16 = 4;
        const TAG_EXTERNAL_HUB_PORT_COUNT_HINT: u16 = 5;
        const TAG_WEBHID_DEVICES: u16 = 6;
        const TAG_WEBUSB_STATE: u16 = 7;

        let mut w = SnapshotWriter::new(UHCI_RUNTIME_DEVICE_ID, UHCI_RUNTIME_DEVICE_VERSION);
        w.field_bytes(TAG_CONTROLLER, self.ctrl.save_state());
        w.field_bool(TAG_IRQ_LEVEL, self.irq.level);

        if let Some(state) = self.external_hub.as_ref() {
            w.field_u8(TAG_EXTERNAL_HUB_PORT_COUNT, state.port_count);
            if let Some(hub) = self.external_hub_ref() {
                w.field_bytes(TAG_EXTERNAL_HUB_STATE, hub.save_state());
            }
        }
        if let Some(hint) = self.external_hub_port_count_hint {
            w.field_u8(TAG_EXTERNAL_HUB_PORT_COUNT_HINT, hint);
        }

        let mut webhid_records: Vec<(u32, u8, u8, Vec<u8>)> = self
            .webhid_devices
            .iter()
            .map(|(&device_id, state)| {
                let (loc_kind, loc_port) = match state.location {
                    WebHidDeviceLocation::RootPort(port) => (0u8, port as u8),
                    WebHidDeviceLocation::ExternalHubPort(port) => (1u8, port),
                };
                let dev_state = state.dev.borrow().save_state();
                let record = Encoder::new()
                    .u32(device_id)
                    .u8(loc_kind)
                    .u8(loc_port)
                    .u16(state.vendor_id)
                    .u16(state.product_id)
                    .vec_u8(state.product.as_bytes())
                    .vec_u8(&state.report_descriptor)
                    .bool(state.has_interrupt_out)
                    .vec_u8(&dev_state)
                    .finish();
                (device_id, loc_kind, loc_port, record)
            })
            .collect();
        webhid_records
            .sort_by_key(|(device_id, loc_kind, loc_port, _)| (*device_id, *loc_kind, *loc_port));
        let webhid_bytes: Vec<Vec<u8>> = webhid_records
            .into_iter()
            .map(|(_, _, _, record)| record)
            .collect();
        w.field_bytes(
            TAG_WEBHID_DEVICES,
            Encoder::new().vec_bytes(&webhid_bytes).finish(),
        );

        if let Some(webusb) = self.webusb.as_ref() {
            w.field_bytes(TAG_WEBUSB_STATE, webusb.dev.borrow().save_state());
        }

        w.finish()
    }

    /// Restore runtime state from a snapshot blob produced by [`save_state`].
    ///
    /// This drops any in-flight host actions/completions for passthrough devices (WebUSB/WebHID)
    /// as per their `aero-io-snapshot` semantics.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        if bytes.len() > MAX_USB_SNAPSHOT_BYTES {
            return Err(js_error(&format!(
                "USB snapshot too large ({} bytes, max {})",
                bytes.len(),
                MAX_USB_SNAPSHOT_BYTES
            )));
        }

        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_LEVEL: u16 = 2;
        const TAG_EXTERNAL_HUB_PORT_COUNT: u16 = 3;
        const TAG_EXTERNAL_HUB_STATE: u16 = 4;
        const TAG_EXTERNAL_HUB_PORT_COUNT_HINT: u16 = 5;
        const TAG_WEBHID_DEVICES: u16 = 6;
        const TAG_WEBUSB_STATE: u16 = 7;

        #[derive(Debug)]
        struct WebHidSnapshotEntry {
            device_id: u32,
            location: WebHidDeviceLocation,
            vendor_id: u16,
            product_id: u16,
            product: String,
            report_descriptor: Vec<u8>,
            has_interrupt_out: bool,
            state: Vec<u8>,
        }

        let r = SnapshotReader::parse(bytes, UHCI_RUNTIME_DEVICE_ID)
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot: {e}")))?;
        r.ensure_device_major(UHCI_RUNTIME_DEVICE_VERSION.major)
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot: {e}")))?;

        let ctrl_bytes = r
            .bytes(TAG_CONTROLLER)
            .ok_or_else(|| js_error("UHCI runtime snapshot missing controller state"))?;
        let irq_level = r
            .bool(TAG_IRQ_LEVEL)
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot IRQ latch: {e}")))?
            .unwrap_or(false);

        let hub_port_count = r.u8(TAG_EXTERNAL_HUB_PORT_COUNT).map_err(|e| {
            js_error(&format!(
                "Invalid UHCI runtime snapshot hub port count: {e}"
            ))
        })?;
        let hub_state_bytes = r.bytes(TAG_EXTERNAL_HUB_STATE);
        if hub_port_count.is_some() ^ hub_state_bytes.is_some() {
            return Err(js_error(
                "UHCI runtime snapshot has inconsistent external hub fields (expected both portCount + state)",
            ));
        }
        if let Some(count) = hub_port_count {
            if count == 0 {
                return Err(js_error(
                    "UHCI runtime snapshot has invalid external hub port count 0",
                ));
            }
        }

        let hub_port_count_hint = r
            .u8(TAG_EXTERNAL_HUB_PORT_COUNT_HINT)
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot hub port hint: {e}")))?
            .and_then(|v| (v != 0).then_some(v));

        let webusb_state_bytes = r.bytes(TAG_WEBUSB_STATE);

        let webhid_entries: Vec<WebHidSnapshotEntry> = if let Some(buf) =
            r.bytes(TAG_WEBHID_DEVICES)
        {
            let mut d = Decoder::new(buf);
            let records = d.vec_bytes().map_err(|e| {
                js_error(&format!("Invalid UHCI runtime snapshot WebHID list: {e}"))
            })?;
            d.finish().map_err(|e| {
                js_error(&format!("Invalid UHCI runtime snapshot WebHID list: {e}"))
            })?;

            let mut out = Vec::with_capacity(records.len());
            for (idx, rec) in records.into_iter().enumerate() {
                let mut rd = Decoder::new(&rec);
                let device_id = rd
                    .u32()
                    .map_err(|e| js_error(&format!("Invalid WebHID record #{idx}: {e}")))?;
                let loc_kind = rd.u8().map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} location: {e}"))
                })?;
                let loc_port = rd.u8().map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} location: {e}"))
                })?;
                let vendor_id = rd.u16().map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} vendorId: {e}"))
                })?;
                let product_id = rd.u16().map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} productId: {e}"))
                })?;
                let product_bytes = rd.vec_u8().map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} product: {e}"))
                })?;
                let product = String::from_utf8(product_bytes).map_err(|_| {
                    js_error(&format!(
                        "Invalid WebHID record {device_id} product: expected UTF-8 string"
                    ))
                })?;
                let report_descriptor = rd.vec_u8().map_err(|e| {
                    js_error(&format!(
                        "Invalid WebHID record {device_id} report descriptor: {e}"
                    ))
                })?;
                let has_interrupt_out = rd.bool().map_err(|e| {
                    js_error(&format!(
                        "Invalid WebHID record {device_id} hasInterruptOut: {e}"
                    ))
                })?;
                let state = rd.vec_u8().map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} state: {e}"))
                })?;
                rd.finish().map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} encoding: {e}"))
                })?;

                let location = match loc_kind {
                    0 => WebHidDeviceLocation::RootPort(loc_port as usize),
                    1 => {
                        if loc_port == 0 {
                            return Err(js_error(&format!(
                                "Invalid WebHID record {device_id}: hub port 0 is not valid"
                            )));
                        }
                        WebHidDeviceLocation::ExternalHubPort(loc_port)
                    }
                    _ => {
                        return Err(js_error(&format!(
                            "Invalid WebHID record {device_id}: unknown location kind {loc_kind}"
                        )));
                    }
                };

                out.push(WebHidSnapshotEntry {
                    device_id,
                    location,
                    vendor_id,
                    product_id,
                    product,
                    report_descriptor,
                    has_interrupt_out,
                    state,
                });
            }
            out
        } else {
            Vec::new()
        };

        // Validate WebHID entries against hub/webusb presence so we can fail before mutating state.
        {
            let mut ids = std::collections::HashMap::new();
            let mut root_ports = std::collections::HashMap::new();
            let mut hub_ports = std::collections::HashMap::new();

            for entry in &webhid_entries {
                if let Some(prev) = ids.insert(entry.device_id, ()) {
                    let _ = prev;
                    return Err(js_error(&format!(
                        "UHCI runtime snapshot has duplicate WebHID deviceId {}",
                        entry.device_id
                    )));
                }
                match entry.location {
                    WebHidDeviceLocation::RootPort(port) => {
                        if port >= PORT_COUNT {
                            return Err(js_error(&format!(
                                "UHCI runtime snapshot has invalid WebHID root port {port} (expected 0..{})",
                                PORT_COUNT - 1
                            )));
                        }
                        if webusb_state_bytes.is_some() && port == WEBUSB_ROOT_PORT {
                            return Err(js_error(&format!(
                                "UHCI runtime snapshot attaches WebHID deviceId {} to root port {} reserved for WebUSB",
                                entry.device_id, WEBUSB_ROOT_PORT
                            )));
                        }
                        if hub_port_count.is_some() && port == EXTERNAL_HUB_ROOT_PORT {
                            return Err(js_error(&format!(
                                "UHCI runtime snapshot attaches WebHID deviceId {} to root port {} reserved for the external hub",
                                entry.device_id, EXTERNAL_HUB_ROOT_PORT
                            )));
                        }
                        if let Some(prev) = root_ports.insert(port, entry.device_id) {
                            return Err(js_error(&format!(
                                "UHCI runtime snapshot attaches multiple WebHID devices to root port {port} (deviceId {prev} and {})",
                                entry.device_id
                            )));
                        }
                    }
                    WebHidDeviceLocation::ExternalHubPort(port) => {
                        let Some(hub_count) = hub_port_count else {
                            return Err(js_error(&format!(
                                "UHCI runtime snapshot attaches WebHID deviceId {} behind external hub port {port}, but no external hub snapshot is present",
                                entry.device_id
                            )));
                        };
                        if port == 0 || port > hub_count {
                            return Err(js_error(&format!(
                                "UHCI runtime snapshot attaches WebHID deviceId {} to invalid external hub port {port} (hub has {hub_count} ports)",
                                entry.device_id
                            )));
                        }
                        if let Some(prev) = hub_ports.insert(port, entry.device_id) {
                            return Err(js_error(&format!(
                                "UHCI runtime snapshot attaches multiple WebHID devices to external hub port {port} (deviceId {prev} and {})",
                                entry.device_id
                            )));
                        }
                    }
                }
            }
        }

        // Snapshot header validated and decoded successfully. Clear any existing runtime state before
        // applying the restored snapshot.
        self.reset_for_snapshot_restore();

        self.external_hub_port_count_hint = hub_port_count_hint;

        // Recreate the external hub (if present) and attach all downstream devices before applying
        // the hub/controller snapshots.
        if let Some(port_count) = hub_port_count {
            let hub = UsbHubDevice::new_with_ports(port_count as usize);
            self.ctrl
                .connect_device(EXTERNAL_HUB_ROOT_PORT, Box::new(hub));
            self.external_hub = Some(ExternalHubState { port_count });
        }

        // Restore WebUSB passthrough device first so root-port occupancy is correct.
        if let Some(buf) = webusb_state_bytes {
            let port = WEBUSB_ROOT_PORT;
            let dev = Rc::new(RefCell::new(aero_usb::UsbWebUsbPassthroughDevice::new()));
            self.ctrl
                .connect_device(port, Box::new(RcWebUsbDevice(dev.clone())));
            self.webusb = Some(WebUsbDeviceState {
                port,
                dev: dev.clone(),
            });
            if let Err(err) = dev.borrow_mut().load_state(buf) {
                self.reset_for_snapshot_restore();
                return Err(js_error(&format!(
                    "Invalid UHCI runtime snapshot WebUSB device state: {err}"
                )));
            }
            // WebUSB host actions are backed by JS Promises and cannot be resumed after a VM
            // snapshot restore. Drop any inflight/queued host bookkeeping so UHCI TD retries
            // re-emit fresh actions.
            dev.borrow_mut().reset_host_state_for_restore();
        }

        // Recreate WebHID devices (using stored static config), then apply their dynamic snapshots.
        for entry in webhid_entries {
            let device = UsbHidPassthrough::new(
                entry.vendor_id,
                entry.product_id,
                "WebHID".to_string(),
                entry.product.clone(),
                None,
                entry.report_descriptor.clone(),
                entry.has_interrupt_out,
                None,
                None,
                None,
            );
            let dev = Rc::new(RefCell::new(device));

            match entry.location {
                WebHidDeviceLocation::RootPort(port) => {
                    if !self.port_is_free(port) {
                        self.reset_for_snapshot_restore();
                        return Err(js_error(&format!(
                            "UHCI runtime snapshot WebHID deviceId {} cannot attach to root port {port}: port is not available",
                            entry.device_id
                        )));
                    }
                    self.ctrl
                        .connect_device(port, Box::new(RcWebHidDevice(dev.clone())));
                    self.webhid_ports[port] = Some(entry.device_id);
                }
                WebHidDeviceLocation::ExternalHubPort(hub_port) => {
                    self.webhid_hub_ports.insert(hub_port, entry.device_id);
                    let Some(hub) = self.external_hub_mut() else {
                        self.reset_for_snapshot_restore();
                        return Err(js_error(&format!(
                            "UHCI runtime snapshot WebHID deviceId {} expects external hub, but hub is missing",
                            entry.device_id
                        )));
                    };
                    hub.attach(hub_port, Box::new(RcWebHidDevice(dev.clone())));
                }
            }

            self.webhid_devices.insert(
                entry.device_id,
                WebHidDeviceState {
                    location: entry.location,
                    dev: dev.clone(),
                    vendor_id: entry.vendor_id,
                    product_id: entry.product_id,
                    product: entry.product,
                    report_descriptor: entry.report_descriptor,
                    has_interrupt_out: entry.has_interrupt_out,
                },
            );

            if let Err(err) = dev.borrow_mut().load_state(&entry.state) {
                self.reset_for_snapshot_restore();
                return Err(js_error(&format!(
                    "Invalid UHCI runtime snapshot WebHID deviceId {} state: {err}",
                    entry.device_id
                )));
            }
        }

        // Restore hub dynamic state after attaching downstream devices.
        if let Some(hub_state) = hub_state_bytes {
            let Some(hub) = self.external_hub_mut() else {
                self.reset_for_snapshot_restore();
                return Err(js_error(
                    "UHCI runtime snapshot includes external hub state but hub is missing",
                ));
            };
            if let Err(err) = hub.load_state(hub_state) {
                self.reset_for_snapshot_restore();
                return Err(js_error(&format!(
                    "Invalid UHCI runtime snapshot external hub state: {err}"
                )));
            }
        }

        // Load controller state last so port connected/enabled flags and timers match the snapshot
        // after all devices are attached.
        if let Err(err) = self.ctrl.load_state(ctrl_bytes) {
            self.reset_for_snapshot_restore();
            return Err(js_error(&format!(
                "Invalid UHCI runtime snapshot controller state: {err}"
            )));
        }

        self.irq.level = irq_level;

        Ok(())
    }

    /// Snapshot the full UHCI runtime USB state as deterministic bytes.
    ///
    /// The returned bytes represent only the USB stack state (controller + devices), not guest RAM.
    pub fn snapshot_state(&self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore UHCI runtime USB state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }
}

impl UhciRuntime {
    fn port_is_free(&self, port: usize) -> bool {
        if port >= PORT_COUNT {
            return false;
        }
        if port == EXTERNAL_HUB_ROOT_PORT && self.external_hub.is_some() {
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

    fn external_hub_mut(&mut self) -> Option<&mut UsbHubDevice> {
        let port = self.ctrl.bus_mut().port_mut(EXTERNAL_HUB_ROOT_PORT)?;
        let dev = port.device.as_mut()?;
        dev.as_any_mut().downcast_mut::<UsbHubDevice>()
    }

    fn external_hub_ref(&self) -> Option<&UsbHubDevice> {
        let port = self.ctrl.bus().port(EXTERNAL_HUB_ROOT_PORT)?;
        let dev = port.device.as_ref()?;
        dev.as_any().downcast_ref::<UsbHubDevice>()
    }

    fn reset_for_snapshot_restore(&mut self) {
        self.ctrl = UhciController::new(DEFAULT_IO_BASE, DEFAULT_IRQ_LINE);
        self.irq = RuntimeIrq::default();

        self.webhid_devices.clear();
        self.webhid_ports = [None; PORT_COUNT];
        self.webhid_hub_ports.clear();

        self.external_hub = None;
        self.external_hub_port_count_hint = None;

        self.webusb = None;
    }

    fn ensure_external_hub(&mut self, min_hub_port: u8) -> Result<(), JsValue> {
        if min_hub_port == 0 {
            return Err(js_error(
                "Invalid hub port 0 (hub port numbers are 1-based, expected 1..=255)",
            ));
        }

        let hint = self
            .external_hub_port_count_hint
            .unwrap_or(DEFAULT_EXTERNAL_HUB_PORT_COUNT);
        let desired = hint.max(min_hub_port);

        if let Some(state) = self.external_hub.as_mut() {
            if state.port_count >= desired {
                return Ok(());
            }
            self.grow_external_hub(desired)?;
            return Ok(());
        }

        // Ensure root port 0 is free before attaching the hub.
        if let Some(device_id) = self.webhid_ports[EXTERNAL_HUB_ROOT_PORT] {
            self.webhid_detach(device_id);
        }
        let webusb_on_root = self
            .webusb
            .as_ref()
            .is_some_and(|webusb| webusb.port == EXTERNAL_HUB_ROOT_PORT);
        if webusb_on_root {
            self.webusb_detach();
        }

        let hub = UsbHubDevice::new_with_ports(desired as usize);
        self.ctrl
            .connect_device(EXTERNAL_HUB_ROOT_PORT, Box::new(hub));
        self.external_hub = Some(ExternalHubState {
            port_count: desired,
        });
        Ok(())
    }

    fn grow_external_hub(&mut self, new_port_count: u8) -> Result<(), JsValue> {
        let Some(state) = self.external_hub.as_mut() else {
            return Err(js_error("Cannot grow external hub: hub is not attached"));
        };
        if new_port_count <= state.port_count {
            return Ok(());
        }

        // Replace the hub device at root port 0 so the guest sees a real hotplug event and can
        // re-read the hub descriptor (port count, etc).
        self.ctrl.disconnect_device(EXTERNAL_HUB_ROOT_PORT);

        let hub = UsbHubDevice::new_with_ports(new_port_count as usize);
        self.ctrl
            .connect_device(EXTERNAL_HUB_ROOT_PORT, Box::new(hub));
        state.port_count = new_port_count;

        // Reattach any existing downstream devices behind the new hub.
        let to_reattach: Vec<(u8, Rc<RefCell<UsbHidPassthrough>>)> = self
            .webhid_hub_ports
            .iter()
            .filter_map(|(&hub_port, &device_id)| {
                let rec = self.webhid_devices.get(&device_id)?;
                match rec.location {
                    WebHidDeviceLocation::ExternalHubPort(p) if p == hub_port => {
                        Some((hub_port, rec.dev.clone()))
                    }
                    _ => None,
                }
            })
            .collect();

        let hub = self
            .external_hub_mut()
            .ok_or_else(|| js_error("External hub missing after grow operation"))?;
        for (hub_port, dev) in to_reattach {
            hub.attach(hub_port, Box::new(RcWebHidDevice(dev)));
        }

        Ok(())
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
fn clamp_hub_port_count(value: u32) -> u8 {
    let value = value.clamp(1, u32::from(u8::MAX));
    value as u8
}

fn parse_root_port_guest_path(path: JsValue) -> Result<usize, JsValue> {
    let path: Vec<u32> = serde_wasm_bindgen::from_value(path)
        .map_err(|err| js_error(&format!("Invalid guestPath: {err}")))?;
    let Some(&root_port) = path.first() else {
        return Err(js_error("guestPath must not be empty"));
    };
    if root_port > u32::from(u8::MAX) {
        return Err(js_error(&format!(
            "guestPath root port {root_port} is out of range"
        )));
    }
    Ok(root_port as usize)
}

fn parse_external_hub_guest_path(path: JsValue) -> Result<(usize, u8), JsValue> {
    let path: Vec<u32> = serde_wasm_bindgen::from_value(path)
        .map_err(|err| js_error(&format!("Invalid guestPath: {err}")))?;
    if path.len() < 2 {
        return Err(js_error(
            "guestPath must include a downstream hub port (expected [rootPort, hubPort])",
        ));
    }
    if path.len() > 2 {
        return Err(js_error(
            "Nested hub guestPath segments are not supported by UhciRuntime yet",
        ));
    }
    let root = path[0] as usize;
    let hub_port = path[1];
    let hub_port_u8 = u8::try_from(hub_port)
        .map_err(|_| js_error("guestPath hub port is out of range (expected 1..=255)"))?;
    if hub_port_u8 == 0 {
        return Err(js_error("guestPath hub port is invalid (expected 1..=255)"));
    }
    Ok((root, hub_port_u8))
}
