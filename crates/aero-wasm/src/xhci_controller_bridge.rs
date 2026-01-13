//! WASM-side bridge for exposing a guest-visible xHCI controller.
//!
//! The browser I/O worker exposes this as a PCI function with an MMIO BAR; reads/writes are
//! forwarded into this bridge which updates the canonical Rust xHCI model
//! (`aero_usb::xhci::XhciController`).
//!
//! The controller can DMA into guest RAM. In the browser runtime, guest physical address 0 begins
//! at `guest_base` within the module's linear memory; this bridge provides an `aero_usb::MemoryBus`
//! view over that region so the controller can read/write guest memory.
//!
//! The JS/TS side treats this export as optional because older deployed WASM builds will not
//! include it. When present, the bridge provides:
//! - MMIO register access (`mmio_read`/`mmio_write`)
//! - a coarse stepping hook (`step_frames` / `tick`) for deterministic time progression
//! - IRQ level query (`irq_asserted`)
//! - deterministic snapshot/restore helpers.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::xhci::XhciController;
use aero_usb::xhci::context::{XHCI_ROUTE_STRING_MAX_DEPTH, XHCI_ROUTE_STRING_MAX_PORT};
use aero_usb::{UsbDeviceModel, UsbHubAttachError, UsbSpeed, UsbWebUsbPassthroughDevice};

use crate::guest_memory_bus::{GuestMemoryBus, NoDmaMemory, wasm_memory_byte_len};

const XHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"XHCB";
const XHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

// Maximum downstream port value encoded in an xHCI route string (4-bit nibbles).
const XHCI_MAX_ROUTE_PORT: u32 = XHCI_ROUTE_STRING_MAX_PORT as u32;
// The Route String field is 20 bits wide (5 nibbles), so xHCI can only encode up to 5 downstream
// hub tiers (root port + 5 hub ports).
const XHCI_MAX_ROUTE_TIER_COUNT: usize = XHCI_ROUTE_STRING_MAX_DEPTH;

// Defensive cap for host-provided snapshot payloads. This is primarily to keep the JS→WASM copy
// bounded for `restore_state(bytes: &[u8])` parameters.
const MAX_XHCI_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum number of 1ms frames processed per `step_frames` call.
///
/// `step_frames` is called from JS/TS glue code; while the guest cannot influence this directly,
/// a host-side bug (or malicious embedder) could pass a huge value and stall the WASM worker for a
/// long time. Clamp to a deterministic constant so `step_frames(u32::MAX)` is always bounded.
const MAX_XHCI_STEP_FRAMES_PER_CALL: u32 = 10_000;
// Reserve the 2nd xHCI root port for the WebUSB passthrough device.
//
// This keeps the port assignment stable across snapshots and matches the UHCI bridge's convention
// of leaving root port 0 available for an external hub / HID passthrough in the future.
const WEBUSB_ROOT_PORT: u8 = 1;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn validate_mmio_size(size: u8) -> usize {
    match size {
        1 | 2 | 4 => size as usize,
        _ => 0,
    }
}

fn map_attach_error(err: UsbHubAttachError) -> JsValue {
    match err {
        UsbHubAttachError::NotAHub => js_error("device is not a USB hub"),
        UsbHubAttachError::InvalidPort => js_error("invalid hub/root port"),
        UsbHubAttachError::PortOccupied => js_error("USB hub port already occupied"),
        UsbHubAttachError::NoDevice => js_error("no device attached at hub port"),
    }
}

fn parse_xhci_usb_path(path: JsValue, port_count: u8) -> Result<Vec<u8>, JsValue> {
    if port_count == 0 {
        return Err(js_error("xHCI controller has no root ports"));
    }

    // Share the baseline USB topology path parsing semantics with UHCI:
    // - `path[0]` is a 0-based root port index
    // - `path[1..]` are 1-based hub port indices.
    let max_root_port = port_count.saturating_sub(1);
    let out = crate::usb_topology::parse_usb_path(path, max_root_port)?;

    if out.len() > XHCI_MAX_ROUTE_TIER_COUNT + 1 {
        return Err(js_error(format!(
            "xHCI topology path too deep (max {} downstream hub tiers)",
            XHCI_MAX_ROUTE_TIER_COUNT
        )));
    }
    let reserved_port = if port_count > WEBUSB_ROOT_PORT {
        WEBUSB_ROOT_PORT
    } else {
        0
    };
    if out[0] == reserved_port {
        return Err(js_error(format!(
            "xHCI root port {reserved_port} is reserved for WebUSB passthrough"
        )));
    }
    if out
        .iter()
        .skip(1)
        .any(|&p| u32::from(p) > XHCI_MAX_ROUTE_PORT)
    {
        return Err(js_error(format!(
            "xHCI hub port numbers must be in 1..={XHCI_MAX_ROUTE_PORT}"
        )));
    }

    Ok(out)
}

fn attach_device_at_path(
    ctrl: &mut XhciController,
    path: &[u8],
    device: Box<dyn UsbDeviceModel>,
) -> Result<(), JsValue> {
    // Replace semantics: detach any existing device at the path first.
    let _ = ctrl.detach_at_path(path);
    ctrl.attach_at_path(path, device).map_err(map_attach_error)
}

fn detach_device_at_path(ctrl: &mut XhciController, path: &[u8]) -> Result<(), JsValue> {
    match ctrl.detach_at_path(path) {
        Ok(()) => Ok(()),
        // Detach is intentionally idempotent for host-side topology management.
        Err(UsbHubAttachError::NoDevice) => Ok(()),
        Err(e) => Err(map_attach_error(e)),
    }
}
/// WASM export: reusable xHCI controller model for the browser I/O worker.
///
/// The controller reads/writes guest RAM directly from the module's linear memory (shared across
/// workers in the threaded build) using `guest_base` and `guest_size` from the `guest_ram_layout`
/// contract.
#[wasm_bindgen]
pub struct XhciControllerBridge {
    ctrl: XhciController,
    guest_base: u32,
    guest_size: u64,
    pci_command: u16,
    tick_count: u64,
    webusb: Option<UsbWebUsbPassthroughDevice>,
    webusb_connected: bool,
}

impl XhciControllerBridge {
    /// Rust-only helper for tests: return a clone of the current WebUSB device handle (if any).
    pub fn webusb_device_for_test(&mut self) -> UsbWebUsbPassthroughDevice {
        self.webusb
            .get_or_insert_with(|| UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High))
            .clone()
    }

    fn webusb_root_port(&self) -> u8 {
        let port_count = self.ctrl.port_count();
        if port_count > WEBUSB_ROOT_PORT {
            WEBUSB_ROOT_PORT
        } else {
            0
        }
    }
}

#[wasm_bindgen]
impl XhciControllerBridge {
    /// Create a new xHCI controller bound to the provided guest RAM mapping.
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
            ctrl: XhciController::new(),
            guest_base,
            guest_size: guest_size_u64,
            pci_command: 0,
            tick_count: 0,
            webusb: None,
            webusb_connected: false,
        })
    }

    pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0;
        }

        // Gate DMA on PCI Bus Master Enable (command bit 2). When bus mastering is disabled, the
        // controller must not touch guest memory.
        let dma_enabled = (self.pci_command & (1 << 2)) != 0;
        if dma_enabled {
            let mut mem = GuestMemoryBus::new(self.guest_base, self.guest_size);
            self.ctrl.mmio_read(&mut mem, u64::from(offset), size)
        } else {
            let mut mem = NoDmaMemory;
            self.ctrl.mmio_read(&mut mem, u64::from(offset), size)
        }
    }

    pub fn mmio_write(&mut self, offset: u32, size: u8, value: u32) {
        let size = validate_mmio_size(size);
        if size == 0 {
            return;
        }

        // Gate DMA on PCI Bus Master Enable (command bit 2). When bus mastering is disabled, the
        // controller must not touch guest memory.
        let dma_enabled = (self.pci_command & (1 << 2)) != 0;
        if dma_enabled {
            let mut mem = GuestMemoryBus::new(self.guest_base, self.guest_size);
            self.ctrl
                .mmio_write(&mut mem, u64::from(offset), size, value);
        } else {
            let mut mem = NoDmaMemory;
            self.ctrl
                .mmio_write(&mut mem, u64::from(offset), size, value);
        }
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

    /// Advance the controller by `frames` 1ms frames.
    ///
    /// This drives the underlying [`aero_usb::xhci::XhciController`] model. One "frame" is treated
    /// as 1ms of guest time (USB frame).
    ///
    /// DMA is gated on PCI Bus Master Enable (command bit 2):
    /// - When BME is set, the controller can DMA into guest RAM via [`GuestMemoryBus`]. Stepping also
    ///   runs transfer-ring work and drains queued events into the guest-configured event ring.
    /// - When BME is clear, the controller still advances internal timers (e.g. port reset timers)
    ///   but does not perform any DMA.
    pub fn step_frames(&mut self, frames: u32) {
        let frames = frames.min(MAX_XHCI_STEP_FRAMES_PER_CALL);
        if frames == 0 {
            return;
        }

        // Gate DMA on PCI Bus Master Enable (command bit 2). When bus mastering is disabled, the
        // controller must not touch guest memory.
        let dma_enabled = (self.pci_command & (1 << 2)) != 0;
        if dma_enabled {
            let mut mem = GuestMemoryBus::new(self.guest_base, self.guest_size);
            for _ in 0..frames {
                // `XhciController::tick_1ms` performs DMA and drains queued events into the guest
                // event ring, so a single call per frame is sufficient here.
                self.ctrl.tick_1ms(&mut mem);
            }
        } else {
            // Advance controller/port timers. Without this, operations like PORTSC port reset will
            // never complete (the xHCI model clears PR/PED after a timeout).
            for _ in 0..frames {
                self.ctrl.tick_1ms_no_dma();
            }
        }

        self.tick_count = self.tick_count.wrapping_add(u64::from(frames));
    }

    /// Convenience wrapper for stepping a single frame.
    pub fn step_frame(&mut self) {
        self.step_frames(1);
    }

    /// Alias for `step_frame` to match other USB controller bridges.
    pub fn tick_1ms(&mut self) {
        self.step_frame();
    }

    /// Alias for {@link step_frames} retained for older call sites.
    pub fn tick(&mut self, frames: u32) {
        self.step_frames(frames);
    }

    /// Optional polling hook for JS wrappers that expect a `poll()` method.
    pub fn poll(&mut self) {
        // Drain any queued event TRBs into the guest-configured event ring. This is non-advancing
        // work: it should not mutate the controller's time base, only make forward progress on
        // already-due operations.
        //
        // This performs DMA into guest memory and must therefore be gated on PCI Bus Master Enable
        // (command bit 2).
        let dma_enabled = (self.pci_command & (1 << 2)) != 0;
        if !dma_enabled {
            return;
        }

        let mut mem = GuestMemoryBus::new(self.guest_base, self.guest_size);
        self.ctrl.service_event_ring(&mut mem);
    }

    /// Whether the xHCI interrupt line should be raised.
    pub fn irq_asserted(&self) -> bool {
        self.ctrl.irq_level()
    }

    /// Connect or disconnect the WebUSB passthrough device on a reserved xHCI root port.
    ///
    /// The passthrough device is implemented by `aero_usb::UsbWebUsbPassthroughDevice` and emits
    /// host actions that must be executed by the browser `UsbBroker` (see `web/src/usb`).
    pub fn set_connected(&mut self, connected: bool) {
        let was_connected = self.webusb_connected;

        match (was_connected, connected) {
            (true, true) | (false, false) => {}
            (false, true) => {
                let root_port = self.webusb_root_port();
                // xHCI models a USB 2.0 root hub, so default the passthrough device to high-speed.
                //
                // We keep the handle alive across disconnects so action IDs remain monotonic across
                // reconnects.
                let dev = self.webusb.get_or_insert_with(|| {
                    UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High)
                });
                // Ensure the device is attached at a stable root port so guest activity routes into
                // the shared passthrough handle.
                let _ = attach_device_at_path(&mut self.ctrl, &[root_port], Box::new(dev.clone()));
                self.webusb_connected = true;
            }
            (true, false) => {
                let root_port = self.webusb_root_port();
                let _ = detach_device_at_path(&mut self.ctrl, &[root_port]);
                self.webusb_connected = false;
                // Preserve UHCI semantics: disconnect drops queued/in-flight host state, but keep
                // the handle alive so `UsbPassthroughDevice.next_id` remains monotonic.
                if let Some(dev) = self.webusb.as_ref() {
                    dev.reset();
                }
            }
        }
    }

    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        if !self.webusb_connected {
            return Ok(JsValue::NULL);
        }
        let Some(dev) = self.webusb.as_ref() else {
            return Ok(JsValue::NULL);
        };
        let actions: Vec<UsbHostAction> = dev.drain_actions();
        if actions.is_empty() {
            return Ok(JsValue::NULL);
        }
        serde_wasm_bindgen::to_value(&actions).map_err(|e| js_error(e))
    }

    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        // Completions can race disconnects due to async host WebUSB operations; ignore late
        // completions when the passthrough device is detached.
        if !self.webusb_connected {
            return Ok(());
        }

        let completion: UsbHostCompletion =
            serde_wasm_bindgen::from_value(completion).map_err(|e| js_error(e))?;

        if let Some(dev) = self.webusb.as_ref() {
            dev.push_completion(completion);
        }
        Ok(())
    }
    pub fn reset(&mut self) {
        if self.webusb_connected {
            if let Some(dev) = self.webusb.as_ref() {
                dev.reset();
            }
        }
    }

    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        if !self.webusb_connected {
            return Ok(JsValue::NULL);
        }
        let Some(summary) = self.webusb.as_ref().map(|d| d.pending_summary()) else {
            return Ok(JsValue::NULL);
        };
        serde_wasm_bindgen::to_value(&summary).map_err(|e| js_error(e))
    }

    /// Serialize the current xHCI controller state into a deterministic snapshot blob.
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_TICK_COUNT: u16 = 2;
        const TAG_WEBUSB_DEVICE: u16 = 3;

        let mut w = SnapshotWriter::new(XHCI_BRIDGE_DEVICE_ID, XHCI_BRIDGE_DEVICE_VERSION);
        w.field_bytes(TAG_CONTROLLER, self.ctrl.save_state());
        w.field_u64(TAG_TICK_COUNT, self.tick_count);
        if self.webusb_connected {
            if let Some(dev) = self.webusb.as_ref() {
                w.field_bytes(TAG_WEBUSB_DEVICE, dev.save_state());
            }
        }
        w.finish()
    }

    /// Restore xHCI controller state from a snapshot blob produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        if bytes.len() > MAX_XHCI_SNAPSHOT_BYTES {
            return Err(js_error(format!(
                "xHCI snapshot too large ({} bytes, max {})",
                bytes.len(),
                MAX_XHCI_SNAPSHOT_BYTES
            )));
        }

        const TAG_CONTROLLER: u16 = 1;
        const TAG_TICK_COUNT: u16 = 2;
        const TAG_WEBUSB_DEVICE: u16 = 3;

        let r = SnapshotReader::parse(bytes, XHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(XHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?;

        let has_webusb = r.bytes(TAG_WEBUSB_DEVICE).is_some();

        let ctrl_bytes = r
            .bytes(TAG_CONTROLLER)
            .ok_or_else(|| js_error("xHCI bridge snapshot missing controller state"))?;
        self.ctrl
            .load_state(ctrl_bytes)
            .map_err(|e| js_error(format!("Invalid xHCI controller snapshot: {e}")))?;
        self.ctrl.reset_host_state_for_restore();

        self.tick_count = r
            .u64(TAG_TICK_COUNT)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?
            .unwrap_or(0);

        // Attach/detach the WebUSB passthrough device after restoring controller state so the guest
        // topology matches the bridge-level snapshot.
        //
        // WebUSB passthrough is backed by a host-managed `UsbWebUsbPassthroughDevice` handle. We
        // reattach that stable handle here (rather than relying on any device instance
        // reconstructed purely from the controller snapshot bytes) so the JS host integration keeps
        // draining actions / pushing completions into the correct device model.
        self.set_connected(has_webusb);

        if let Some(buf) = r.bytes(TAG_WEBUSB_DEVICE) {
            let dev = self
                .webusb
                .as_mut()
                .ok_or_else(|| js_error("xHCI bridge snapshot missing WebUSB device"))?;
            dev.load_state(buf)
                .map_err(|e| js_error(format!("Invalid WebUSB device snapshot: {e}")))?;
            dev.reset_host_state_for_restore();
        }

        Ok(())
    }

    /// Snapshot the xHCI controller state as deterministic bytes.
    pub fn snapshot_state(&self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore xHCI controller state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }

    /// Attach a USB hub device to a root port.
    ///
    /// `port_count` is the number of downstream ports on the hub.
    ///
    /// This is clamped to `1..=15` to preserve xHCI route-string constraints (hub port numbers are
    /// encoded as 4-bit nibbles).
    pub fn attach_hub(&mut self, root_port: u32, port_count: u32) -> Result<(), JsValue> {
        let ctrl_ports = self.ctrl.port_count();
        let reserved_port = self.webusb_root_port();
        if root_port == reserved_port as u32 {
            return Err(js_error(format!(
                "xHCI root port {reserved_port} is reserved for WebUSB passthrough"
            )));
        }
        if root_port >= ctrl_ports as u32 {
            let max = ctrl_ports.saturating_sub(1);
            return Err(js_error(format!(
                "xHCI root port out of range (expected 0..={max})"
            )));
        }

        let root_port = root_port as u8;
        let port_count = port_count.clamp(1, XHCI_MAX_ROUTE_PORT) as u8;

        // Replace semantics: detach any existing device at the root port first.
        let _ = self.ctrl.detach_at_path(&[root_port]);
        self.ctrl
            .attach_hub(root_port, port_count)
            .map_err(map_attach_error)
    }

    /// Detach any USB device attached at the given topology path.
    pub fn detach_at_path(&mut self, path: JsValue) -> Result<(), JsValue> {
        let path = parse_xhci_usb_path(path, self.ctrl.port_count())?;
        detach_device_at_path(&mut self.ctrl, &path)
    }

    /// Attach a WebHID-backed USB HID device at the given topology path.
    pub fn attach_webhid_device(
        &mut self,
        path: JsValue,
        device: &crate::WebHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = parse_xhci_usb_path(path, self.ctrl.port_count())?;
        attach_device_at_path(&mut self.ctrl, &path, Box::new(device.as_usb_device()))
    }

    /// Attach a generic USB HID passthrough device at the given topology path.
    pub fn attach_usb_hid_passthrough_device(
        &mut self,
        path: JsValue,
        device: &crate::UsbHidPassthroughBridge,
    ) -> Result<(), JsValue> {
        let path = parse_xhci_usb_path(path, self.ctrl.port_count())?;
        attach_device_at_path(&mut self.ctrl, &path, Box::new(device.as_usb_device()))
    }
}
