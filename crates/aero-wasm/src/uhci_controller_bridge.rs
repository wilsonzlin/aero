//! WASM-side bridge for exposing a guest-visible UHCI controller.
//!
//! The browser I/O worker exposes this as a PCI device with an IO BAR; port I/O reads/writes are
//! forwarded into this bridge which updates a Rust UHCI model (`aero_usb::uhci::UhciController`).
//!
//! The UHCI schedule (frame list / QHs / TDs) lives in guest RAM. In the browser runtime, guest
//! physical address 0 begins at `guest_base` within the WASM linear memory; this bridge implements
//! `aero_usb::MemoryBus` so the controller can read/write descriptors directly.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::UhciController;
use aero_usb::{MemoryBus, UsbDeviceModel, UsbHubAttachError, UsbWebUsbPassthroughDevice};

const UHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"UHCB";
const UHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

// UHCI register layout (0x20 bytes).
const REG_USBCMD: u16 = 0x00;
const REG_PORTSC1: u16 = 0x10;
const REG_PORTSC2: u16 = 0x12;

// Reserve the 2nd UHCI root port for the WebUSB passthrough device. Root port 0 is used for the
// external WebHID hub by default (see `web/src/platform/webhid_passthrough.ts`).
const WEBUSB_ROOT_PORT: usize = 1;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

#[derive(Clone, Copy)]
struct WasmGuestMemory {
    guest_base: u32,
    guest_size: u64,
}

impl WasmGuestMemory {
    #[inline]
    fn linear_ptr(&self, paddr: u64, len: usize) -> Option<*const u8> {
        let len_u64 = len as u64;
        let end = paddr.checked_add(len_u64)?;
        if end > self.guest_size {
            return None;
        }
        let linear = (self.guest_base as u64).checked_add(paddr)?;
        u32::try_from(linear).ok().map(|v| v as *const u8)
    }

    #[inline]
    fn linear_ptr_mut(&self, paddr: u64, len: usize) -> Option<*mut u8> {
        Some(self.linear_ptr(paddr, len)? as *mut u8)
    }
}

impl MemoryBus for WasmGuestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        // If the request goes out of bounds, read as much as possible and fill the rest with 0.
        let Some(max_len) = self.guest_size.checked_sub(paddr) else {
            buf.fill(0);
            return;
        };
        let copy_len = buf.len().min(max_len.min(usize::MAX as u64) as usize);
        if copy_len == 0 {
            buf.fill(0);
            return;
        }

        let Some(ptr) = self.linear_ptr(paddr, copy_len) else {
            buf.fill(0);
            return;
        };

        // Safety: `linear_ptr` bounds-checks against the configured guest region.
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), copy_len);
        }
        if copy_len < buf.len() {
            buf[copy_len..].fill(0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        let Some(max_len) = self.guest_size.checked_sub(paddr) else {
            return;
        };
        let copy_len = buf.len().min(max_len.min(usize::MAX as u64) as usize);
        if copy_len == 0 {
            return;
        }

        let Some(ptr) = self.linear_ptr_mut(paddr, copy_len) else {
            return;
        };

        // Safety: `linear_ptr_mut` bounds-checks against the configured guest region.
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, copy_len);
        }
    }
}

fn validate_port_size(size: u8) -> usize {
    match size {
        1 | 2 | 4 => size as usize,
        _ => 0,
    }
}

pub(crate) fn parse_usb_path(path: JsValue) -> Result<Vec<u8>, JsValue> {
    let parts: Vec<u32> = serde_wasm_bindgen::from_value(path)
        .map_err(|e| js_error(format!("Invalid USB topology path: {e}")))?;
    if parts.is_empty() {
        return Err(js_error("USB topology path must not be empty"));
    }

    let mut out = Vec::with_capacity(parts.len());
    for (i, part) in parts.into_iter().enumerate() {
        if i == 0 {
            if part > 1 {
                return Err(js_error("USB root port must be 0 or 1"));
            }
            out.push(part as u8);
            continue;
        }
        if !(1..=255).contains(&part) {
            return Err(js_error("USB hub port numbers must be in 1..=255"));
        }
        out.push(part as u8);
    }
    Ok(out)
}

fn map_attach_error(err: UsbHubAttachError) -> JsValue {
    match err {
        UsbHubAttachError::NotAHub => js_error("device is not a USB hub"),
        UsbHubAttachError::InvalidPort => js_error("invalid hub/root port"),
        UsbHubAttachError::PortOccupied => js_error("USB hub port already occupied"),
        UsbHubAttachError::NoDevice => js_error("no device attached at hub port"),
    }
}

pub(crate) fn attach_device_at_path(
    ctrl: &mut UhciController,
    path: &[u8],
    device: Box<dyn UsbDeviceModel>,
) -> Result<(), JsValue> {
    // Replace semantics: detach any existing device at the path first.
    let _ = ctrl.hub_mut().detach_at_path(path);
    ctrl.hub_mut()
        .attach_at_path(path, device)
        .map_err(map_attach_error)
}

pub(crate) fn detach_device_at_path(ctrl: &mut UhciController, path: &[u8]) -> Result<(), JsValue> {
    match ctrl.hub_mut().detach_at_path(path) {
        Ok(()) => Ok(()),
        // Detach is intentionally idempotent for host-side topology management.
        Err(UsbHubAttachError::NoDevice) => Ok(()),
        Err(e) => Err(map_attach_error(e)),
    }
}

/// WASM export: reusable UHCI controller model for the browser I/O worker.
///
/// The controller reads/writes guest RAM directly from the module's linear memory (shared across
/// workers in the threaded build) using `guest_base` and `guest_size` from the `guest_ram_layout`
/// contract.
#[wasm_bindgen]
pub struct UhciControllerBridge {
    ctrl: UhciController,
    guest_base: u32,
    guest_size: u64,
    webusb: Option<UsbWebUsbPassthroughDevice>,
    pci_command: u16,
}

impl UhciControllerBridge {
    /// Rust-only helper for tests: connect an arbitrary USB device model to a root port.
    pub fn connect_device_for_test(&mut self, root_port: usize, device: Box<dyn UsbDeviceModel>) {
        self.ctrl.hub_mut().attach(root_port, device);
    }
}

#[wasm_bindgen]
impl UhciControllerBridge {
    /// Create a new UHCI controller bound to the provided guest RAM mapping.
    ///
    /// - `guest_base` is the byte offset inside wasm linear memory where guest physical address 0
    ///   begins (see `guest_ram_layout`).
    /// - `guest_size` is the guest RAM size in bytes. Pass `0` to use "the remainder of linear
    ///   memory" as guest RAM (mirrors `WasmVm`).
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(js_error("guest_base must be non-zero"));
        }

        let mem_bytes = wasm_memory_byte_len();
        let guest_size_u64 = if guest_size == 0 {
            mem_bytes.saturating_sub(guest_base as u64)
        } else {
            guest_size as u64
        };
        // Keep guest RAM below the PCI MMIO aperture (see `guest_ram_layout` contract).
        let guest_size_u64 = guest_size_u64.min(crate::guest_layout::PCI_MMIO_BASE);

        let end = (guest_base as u64)
            .checked_add(guest_size_u64)
            .ok_or_else(|| js_error("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(js_error(format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        Ok(Self {
            // The TS PCI bus passes offset-within-BAR for I/O access, so keep the controller's view
            // of the port space anchored at 0 and treat `offset` as the full register offset.
            ctrl: UhciController::new(),
            guest_base,
            guest_size: guest_size_u64,
            webusb: None,
            pci_command: 0,
        })
    }

    /// Mirror the guest-written PCI command register (0x04, low 16 bits) into the WASM device
    /// wrapper.
    ///
    /// This is used to enforce PCI Bus Master Enable gating for DMA. In a JS runtime, the PCI
    /// configuration space lives in TypeScript (`PciBus`), so the WASM bridge must be updated via
    /// this explicit hook.
    pub fn set_pci_command(&mut self, command: u32) {
        self.pci_command = (command & 0xffff) as u16;
    }

    #[wasm_bindgen(getter)]
    pub fn guest_base(&self) -> u32 {
        self.guest_base
    }

    #[wasm_bindgen(getter)]
    pub fn guest_size(&self) -> u32 {
        self.guest_size.min(u64::from(u32::MAX)) as u32
    }

    pub fn io_read(&mut self, offset: u16, size: u8) -> u32 {
        let size = validate_port_size(size);
        if size == 0 {
            return 0xFFFF_FFFF;
        }
        self.ctrl.io_read(offset, size)
    }

    pub fn io_write(&mut self, offset: u16, size: u8, value: u32) {
        let size = validate_port_size(size);
        if size == 0 {
            return;
        }
        self.ctrl.io_write(offset, size, value);
    }

    /// Advance the controller by exactly `frames` UHCI frames (1ms each).
    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }
        // Only DMA when PCI Bus Master Enable is set (command bit 2).
        if (self.pci_command & (1 << 2)) == 0 {
            return;
        }
        let mut mem = WasmGuestMemory {
            guest_base: self.guest_base,
            guest_size: self.guest_size,
        };
        for _ in 0..frames {
            self.ctrl.tick_1ms(&mut mem);
        }
    }

    /// Convenience wrapper for stepping a single UHCI frame (1ms).
    pub fn step_frame(&mut self) {
        self.step_frames(1);
    }

    /// Alias for `step_frame` to match older call sites.
    pub fn tick_1ms(&mut self) {
        self.step_frame();
    }

    /// Whether the UHCI interrupt line should be raised.
    pub fn irq_asserted(&self) -> bool {
        self.ctrl.irq_level()
    }

    /// Connect or disconnect the WebUSB passthrough device on a reserved UHCI root port.
    ///
    /// The passthrough device is implemented by `aero_usb::UsbWebUsbPassthroughDevice` and emits
    /// host actions that must be executed by the browser `UsbBroker` (see `web/src/usb`).
    pub fn set_connected(&mut self, connected: bool) {
        let was_connected = self.webusb.is_some();

        match (was_connected, connected) {
            (true, true) | (false, false) => {}
            (false, true) => {
                let dev = UsbWebUsbPassthroughDevice::new();
                self.ctrl
                    .hub_mut()
                    .attach(WEBUSB_ROOT_PORT, Box::new(dev.clone()));
                self.webusb = Some(dev);
            }
            (true, false) => {
                self.ctrl.hub_mut().detach(WEBUSB_ROOT_PORT);
                self.webusb = None;
            }
        }
    }

    /// Drain queued WebUSB passthrough host actions as plain JS objects.
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

    /// Push a host completion into the WebUSB passthrough device.
    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if let Some(dev) = self.webusb.as_ref() {
            dev.push_completion(completion);
        }
        Ok(())
    }

    /// Reset the WebUSB passthrough device without disturbing the rest of the USB topology.
    pub fn reset(&mut self) {
        if let Some(dev) = self.webusb.as_ref() {
            dev.reset();
        }
    }

    /// Return a debug summary of queued actions/completions for the WebUSB passthrough device.
    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        let Some(summary) = self.webusb.as_ref().map(|d| d.pending_summary()) else {
            return Ok(JsValue::NULL);
        };
        serde_wasm_bindgen::to_value(&summary).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Attach a USB 1.1 external hub device to a root port.
    ///
    /// `port_count` is the number of downstream ports on the hub (1..=255).
    pub fn attach_hub(&mut self, root_port: u8, port_count: u8) -> Result<(), JsValue> {
        if root_port > 1 {
            return Err(js_error("root_port must be 0 or 1"));
        }
        if port_count == 0 {
            return Err(js_error("port_count must be 1..=255"));
        }
        let hub = UsbHubDevice::with_port_count(port_count);
        self.ctrl
            .hub_mut()
            .attach(root_port as usize, Box::new(hub));
        Ok(())
    }

    /// Detach any USB device attached at the given topology path.
    ///
    /// Path numbering follows the `aero_usb::hub::RootHub` contract:
    /// - `path[0]` is the root port index (0-based).
    /// - `path[1..]` are hub ports (1-based).
    pub fn detach_at_path(&mut self, path: JsValue) -> Result<(), JsValue> {
        let path = parse_usb_path(path)?;
        detach_device_at_path(&mut self.ctrl, &path)
    }

    /// Attach a WebHID-backed USB HID device at the given topology path.
    pub fn attach_webhid_device(
        &mut self,
        path: JsValue,
        device: &crate::WebHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = parse_usb_path(path)?;
        attach_device_at_path(&mut self.ctrl, &path, Box::new(device.as_usb_device()))
    }

    /// Attach a generic USB HID passthrough device at the given topology path.
    pub fn attach_usb_hid_passthrough_device(
        &mut self,
        path: JsValue,
        device: &crate::UsbHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = parse_usb_path(path)?;
        attach_device_at_path(&mut self.ctrl, &path, Box::new(device.as_usb_device()))
    }

    /// Serialize the current UHCI controller state into a deterministic snapshot blob.
    ///
    /// The returned bytes use the canonical `aero-io-snapshot` TLV format:
    /// - tag 1: `aero_usb::uhci::UhciController` snapshot bytes
    /// - tag 2: IRQ latch (`irq_level`) (redundant; derived from UHCI state)
    /// - tag 3: WebUSB passthrough device (`UsbWebUsbPassthroughDevice`) snapshot bytes (when connected)
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_ASSERTED: u16 = 2;
        const TAG_WEBUSB_DEVICE: u16 = 3;

        let mut w = SnapshotWriter::new(UHCI_BRIDGE_DEVICE_ID, UHCI_BRIDGE_DEVICE_VERSION);
        w.field_bytes(TAG_CONTROLLER, self.ctrl.save_state());
        w.field_bool(TAG_IRQ_ASSERTED, self.ctrl.irq_level());
        if let Some(dev) = self.webusb.as_ref() {
            w.field_bytes(TAG_WEBUSB_DEVICE, dev.save_state());
        }
        w.finish()
    }

    /// Restore UHCI controller state from a snapshot blob produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_WEBUSB_DEVICE: u16 = 3;

        let r = SnapshotReader::parse(bytes, UHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| js_error(format!("Invalid UHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(UHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| js_error(format!("Invalid UHCI bridge snapshot: {e}")))?;

        // Ensure the WebUSB passthrough device is connected before restoring port-connected state.
        // The UHCI controller snapshot includes connected/enabled bits but does not create USB
        // device instances.
        if r.bytes(TAG_WEBUSB_DEVICE).is_some() && self.webusb.is_none() {
            self.set_connected(true);
        }

        let ctrl_bytes = r
            .bytes(TAG_CONTROLLER)
            .ok_or_else(|| js_error("UHCI bridge snapshot missing controller state"))?;
        self.ctrl
            .load_state(ctrl_bytes)
            .map_err(|e| js_error(format!("Invalid UHCI controller snapshot: {e}")))?;

        if let Some(buf) = r.bytes(TAG_WEBUSB_DEVICE) {
            let dev = self
                .webusb
                .as_mut()
                .ok_or_else(|| js_error("UHCI bridge snapshot missing WebUSB device"))?;
            dev.load_state(buf)
                .map_err(|e| js_error(format!("Invalid WebUSB device snapshot: {e}")))?;
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

    /// Convenience export: set `USBCMD.HCRESET`.
    ///
    /// Some host code expects a "reset" action that clears the register block but preserves
    /// attached topology.
    pub fn reset_controller(&mut self) {
        self.ctrl.io_write(
            REG_USBCMD,
            2,
            u32::from(aero_usb::uhci::regs::USBCMD_HCRESET),
        );
    }

    /// Convenience export: set `PORTSC1/2.PR` (port reset) for a given root port.
    pub fn reset_port(&mut self, root_port: u8) -> Result<(), JsValue> {
        if root_port > 1 {
            return Err(js_error("root_port must be 0 or 1"));
        }
        let reg = if root_port == 0 {
            REG_PORTSC1
        } else {
            REG_PORTSC2
        };
        self.ctrl.io_write(reg, 2, u32::from(1u16 << 9));
        Ok(())
    }
}
