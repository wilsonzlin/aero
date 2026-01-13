//! WASM-side bridge for exposing a guest-visible xHCI controller.
//!
//! At the time of writing the browser runtime primarily relies on UHCI for USB
//! passthrough. This bridge exists to reserve a stable wasm-bindgen surface for
//! future xHCI work and for wiring through the existing WASM loader/typechecking
//! infrastructure in `web/src/runtime/wasm_loader.ts`.
//!
//! The JS/TS side treats this export as optional because older deployed WASM
//! builds will not include it. When present, the bridge provides a minimal MMIO
//! register file, a `tick()` entry point, IRQ query, and deterministic
//! snapshot/restore helpers.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_io_snapshot::io::state::{SnapshotReader, SnapshotVersion, SnapshotWriter};

const XHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"XHCB";
const XHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

// Keep the stub MMIO region bounded. Real xHCI controllers expose a larger BAR
// but the exact size is not relevant for the JS integration layer.
const XHCI_MMIO_BYTES: usize = 0x4000;

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

#[wasm_bindgen]
pub struct XhciControllerBridge {
    #[allow(dead_code)]
    guest_base: u32,
    #[allow(dead_code)]
    guest_size: u64,
    regs: Vec<u8>,
    tick_count: u64,
}

#[wasm_bindgen]
impl XhciControllerBridge {
    /// Create a new xHCI controller bridge bound to the provided guest RAM mapping.
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
            guest_base,
            guest_size: guest_size_u64,
            regs: vec![0u8; XHCI_MMIO_BYTES],
            tick_count: 0,
        })
    }

    /// Read from the xHCI MMIO register space.
    ///
    /// This is intentionally minimal: the bridge currently models the MMIO area as a byte-addressed
    /// register file so the JS runtime can be wired up without depending on a full xHCI device model.
    pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0;
        }

        let offset = offset as usize;
        let Some(end) = offset.checked_add(size) else {
            return 0;
        };
        if end > self.regs.len() {
            return 0;
        }

        match size {
            1 => u32::from(self.regs[offset]),
            2 => {
                let bytes = [self.regs[offset], self.regs[offset + 1]];
                u32::from(u16::from_le_bytes(bytes))
            }
            4 => {
                let bytes = [
                    self.regs[offset],
                    self.regs[offset + 1],
                    self.regs[offset + 2],
                    self.regs[offset + 3],
                ];
                u32::from_le_bytes(bytes)
            }
            _ => 0,
        }
    }

    /// Write to the xHCI MMIO register space.
    pub fn mmio_write(&mut self, offset: u32, size: u8, value: u32) {
        let size = validate_mmio_size(size);
        if size == 0 {
            return;
        }

        let offset = offset as usize;
        let Some(end) = offset.checked_add(size) else {
            return;
        };
        if end > self.regs.len() {
            return;
        }

        let bytes = value.to_le_bytes();
        self.regs[offset..end].copy_from_slice(&bytes[..size]);
    }

    /// Advance the controller by one host "tick".
    ///
    /// The unit of time is defined by the JS runtime driving this bridge. For now the bridge only
    /// tracks a monotonically increasing tick counter.
    pub fn tick(&mut self) {
        self.tick_count = self.tick_count.wrapping_add(1);
    }

    /// Whether the controller interrupt line should be raised.
    pub fn irq_asserted(&self) -> bool {
        false
    }

    /// Serialize the current bridge state into a deterministic snapshot blob.
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_REGS: u16 = 1;
        const TAG_TICK_COUNT: u16 = 2;

        let mut w = SnapshotWriter::new(XHCI_BRIDGE_DEVICE_ID, XHCI_BRIDGE_DEVICE_VERSION);
        w.field_bytes(TAG_REGS, self.regs.clone());
        w.field_u64(TAG_TICK_COUNT, self.tick_count);
        w.finish()
    }

    /// Restore bridge state from a snapshot blob produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        const TAG_REGS: u16 = 1;
        const TAG_TICK_COUNT: u16 = 2;

        let r = SnapshotReader::parse(bytes, XHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(XHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?;

        let regs = r
            .bytes(TAG_REGS)
            .ok_or_else(|| js_error("xHCI bridge snapshot missing register state"))?;
        if regs.len() != self.regs.len() {
            return Err(js_error(format!(
                "xHCI bridge snapshot register state size mismatch (expected {}, found {})",
                self.regs.len(),
                regs.len()
            )));
        }
        self.regs.copy_from_slice(regs);

        self.tick_count = r
            .u64(TAG_TICK_COUNT)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?
            .unwrap_or(0);

        Ok(())
    }

    /// Snapshot the bridge state as deterministic bytes.
    pub fn snapshot_state(&self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore the bridge state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }
}

