#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::UhciController;
use aero_usb::{MemoryBus, UsbWebUsbPassthroughDevice};

const WEBUSB_UHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"WUHB";
const WEBUSB_UHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

// UHCI register offsets (0x20 bytes).
const REG_USBCMD: u16 = 0x00;

const ROOT_PORT_EXTERNAL_HUB: usize = 0;
const ROOT_PORT_WEBUSB: usize = 1;
// Must match `web/src/usb/uhci_external_hub.ts::DEFAULT_EXTERNAL_HUB_PORT_COUNT`.
const EXTERNAL_HUB_PORT_COUNT: u8 = 16;

/// Guest memory accessor backed by the module's wasm linear memory.
///
/// Guest physical address 0 maps to `guest_base` inside the linear memory (see `guest_ram_layout()`
/// in `aero-wasm`).
#[derive(Debug, Clone, Copy)]
struct WasmGuestMemory {
    guest_base: u32,
    guest_size: u64,
    mem_bytes: u64,
}

impl WasmGuestMemory {
    fn new(guest_base: u32) -> Self {
        let pages = core::arch::wasm32::memory_size(0) as u64;
        let mem_bytes = pages.saturating_mul(64 * 1024);
        let guest_size = mem_bytes
            .saturating_sub(guest_base as u64)
            .min(crate::guest_layout::PCI_MMIO_BASE);
        Self {
            guest_base,
            guest_size,
            mem_bytes,
        }
    }

    fn translate(&self, paddr: u64, len: usize) -> Option<u32> {
        let end = paddr.checked_add(len as u64)?;
        if end > self.guest_size {
            return None;
        }
        let mapped = (self.guest_base as u64).checked_add(end)?;
        if mapped > self.mem_bytes {
            return None;
        }
        let base = (self.guest_base as u64).checked_add(paddr)?;
        u32::try_from(base).ok()
    }
}

impl MemoryBus for WasmGuestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let Some(start) = self.translate(paddr, buf.len()) else {
            buf.fill(0);
            return;
        };

        // SAFETY: Bounds checked against the current linear memory size and `buf` is a valid slice.
        unsafe {
            let src = core::slice::from_raw_parts(start as *const u8, buf.len());
            buf.copy_from_slice(src);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let Some(start) = self.translate(paddr, buf.len()) else {
            return;
        };

        // SAFETY: Bounds checked against the current linear memory size and `buf` is a valid slice.
        unsafe {
            let dst = core::slice::from_raw_parts_mut(start as *mut u8, buf.len());
            dst.copy_from_slice(buf);
        }
    }
}

#[wasm_bindgen]
pub struct WebUsbUhciBridge {
    guest_base: u32,
    controller: UhciController,
    webusb: Option<UsbWebUsbPassthroughDevice>,
}

#[wasm_bindgen]
impl WebUsbUhciBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32) -> Self {
        let mut controller = UhciController::new();
        controller.hub_mut().attach(
            ROOT_PORT_EXTERNAL_HUB,
            Box::new(UsbHubDevice::with_port_count(EXTERNAL_HUB_PORT_COUNT)),
        );

        Self {
            guest_base,
            controller,
            webusb: None,
        }
    }

    pub fn io_read(&mut self, offset: u32, size: u32) -> u32 {
        let Ok(offset) = u16::try_from(offset) else {
            return 0xffff_ffff;
        };
        let Ok(size) = usize::try_from(size) else {
            return 0xffff_ffff;
        };

        match size {
            1 | 2 | 4 => self.controller.io_read(offset, size),
            _ => 0xffff_ffff,
        }
    }

    pub fn io_write(&mut self, offset: u32, size: u32, value: u32) {
        let Ok(offset) = u16::try_from(offset) else {
            return;
        };
        let Ok(size) = usize::try_from(size) else {
            return;
        };
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        self.controller.io_write(offset, size, value);
    }

    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }

        let mut mem = WasmGuestMemory::new(self.guest_base);
        for _ in 0..frames {
            self.controller.tick_1ms(&mut mem);
        }
    }

    pub fn irq_level(&self) -> bool {
        self.controller.irq_level()
    }

    pub fn set_connected(&mut self, connected: bool) {
        let was_connected = self.webusb.is_some();

        match (was_connected, connected) {
            (true, true) | (false, false) => {}
            (false, true) => {
                let dev = UsbWebUsbPassthroughDevice::new();
                self.controller
                    .hub_mut()
                    .attach(ROOT_PORT_WEBUSB, Box::new(dev.clone()));
                self.webusb = Some(dev);
            }
            (true, false) => {
                self.controller.hub_mut().detach(ROOT_PORT_WEBUSB);
                self.webusb = None;
            }
        }
    }

    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let Some(dev) = self.webusb.as_ref() else {
            return Ok(JsValue::NULL);
        };
        let actions: Vec<UsbHostAction> = dev.drain_actions();
        if actions.is_empty() {
            return Ok(JsValue::NULL);
        }
        serde_wasm_bindgen::to_value(&actions).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        if let Some(dev) = self.webusb.as_ref() {
            dev.push_completion(completion);
        }

        Ok(())
    }

    pub fn reset(&mut self) {
        self.controller.io_write(
            REG_USBCMD,
            2,
            u32::from(aero_usb::uhci::regs::USBCMD_HCRESET),
        );

        if let Some(dev) = self.webusb.as_ref() {
            dev.reset();
        }
    }

    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        let Some(summary) = self.webusb.as_ref().map(|d| d.pending_summary()) else {
            return Ok(JsValue::NULL);
        };
        serde_wasm_bindgen::to_value(&summary).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Detach any USB device attached at the given topology path.
    ///
    /// Path numbering follows `aero_usb::hub::RootHub`:
    /// - `path[0]` is the root port index (0-based).
    /// - `path[1..]` are hub ports (1-based).
    pub fn detach_at_path(&mut self, path: JsValue) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        if path.len() == 1 && path[0] as usize == ROOT_PORT_EXTERNAL_HUB {
            return Err(
                js_sys::Error::new("Cannot detach the external USB hub from root port 0").into(),
            );
        }
        crate::uhci_controller_bridge::detach_device_at_path(&mut self.controller, &path)
    }

    /// Attach a WebHID-backed USB HID device at the given topology path.
    pub fn attach_webhid_device(
        &mut self,
        path: JsValue,
        device: &crate::WebHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        validate_webhid_attach_path(&path)?;
        crate::uhci_controller_bridge::attach_device_at_path(
            &mut self.controller,
            &path,
            Box::new(device.as_usb_device()),
        )
    }

    /// Attach a generic USB HID passthrough device at the given topology path.
    pub fn attach_usb_hid_passthrough_device(
        &mut self,
        path: JsValue,
        device: &crate::UsbHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        validate_webhid_attach_path(&path)?;
        crate::uhci_controller_bridge::attach_device_at_path(
            &mut self.controller,
            &path,
            Box::new(device.as_usb_device()),
        )
    }

    /// Serialize the current bridge state into a deterministic snapshot blob.
    ///
    /// Format: top-level `aero-io-snapshot` TLV with:
    /// - tag 1: `aero_usb::uhci::UhciController` snapshot bytes
    /// - tag 2: IRQ latch (`irq_level`) (redundant; derived from UHCI state)
    /// - tag 3: external hub (`UsbHubDevice`) snapshot bytes
    /// - tag 4: WebUSB passthrough device (`UsbWebUsbPassthroughDevice`) snapshot bytes
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_ASSERTED: u16 = 2;
        const TAG_EXTERNAL_HUB: u16 = 3;
        const TAG_WEBUSB_DEVICE: u16 = 4;

        let mut w = SnapshotWriter::new(
            WEBUSB_UHCI_BRIDGE_DEVICE_ID,
            WEBUSB_UHCI_BRIDGE_DEVICE_VERSION,
        );
        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());
        w.field_bool(TAG_IRQ_ASSERTED, self.controller.irq_level());

        if let Some(hub) = self.external_hub() {
            w.field_bytes(TAG_EXTERNAL_HUB, hub.save_state());
        }
        if let Some(dev) = self.webusb.as_ref() {
            // Persist the WebUSB passthrough device's USB-visible state (address, control-transfer
            // stage, etc) so that after restoring a VM snapshot the guest's TD retries can make
            // forward progress. Host-side action queues are cleared on restore (see `load_state`).
            w.field_bytes(TAG_WEBUSB_DEVICE, dev.save_state());
        }
        w.finish()
    }

    /// Restore bridge state from a snapshot blob produced by [`save_state`].
    ///
    /// WebUSB host actions are backed by JS Promises and cannot be resumed after restoring a VM
    /// snapshot. As part of restore we drop the passthrough device's host-action queues and
    /// in-flight maps so the guest's UHCI TD retries will re-emit host actions instead of waiting
    /// forever for completions that will never arrive.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_EXTERNAL_HUB: u16 = 3;
        const TAG_WEBUSB_DEVICE: u16 = 4;

        let r = SnapshotReader::parse(bytes, WEBUSB_UHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| JsValue::from_str(&format!("Invalid WebUSB UHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(WEBUSB_UHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| JsValue::from_str(&format!("Invalid WebUSB UHCI bridge snapshot: {e}")))?;

        // Ensure the topology exists before restoring port-connected state. The controller snapshot
        // includes per-port connected/enabled bits but does not create USB device instances.
        if r.bytes(TAG_EXTERNAL_HUB).is_some() && self.external_hub().is_none() {
            self.controller.hub_mut().attach(
                ROOT_PORT_EXTERNAL_HUB,
                Box::new(UsbHubDevice::with_port_count(EXTERNAL_HUB_PORT_COUNT)),
            );
        }
        if r.bytes(TAG_WEBUSB_DEVICE).is_some() {
            self.set_connected(true);
        } else {
            self.set_connected(false);
        }

        let controller_bytes = r.bytes(TAG_CONTROLLER).ok_or_else(|| {
            JsValue::from_str("WebUSB UHCI bridge snapshot missing controller state")
        })?;
        self.controller
            .load_state(controller_bytes)
            .map_err(|e| JsValue::from_str(&format!("Invalid UHCI controller snapshot: {e}")))?;

        if let Some(buf) = r.bytes(TAG_EXTERNAL_HUB) {
            let hub = self.external_hub_mut().ok_or_else(|| {
                JsValue::from_str("WebUSB UHCI bridge missing external hub device")
            })?;
            hub.load_state(buf)
                .map_err(|e| JsValue::from_str(&format!("Invalid external hub snapshot: {e}")))?;
        }

        if let Some(buf) = r.bytes(TAG_WEBUSB_DEVICE) {
            let dev = self.webusb.as_mut().ok_or_else(|| {
                JsValue::from_str("WebUSB UHCI bridge missing passthrough device")
            })?;
            dev.load_state(buf)
                .map_err(|e| JsValue::from_str(&format!("Invalid WebUSB device snapshot: {e}")))?;
            dev.reset_host_state_for_restore();
        }

        Ok(())
    }

    /// Snapshot the full UHCI + USB device tree state as deterministic bytes.
    ///
    /// The returned bytes represent only the USB stack state (controller + devices), not guest RAM.
    pub fn snapshot_state(&self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore UHCI + USB device state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }
}

impl WebUsbUhciBridge {
    fn external_hub(&self) -> Option<&UsbHubDevice> {
        let dev = self.controller.hub().port_device(ROOT_PORT_EXTERNAL_HUB)?;
        let any = dev.model() as &dyn core::any::Any;
        any.downcast_ref::<UsbHubDevice>()
    }

    fn external_hub_mut(&mut self) -> Option<&mut UsbHubDevice> {
        let dev = self
            .controller
            .hub_mut()
            .port_device_mut(ROOT_PORT_EXTERNAL_HUB)?;
        let any = dev.model_mut() as &mut dyn core::any::Any;
        any.downcast_mut::<UsbHubDevice>()
    }
}

fn validate_webhid_attach_path(path: &[u8]) -> Result<(), JsValue> {
    if path.len() < 2 {
        return Err(js_sys::Error::new(
            "WebHID devices must attach behind the external hub (expected path like [0, <hubPort>])",
        )
        .into());
    }
    if path[0] as usize != ROOT_PORT_EXTERNAL_HUB {
        return Err(js_sys::Error::new(
            "WebHID devices must attach behind the external hub on root port 0",
        )
        .into());
    }
    Ok(())
}
