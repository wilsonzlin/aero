use wasm_bindgen::prelude::*;

use aero_usb::hub::UsbHubDevice;
use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::{GuestMemory as UsbGuestMemory, UsbWebUsbPassthroughDevice};

// UHCI register layout (0x20 bytes).
const REG_USBCMD: u16 = 0x00;
const REG_USBSTS: u16 = 0x02;
const REG_USBINTR: u16 = 0x04;
const REG_FRNUM: u16 = 0x06;
const REG_FRBASEADD: u16 = 0x08;
const REG_SOFMOD: u16 = 0x0C;
const REG_PORTSC1: u16 = 0x10;
const REG_PORTSC2: u16 = 0x12;

// PORTSC bits we need for masked byte writes.
const PORTSC_CSC: u16 = 1 << 1;
const PORTSC_PEDC: u16 = 1 << 3;
const PORTSC_PR: u16 = 1 << 9;

const USBCMD_HCRESET: u32 = 1 << 1;

const ROOT_PORT_EXTERNAL_HUB: usize = 0;
const ROOT_PORT_WEBUSB: usize = 1;
// Must match `web/src/platform/webhid_passthrough.ts::DEFAULT_EXTERNAL_HUB_PORT_COUNT`.
const EXTERNAL_HUB_PORT_COUNT: u8 = 16;

#[derive(Debug, Default)]
struct WasmIrqCapture {
    asserted: bool,
}

impl InterruptController for WasmIrqCapture {
    fn raise_irq(&mut self, _irq: u8) {
        self.asserted = true;
    }

    fn lower_irq(&mut self, _irq: u8) {
        self.asserted = false;
    }
}

/// Guest memory accessor backed by the module's wasm linear memory.
///
/// Guest physical address 0 maps to `guest_base` inside the linear memory (see
/// `guest_ram_layout()` in `aero-wasm`).
#[derive(Debug, Clone, Copy)]
struct WasmGuestMemory {
    guest_base: u32,
    mem_bytes: u64,
}

impl WasmGuestMemory {
    fn new(guest_base: u32) -> Self {
        let pages = core::arch::wasm32::memory_size(0) as u64;
        Self {
            guest_base,
            mem_bytes: pages.saturating_mul(64 * 1024),
        }
    }

    fn translate(&self, addr: u32) -> Option<u32> {
        let mapped = (self.guest_base as u64).saturating_add(addr as u64);
        if mapped >= self.mem_bytes {
            return None;
        }
        u32::try_from(mapped).ok()
    }
}

impl UsbGuestMemory for WasmGuestMemory {
    fn read(&self, addr: u32, buf: &mut [u8]) {
        let Some(start) = self.translate(addr) else {
            buf.fill(0);
            return;
        };

        let start_u64 = start as u64;
        let available = (self.mem_bytes - start_u64).min(buf.len() as u64) as usize;
        if available == 0 {
            buf.fill(0);
            return;
        }

        // SAFETY: Bounds checked against the current linear memory size and `buf` is a valid slice.
        unsafe {
            let src = core::slice::from_raw_parts(start as *const u8, available);
            buf[..available].copy_from_slice(src);
        }
        if available < buf.len() {
            buf[available..].fill(0);
        }
    }

    fn write(&mut self, addr: u32, buf: &[u8]) {
        let Some(start) = self.translate(addr) else {
            return;
        };

        let start_u64 = start as u64;
        let available = (self.mem_bytes - start_u64).min(buf.len() as u64) as usize;
        if available == 0 {
            return;
        }

        // SAFETY: Bounds checked against the current linear memory size and `buf` is a valid slice.
        unsafe {
            let dst = core::slice::from_raw_parts_mut(start as *mut u8, available);
            dst.copy_from_slice(&buf[..available]);
        }
    }
}

#[wasm_bindgen]
pub struct WebUsbUhciBridge {
    guest_base: u32,
    controller: UhciController,
    irq: WasmIrqCapture,
}

#[wasm_bindgen]
impl WebUsbUhciBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32) -> Self {
        let mut controller = UhciController::new(0, 11);
        controller.connect_device(
            ROOT_PORT_EXTERNAL_HUB,
            Box::new(UsbHubDevice::with_port_count(EXTERNAL_HUB_PORT_COUNT)),
        );
        Self {
            guest_base,
            controller,
            irq: WasmIrqCapture::default(),
        }
    }

    pub fn io_read(&mut self, offset: u32, size: u32) -> u32 {
        let Ok(offset) = u16::try_from(offset) else {
            return 0xffff_ffff;
        };
        let Ok(size) = u8::try_from(size) else {
            return 0xffff_ffff;
        };

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
            _ => 0xffff_ffff,
        }
    }

    pub fn io_write(&mut self, offset: u32, size: u32, value: u32) {
        let Ok(offset) = u16::try_from(offset) else {
            return;
        };
        let Ok(size) = u8::try_from(size) else {
            return;
        };

        match (offset, size) {
            // Use native 16-bit writes for the 16-bit registers.
            (REG_USBCMD | REG_USBSTS | REG_USBINTR | REG_FRNUM | REG_PORTSC1 | REG_PORTSC2, 2) => {
                self.controller.port_write(offset, 2, value, &mut self.irq);
            }
            // FRBASEADD is natively 32-bit.
            (REG_FRBASEADD, 4) => {
                self.controller
                    .port_write(REG_FRBASEADD, 4, value, &mut self.irq);
            }
            // Some drivers use 32-bit I/O at offset 0/4 to access paired 16-bit registers.
            (REG_USBCMD, 4) => {
                let cmd = value & 0xffff;
                let sts = (value >> 16) & 0xffff;
                self.controller
                    .port_write(REG_USBCMD, 2, cmd, &mut self.irq);
                self.controller
                    .port_write(REG_USBSTS, 2, sts, &mut self.irq);
            }
            (REG_USBINTR, 4) => {
                let intr = value & 0xffff;
                let frnum = (value >> 16) & 0xffff;
                self.controller
                    .port_write(REG_USBINTR, 2, intr, &mut self.irq);
                self.controller
                    .port_write(REG_FRNUM, 2, frnum, &mut self.irq);
            }
            (REG_PORTSC1, 4) => {
                let p0 = value & 0xffff;
                let p1 = (value >> 16) & 0xffff;
                self.controller
                    .port_write(REG_PORTSC1, 2, p0, &mut self.irq);
                self.controller
                    .port_write(REG_PORTSC2, 2, p1, &mut self.irq);
            }
            _ => match size {
                1 => self.write_u8(offset, value as u8),
                2 => {
                    self.write_u8(offset, value as u8);
                    self.write_u8(offset.wrapping_add(1), (value >> 8) as u8);
                }
                4 => {
                    self.write_u8(offset, value as u8);
                    self.write_u8(offset.wrapping_add(1), (value >> 8) as u8);
                    self.write_u8(offset.wrapping_add(2), (value >> 16) as u8);
                    self.write_u8(offset.wrapping_add(3), (value >> 24) as u8);
                }
                _ => {}
            },
        }
    }

    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }

        let mut mem = WasmGuestMemory::new(self.guest_base);
        for _ in 0..frames {
            self.controller.step_frame(&mut mem, &mut self.irq);
        }
    }

    pub fn irq_level(&self) -> bool {
        self.irq.asserted
    }

    pub fn set_connected(&mut self, connected: bool) {
        let was_connected = self
            .controller
            .bus()
            .port(ROOT_PORT_WEBUSB)
            .is_some_and(|p| p.connected);

        match (was_connected, connected) {
            (true, true) | (false, false) => {}
            (false, true) => {
                self.controller
                    .connect_device(ROOT_PORT_WEBUSB, Box::new(UsbWebUsbPassthroughDevice::new()));
            }
            (true, false) => {
                self.controller.disconnect_device(ROOT_PORT_WEBUSB);
            }
        }
    }

    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let Some(dev) = self.passthrough_device_mut() else {
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

        if let Some(dev) = self.passthrough_device_mut() {
            dev.push_completion(completion);
        }

        Ok(())
    }

    pub fn reset(&mut self) {
        // Clear any asserted IRQ before resetting the controller registers.
        self.irq.asserted = false;

        let port = self.controller.io_base().wrapping_add(REG_USBCMD);
        self.controller
            .port_write(port, 2, USBCMD_HCRESET, &mut self.irq);

        if let Some(dev) = self.passthrough_device_mut() {
            dev.reset();
        }
    }

    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        let Some(summary) = self.passthrough_device().map(|d| d.pending_summary()) else {
            return Ok(JsValue::NULL);
        };
        serde_wasm_bindgen::to_value(&summary).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Detach any USB device attached at the given topology path.
    ///
    /// Path numbering follows `aero_usb::usb::UsbBus`:
    /// - `path[0]` is the root port index (0-based).
    /// - `path[1..]` are hub ports (1-based).
    pub fn detach_at_path(&mut self, path: JsValue) -> Result<(), JsValue> {
        let path = crate::uhci_controller_bridge::parse_usb_path(path)?;
        if path.len() == 1 && path[0] == ROOT_PORT_EXTERNAL_HUB {
            return Err(js_sys::Error::new("Cannot detach the external USB hub from root port 0").into());
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
}

impl WebUsbUhciBridge {
    fn read_u8(&mut self, offset: u16) -> u8 {
        match offset {
            0x00 | 0x01 => {
                let w = self.controller.port_read(REG_USBCMD, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x02 | 0x03 => {
                let w = self.controller.port_read(REG_USBSTS, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x04 | 0x05 => {
                let w = self.controller.port_read(REG_USBINTR, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x06 | 0x07 => {
                let w = self.controller.port_read(REG_FRNUM, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x08..=0x0b => {
                let d = self.controller.port_read(REG_FRBASEADD, 4);
                let shift = (offset - REG_FRBASEADD) * 8;
                ((d >> shift) & 0xff) as u8
            }
            0x0c => self.controller.port_read(REG_SOFMOD, 1) as u8,
            0x10 | 0x11 => {
                let w = self.controller.port_read(REG_PORTSC1, 2) as u16;
                if offset & 1 == 0 {
                    (w & 0xff) as u8
                } else {
                    (w >> 8) as u8
                }
            }
            0x12 | 0x13 => {
                let w = self.controller.port_read(REG_PORTSC2, 2) as u16;
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
        let cur = self.controller.port_read(reg, 2) as u16;
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

        self.controller
            .port_write(reg, 2, next as u32, &mut self.irq);
    }

    fn write_u8(&mut self, offset: u16, value: u8) {
        match offset {
            // USBCMD: read/modify/write 16-bit register.
            0x00 | 0x01 => {
                let cur = self.controller.port_read(REG_USBCMD, 2) as u16;
                let shift = (offset & 1) * 8;
                let mask = 0xffu16 << shift;
                let next = (cur & !mask) | ((value as u16) << shift);
                self.controller
                    .port_write(REG_USBCMD, 2, next as u32, &mut self.irq);
            }

            // USBSTS: W1C (write-one-to-clear). Byte writes should only clear bits in that byte.
            0x02 | 0x03 => {
                let shift = (offset & 1) * 8;
                let v = (value as u16) << shift;
                self.controller
                    .port_write(REG_USBSTS, 2, v as u32, &mut self.irq);
            }

            // USBINTR: read/modify/write so high-byte writes don't clear the low-byte enables.
            0x04 | 0x05 => {
                let cur = self.controller.port_read(REG_USBINTR, 2) as u16;
                let shift = (offset & 1) * 8;
                let mask = 0xffu16 << shift;
                let next = (cur & !mask) | ((value as u16) << shift);
                self.controller
                    .port_write(REG_USBINTR, 2, next as u32, &mut self.irq);
            }

            // FRNUM: 11-bit register; read/modify/write for byte accesses.
            0x06 | 0x07 => {
                let cur = self.controller.port_read(REG_FRNUM, 2) as u16;
                let shift = (offset & 1) * 8;
                let mask = 0xffu16 << shift;
                let next = (cur & !mask) | ((value as u16) << shift);
                self.controller
                    .port_write(REG_FRNUM, 2, next as u32, &mut self.irq);
            }

            // FRBASEADD: 32-bit.
            0x08..=0x0b => {
                let cur = self.controller.port_read(REG_FRBASEADD, 4);
                let shift = (offset - REG_FRBASEADD) * 8;
                let mask = 0xffu32 << shift;
                let next = (cur & !mask) | ((value as u32) << shift);
                self.controller
                    .port_write(REG_FRBASEADD, 4, next, &mut self.irq);
            }

            // SOFMOD: 8-bit register.
            0x0c => {
                self.controller
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

    fn passthrough_device(&self) -> Option<&UsbWebUsbPassthroughDevice> {
        let port = self.controller.bus().port(ROOT_PORT_WEBUSB)?;
        let dev = port.device.as_ref()?;
        dev.as_any().downcast_ref::<UsbWebUsbPassthroughDevice>()
    }

    fn passthrough_device_mut(&mut self) -> Option<&mut UsbWebUsbPassthroughDevice> {
        let port = self.controller.bus_mut().port_mut(ROOT_PORT_WEBUSB)?;
        let dev = port.device.as_mut()?;
        dev.as_any_mut()
            .downcast_mut::<UsbWebUsbPassthroughDevice>()
    }
}

fn validate_webhid_attach_path(path: &[usize]) -> Result<(), JsValue> {
    if path.len() < 2 {
        return Err(js_sys::Error::new("WebHID devices must attach behind the external hub (expected path like [0, <hubPort>])").into());
    }
    if path[0] != ROOT_PORT_EXTERNAL_HUB {
        return Err(js_sys::Error::new("WebHID devices must attach behind the external hub on root port 0").into());
    }
    // Root port 0 is reserved for the hub itself; root port 1 is reserved for WebUSB.
    Ok(())
}
