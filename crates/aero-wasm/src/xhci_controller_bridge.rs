//! WASM-side bridge for exposing a guest-visible xHCI controller.
//!
//! The browser I/O worker exposes this as a PCI function with an MMIO BAR; reads/writes are
//! forwarded into this bridge which updates the canonical Rust xHCI model
//! (`aero_usb::xhci::XhciController`).
//!
//! The JS/TS side treats this export as optional because older deployed WASM builds will not
//! include it. When present, the bridge provides:
//! - MMIO register access (`mmio_read`/`mmio_write`)
//! - a coarse stepping hook (`step_frames` / `tick`) for future expansion
//! - IRQ level query (`irq_asserted`)
//! - deterministic snapshot/restore helpers.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::Uint8Array;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::xhci::XhciController;
use aero_usb::MemoryBus;

const XHCI_BRIDGE_DEVICE_ID: [u8; 4] = *b"XHCB";
const XHCI_BRIDGE_DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

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
    /// The current xHCI model in `aero-usb` does not yet have a time-based scheduler; this exists
    /// primarily so the JS-side PCI wrapper can share a common device-stepping contract across
    /// controllers.
    pub fn step_frames(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }
        self.tick_count = self.tick_count.wrapping_add(u64::from(frames));
        // Advance controller/port timers. Without this, operations like PORTSC port reset will never
        // complete (the xHCI model clears PR/PED after a timeout in `tick_1ms`).
        for _ in 0..frames {
            self.ctrl.tick_1ms();
        }
    }

    /// Convenience wrapper for stepping a single frame.
    pub fn step_frame(&mut self) {
        self.step_frames(1);
    }

    /// Alias for {@link step_frames} retained for older call sites.
    pub fn tick(&mut self, frames: u32) {
        self.step_frames(frames);
    }

    /// Optional polling hook for JS wrappers that expect a `poll()` method.
    pub fn poll(&mut self) {
        // Drain any queued event TRBs into the guest event ring. This performs DMA into guest memory
        // and must therefore be gated on PCI Bus Master Enable (BME).
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

    /// Serialize the current xHCI controller state into a deterministic snapshot blob.
    pub fn save_state(&self) -> Vec<u8> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_TICK_COUNT: u16 = 2;

        let mut w = SnapshotWriter::new(XHCI_BRIDGE_DEVICE_ID, XHCI_BRIDGE_DEVICE_VERSION);
        w.field_bytes(TAG_CONTROLLER, self.ctrl.save_state());
        w.field_u64(TAG_TICK_COUNT, self.tick_count);
        w.finish()
    }

    /// Restore xHCI controller state from a snapshot blob produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        const TAG_CONTROLLER: u16 = 1;
        const TAG_TICK_COUNT: u16 = 2;

        let r = SnapshotReader::parse(bytes, XHCI_BRIDGE_DEVICE_ID)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?;
        r.ensure_device_major(XHCI_BRIDGE_DEVICE_VERSION.major)
            .map_err(|e| js_error(format!("Invalid xHCI bridge snapshot: {e}")))?;

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
}
