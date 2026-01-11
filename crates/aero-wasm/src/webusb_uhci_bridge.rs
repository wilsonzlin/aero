use wasm_bindgen::prelude::*;

use aero_usb::passthrough::{UsbHostAction, UsbHostCompletion};
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::{GuestMemory as UsbGuestMemory, UsbWebUsbPassthroughDevice};

const REG_USBCMD: u16 = 0x00;
const USBCMD_HCRESET: u32 = 1 << 1;

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
        Self {
            guest_base,
            controller: UhciController::new(0, 11),
            irq: WasmIrqCapture::default(),
        }
    }

    pub fn io_read(&mut self, offset: u32, size: u32) -> u32 {
        let Ok(offset) = u16::try_from(offset) else {
            return 0xffff_ffff;
        };
        let Ok(size) = usize::try_from(size) else {
            return 0xffff_ffff;
        };
        let port = self.controller.io_base().wrapping_add(offset);
        self.controller.port_read(port, size)
    }

    pub fn io_write(&mut self, offset: u32, size: u32, value: u32) {
        let Ok(offset) = u16::try_from(offset) else {
            return;
        };
        let Ok(size) = usize::try_from(size) else {
            return;
        };
        let port = self.controller.io_base().wrapping_add(offset);
        self.controller.port_write(port, size, value, &mut self.irq);
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
        let was_connected = self.controller.bus().port(0).is_some_and(|p| p.connected);

        match (was_connected, connected) {
            (true, true) | (false, false) => {}
            (false, true) => {
                self.controller
                    .connect_device(0, Box::new(UsbWebUsbPassthroughDevice::new()));
            }
            (true, false) => {
                self.controller.disconnect_device(0);
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
}

impl WebUsbUhciBridge {
    fn passthrough_device(&self) -> Option<&UsbWebUsbPassthroughDevice> {
        let port = self.controller.bus().port(0)?;
        let dev = port.device.as_ref()?;
        dev.as_any().downcast_ref::<UsbWebUsbPassthroughDevice>()
    }

    fn passthrough_device_mut(&mut self) -> Option<&mut UsbWebUsbPassthroughDevice> {
        let port = self.controller.bus_mut().port_mut(0)?;
        let dev = port.device.as_mut()?;
        dev.as_any_mut()
            .downcast_mut::<UsbWebUsbPassthroughDevice>()
    }
}
