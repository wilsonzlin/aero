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
use aero_usb::MemoryBus;
use aero_usb::{UsbDeviceModel, UsbHubAttachError, UsbSpeed, UsbWebUsbPassthroughDevice};

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
// Reserve the 2nd xHCI root port for the WebUSB passthrough device.
//
// This keeps the port assignment stable across snapshots and matches the UHCI bridge's convention
// of leaving root port 0 available for an external hub / HID passthrough in the future.
const WEBUSB_ROOT_PORT: u8 = 1;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
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
    let parts: Vec<u32> = serde_wasm_bindgen::from_value(path)
        .map_err(|e| js_error(format!("Invalid USB topology path: {e}")))?;
    if parts.is_empty() {
        return Err(js_error("USB topology path must not be empty"));
    }
    if parts.len() > XHCI_MAX_ROUTE_TIER_COUNT + 1 {
        return Err(js_error(format!(
            "xHCI topology path too deep (max {} downstream hub tiers)",
            XHCI_MAX_ROUTE_TIER_COUNT
        )));
    }

    let root = parts[0];
    if root >= port_count as u32 {
        // Root ports are 0-based in the guest-facing contract; xHCI itself uses 1-based port IDs.
        let max = port_count.saturating_sub(1);
        return Err(js_error(format!(
            "xHCI root port out of range (expected 0..={max})"
        )));
    }
    if root == WEBUSB_ROOT_PORT as u32 {
        return Err(js_error(format!(
            "xHCI root port {WEBUSB_ROOT_PORT} is reserved for WebUSB passthrough"
        )));
    }

    let mut out = Vec::with_capacity(parts.len());
    out.push(root as u8);
    for &part in &parts[1..] {
        if !(1..=XHCI_MAX_ROUTE_PORT).contains(&part) {
            return Err(js_error(format!(
                "xHCI hub port numbers must be in 1..={XHCI_MAX_ROUTE_PORT}"
            )));
        }
        out.push(part as u8);
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

#[derive(Clone, Copy)]
struct WasmGuestMemory {
    guest_base: u32,
    ram_bytes: u64,
}

impl WasmGuestMemory {
    #[inline]
    fn linear_ptr(&self, ram_offset: u64, len: usize) -> Option<*const u8> {
        let end = ram_offset.checked_add(len as u64)?;
        if end > self.ram_bytes {
            return None;
        }
        let linear = (self.guest_base as u64).checked_add(ram_offset)?;
        u32::try_from(linear).ok().map(|v| v as *const u8)
    }

    #[inline]
    fn linear_ptr_mut(&self, ram_offset: u64, len: usize) -> Option<*mut u8> {
        Some(self.linear_ptr(ram_offset, len)? as *mut u8)
    }
}

impl MemoryBus for WasmGuestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }
        let mut cur_paddr = paddr;
        let mut off = 0usize;

        while off < buf.len() {
            let remaining = buf.len() - off;
            let chunk = crate::guest_phys::translate_guest_paddr_chunk(
                self.ram_bytes,
                cur_paddr,
                remaining,
            );
            let chunk_len = match chunk {
                crate::guest_phys::GuestRamChunk::Ram { ram_offset, len } => {
                    let Some(ptr) = self.linear_ptr(ram_offset, len) else {
                        buf[off..].fill(0);
                        return;
                    };
                    // Safety: `translate_guest_paddr_chunk` bounds-checks against the configured guest
                    // RAM size.
                    unsafe {
                        core::ptr::copy_nonoverlapping(ptr, buf[off..].as_mut_ptr(), len);
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::Hole { len } => {
                    buf[off..off + len].fill(0xFF);
                    len
                }
                crate::guest_phys::GuestRamChunk::OutOfBounds { len } => {
                    buf[off..off + len].fill(0);
                    len
                }
            };

            if chunk_len == 0 {
                break;
            }
            off += chunk_len;
            cur_paddr = match cur_paddr.checked_add(chunk_len as u64) {
                Some(v) => v,
                None => {
                    buf[off..].fill(0);
                    return;
                }
            };
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        let mut cur_paddr = paddr;
        let mut off = 0usize;

        while off < buf.len() {
            let remaining = buf.len() - off;
            let chunk = crate::guest_phys::translate_guest_paddr_chunk(
                self.ram_bytes,
                cur_paddr,
                remaining,
            );
            let chunk_len = match chunk {
                crate::guest_phys::GuestRamChunk::Ram { ram_offset, len } => {
                    let Some(ptr) = self.linear_ptr_mut(ram_offset, len) else {
                        return;
                    };
                    // Safety: `translate_guest_paddr_chunk` bounds-checks against the configured guest
                    // RAM size.
                    unsafe {
                        core::ptr::copy_nonoverlapping(buf[off..].as_ptr(), ptr, len);
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::Hole { len } => {
                    // Open bus: writes are ignored.
                    len
                }
                crate::guest_phys::GuestRamChunk::OutOfBounds { len } => {
                    // Preserve existing semantics: out-of-range writes are ignored.
                    len
                }
            };

            if chunk_len == 0 {
                break;
            }
            off += chunk_len;
            cur_paddr = match cur_paddr.checked_add(chunk_len as u64) {
                Some(v) => v,
                None => return,
            };
        }
    }
}

struct NoDmaMemory;

impl MemoryBus for NoDmaMemory {
    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        buf.fill(0xFF);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
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
            let mut mem = WasmGuestMemory {
                guest_base: self.guest_base,
                ram_bytes: self.guest_size,
            };
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
            let mut mem = WasmGuestMemory {
                guest_base: self.guest_base,
                ram_bytes: self.guest_size,
            };
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
    /// This advances controller internal time (e.g. port reset timers). When PCI Bus Master Enable
    /// (BME) is set, it also executes any pending transfer-ring work and drains queued events into
    /// the guest event ring.
    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }
        self.tick_count = self.tick_count.wrapping_add(u64::from(frames));

        let dma_enabled = (self.pci_command & (1 << 2)) != 0;
        if dma_enabled {
            let mut mem = WasmGuestMemory {
                guest_base: self.guest_base,
                ram_bytes: self.guest_size,
            };
            for _ in 0..frames {
                self.ctrl.tick_1ms_and_service_event_ring(&mut mem);
            }
        } else {
            // Advance controller/port timers. Without this, operations like PORTSC port reset will never
            // complete (the xHCI model clears PR/PED after a timeout in `tick_1ms`).
            for _ in 0..frames {
                self.ctrl.tick_1ms();
            }
        }
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

        let mut mem = WasmGuestMemory {
            guest_base: self.guest_base,
            ram_bytes: self.guest_size,
        };
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
                // xHCI models a USB 2.0 root hub, so default the passthrough device to high-speed.
                //
                // We keep the handle alive across disconnects so action IDs remain monotonic across
                // reconnects.
                let dev = self
                    .webusb
                    .get_or_insert_with(|| UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High));
                // Ensure the device is attached at a stable root port so guest activity routes into
                // the shared passthrough handle.
                let _ = attach_device_at_path(&mut self.ctrl, &[WEBUSB_ROOT_PORT], Box::new(dev.clone()));
                self.webusb_connected = true;
            }
            (true, false) => {
                let _ = detach_device_at_path(&mut self.ctrl, &[WEBUSB_ROOT_PORT]);
                self.webusb_connected = false;
                // Preserve pre-existing semantics: disconnecting the device drops any queued
                // actions and in-flight state, but we keep the handle alive so
                // `UsbPassthroughDevice.next_id` remains monotonic across reconnects.
                if let Some(dev) = self.webusb.as_ref() {
                    dev.reset();
                }
            }
        }
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
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if self.webusb_connected {
            if let Some(dev) = self.webusb.as_ref() {
                dev.push_completion(completion);
            }
        }
        Ok(())
    }

    /// Reset the WebUSB passthrough device without disturbing the rest of the xHCI controller.
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

        self.tick_count = r
            .u64(TAG_TICK_COUNT)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?
            .unwrap_or(0);

        // Attach/detach the WebUSB passthrough device after restoring controller state so the
        // topology is preserved (the xHCI controller snapshot does not yet include device
        // attachments).
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
    /// `port_count` is the number of downstream ports on the hub (1..=15). This is capped to 15 to
    /// preserve xHCI route-string constraints (hub port numbers are encoded as 4-bit nibbles).
    pub fn attach_hub(&mut self, root_port: u32, port_count: u32) -> Result<(), JsValue> {
        let ctrl_ports = self.ctrl.port_count();
        if root_port == WEBUSB_ROOT_PORT as u32 {
            return Err(js_error(format!(
                "xHCI root port {WEBUSB_ROOT_PORT} is reserved for WebUSB passthrough"
            )));
        }
        if root_port >= ctrl_ports as u32 {
            let max = ctrl_ports.saturating_sub(1);
            return Err(js_error(format!(
                "xHCI root port out of range (expected 0..={max})"
            )));
        }
        if !(1..=XHCI_MAX_ROUTE_PORT).contains(&port_count) {
            return Err(js_error(format!(
                "xHCI hub port count must be in 1..={XHCI_MAX_ROUTE_PORT}"
            )));
        }

        let root_port = root_port as u8;
        let port_count = port_count as u8;

        // Replace semantics: detach any existing device at the root port first.
        let _ = self.ctrl.detach_at_path(&[root_port]);
        self.ctrl.attach_hub(root_port, port_count).map_err(map_attach_error)
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
