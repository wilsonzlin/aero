#![cfg(target_arch = "wasm32")]

use std::collections::HashMap;

use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::hid::passthrough::{UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};
use aero_usb::hid::webhid;
use aero_usb::hub::{UsbHub, UsbHubDevice};
use aero_usb::passthrough::{
    SetupPacket as PassthroughSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
    UsbHostCompletionOut,
};
use aero_usb::uhci::UhciController;
use aero_usb::{MemoryBus, UsbWebUsbPassthroughDevice};

const DEFAULT_IO_BASE: u16 = 0x5000;
const DEFAULT_IRQ_LINE: u8 = 11;
const PORT_COUNT: usize = 2;
const EXTERNAL_HUB_ROOT_PORT: usize = 0;
const DEFAULT_EXTERNAL_HUB_PORT_COUNT: u8 = 16;
const WEBUSB_ROOT_PORT: usize = 1;
const MAX_USB_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
const MAX_WEBHID_SNAPSHOT_DEVICES: usize = 1024;
const MAX_USB_STRING_DESCRIPTOR_UTF16_UNITS: usize = 126;

const UHCI_RUNTIME_DEVICE_ID: [u8; 4] = *b"UHRT";
const UHCI_RUNTIME_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

struct LinearGuestMemory {
    guest_base: u32,
    guest_size: u32,
}

impl LinearGuestMemory {
    fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        let pages = core::arch::wasm32::memory_size(0) as u64;
        let mem_bytes = pages.saturating_mul(64 * 1024);

        // Keep guest RAM below the PCI MMIO aperture (see `guest_ram_layout` contract).
        let guest_size_u64 = u64::from(guest_size).min(crate::guest_layout::PCI_MMIO_BASE);
        let guest_size: u32 = guest_size_u64
            .try_into()
            .map_err(|_| js_error("guest_size does not fit in u32"))?;

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

    fn translate(&self, paddr: u64, len: usize) -> Option<u32> {
        let paddr_u32 = u32::try_from(paddr).ok()?;
        if paddr_u32 >= self.guest_size {
            return None;
        }
        let end = paddr_u32.checked_add(len as u32)?;
        if end > self.guest_size {
            return None;
        }
        self.guest_base.checked_add(paddr_u32)
    }
}

impl MemoryBus for LinearGuestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let Some(linear) = self.translate(paddr, buf.len()) else {
            buf.fill(0);
            return;
        };

        unsafe {
            let src = core::slice::from_raw_parts(linear as *const u8, buf.len());
            buf.copy_from_slice(src);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let Some(linear) = self.translate(paddr, buf.len()) else {
            return;
        };

        unsafe {
            let dst = core::slice::from_raw_parts_mut(linear as *mut u8, buf.len());
            dst.copy_from_slice(buf);
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

fn parse_webhid_collections(
    collections_json: &JsValue,
) -> Result<Vec<webhid::HidCollectionInfo>, JsValue> {
    let collections_json_str = js_sys::JSON::stringify(collections_json)
        .map_err(|err| {
            js_error(&format!(
                "Invalid WebHID collection schema (stringify failed): {err:?}"
            ))
        })?
        .as_string()
        .ok_or_else(|| {
            js_error("Invalid WebHID collection schema (stringify returned non-string)")
        })?;

    let mut deserializer = serde_json::Deserializer::from_str(&collections_json_str);
    serde_path_to_error::deserialize(&mut deserializer)
        .map_err(|err| js_error(&format!("Invalid WebHID collection schema: {err}")))
}
struct WebHidDeviceState {
    location: WebHidDeviceLocation,
    dev: UsbHidPassthroughHandle,
    vendor_id: u16,
    product_id: u16,
    product: String,
    report_descriptor: Vec<u8>,
    has_interrupt_out: bool,
}

struct WebUsbDeviceState {
    port: usize,
    dev: UsbWebUsbPassthroughDevice,
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
    io_base: u16,
    irq_line: u8,

    webhid_devices: HashMap<u32, WebHidDeviceState>,
    webhid_ports: [Option<u32>; PORT_COUNT],
    webhid_hub_ports: HashMap<u8, u32>,

    external_hub: Option<ExternalHubState>,
    external_hub_port_count_hint: Option<u8>,

    /// Externally managed USB HID passthrough devices attached via
    /// [`UhciRuntime::attach_usb_hid_passthrough_device`].
    ///
    /// These are usually synthetic browser input devices (keyboard/mouse/gamepad) that live in JS
    /// as `UsbHidPassthroughBridge` instances. We track their handles so we can reattach them when
    /// the external hub is replaced (e.g. due to a port-count grow) or during snapshot restore.
    ///
    /// NOTE: The UHCI runtime snapshot captures the *USB topology state* (hub ports + per-device
    /// dynamic state) inside the controller/hub snapshots. However, because these devices are
    /// created/owned externally (in JS), the runtime cannot recreate them from the snapshot alone.
    ///
    /// Instead, we require that the host has attached the devices at least once (so the runtime
    /// has a `UsbHidPassthroughHandle` to clone), and then we reattach those handles before
    /// applying the hub snapshot so the saved device state can be restored.
    usb_hid_passthrough_devices: HashMap<Vec<u8>, UsbHidPassthroughHandle>,

    webusb: Option<WebUsbDeviceState>,
}

#[wasm_bindgen]
impl UhciRuntime {
    fn sanitize_usb_string(s: &str) -> String {
        let mut units = 0usize;
        let mut end = 0usize;
        for (idx, ch) in s.char_indices() {
            let next_units = units + ch.len_utf16();
            if next_units > MAX_USB_STRING_DESCRIPTOR_UTF16_UNITS {
                break;
            }
            units = next_units;
            end = idx + ch.len_utf8();
        }
        if end == s.len() {
            s.to_string()
        } else {
            s[..end].to_string()
        }
    }

    fn sanitize_usb_string_owned(mut s: String) -> String {
        let mut units = 0usize;
        let mut end = 0usize;
        for (idx, ch) in s.char_indices() {
            let next_units = units + ch.len_utf16();
            if next_units > MAX_USB_STRING_DESCRIPTOR_UTF16_UNITS {
                break;
            }
            units = next_units;
            end = idx + ch.len_utf8();
        }
        if end < s.len() {
            s.truncate(end);
            // `truncate` preserves capacity; ensure we don't keep the original allocation if a host
            // integration passes a very large product string.
            s.shrink_to_fit();
        }
        s
    }

    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        let mem = LinearGuestMemory::new(guest_base, guest_size)?;
        Ok(Self {
            ctrl: UhciController::new(),
            mem,
            io_base: DEFAULT_IO_BASE,
            irq_line: DEFAULT_IRQ_LINE,
            webhid_devices: HashMap::new(),
            webhid_ports: [None, None],
            webhid_hub_ports: HashMap::new(),
            external_hub: None,
            external_hub_port_count_hint: None,
            usb_hid_passthrough_devices: HashMap::new(),
            webusb: None,
        })
    }

    pub fn io_base(&self) -> u16 {
        self.io_base
    }

    pub fn irq_line(&self) -> u8 {
        self.irq_line
    }

    pub fn irq_level(&self) -> bool {
        self.ctrl.irq_level()
    }

    pub fn port_read(&mut self, offset: u16, size: u8) -> u32 {
        let size = size as usize;
        match size {
            1 | 2 | 4 => self.ctrl.io_read(offset, size),
            _ => 0xFFFF_FFFF,
        }
    }

    pub fn port_write(&mut self, offset: u16, size: u8, value: u32) {
        let size = size as usize;
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        self.ctrl.io_write(offset, size, value);
    }

    pub fn tick_1ms(&mut self) {
        self.step_frame();
    }

    pub fn step_frame(&mut self) {
        self.ctrl.tick_1ms(&mut self.mem);
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

        let collections = parse_webhid_collections(&collections_json)?;

        let report_descriptor =
            webhid::synthesize_report_descriptor(&collections).map_err(|err| {
                js_error(&format!(
                    "Failed to synthesize HID report descriptor: {err}"
                ))
            })?;

        let has_interrupt_out = collections_have_output_reports(&collections);
        let product = Self::sanitize_usb_string_owned(
            product_name.unwrap_or_else(|| "WebHID HID Device".to_string()),
        );

        let dev = UsbHidPassthroughHandle::new(
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

        self.ctrl.hub_mut().attach(port, Box::new(dev.clone()));

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

        let collections = parse_webhid_collections(&collections_json)?;

        let report_descriptor =
            webhid::synthesize_report_descriptor(&collections).map_err(|err| {
                js_error(&format!(
                    "Failed to synthesize HID report descriptor: {err}"
                ))
            })?;

        let has_interrupt_out = collections_have_output_reports(&collections);
        let product = Self::sanitize_usb_string_owned(
            product_name.unwrap_or_else(|| "WebHID HID Device".to_string()),
        );

        let dev = UsbHidPassthroughHandle::new(
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

        let path = [EXTERNAL_HUB_ROOT_PORT as u8, hub_port];
        crate::uhci_controller_bridge::attach_device_at_path(
            &mut self.ctrl,
            &path,
            Box::new(dev.clone()),
        )?;

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
            let validated = validate_hub_port_count(count)?;
            self.external_hub_port_count_hint = Some(validated);
            validated
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
                self.ctrl.hub_mut().detach(port);
                if self.webhid_ports[port] == Some(device_id) {
                    self.webhid_ports[port] = None;
                }
            }
            WebHidDeviceLocation::ExternalHubPort(port) => {
                self.webhid_hub_ports.remove(&port);
                let path = [EXTERNAL_HUB_ROOT_PORT as u8, port];
                let _ = self.ctrl.hub_mut().detach_at_path(&path);
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

        state.dev.push_input_report(report_id, data);
        Ok(())
    }

    /// Attach a generic USB HID passthrough device at the given guest USB topology path.
    ///
    /// This is primarily used for Aero's synthetic browser input devices (keyboard/mouse/gamepad),
    /// which are modeled as fixed USB HID devices behind the external hub on root port 0.
    pub fn attach_usb_hid_passthrough_device(
        &mut self,
        path: JsValue,
        device: &crate::UsbHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        if path.len() < 2 {
            return Err(js_error(
                "USB HID passthrough devices must attach behind the external hub (expected path like [0, <hubPort>])",
            ));
        }
        if path.len() != 2 {
            return Err(js_error(
                "Nested USB topology paths are not supported by UhciRuntime yet (expected [0, <hubPort>])",
            ));
        }
        if path[0] as usize != EXTERNAL_HUB_ROOT_PORT {
            return Err(js_error(
                "USB HID passthrough devices must attach behind the external hub on root port 0",
            ));
        }

        let hub_port = path[1];
        self.ensure_external_hub(hub_port)?;
        if self.webhid_hub_ports.contains_key(&hub_port) {
            return Err(js_error(&format!(
                "USB HID passthrough device cannot attach to external hub port {hub_port}: port is occupied by a WebHID device"
            )));
        }

        let dev = device.as_usb_device();
        crate::uhci_controller_bridge::attach_device_at_path(
            &mut self.ctrl,
            &path,
            Box::new(dev.clone()),
        )?;
        // Remember the handle so we can reattach it after hub replacement / snapshot restore.
        self.usb_hid_passthrough_devices.insert(path, dev);
        Ok(())
    }

    pub fn webhid_drain_output_reports(&mut self) -> JsValue {
        let out = Array::new();
        for (&device_id, state) in self.webhid_devices.iter_mut() {
            loop {
                let report = state.dev.pop_output_report();
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

        if !self.port_is_free(WEBUSB_ROOT_PORT) {
            return Err(js_error(&format!(
                "UHCI root port {WEBUSB_ROOT_PORT} is not available for WebUSB"
            )));
        }

        let port = WEBUSB_ROOT_PORT;
        let dev = UsbWebUsbPassthroughDevice::new();
        self.ctrl.hub_mut().attach(port, Box::new(dev.clone()));
        self.webusb = Some(WebUsbDeviceState { port, dev });
        Ok(port as u32)
    }

    pub fn webusb_detach(&mut self) {
        let Some(state) = self.webusb.take() else {
            return;
        };
        self.ctrl.hub_mut().detach(state.port);
    }

    pub fn webusb_drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let actions: Vec<UsbHostAction> = if let Some(state) = self.webusb.as_ref() {
            state.dev.drain_actions()
        } else {
            Vec::new()
        };
        let out = Array::new();
        for action in actions {
            out.push(&webusb_action_to_js(action));
        }
        Ok(out.into())
    }

    pub fn webusb_push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let Some(state) = self.webusb.as_ref() else {
            return Ok(());
        };
        let obj: Object = completion
            .dyn_into()
            .map_err(|_| js_error("Invalid UsbHostCompletion: expected an object"))?;

        let kind = Reflect::get(&obj, &JsValue::from_str("kind"))
            .map_err(|_| js_error("Invalid UsbHostCompletion: missing kind"))?
            .as_string()
            .ok_or_else(|| js_error("Invalid UsbHostCompletion: kind must be a string"))?;

        let id = Reflect::get(&obj, &JsValue::from_str("id"))
            .map_err(|_| js_error("Invalid UsbHostCompletion: missing id"))?
            .as_f64()
            .and_then(|v| {
                if v.is_finite() && v.fract() == 0.0 && v >= 0.0 && v <= u32::MAX as f64 {
                    Some(v as u32)
                } else {
                    None
                }
            })
            .ok_or_else(|| js_error("Invalid UsbHostCompletion: id must be a u32 number"))?;

        let status = Reflect::get(&obj, &JsValue::from_str("status"))
            .map_err(|_| js_error("Invalid UsbHostCompletion: missing status"))?
            .as_string()
            .ok_or_else(|| js_error("Invalid UsbHostCompletion: status must be a string"))?;

        let read_data_bytes = || -> Result<Vec<u8>, JsValue> {
            let val = Reflect::get(&obj, &JsValue::from_str("data"))
                .map_err(|_| js_error("Invalid UsbHostCompletion: missing data"))?;

            if let Ok(buf) = val.clone().dyn_into::<Uint8Array>() {
                return Ok(buf.to_vec());
            }

            if Array::is_array(&val) {
                let arr = Array::from(&val);
                let mut out = Vec::with_capacity(arr.length() as usize);
                for i in 0..arr.length() {
                    let b = arr
                        .get(i)
                        .as_f64()
                        .and_then(|v| {
                            if v.is_finite() && v.fract() == 0.0 && v >= 0.0 && v <= 255.0 {
                                Some(v as u8)
                            } else {
                                None
                            }
                        })
                        .ok_or_else(|| {
                            js_error("Invalid UsbHostCompletion: data must be a Uint8Array or number[]")
                        })?;
                    out.push(b);
                }
                return Ok(out);
            }

            Err(js_error(
                "Invalid UsbHostCompletion: data must be a Uint8Array or number[]",
            ))
        };

        let completion = match kind.as_str() {
            "controlIn" => UsbHostCompletion::ControlIn {
                id,
                result: match status.as_str() {
                    "success" => UsbHostCompletionIn::Success {
                        data: read_data_bytes()?,
                    },
                    "stall" => UsbHostCompletionIn::Stall,
                    "error" => {
                        let msg = Reflect::get(&obj, &JsValue::from_str("message"))
                            .map_err(|_| js_error("Invalid UsbHostCompletion: missing message"))?
                            .as_string()
                            .ok_or_else(|| {
                                js_error("Invalid UsbHostCompletion: message must be a string")
                            })?;
                        UsbHostCompletionIn::Error { message: msg }
                    }
                    other => {
                        return Err(js_error(&format!(
                            "Invalid UsbHostCompletion: unknown status {other} (expected success|stall|error)"
                        )));
                    }
                },
            },
            "bulkIn" => UsbHostCompletion::BulkIn {
                id,
                result: match status.as_str() {
                    "success" => UsbHostCompletionIn::Success {
                        data: read_data_bytes()?,
                    },
                    "stall" => UsbHostCompletionIn::Stall,
                    "error" => {
                        let msg = Reflect::get(&obj, &JsValue::from_str("message"))
                            .map_err(|_| js_error("Invalid UsbHostCompletion: missing message"))?
                            .as_string()
                            .ok_or_else(|| {
                                js_error("Invalid UsbHostCompletion: message must be a string")
                            })?;
                        UsbHostCompletionIn::Error { message: msg }
                    }
                    other => {
                        return Err(js_error(&format!(
                            "Invalid UsbHostCompletion: unknown status {other} (expected success|stall|error)"
                        )));
                    }
                },
            },
            "controlOut" => UsbHostCompletion::ControlOut {
                id,
                result: match status.as_str() {
                    "success" => {
                        let bytes_written = Reflect::get(&obj, &JsValue::from_str("bytesWritten"))
                            .map_err(|_| js_error("Invalid UsbHostCompletion: missing bytesWritten"))?
                            .as_f64()
                            .and_then(|v| {
                                if v.is_finite()
                                    && v.fract() == 0.0
                                    && v >= 0.0
                                    && v <= u32::MAX as f64
                                {
                                    Some(v as u32)
                                } else {
                                    None
                                }
                            })
                            .ok_or_else(|| {
                                js_error("Invalid UsbHostCompletion: bytesWritten must be a u32 number")
                            })?;
                        UsbHostCompletionOut::Success { bytes_written }
                    }
                    "stall" => UsbHostCompletionOut::Stall,
                    "error" => {
                        let msg = Reflect::get(&obj, &JsValue::from_str("message"))
                            .map_err(|_| js_error("Invalid UsbHostCompletion: missing message"))?
                            .as_string()
                            .ok_or_else(|| {
                                js_error("Invalid UsbHostCompletion: message must be a string")
                            })?;
                        UsbHostCompletionOut::Error { message: msg }
                    }
                    other => {
                        return Err(js_error(&format!(
                            "Invalid UsbHostCompletion: unknown status {other} (expected success|stall|error)"
                        )));
                    }
                },
            },
            "bulkOut" => UsbHostCompletion::BulkOut {
                id,
                result: match status.as_str() {
                    "success" => {
                        let bytes_written = Reflect::get(&obj, &JsValue::from_str("bytesWritten"))
                            .map_err(|_| js_error("Invalid UsbHostCompletion: missing bytesWritten"))?
                            .as_f64()
                            .and_then(|v| {
                                if v.is_finite()
                                    && v.fract() == 0.0
                                    && v >= 0.0
                                    && v <= u32::MAX as f64
                                {
                                    Some(v as u32)
                                } else {
                                    None
                                }
                            })
                            .ok_or_else(|| {
                                js_error("Invalid UsbHostCompletion: bytesWritten must be a u32 number")
                            })?;
                        UsbHostCompletionOut::Success { bytes_written }
                    }
                    "stall" => UsbHostCompletionOut::Stall,
                    "error" => {
                        let msg = Reflect::get(&obj, &JsValue::from_str("message"))
                            .map_err(|_| js_error("Invalid UsbHostCompletion: missing message"))?
                            .as_string()
                            .ok_or_else(|| {
                                js_error("Invalid UsbHostCompletion: message must be a string")
                            })?;
                        UsbHostCompletionOut::Error { message: msg }
                    }
                    other => {
                        return Err(js_error(&format!(
                            "Invalid UsbHostCompletion: unknown status {other} (expected success|stall|error)"
                        )));
                    }
                },
            },
            other => {
                return Err(js_error(&format!(
                    "Invalid UsbHostCompletion: unknown kind {other}"
                )));
            }
        };
        state.dev.push_completion(completion);
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
        w.field_bool(TAG_IRQ_LEVEL, self.ctrl.irq_level());

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
                let dev_state = state.dev.save_state();
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
            w.field_bytes(TAG_WEBUSB_STATE, webusb.dev.save_state());
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
        let _irq_level = r
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
            let count = d
                .u32()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot WebHID list: {e}")))?
                as usize;
            if count > MAX_WEBHID_SNAPSHOT_DEVICES {
                return Err(js_error(&format!(
                    "UHCI runtime snapshot has too many WebHID devices ({count}, max {MAX_WEBHID_SNAPSHOT_DEVICES})"
                )));
            }

            let mut out = Vec::with_capacity(count);
            for idx in 0..count {
                let rec_len = d.u32().map_err(|e| {
                    js_error(&format!("Invalid UHCI runtime snapshot WebHID list: {e}"))
                })? as usize;
                let rec = d.bytes(rec_len).map_err(|e| {
                    js_error(&format!("Invalid UHCI runtime snapshot WebHID list: {e}"))
                })?;

                let mut rd = Decoder::new(rec);
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
                let product_len = rd.u32().map_err(|e| {
                    js_error(&format!(
                        "Invalid WebHID record {device_id} product length: {e}"
                    ))
                })? as usize;
                let product_bytes = rd.bytes(product_len).map_err(|e| {
                    js_error(&format!("Invalid WebHID record {device_id} product: {e}"))
                })?;
                let product = std::str::from_utf8(product_bytes).map_err(|_| {
                    js_error(&format!(
                        "Invalid WebHID record {device_id} product: expected UTF-8 string"
                    ))
                })?;
                let product = Self::sanitize_usb_string(product);
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
            d.finish().map_err(|e| {
                js_error(&format!("Invalid UHCI runtime snapshot WebHID list: {e}"))
            })?;
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
                .hub_mut()
                .attach(EXTERNAL_HUB_ROOT_PORT, Box::new(hub));
            self.external_hub = Some(ExternalHubState { port_count });
        }

        // Restore WebUSB passthrough device first so root-port occupancy is correct.
        if let Some(buf) = webusb_state_bytes {
            let port = WEBUSB_ROOT_PORT;
            let mut dev = UsbWebUsbPassthroughDevice::new();
            self.ctrl.hub_mut().attach(port, Box::new(dev.clone()));
            if let Err(err) = dev.load_state(buf) {
                self.reset_for_snapshot_restore();
                return Err(js_error(&format!(
                    "Invalid UHCI runtime snapshot WebUSB device state: {err}"
                )));
            }
            // WebUSB host actions are backed by JS Promises and cannot be resumed after a VM
            // snapshot restore. Drop any inflight/queued host bookkeeping so UHCI TD retries
            // re-emit fresh actions.
            dev.reset_host_state_for_restore();
            self.webusb = Some(WebUsbDeviceState { port, dev });
        }

        // Recreate WebHID devices (using stored static config), then apply their dynamic snapshots.
        for entry in webhid_entries {
            let mut dev = UsbHidPassthroughHandle::new(
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

            match entry.location {
                WebHidDeviceLocation::RootPort(port) => {
                    if !self.port_is_free(port) {
                        self.reset_for_snapshot_restore();
                        return Err(js_error(&format!(
                            "UHCI runtime snapshot WebHID deviceId {} cannot attach to root port {port}: port is not available",
                            entry.device_id
                        )));
                    }
                    self.ctrl.hub_mut().attach(port, Box::new(dev.clone()));
                    self.webhid_ports[port] = Some(entry.device_id);
                }
                WebHidDeviceLocation::ExternalHubPort(hub_port) => {
                    self.webhid_hub_ports.insert(hub_port, entry.device_id);
                    let path = [EXTERNAL_HUB_ROOT_PORT as u8, hub_port];
                    if let Err(err) = crate::uhci_controller_bridge::attach_device_at_path(
                        &mut self.ctrl,
                        &path,
                        Box::new(dev.clone()),
                    ) {
                        self.reset_for_snapshot_restore();
                        let msg = err.as_string().unwrap_or_else(|| format!("{err:?}"));
                        return Err(js_error(&format!(
                            "UHCI runtime snapshot WebHID deviceId {} cannot attach behind external hub port {hub_port}: {msg}",
                            entry.device_id
                        )));
                    }
                }
            }

            if let Err(err) = dev.load_state(&entry.state) {
                self.reset_for_snapshot_restore();
                return Err(js_error(&format!(
                    "Invalid UHCI runtime snapshot WebHID deviceId {} state: {err}",
                    entry.device_id
                )));
            }

            self.webhid_devices.insert(
                entry.device_id,
                WebHidDeviceState {
                    location: entry.location,
                    dev,
                    vendor_id: entry.vendor_id,
                    product_id: entry.product_id,
                    product: entry.product,
                    report_descriptor: entry.report_descriptor,
                    has_interrupt_out: entry.has_interrupt_out,
                },
            );
        }

        // Reattach any externally managed HID passthrough devices before restoring hub state.
        //
        // These devices are typically created and owned by JS (e.g. Aero's synthetic keyboard/mouse/gamepad),
        // so we cannot recreate them here. Instead, we keep track of their `UsbHidPassthroughHandle`s when
        // they are attached and reattach them after snapshot reset so the hub snapshot can load their
        // dynamic state.
        if let Some(hub_state) = hub_state_bytes {
            // Only reattach passthrough devices that the external hub snapshot expects to exist.
            //
            // `UsbHubDevice::load_state` restores the `connected` flag from the snapshot regardless
            // of whether a device is actually attached in memory. If we blindly attach all JS-owned
            // passthrough devices here, we can end up with "hidden" devices (device present but
            // connected=false) that later reappear after an upstream hub bus reset.
            let expected_ports =
                match Self::external_hub_ports_with_snapshot_devices(hub_state) {
                    Ok(ports) => ports,
                    Err(err) => {
                        self.reset_for_snapshot_restore();
                        return Err(err);
                    }
                };

            for hub_port in expected_ports {
                // Avoid clobbering WebHID devices restored from the snapshot.
                if self.webhid_hub_ports.contains_key(&hub_port) {
                    continue;
                }

                let path = [EXTERNAL_HUB_ROOT_PORT as u8, hub_port];
                let Some(dev) = self.usb_hid_passthrough_devices.get(&path[..]).cloned() else {
                    continue;
                };

                // Best-effort: if a passthrough device can't be reattached, continue restoring the
                // snapshot so other devices still come back.
                let _ = crate::uhci_controller_bridge::attach_device_at_path(
                    &mut self.ctrl,
                    &path,
                    Box::new(dev),
                );
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
    fn external_hub_ports_with_snapshot_devices(hub_state: &[u8]) -> Result<Vec<u8>, JsValue> {
        const TAG_PORTS: u16 = 6;

        let r = SnapshotReader::parse(hub_state, UsbHubDevice::DEVICE_ID)
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
        r.ensure_device_major(UsbHubDevice::DEVICE_VERSION.major)
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;

        let Some(buf) = r.bytes(TAG_PORTS) else {
            return Ok(Vec::new());
        };

        let mut d = Decoder::new(buf);
        let port_records = d
            .vec_bytes()
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
        d.finish()
            .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;

        let mut ports = Vec::new();
        for (idx, rec) in port_records.iter().enumerate() {
            let mut pd = Decoder::new(rec);
            // Keep this in sync with `UsbHubDevice::save_state` (`crates/aero-usb/src/hub.rs`).
            let _connected = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _connect_change = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _enabled = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _enable_change = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _suspended = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _suspend_change = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _powered = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _reset = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _reset_countdown_ms = pd
                .u8()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let _reset_change = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            let has_device_state = pd
                .bool()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            if has_device_state {
                let len = pd
                    .u32()
                    .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?
                    as usize;
                let _ = pd
                    .bytes(len)
                    .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;
            }
            pd.finish()
                .map_err(|e| js_error(&format!("Invalid UHCI runtime snapshot external hub state: {e}")))?;

            if has_device_state {
                let port = (idx + 1)
                    .try_into()
                    .map_err(|_| js_error("UHCI runtime snapshot external hub has too many ports"))?;
                ports.push(port);
            }
        }

        Ok(ports)
    }

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
        let dev = self
            .ctrl
            .hub_mut()
            .port_device_mut(EXTERNAL_HUB_ROOT_PORT)?;
        let any = dev.model_mut() as &mut dyn core::any::Any;
        any.downcast_mut::<UsbHubDevice>()
    }

    fn external_hub_ref(&self) -> Option<&UsbHubDevice> {
        let dev = self.ctrl.hub().port_device(EXTERNAL_HUB_ROOT_PORT)?;
        let any = dev.model() as &dyn core::any::Any;
        any.downcast_ref::<UsbHubDevice>()
    }

    fn reset_for_snapshot_restore(&mut self) {
        self.ctrl = UhciController::new();

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
            .hub_mut()
            .attach(EXTERNAL_HUB_ROOT_PORT, Box::new(hub));
        self.external_hub = Some(ExternalHubState {
            port_count: desired,
        });
        Ok(())
    }

    fn grow_external_hub(&mut self, new_port_count: u8) -> Result<(), JsValue> {
        let Some(current_state) = self.external_hub.as_ref() else {
            return Err(js_error("Cannot grow external hub: hub is not attached"));
        };
        if new_port_count <= current_state.port_count {
            return Ok(());
        }

        let passthrough_candidates: Vec<(Vec<u8>, u8, UsbHidPassthroughHandle)> = self
            .usb_hid_passthrough_devices
            .iter()
            .filter_map(|(path, dev)| {
                if path.len() != 2 || path[0] as usize != EXTERNAL_HUB_ROOT_PORT {
                    return None;
                }
                let hub_port = path[1];
                if hub_port == 0 {
                    return None;
                }
                // Avoid clobbering WebHID devices.
                if self.webhid_hub_ports.contains_key(&hub_port) {
                    return None;
                }

                Some((path.clone(), hub_port, dev.clone()))
            })
            .collect();

        // Identify which externally managed passthrough devices are currently attached behind the
        // old hub so we can reattach only those after hub replacement. The handle map can include
        // devices that were attached previously but are not part of the current topology (e.g.
        // devices attached after a snapshot, then removed by restore).
        let passthrough_to_reattach: Vec<(Vec<u8>, UsbHidPassthroughHandle)> = {
            let Some(hub) = self.external_hub_mut() else {
                return Err(js_error("Cannot grow external hub: hub device is missing"));
            };

            passthrough_candidates
                .into_iter()
                .filter_map(|(path, hub_port, dev)| {
                    let idx = (hub_port - 1) as usize;
                    if hub.downstream_device_mut(idx).is_none() {
                        return None;
                    }

                    Some((path.clone(), dev.clone()))
                })
                .collect()
        };

        // Replace the hub device at root port 0 so the guest sees a real hotplug event and can
        // re-read the hub descriptor (port count, etc).
        self.ctrl.hub_mut().detach(EXTERNAL_HUB_ROOT_PORT);

        let hub = UsbHubDevice::new_with_ports(new_port_count as usize);
        self.ctrl
            .hub_mut()
            .attach(EXTERNAL_HUB_ROOT_PORT, Box::new(hub));
        if let Some(state) = self.external_hub.as_mut() {
            state.port_count = new_port_count;
        }

        // Reattach any existing downstream devices behind the new hub.
        let to_reattach: Vec<(u8, UsbHidPassthroughHandle)> = self
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

        for (hub_port, dev) in to_reattach {
            let path = [EXTERNAL_HUB_ROOT_PORT as u8, hub_port];
            crate::uhci_controller_bridge::attach_device_at_path(
                &mut self.ctrl,
                &path,
                Box::new(dev),
            )?;
        }

        // Reattach externally managed HID passthrough devices that were attached behind the old
        // hub.
        for (path, dev) in passthrough_to_reattach {
            crate::uhci_controller_bridge::attach_device_at_path(
                &mut self.ctrl,
                &path,
                Box::new(dev),
            )?;
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
fn webusb_setup_packet_to_js(setup: PassthroughSetupPacket) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("bmRequestType"),
        &JsValue::from_f64(f64::from(setup.bm_request_type)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("bRequest"),
        &JsValue::from_f64(f64::from(setup.b_request)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("wValue"),
        &JsValue::from_f64(f64::from(setup.w_value)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("wIndex"),
        &JsValue::from_f64(f64::from(setup.w_index)),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("wLength"),
        &JsValue::from_f64(f64::from(setup.w_length)),
    );
    obj.into()
}

fn webusb_action_to_js(action: UsbHostAction) -> JsValue {
    let obj = Object::new();
    match action {
        UsbHostAction::ControlIn { id, setup } => {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("kind"),
                &JsValue::from_str("controlIn"),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("id"),
                &JsValue::from_f64(f64::from(id)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("setup"),
                &webusb_setup_packet_to_js(setup),
            );
        }
        UsbHostAction::ControlOut { id, setup, data } => {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("kind"),
                &JsValue::from_str("controlOut"),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("id"),
                &JsValue::from_f64(f64::from(id)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("setup"),
                &webusb_setup_packet_to_js(setup),
            );
            let data = Uint8Array::from(data.as_slice());
            let _ = Reflect::set(&obj, &JsValue::from_str("data"), data.as_ref());
        }
        UsbHostAction::BulkIn {
            id,
            endpoint,
            length,
        } => {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("kind"),
                &JsValue::from_str("bulkIn"),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("id"),
                &JsValue::from_f64(f64::from(id)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("endpoint"),
                &JsValue::from_f64(f64::from(endpoint)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("length"),
                &JsValue::from_f64(f64::from(length)),
            );
        }
        UsbHostAction::BulkOut { id, endpoint, data } => {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("kind"),
                &JsValue::from_str("bulkOut"),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("id"),
                &JsValue::from_f64(f64::from(id)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("endpoint"),
                &JsValue::from_f64(f64::from(endpoint)),
            );
            let data = Uint8Array::from(data.as_slice());
            let _ = Reflect::set(&obj, &JsValue::from_str("data"), data.as_ref());
        }
    };
    obj.into()
}

fn validate_hub_port_count(value: u32) -> Result<u8, JsValue> {
    let count = u8::try_from(value).map_err(|_| js_error("portCount must be in 1..=255"))?;
    if count == 0 {
        return Err(js_error("portCount must be in 1..=255"));
    }
    Ok(count)
}

fn parse_root_port_guest_path(path: JsValue) -> Result<usize, JsValue> {
    if !Array::is_array(&path) {
        return Err(js_error("Invalid guestPath: expected an array"));
    }
    let path = Array::from(&path);
    if path.length() == 0 {
        return Err(js_error("guestPath must not be empty"));
    }
    let root_port = path
        .get(0)
        .as_f64()
        .and_then(|v| {
            if v.is_finite() && v.fract() == 0.0 && v >= 0.0 && v <= u32::MAX as f64 {
                Some(v as u32)
            } else {
                None
            }
        })
        .ok_or_else(|| js_error("guestPath root port must be a u32 number"))?;

    if root_port > u32::from(u8::MAX) {
        return Err(js_error(&format!(
            "guestPath root port {root_port} is out of range"
        )));
    }
    Ok(root_port as usize)
}

fn parse_external_hub_guest_path(path: JsValue) -> Result<(usize, u8), JsValue> {
    if !Array::is_array(&path) {
        return Err(js_error("Invalid guestPath: expected an array"));
    }
    let path = Array::from(&path);
    if path.length() < 2 {
        return Err(js_error(
            "guestPath must include a downstream hub port (expected [rootPort, hubPort])",
        ));
    }
    if path.length() > 2 {
        return Err(js_error(
            "Nested hub guestPath segments are not supported by UhciRuntime yet",
        ));
    }

    let root = path
        .get(0)
        .as_f64()
        .and_then(|v| {
            if v.is_finite() && v.fract() == 0.0 && v >= 0.0 && v <= u32::MAX as f64 {
                Some(v as u32)
            } else {
                None
            }
        })
        .ok_or_else(|| js_error("guestPath root port must be a u32 number"))?;
    let hub_port = path
        .get(1)
        .as_f64()
        .and_then(|v| {
            if v.is_finite() && v.fract() == 0.0 && v >= 0.0 && v <= u32::MAX as f64 {
                Some(v as u32)
            } else {
                None
            }
        })
        .ok_or_else(|| js_error("guestPath hub port must be a u32 number"))?;

    let root = root as usize;
    let hub_port_u8 = u8::try_from(hub_port)
        .map_err(|_| js_error("guestPath hub port is out of range (expected 1..=255)"))?;
    if hub_port_u8 == 0 {
        return Err(js_error("guestPath hub port is invalid (expected 1..=255)"));
    }
    Ok((root, hub_port_u8))
}
