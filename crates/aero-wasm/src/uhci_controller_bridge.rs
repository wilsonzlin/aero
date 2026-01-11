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

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::GuestMemory;
use aero_usb::UsbWebUsbPassthroughDevice;
use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::usb::UsbDevice;

const UHCI_IO_BASE: u16 = 0;
const UHCI_IRQ_LINE: u8 = 0x0b;

const UHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"UHCB";
const UHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

// UHCI register layout (0x20 bytes).
const REG_USBCMD: u16 = 0x00;
const REG_USBSTS: u16 = 0x02;
const REG_USBINTR: u16 = 0x04;
const REG_FRNUM: u16 = 0x06;
const REG_FRBASEADD: u16 = 0x08;
const REG_SOFMOD: u16 = 0x0C;
const REG_PORTSC1: u16 = 0x10;
const REG_PORTSC2: u16 = 0x12;

// Reserve the 2nd UHCI root port for the WebUSB passthrough device. Root port 0 is used for
// the external WebHID hub by default (see `web/src/platform/webhid_passthrough.ts`).
const WEBUSB_ROOT_PORT: usize = 1;

// PORTSC bits used by the `aero_usb::uhci` model. We only need these for masked writes.
const PORTSC_CSC: u16 = 1 << 1;
const PORTSC_PEDC: u16 = 1 << 3;
const PORTSC_PR: u16 = 1 << 9;

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

pub(crate) fn parse_usb_path(path: JsValue) -> Result<Vec<usize>, JsValue> {
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

pub(crate) fn attach_device_at_path(
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

pub(crate) fn detach_device_at_path(
    ctrl: &mut UhciController,
    path: &[usize],
) -> Result<(), JsValue> {
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

    fn read_u8(&mut self, offset: u16) -> u8 {
        match offset {
            0x00 | 0x01 => {
                let w = self.ctrl.port_read(REG_USBCMD, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x02 | 0x03 => {
                let w = self.ctrl.port_read(REG_USBSTS, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x04 | 0x05 => {
                let w = self.ctrl.port_read(REG_USBINTR, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x06 | 0x07 => {
                let w = self.ctrl.port_read(REG_FRNUM, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x08..=0x0b => {
                let d = self.ctrl.port_read(REG_FRBASEADD, 4);
                let shift = (offset - REG_FRBASEADD) * 8;
                ((d >> shift) & 0xff) as u8
            }
            0x0c => self.ctrl.port_read(REG_SOFMOD, 1) as u8,
            0x10 | 0x11 => {
                let w = self.ctrl.port_read(REG_PORTSC1, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x12 | 0x13 => {
                let w = self.ctrl.port_read(REG_PORTSC2, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            // Reserved bytes in the decoded 0x20-byte UHCI window should read as 0 so that
            // wide I/O operations don't see spurious 0xFF in the upper bytes.
            _ => 0,
        }
    }

    fn write_portsc_masked(&mut self, reg: u16, shift: u16, value: u8) {
        let cur = self.ctrl.port_read(reg, 2) as u16;
        let mask: u16 = 0xff << shift;
        let written = (value as u16) << shift;

        let mut next = (cur & !mask) | (written & mask);

        // W1C bits: only clear when explicitly written.
        let w1c = PORTSC_CSC | PORTSC_PEDC;
        next &= !w1c;
        next |= written & w1c;

        // Reset bit: treat as a "write-1-to-start" action bit; do not re-assert just because
        // it is currently set in the readable value.
        next &= !PORTSC_PR;
        next |= written & PORTSC_PR;

        self.ctrl.port_write(reg, 2, next as u32, &mut self.irq);
    }

    fn write_u8(&mut self, offset: u16, value: u8) {
        match offset {
            // USBCMD: read/modify/write 16-bit register.
            0x00 | 0x01 => {
                let cur = self.ctrl.port_read(REG_USBCMD, 2) as u16;
                let shift = (offset & 1) * 8;
                let mask = 0xffu16 << shift;
                let next = (cur & !mask) | ((value as u16) << shift);
                self.ctrl
                    .port_write(REG_USBCMD, 2, next as u32, &mut self.irq);
            }

            // USBSTS: W1C (write-one-to-clear). Byte writes should only clear bits in that byte.
            0x02 | 0x03 => {
                let shift = (offset & 1) * 8;
                let v = (value as u16) << shift;
                self.ctrl.port_write(REG_USBSTS, 2, v as u32, &mut self.irq);
            }

            // USBINTR: read/modify/write so high-byte writes don't clear the low-byte enables.
            0x04 | 0x05 => {
                let cur = self.ctrl.port_read(REG_USBINTR, 2) as u16;
                let shift = (offset & 1) * 8;
                let mask = 0xffu16 << shift;
                let next = (cur & !mask) | ((value as u16) << shift);
                self.ctrl
                    .port_write(REG_USBINTR, 2, next as u32, &mut self.irq);
            }

            // FRNUM: 11-bit register; read/modify/write for byte accesses.
            0x06 | 0x07 => {
                let cur = self.ctrl.port_read(REG_FRNUM, 2) as u16;
                let shift = (offset & 1) * 8;
                let mask = 0xffu16 << shift;
                let next = (cur & !mask) | ((value as u16) << shift);
                self.ctrl
                    .port_write(REG_FRNUM, 2, next as u32, &mut self.irq);
            }

            // FRBASEADD: 32-bit.
            0x08..=0x0b => {
                let cur = self.ctrl.port_read(REG_FRBASEADD, 4);
                let shift = (offset - REG_FRBASEADD) * 8;
                let mask = 0xffu32 << shift;
                let next = (cur & !mask) | ((value as u32) << shift);
                self.ctrl.port_write(REG_FRBASEADD, 4, next, &mut self.irq);
            }

            // SOFMOD: 8-bit register at 0x0C.
            0x0c => {
                self.ctrl
                    .port_write(REG_SOFMOD, 1, value as u32, &mut self.irq);
            }

            // PORTSC1/2: masked writes so high-byte accesses don't clear low-byte W1C bits.
            0x10 => self.write_portsc_masked(REG_PORTSC1, 0, value),
            0x11 => self.write_portsc_masked(REG_PORTSC1, 8, value),
            0x12 => self.write_portsc_masked(REG_PORTSC2, 0, value),
            0x13 => self.write_portsc_masked(REG_PORTSC2, 8, value),

            // Reserved/unimplemented bytes are ignored.
            _ => {}
        }
    }

    fn webusb_device(&self) -> Option<&UsbWebUsbPassthroughDevice> {
        let port = self.ctrl.bus().port(WEBUSB_ROOT_PORT)?;
        let dev = port.device.as_ref()?;
        dev.as_any().downcast_ref::<UsbWebUsbPassthroughDevice>()
    }

    fn webusb_device_mut(&mut self) -> Option<&mut UsbWebUsbPassthroughDevice> {
        let port = self.ctrl.bus_mut().port_mut(WEBUSB_ROOT_PORT)?;
        let dev = port.device.as_mut()?;
        dev.as_any_mut()
            .downcast_mut::<UsbWebUsbPassthroughDevice>()
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
        match size {
            1 => u32::from(self.read_u8(offset)),
            2 => {
                let lo = self.read_u8(offset);
                let hi = self.read_u8(offset.wrapping_add(1));
                u32::from(lo) | (u32::from(hi) << 8)
            }
            4 => {
                let b0 = self.read_u8(offset);
                let b1 = self.read_u8(offset.wrapping_add(1));
                let b2 = self.read_u8(offset.wrapping_add(2));
                let b3 = self.read_u8(offset.wrapping_add(3));
                u32::from(b0) | (u32::from(b1) << 8) | (u32::from(b2) << 16) | (u32::from(b3) << 24)
            }
            _ => 0xFFFF_FFFF,
        }
    }

    pub fn io_write(&mut self, offset: u16, size: u8, value: u32) {
        let size = validate_port_size(size);

        match (offset, size) {
            // Use native 16-bit writes for the 16-bit registers.
            (REG_USBCMD | REG_USBSTS | REG_USBINTR | REG_FRNUM | REG_PORTSC1 | REG_PORTSC2, 2) => {
                self.ctrl.port_write(offset, 2, value, &mut self.irq);
            }
            // FRBASEADD is natively 32-bit.
            (REG_FRBASEADD, 4) => {
                self.ctrl.port_write(REG_FRBASEADD, 4, value, &mut self.irq);
            }
            // Some drivers use 32-bit I/O at offset 0/4 to access paired 16-bit registers.
            (REG_USBCMD, 4) => {
                let cmd = value & 0xffff;
                let sts = (value >> 16) & 0xffff;
                self.ctrl.port_write(REG_USBCMD, 2, cmd, &mut self.irq);
                self.ctrl.port_write(REG_USBSTS, 2, sts, &mut self.irq);
            }
            (REG_USBINTR, 4) => {
                let intr = value & 0xffff;
                let frnum = (value >> 16) & 0xffff;
                self.ctrl.port_write(REG_USBINTR, 2, intr, &mut self.irq);
                self.ctrl.port_write(REG_FRNUM, 2, frnum, &mut self.irq);
            }
            (REG_PORTSC1, 4) => {
                let p0 = value & 0xffff;
                let p1 = (value >> 16) & 0xffff;
                self.ctrl.port_write(REG_PORTSC1, 2, p0, &mut self.irq);
                self.ctrl.port_write(REG_PORTSC2, 2, p1, &mut self.irq);
            }

            // Fallback: treat as a sequence of byte writes.
            (_, 1) => self.write_u8(offset, value as u8),
            (_, 2) => {
                self.write_u8(offset, (value & 0xff) as u8);
                self.write_u8(offset.wrapping_add(1), ((value >> 8) & 0xff) as u8);
            }
            (_, 4) => {
                for i in 0..4u16 {
                    self.write_u8(offset.wrapping_add(i), ((value >> (i * 8)) & 0xff) as u8);
                }
            }
            _ => {}
        }
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

    /// Connect or disconnect the WebUSB passthrough device on a reserved UHCI root port.
    ///
    /// The passthrough device is implemented by `aero_usb::UsbWebUsbPassthroughDevice` and emits
    /// host actions that must be executed by the browser `UsbBroker` (see `web/src/usb`).
    pub fn set_connected(&mut self, connected: bool) {
        let was_connected = self
            .ctrl
            .bus()
            .port(WEBUSB_ROOT_PORT)
            .is_some_and(|p| p.connected);

        match (was_connected, connected) {
            (true, true) | (false, false) => {}
            (false, true) => {
                self.ctrl.connect_device(
                    WEBUSB_ROOT_PORT,
                    Box::new(UsbWebUsbPassthroughDevice::new()),
                );
            }
            (true, false) => {
                self.ctrl.disconnect_device(WEBUSB_ROOT_PORT);
            }
        }
    }

    /// Drain queued WebUSB passthrough host actions as plain JS objects.
    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let Some(dev) = self.webusb_device_mut() else {
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
        if let Some(dev) = self.webusb_device_mut() {
            dev.push_completion(completion);
        }
        Ok(())
    }

    /// Reset the WebUSB passthrough device without disturbing the rest of the USB topology.
    pub fn reset(&mut self) {
        if let Some(dev) = self.webusb_device_mut() {
            dev.reset();
        }
    }

    /// Return a debug summary of queued actions/completions for the WebUSB passthrough device.
    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        let Some(summary) = self.webusb_device().map(|d| d.pending_summary()) else {
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

    /// Serialize the current UHCI controller state into a deterministic snapshot blob.
    ///
    /// The returned bytes use the canonical `aero-io-snapshot` TLV format:
    /// - tag 1: `aero_usb::uhci::UhciController` snapshot bytes
    /// - tag 2: bridge-side IRQ latch (`irq_asserted`)
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_ASSERTED: u16 = 2;

        let mut w = SnapshotWriter::new(UHCI_BRIDGE_DEVICE_ID, UHCI_BRIDGE_DEVICE_VERSION);
        w.field_bytes(TAG_CONTROLLER, self.ctrl.save_state());
        w.field_bool(TAG_IRQ_ASSERTED, self.irq.asserted);
        w.finish()
    }

    /// Restore UHCI controller state from a snapshot blob produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_IRQ_ASSERTED: u16 = 2;

        let r = SnapshotReader::parse(bytes, UHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| js_error(format!("Invalid UHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(UHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| js_error(format!("Invalid UHCI bridge snapshot: {e}")))?;

        let ctrl_bytes = r
            .bytes(TAG_CONTROLLER)
            .ok_or_else(|| js_error("UHCI bridge snapshot missing controller state"))?;
        self.ctrl
            .load_state(ctrl_bytes)
            .map_err(|e| js_error(format!("Invalid UHCI controller snapshot: {e}")))?;

        self.irq.asserted = r
            .bool(TAG_IRQ_ASSERTED)
            .map_err(|e| js_error(format!("Invalid UHCI bridge snapshot: {e}")))?
            .unwrap_or(false);
        self.irq.last_irq = if self.irq.asserted {
            Some(UHCI_IRQ_LINE)
        } else {
            None
        };

        Ok(())
    }
}
