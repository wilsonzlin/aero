//! WASM-side bridge for exposing a guest-visible EHCI controller.
//!
//! The browser I/O worker exposes this as a PCI device with an MMIO BAR; MMIO reads/writes are
//! forwarded into this bridge which updates a Rust EHCI model (`aero_usb::ehci::EhciController`).
//!
//! Design notes + emulator/runtime contracts: see `docs/usb-ehci.md`.
//!
//! EHCI schedules (periodic/asynchronous lists, qTDs, etc.) live in guest RAM. In the browser
//! runtime, guest physical address 0 begins at `guest_base` within the WASM linear memory; this
//! bridge implements `aero_usb::MemoryBus` so the controller can read/write descriptors directly.
//!
//! PCI Bus Master Enable gating:
//! - When the guest clears PCI command bit 2 (BME), the controller must not be able to DMA into
//!   guest RAM. We enforce this by swapping in a `NoDmaMemory` adapter (open-bus reads, ignored
//!   writes) while still advancing controller time / FRINDEX.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::ehci::EhciController;
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::{UsbDeviceModel, UsbHubAttachError, UsbWebUsbPassthroughDevice};

use crate::guest_memory_bus::{GuestMemoryBus, NoDmaMemory, wasm_memory_byte_len};

const EHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"EHCB";
const EHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

/// Reserve EHCI root port 1 for the WebUSB passthrough device.
///
/// Root port 0 is reserved for the runtime's external hub / topology manager (synthetic HID +
/// WebHID). Keeping this stable lets host-side code treat the port index as part of the public ABI
/// and matches the UHCI + xHCI WASM bridges.
const WEBUSB_ROOT_PORT: u8 = crate::webusb_ports::WEBUSB_ROOT_PORT;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn ensure_not_webusb_root_port(root_port: u8) -> Result<(), JsValue> {
    if root_port == WEBUSB_ROOT_PORT {
        return Err(js_error(format!(
            "EHCI root port {WEBUSB_ROOT_PORT} is reserved for WebUSB passthrough"
        )));
    }
    Ok(())
}

fn validate_mmio_size(size: u8) -> usize {
    match size {
        1 | 2 | 4 => size as usize,
        _ => 0,
    }
}

fn parse_usb_path(path: JsValue) -> Result<Vec<u8>, JsValue> {
    let parts: Vec<u32> = serde_wasm_bindgen::from_value(path)
        .map_err(|e| js_error(format!("Invalid USB topology path: {e}")))?;
    if parts.is_empty() {
        return Err(js_error("USB topology path must not be empty"));
    }

    let mut out = Vec::with_capacity(parts.len());
    for (i, part) in parts.into_iter().enumerate() {
        if i == 0 {
            if part > 255 {
                return Err(js_error("USB root port must be in 0..=255"));
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

fn parse_ehci_usb_path(path: JsValue) -> Result<Vec<u8>, JsValue> {
    let parsed = parse_usb_path(path)?;
    if let Some(&root) = parsed.first() {
        ensure_not_webusb_root_port(root)?;
    }
    Ok(parsed)
}

fn map_attach_error(err: UsbHubAttachError) -> JsValue {
    match err {
        UsbHubAttachError::NotAHub => js_error("device is not a USB hub"),
        UsbHubAttachError::InvalidPort => js_error("invalid hub/root port"),
        UsbHubAttachError::PortOccupied => js_error("USB hub port already occupied"),
        UsbHubAttachError::NoDevice => js_error("no device attached at hub port"),
    }
}

fn attach_device_at_path(
    ctrl: &mut EhciController,
    path: &[u8],
    device: Box<dyn UsbDeviceModel>,
) -> Result<(), JsValue> {
    // Replace semantics: detach any existing device at the path first.
    let _ = ctrl.hub_mut().detach_at_path(path);
    ctrl.hub_mut()
        .attach_at_path(path, device)
        .map_err(map_attach_error)
}

fn detach_device_at_path(ctrl: &mut EhciController, path: &[u8]) -> Result<(), JsValue> {
    match ctrl.hub_mut().detach_at_path(path) {
        Ok(()) => Ok(()),
        // Detach is intentionally idempotent for host-side topology management.
        Err(UsbHubAttachError::NoDevice) => Ok(()),
        Err(e) => Err(map_attach_error(e)),
    }
}

fn reset_webusb_host_state_for_restore(dev: &mut AttachedUsbDevice) {
    // If this is a WebUSB passthrough device, clear host-side bookkeeping that cannot be resumed
    // after a snapshot restore (the browser side uses JS Promises).
    let model_any = dev.model() as &dyn core::any::Any;
    if let Some(webusb) = model_any.downcast_ref::<UsbWebUsbPassthroughDevice>() {
        webusb.reset_host_state_for_restore();
    }

    // Recurse into nested hubs so downstream WebUSB devices also get reset.
    if let Some(hub) = dev.as_hub_mut() {
        for port in 0..hub.num_ports() {
            if let Some(child) = hub.downstream_device_mut(port) {
                reset_webusb_host_state_for_restore(child);
            }
        }
    }
}

fn find_webusb_passthrough_device(
    dev: &mut AttachedUsbDevice,
) -> Option<UsbWebUsbPassthroughDevice> {
    let model_any = dev.model() as &dyn core::any::Any;
    if let Some(webusb) = model_any.downcast_ref::<UsbWebUsbPassthroughDevice>() {
        return Some(webusb.clone());
    }

    if let Some(hub) = dev.as_hub_mut() {
        for port in 0..hub.num_ports() {
            if let Some(child) = hub.downstream_device_mut(port) {
                if let Some(found) = find_webusb_passthrough_device(child) {
                    return Some(found);
                }
            }
        }
    }

    None
}

fn find_webusb_passthrough_path_in_device(
    dev: &mut AttachedUsbDevice,
    prefix: &mut Vec<u8>,
) -> Option<Vec<u8>> {
    let model_any = dev.model() as &dyn core::any::Any;
    if model_any
        .downcast_ref::<UsbWebUsbPassthroughDevice>()
        .is_some()
    {
        return Some(prefix.clone());
    }

    if let Some(hub) = dev.as_hub_mut() {
        for port in 0..hub.num_ports() {
            if let Some(child) = hub.downstream_device_mut(port) {
                // Hub ports are 1-based in the guest-visible topology path contract.
                prefix.push((port + 1) as u8);
                if let Some(found) = find_webusb_passthrough_path_in_device(child, prefix) {
                    prefix.pop();
                    return Some(found);
                }
                prefix.pop();
            }
        }
    }

    None
}

fn find_webusb_passthrough_device_path(ctrl: &mut EhciController) -> Option<Vec<u8>> {
    let hub = ctrl.hub_mut();
    for root_port in 0..hub.num_ports() {
        if let Some(mut dev) = hub.port_device_mut(root_port) {
            let mut prefix = vec![root_port as u8];
            if let Some(found) = find_webusb_passthrough_path_in_device(&mut dev, &mut prefix) {
                return Some(found);
            }
        }
    }
    None
}

fn recover_webusb_passthrough_device(ctrl: &mut EhciController) -> Option<UsbWebUsbPassthroughDevice> {
    // Prefer the reserved root port.
    let hub = ctrl.hub_mut();
    let preferred = WEBUSB_ROOT_PORT as usize;
    if preferred < hub.num_ports() {
        if let Some(mut dev) = hub.port_device_mut(preferred) {
            if let Some(found) = find_webusb_passthrough_device(&mut dev) {
                return Some(found);
            }
        }
    }

    // Fall back to scanning the full topology in case older snapshots attached the device elsewhere.
    for port in 0..hub.num_ports() {
        if port == preferred {
            continue;
        }
        if let Some(mut dev) = hub.port_device_mut(port) {
            if let Some(found) = find_webusb_passthrough_device(&mut dev) {
                return Some(found);
            }
        }
    }

    None
}

/// WASM export: reusable EHCI controller model for the browser I/O worker.
///
/// The controller reads/writes guest RAM directly from the module's linear memory (shared across
/// workers in the threaded build) using `guest_base` and `guest_size` from the `guest_ram_layout`
/// contract.
#[wasm_bindgen]
pub struct EhciControllerBridge {
    ctrl: EhciController,
    guest_base: u32,
    guest_size: u64,
    webusb: Option<UsbWebUsbPassthroughDevice>,
    webusb_connected: bool,
    pci_command: u16,
}

impl EhciControllerBridge {
    /// Rust-only helper for tests: connect an arbitrary USB device model to a root port.
    pub fn connect_device_for_test(&mut self, root_port: usize, device: Box<dyn UsbDeviceModel>) {
        self.ctrl
            .hub_mut()
            .attach_at_path(&[root_port as u8], device)
            .ok();
    }
}

#[wasm_bindgen]
impl EhciControllerBridge {
    /// Create a new EHCI controller bound to the provided guest RAM mapping.
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

        // Keep guest RAM below the PCI MMIO BAR window (see `guest_ram_layout` contract).
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
            ctrl: EhciController::new(),
            guest_base,
            guest_size: guest_size_u64,
            webusb: None,
            webusb_connected: false,
            pci_command: 0,
        })
    }

    /// Mirror the guest-written PCI command register (0x04, low 16 bits) into the WASM device
    /// wrapper.
    ///
    /// This is used to enforce PCI Bus Master Enable gating for DMA (bit 2) and INTx disable
    /// masking (bit 10). In a JS runtime, the PCI configuration space lives in TypeScript
    /// (`PciBus`), so the WASM bridge must be updated via this explicit hook.
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

    pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0;
        }
        self.ctrl.mmio_read(offset as u64, size)
    }

    pub fn mmio_write(&mut self, offset: u32, size: u8, value: u32) {
        let size = validate_mmio_size(size);
        if size == 0 {
            return;
        }
        self.ctrl.mmio_write(offset as u64, size, value);
    }

    /// Advance the controller by exactly `frames` USB frames (1ms each).
    ///
    /// EHCI's `FRINDEX` is a microframe index; each 1ms frame advances it by 8.
    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }

        // Only DMA when PCI Bus Master Enable is set (command bit 2). When bus mastering is
        // disabled the controller should continue advancing its internal frame counter and root hub
        // state, but it must not be able to read or write guest memory for schedule traversal.
        let dma_enabled = (self.pci_command & (1 << 2)) != 0;
        if dma_enabled {
            let mut mem = GuestMemoryBus::new(self.guest_base, self.guest_size);
            for _ in 0..frames {
                self.ctrl.tick_1ms(&mut mem);
            }
        } else {
            let mut mem = NoDmaMemory;
            for _ in 0..frames {
                self.ctrl.tick_1ms(&mut mem);
            }
        }
    }

    /// Convenience wrapper for stepping a single USB frame (1ms).
    pub fn step_frame(&mut self) {
        self.step_frames(1);
    }

    /// Alias for `step_frame` to match other controller bridges.
    pub fn tick_1ms(&mut self) {
        self.step_frame();
    }

    /// Whether the EHCI interrupt line should be raised (INTx).
    pub fn irq_asserted(&self) -> bool {
        // PCI command bit 10: INTx Disable.
        if (self.pci_command & (1 << 10)) != 0 {
            return false;
        }
        self.ctrl.irq_level()
    }

    /// Connect or disconnect the WebUSB passthrough device on a reserved EHCI root port.
    ///
    /// The passthrough device is implemented by `aero_usb::UsbWebUsbPassthroughDevice` and emits
    /// host actions that must be executed by the browser `UsbBroker` (see `web/src/usb`).
    pub fn set_connected(&mut self, connected: bool) {
        self.webusb_connected = crate::ehci_webusb_topology::set_ehci_webusb_connected(
            &mut self.ctrl,
            &mut self.webusb,
            connected,
        );
    }

    /// Drain queued WebUSB passthrough host actions as plain JS objects.
    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        if !self.webusb_connected {
            return Ok(JsValue::NULL);
        };
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
        // Completions may arrive after the device is disconnected (race with async host execution).
        // Ignore them to match UHCI bridge semantics.
        if !self.webusb_connected {
            return Ok(());
        }

        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        if let Some(dev) = self.webusb.as_ref() {
            dev.push_completion(completion);
        }

        Ok(())
    }

    /// Reset the WebUSB passthrough device without disturbing the rest of the USB topology.
    pub fn reset(&mut self) {
        if self.webusb_connected {
            if let Some(dev) = self.webusb.as_ref() {
                dev.reset();
            }
        }
    }

    /// Return a debug summary of queued actions/completions for the WebUSB passthrough device.
    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        if !self.webusb_connected {
            return Ok(JsValue::NULL);
        };
        let Some(summary) = self.webusb.as_ref().map(|d| d.pending_summary()) else {
            return Ok(JsValue::NULL);
        };
        serde_wasm_bindgen::to_value(&summary).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Attach a USB hub device to a root port.
    ///
    /// `port_count` is the number of downstream ports on the hub (1..=255).
    pub fn attach_hub(&mut self, root_port: u8, port_count: u8) -> Result<(), JsValue> {
        ensure_not_webusb_root_port(root_port)?;
        if port_count == 0 {
            return Err(js_error("port_count must be 1..=255"));
        }
        let hub = UsbHubDevice::with_port_count(port_count);
        attach_device_at_path(&mut self.ctrl, &[root_port], Box::new(hub))
    }

    /// Detach any USB device attached at the given topology path.
    ///
    /// Path numbering follows the `aero_usb::hub::RootHub` contract:
    /// - `path[0]` is the root port index (0-based).
    /// - `path[1..]` are hub ports (1-based).
    pub fn detach_at_path(&mut self, path: JsValue) -> Result<(), JsValue> {
        let path = parse_ehci_usb_path(path)?;
        detach_device_at_path(&mut self.ctrl, &path)
    }

    /// Attach a WebHID-backed USB HID device at the given topology path.
    pub fn attach_webhid_device(
        &mut self,
        path: JsValue,
        device: &crate::WebHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = parse_ehci_usb_path(path)?;
        attach_device_at_path(&mut self.ctrl, &path, Box::new(device.as_usb_device()))
    }

    /// Attach a generic USB HID passthrough device at the given topology path.
    pub fn attach_usb_hid_passthrough_device(
        &mut self,
        path: JsValue,
        device: &crate::UsbHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = parse_ehci_usb_path(path)?;
        attach_device_at_path(&mut self.ctrl, &path, Box::new(device.as_usb_device()))
    }

    /// Serialize the current EHCI controller state into a deterministic snapshot blob.
    ///
    /// The returned bytes use the canonical `aero-io-snapshot` TLV format:
    /// - tag 1: `aero_usb::ehci::EhciController` snapshot bytes
    /// - tag 2: IRQ latch (`irq_level`) (redundant; derived from EHCI state)
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_ASSERTED: u16 = 2;

        let mut w = SnapshotWriter::new(EHCI_BRIDGE_DEVICE_ID, EHCI_BRIDGE_DEVICE_VERSION);
        w.field_bytes(TAG_CONTROLLER, self.ctrl.save_state());
        w.field_bool(TAG_IRQ_ASSERTED, self.ctrl.irq_level());
        w.finish()
    }

    /// Restore EHCI controller state from a snapshot blob produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        const TAG_CONTROLLER: u16 = 1;

        let r = SnapshotReader::parse(bytes, EHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| js_error(format!("Invalid EHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(EHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| js_error(format!("Invalid EHCI bridge snapshot: {e}")))?;

        let ctrl_bytes = r
            .bytes(TAG_CONTROLLER)
            .ok_or_else(|| js_error("EHCI bridge snapshot missing controller state"))?;
        self.ctrl
            .load_state(ctrl_bytes)
            .map_err(|e| js_error(format!("Invalid EHCI controller snapshot: {e}")))?;

        // WebUSB host actions are backed by JS Promises and cannot be resumed after restoring a VM
        // snapshot. Reset any restored passthrough device state so guest retries re-emit actions.
        let hub = self.ctrl.hub_mut();
        for port in 0..hub.num_ports() {
            if let Some(mut dev) = hub.port_device_mut(port) {
                reset_webusb_host_state_for_restore(&mut dev);
            }
        }

        // Recover an owned handle to a restored WebUSB passthrough device so the bridge can continue
        // draining actions / pushing completions after snapshot restore.
        self.webusb = recover_webusb_passthrough_device(&mut self.ctrl);
        self.webusb_connected = self.webusb.is_some();
        if let Some(dev) = self.webusb.as_ref() {
            // Ensure the recovered handle also has its host-side promise bookkeeping cleared.
            dev.reset_host_state_for_restore();
        }

        Ok(())
    }

    /// Snapshot the full EHCI + USB device tree state as deterministic bytes.
    ///
    /// The returned bytes represent only the USB stack state (controller + devices), not guest RAM.
    pub fn snapshot_state(&self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore EHCI + USB device state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }
}
