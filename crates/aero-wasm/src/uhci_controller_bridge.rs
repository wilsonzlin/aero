//! WASM-side bridge for exposing a guest-visible UHCI controller.
//!
//! The browser I/O worker exposes this as a PCI device with an IO BAR; port I/O
//! reads/writes are forwarded into this bridge which updates a Rust UHCI model
//! (`aero_usb::uhci::UhciController`).
//!
//! The UHCI schedule (frame list / QHs / TDs) lives in guest RAM. In the browser
//! runtime, guest physical address 0 begins at `guest_base` within the WASM
//! linear memory; this bridge implements `aero_usb::GuestMemory` to allow the
//! controller to read/write descriptors directly.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use aero_usb::GuestMemory;
use aero_usb::hub::UsbHubDevice;
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::usb::UsbDevice;

const UHCI_IO_BASE: u16 = 0;
const UHCI_IRQ_LINE: u8 = 0x0b;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

#[derive(Default)]
struct IrqLatch {
    asserted: bool,
    last_irq: Option<u8>,
}

impl InterruptController for IrqLatch {
    fn raise_irq(&mut self, irq: u8) {
        self.asserted = true;
        self.last_irq = Some(irq);
    }

    fn lower_irq(&mut self, _irq: u8) {
        self.asserted = false;
    }
}

#[derive(Clone, Copy)]
struct WasmGuestMemory {
    guest_base: u32,
    guest_size: u64,
}

impl WasmGuestMemory {
    #[inline]
    fn linear_ptr(&self, addr: u32, len: usize) -> Option<*const u8> {
        let addr_u64 = addr as u64;
        let len_u64 = len as u64;
        let end = addr_u64.checked_add(len_u64)?;
        if end > self.guest_size {
            return None;
        }
        let linear = (self.guest_base as u64).checked_add(addr_u64)?;
        Some(linear as *const u8)
    }

    #[inline]
    fn linear_ptr_mut(&self, addr: u32, len: usize) -> Option<*mut u8> {
        Some(self.linear_ptr(addr, len)? as *mut u8)
    }
}

impl GuestMemory for WasmGuestMemory {
    fn read(&self, addr: u32, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        // If the request goes out of bounds, read as much as possible and fill the rest with 0.
        let Some(max_len) = self.guest_size.checked_sub(addr as u64) else {
            buf.fill(0);
            return;
        };
        let copy_len = buf.len().min(max_len as usize);
        if copy_len == 0 {
            buf.fill(0);
            return;
        }

        let Some(ptr) = self.linear_ptr(addr, copy_len) else {
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

    fn write(&mut self, addr: u32, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        let Some(max_len) = self.guest_size.checked_sub(addr as u64) else {
            return;
        };
        let copy_len = buf.len().min(max_len as usize);
        if copy_len == 0 {
            return;
        }

        let Some(ptr) = self.linear_ptr_mut(addr, copy_len) else {
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

fn parse_usb_path(path: JsValue) -> Result<Vec<usize>, JsValue> {
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
            out.push(part as usize);
            continue;
        }
        if !(1..=255).contains(&part) {
            return Err(js_error("USB hub port numbers must be in 1..=255"));
        }
        out.push(part as usize);
    }
    Ok(out)
}

fn attach_device_at_path(
    ctrl: &mut UhciController,
    path: &[usize],
    device: Box<dyn UsbDevice>,
) -> Result<(), JsValue> {
    let Some((&root, rest)) = path.split_first() else {
        return Err(js_error("USB topology path must not be empty"));
    };

    if rest.is_empty() {
        ctrl.connect_device(root, device);
        return Ok(());
    }

    let Some(port) = ctrl.bus_mut().port_mut(root) else {
        return Err(js_error(format!("Invalid root port index {root}")));
    };
    let Some(root_dev) = port.device.as_mut() else {
        return Err(js_error(format!("No device attached at root port {root}")));
    };

    let mut current: &mut dyn UsbDevice = root_dev.as_mut();
    for (depth, &hub_port) in rest[..rest.len() - 1].iter().enumerate() {
        let Some(hub) = current.as_hub_mut() else {
            return Err(js_error(format!(
                "Device at depth {depth} is not a USB hub (cannot traverse hub_port={hub_port})"
            )));
        };
        let hub_idx = hub_port.checked_sub(1).ok_or_else(|| {
            js_error(format!(
                "Hub port numbers are 1-based (got 0 at depth {depth})"
            ))
        })?;
        let num_ports = hub.num_ports();
        if hub_idx >= num_ports {
            return Err(js_error(format!(
                "Invalid hub port {hub_port} at depth {depth} (hub has {num_ports} ports)"
            )));
        }
        let Some(next) = hub.downstream_device_mut(hub_idx) else {
            return Err(js_error(format!(
                "No device attached at hub port {hub_port} (depth {depth})"
            )));
        };
        current = next;
    }

    let last_port = rest[rest.len() - 1];
    let Some(hub) = current.as_hub_mut() else {
        return Err(js_error(format!(
            "Device at depth {} is not a USB hub",
            rest.len() - 1
        )));
    };
    let hub_idx = last_port.checked_sub(1).ok_or_else(|| {
        js_error(format!(
            "Hub port numbers are 1-based (got 0 at depth {})",
            rest.len() - 1
        ))
    })?;
    let num_ports = hub.num_ports();
    if hub_idx >= num_ports {
        return Err(js_error(format!(
            "Invalid hub port {last_port} at depth {} (hub has {num_ports} ports)",
            rest.len() - 1
        )));
    }

    hub.attach_downstream(hub_idx, device);
    Ok(())
}

fn detach_device_at_path(ctrl: &mut UhciController, path: &[usize]) -> Result<(), JsValue> {
    let Some((&root, rest)) = path.split_first() else {
        return Err(js_error("USB topology path must not be empty"));
    };

    if rest.is_empty() {
        ctrl.disconnect_device(root);
        return Ok(());
    }

    let Some(port) = ctrl.bus_mut().port_mut(root) else {
        return Err(js_error(format!("Invalid root port index {root}")));
    };
    let Some(root_dev) = port.device.as_mut() else {
        return Err(js_error(format!("No device attached at root port {root}")));
    };

    let mut current: &mut dyn UsbDevice = root_dev.as_mut();
    for (depth, &hub_port) in rest[..rest.len() - 1].iter().enumerate() {
        let Some(hub) = current.as_hub_mut() else {
            return Err(js_error(format!(
                "Device at depth {depth} is not a USB hub (cannot traverse hub_port={hub_port})"
            )));
        };
        let hub_idx = hub_port.checked_sub(1).ok_or_else(|| {
            js_error(format!(
                "Hub port numbers are 1-based (got 0 at depth {depth})"
            ))
        })?;
        let num_ports = hub.num_ports();
        if hub_idx >= num_ports {
            return Err(js_error(format!(
                "Invalid hub port {hub_port} at depth {depth} (hub has {num_ports} ports)"
            )));
        }
        let Some(next) = hub.downstream_device_mut(hub_idx) else {
            return Err(js_error(format!(
                "No device attached at hub port {hub_port} (depth {depth})"
            )));
        };
        current = next;
    }

    let last_port = rest[rest.len() - 1];
    let Some(hub) = current.as_hub_mut() else {
        return Err(js_error(format!(
            "Device at depth {} is not a USB hub",
            rest.len() - 1
        )));
    };
    let hub_idx = last_port.checked_sub(1).ok_or_else(|| {
        js_error(format!(
            "Hub port numbers are 1-based (got 0 at depth {})",
            rest.len() - 1
        ))
    })?;
    let num_ports = hub.num_ports();
    if hub_idx >= num_ports {
        return Err(js_error(format!(
            "Invalid hub port {last_port} at depth {} (hub has {num_ports} ports)",
            rest.len() - 1
        )));
    }
    hub.detach_downstream(hub_idx);
    Ok(())
}

/// WASM export: reusable UHCI controller model for the browser I/O worker.
///
/// The controller reads/writes guest RAM directly from the module's linear memory
/// (shared across workers in the threaded build) using `guest_base` and `guest_size`
/// from the `guest_ram_layout` contract.
#[wasm_bindgen]
pub struct UhciControllerBridge {
    ctrl: UhciController,
    guest_base: u32,
    guest_size: u64,
    irq: IrqLatch,
}

impl UhciControllerBridge {
    /// Rust-only helper for tests: connect an arbitrary USB device to a root port.
    pub fn connect_device_for_test(&mut self, root_port: usize, device: Box<dyn UsbDevice>) {
        self.ctrl.connect_device(root_port, device);
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

        let end = (guest_base as u64)
            .checked_add(guest_size_u64)
            .ok_or_else(|| js_error("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(js_error(format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        Ok(Self {
            // The TS PCI bus passes offset-within-BAR for I/O access, so keep the controller's
            // `io_base` at 0 and treat `offset` as the full port value.
            ctrl: UhciController::new(UHCI_IO_BASE, UHCI_IRQ_LINE),
            guest_base,
            guest_size: guest_size_u64,
            irq: IrqLatch::default(),
        })
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
        self.ctrl.port_read(offset, size)
    }

    pub fn io_write(&mut self, offset: u16, size: u8, value: u32) {
        let size = validate_port_size(size);
        if size == 0 {
            return;
        }
        self.ctrl.port_write(offset, size, value, &mut self.irq);
    }

    /// Advance the controller by exactly `frames` UHCI frames (1ms each).
    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }
        let mut mem = WasmGuestMemory {
            guest_base: self.guest_base,
            guest_size: self.guest_size,
        };
        for _ in 0..frames {
            self.ctrl.step_frame(&mut mem, &mut self.irq);
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
        self.irq.asserted
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
        self.ctrl.connect_device(root_port as usize, Box::new(hub));
        Ok(())
    }

    /// Detach any USB device attached at the given topology path.
    ///
    /// Path numbering follows `aero_usb::usb::UsbBus`:
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
}
