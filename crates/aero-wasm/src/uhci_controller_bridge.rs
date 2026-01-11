//! WASM-side bridge for exposing a guest-visible UHCI controller.
//!
//! The browser I/O worker exposes this as a PCI device with an IO BAR; port IO
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
use aero_usb::uhci::{InterruptController, UhciController};

const UHCI_IO_BASE: u16 = 0;
const UHCI_IRQ_LINE: u8 = 0x0b;

fn wasm_memory_byte_len() -> u64 {
    // `memory_size(0)` returns the number of 64KiB wasm pages.
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

struct WasmGuestMemory {
    guest_base: u32,
    mem_bytes: u64,
}

impl WasmGuestMemory {
    fn new(guest_base: u32) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(JsValue::from_str("guest_base must be non-zero"));
        }
        let mem_bytes = wasm_memory_byte_len();
        if u64::from(guest_base) >= mem_bytes {
            return Err(JsValue::from_str(&format!(
                "guest_base out of bounds: guest_base=0x{guest_base:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }
        Ok(Self {
            guest_base,
            mem_bytes,
        })
    }

    #[inline]
    fn ptr_range(&self, addr: u32, len: usize) -> Option<(*const u8, *mut u8)> {
        let start = u64::from(self.guest_base).saturating_add(u64::from(addr));
        let end = start.saturating_add(len as u64);
        if end > self.mem_bytes {
            return None;
        }
        Some((start as *const u8, start as *mut u8))
    }
}

impl GuestMemory for WasmGuestMemory {
    fn read(&self, addr: u32, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }
        let Some((src, _)) = self.ptr_range(addr, buf.len()) else {
            buf.fill(0);
            return;
        };
        // Safety: `ptr_range()` bounds-checks against the current wasm memory size.
        unsafe {
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len());
        }
    }

    fn write(&mut self, addr: u32, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        let Some((_, dst)) = self.ptr_range(addr, buf.len()) else {
            return;
        };
        // Safety: `ptr_range()` bounds-checks against the current wasm memory size.
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dst, buf.len());
        }
    }
}

#[derive(Default)]
struct IrqLatch {
    asserted: bool,
}

impl InterruptController for IrqLatch {
    fn raise_irq(&mut self, _irq: u8) {
        self.asserted = true;
    }

    fn lower_irq(&mut self, _irq: u8) {
        self.asserted = false;
    }
}

/// Guest-visible UHCI controller bridge.
///
/// JS owns a corresponding PCI function that forwards IO BAR reads/writes into
/// `io_read/io_write` and drives 1ms frames via `tick_1ms`.
#[wasm_bindgen]
pub struct UhciControllerBridge {
    ctrl: UhciController,
    mem: WasmGuestMemory,
    irq: IrqLatch,
}

#[wasm_bindgen]
impl UhciControllerBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32) -> Result<Self, JsValue> {
        let mem = WasmGuestMemory::new(guest_base)?;
        Ok(Self {
            ctrl: UhciController::new(UHCI_IO_BASE, UHCI_IRQ_LINE),
            mem,
            irq: IrqLatch::default(),
        })
    }

    /// Read from the UHCI IO register block.
    pub fn io_read(&mut self, offset: u32, size: u32) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return u32::MAX;
        }
        let port = match u16::try_from(offset) {
            Ok(p) => p,
            Err(_) => return u32::MAX,
        };
        self.ctrl.port_read(port, size as usize)
    }

    /// Write to the UHCI IO register block.
    pub fn io_write(&mut self, offset: u32, size: u32, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let port = match u16::try_from(offset) {
            Ok(p) => p,
            Err(_) => return,
        };
        self.ctrl
            .port_write(port, size as usize, value, &mut self.irq);
    }

    /// Advance the controller by one UHCI frame (1ms).
    pub fn tick_1ms(&mut self) {
        self.ctrl.step_frame(&mut self.mem, &mut self.irq);
    }

    /// Returns the current IRQ line level (after UHCI interrupt gating).
    pub fn irq_asserted(&self) -> bool {
        self.irq.asserted
    }
}
