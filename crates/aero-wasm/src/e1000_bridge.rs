//! WASM-side bridge for exposing a guest-visible Intel E1000 NIC.
//!
//! The browser runtime wires this device to the raw Ethernet frame rings:
//! - Guest -> host: E1000 TX queue -> IO_IPC_NET_TX_QUEUE_KIND
//! - Host -> guest: IO_IPC_NET_RX_QUEUE_KIND -> E1000 RX queue
//!
//! The E1000's descriptor rings live in guest RAM; this bridge implements
//! [`memory::MemoryBus`] so the Rust device model can DMA into the shared wasm
//! linear memory guest region (see `guest_ram_layout`).
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_net_e1000::{E1000Device, E1000_IO_SIZE, E1000_MMIO_SIZE, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
use memory::MemoryBus;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

fn validate_io_size(size: u32) -> usize {
    match size {
        1 | 2 | 4 => size as usize,
        _ => 0,
    }
}

/// Guest physical memory backed by the module's linear memory.
///
/// Guest physical address 0 maps to `guest_base` in linear memory and spans
/// `guest_size` bytes.
#[derive(Clone, Copy)]
struct LinearGuestMemory {
    guest_base: u32,
    guest_size: u32,
}

impl LinearGuestMemory {
    fn translate(&self, paddr: u64, len: usize) -> Option<u32> {
        let paddr_u32 = u32::try_from(paddr).ok()?;
        if paddr_u32 >= self.guest_size {
            return None;
        }
        let end = paddr_u32.checked_add(len as u32)?;
        if end > self.guest_size {
            return None;
        }
        self.guest_base.checked_add(paddr_u32)
    }
}

impl MemoryBus for LinearGuestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        // Out-of-bounds reads return 0.
        let Some(linear) = self.translate(paddr, buf.len()) else {
            buf.fill(0);
            return;
        };

        // Safety: `translate` validates the access is within the configured guest
        // region, and the guest region is bounds-checked against the wasm linear
        // memory size in `E1000Bridge::new`.
        unsafe {
            core::ptr::copy_nonoverlapping(linear as *const u8, buf.as_mut_ptr(), buf.len());
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        // Out-of-bounds writes are ignored.
        let Some(linear) = self.translate(paddr, buf.len()) else {
            return;
        };

        // Safety: `translate` validates the access is within the configured guest
        // region, and the guest region is bounds-checked against the wasm linear
        // memory size in `E1000Bridge::new`.
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), linear as *mut u8, buf.len());
        }
    }
}

const DEFAULT_MAC_ADDR: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

#[wasm_bindgen]
pub struct E1000Bridge {
    dev: E1000Device,
    mem: LinearGuestMemory,
}

#[wasm_bindgen]
impl E1000Bridge {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32, mac: Option<Uint8Array>) -> Result<Self, JsValue> {
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

        let mac_addr = if let Some(mac) = mac {
            if mac.length() != 6 {
                return Err(js_error("E1000Bridge: mac must be a Uint8Array of length 6"));
            }
            let mut out = [0u8; 6];
            mac.copy_to(&mut out);
            out
        } else {
            DEFAULT_MAC_ADDR
        };

        let guest_size_u32 = u32::try_from(guest_size_u64)
            .map_err(|_| js_error("guest_size does not fit in u32"))?;

        // The E1000 model gates all DMA on the PCI command Bus Master Enable bit (COMMAND.BME).
        //
        // In the web runtime, PCI config space is emulated in TypeScript and is not currently
        // plumbed through to the Rust device model. Enable BME by default so `poll()` can perform
        // descriptor/RX buffer DMA once the guest has configured the rings.
        let mut dev = E1000Device::new(mac_addr);
        dev.pci_config_write(0x04, 2, 1 << 2);

        Ok(Self {
            dev,
            mem: LinearGuestMemory {
                guest_base,
                guest_size: guest_size_u32,
            },
        })
    }

    pub fn mmio_read(&mut self, offset: u32, size: u32) -> u32 {
        let size = validate_io_size(size);
        if size == 0 {
            return 0xFFFF_FFFF;
        }
        let end = offset.checked_add(size as u32).unwrap_or(u32::MAX);
        if end > E1000_MMIO_SIZE {
            return 0xFFFF_FFFF;
        }
        self.dev.mmio_read(offset as u64, size)
    }

    pub fn mmio_write(&mut self, offset: u32, size: u32, value: u32) {
        let size = validate_io_size(size);
        if size == 0 {
            return;
        }
        let end = offset.checked_add(size as u32).unwrap_or(u32::MAX);
        if end > E1000_MMIO_SIZE {
            return;
        }
        self.dev.mmio_write(offset as u64, size, value);
    }

    pub fn io_read(&mut self, offset: u32, size: u32) -> u32 {
        let size = validate_io_size(size);
        if size == 0 {
            return 0xFFFF_FFFF;
        }
        let end = offset.checked_add(size as u32).unwrap_or(u32::MAX);
        if end > E1000_IO_SIZE {
            return 0xFFFF_FFFF;
        }
        self.dev.io_read(offset, size)
    }

    pub fn io_write(&mut self, offset: u32, size: u32, value: u32) {
        let size = validate_io_size(size);
        if size == 0 {
            return;
        }
        let end = offset.checked_add(size as u32).unwrap_or(u32::MAX);
        if end > E1000_IO_SIZE {
            return;
        }
        self.dev.io_write(offset, size, value);
    }

    /// Update the device model's PCI command register (offset 0x04, low 16 bits).
    ///
    /// Some integrations (e.g. the JS `PciBus`) model PCI config space separately from the Rust
    /// E1000 device model, but still need the E1000 to observe COMMAND.BME (bit 2) so DMA is gated
    /// correctly. Call this whenever the guest updates the PCI command register.
    pub fn set_pci_command(&mut self, command: u32) {
        self.dev.pci_config_write(0x04, 2, command & 0xffff);
    }

    pub fn poll(&mut self) {
        self.dev.poll(&mut self.mem);
    }

    pub fn irq_level(&self) -> bool {
        self.dev.irq_level()
    }

    pub fn receive_frame(&mut self, frame: &Uint8Array) {
        // NOTE: Keep this signature as `Uint8Array` (not `&[u8]`).
        // wasm-bindgen eagerly copies `&[u8]` parameters into wasm linear memory
        // before calling Rust, which would allow untrusted callers to trigger a
        // large transient allocation even though the E1000 model drops oversized
        // frames. By accepting `Uint8Array` we can validate the length first and
        // only copy bounded frames.
        let len = frame.length() as usize;
        if len < MIN_L2_FRAME_LEN || len > MAX_L2_FRAME_LEN {
            return;
        }
        let mut buf = vec![0u8; len];
        frame.copy_to(&mut buf);
        self.dev.receive_frame(&mut self.mem, &buf);
    }

    pub fn pop_tx_frame(&mut self) -> Option<Uint8Array> {
        let frame = self.dev.pop_tx_frame()?;
        Some(Uint8Array::from(frame.as_slice()))
    }

    pub fn mac_addr(&self) -> Uint8Array {
        let mac = self.dev.mac_addr();
        Uint8Array::from(mac.as_ref())
    }
}
