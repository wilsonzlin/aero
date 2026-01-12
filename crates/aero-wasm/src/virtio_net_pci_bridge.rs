//! WASM-side bridge for exposing a guest-visible virtio-net device via virtio-pci.
//!
//! The TypeScript I/O worker is responsible for wiring this into the emulated PCI bus and for
//! forwarding BAR0 MMIO reads/writes into this bridge. The virtio queues and packet buffers live
//! in guest RAM inside the WASM linear memory; guest physical address 0 maps to `guest_base`
//! (see `guest_ram_layout`).
//!
//! This bridge can optionally enable the virtio-pci legacy I/O port register block (BAR2), either:
//! - as a *transitional* device (legacy + modern), or
//! - as a legacy-only device (legacy BAR2 with modern capabilities disabled).
//!
//! When legacy I/O is enabled, the JS PCI wrapper exposes an additional I/O BAR (BAR2) and forwards
//! port reads/writes into [`VirtioNetPciBridge::legacy_io_read`] / [`VirtioNetPciBridge::legacy_io_write`].
//! Older JS call sites may use the retained aliases [`VirtioNetPciBridge::io_read`] /
//! [`VirtioNetPciBridge::io_write`].
//!
//! Host networking is bridged through the existing Aero IPC (AIPC) rings:
//! - `NET_TX`: guest -> host (packets transmitted by the virtio-net device)
//! - `NET_RX`: host -> guest (packets received by the virtio-net device)
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::SharedArrayBuffer;

use std::cell::Cell;
use std::rc::Rc;

use aero_ipc::layout::io_ipc_queue_kind::{NET_RX, NET_TX};
use aero_ipc::wasm::{open_ring_by_kind, SharedRingBuffer};
use aero_virtio::devices::net::{NetBackend, VirtioNet};
use aero_virtio::memory::{GuestMemory, GuestMemoryError};
use aero_virtio::pci::{InterruptSink, VirtioPciDevice, VIRTIO_PCI_LEGACY_QUEUE_NOTIFY};

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

#[derive(Clone, Copy)]
struct WasmGuestMemory {
    guest_base: u32,
    guest_size: u64,
}

impl WasmGuestMemory {
    #[inline]
    fn validate_range(&self, addr: u64, len: usize) -> Result<u32, GuestMemoryError> {
        let end = addr
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfBounds { addr, len })?;
        if end > self.guest_size {
            return Err(GuestMemoryError::OutOfBounds { addr, len });
        }

        let linear = (self.guest_base as u64)
            .checked_add(addr)
            .ok_or(GuestMemoryError::OutOfBounds { addr, len })?;

        // `GuestMemory` addresses are u64; do not truncate when mapping to wasm32 pointers.
        let linear_u32 =
            u32::try_from(linear).map_err(|_| GuestMemoryError::OutOfBounds { addr, len })?;
        Ok(linear_u32)
    }
}

impl GuestMemory for WasmGuestMemory {
    fn len(&self) -> u64 {
        self.guest_size
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        dst.copy_from_slice(self.get_slice(addr, dst.len())?);
        Ok(())
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        self.get_slice_mut(addr, src.len())?.copy_from_slice(src);
        Ok(())
    }

    fn get_slice(&self, addr: u64, len: usize) -> Result<&[u8], GuestMemoryError> {
        if len == 0 {
            // Avoid edge cases where `guest_base + addr == 4GiB` (not representable as a u32
            // pointer) even though a zero-length slice is valid.
            if addr > self.guest_size {
                return Err(GuestMemoryError::OutOfBounds { addr, len });
            }
            return Ok(&[]);
        }

        let linear = self.validate_range(addr, len)?;
        // Safety: `validate_range` ensures the slice is fully within the guest RAM window.
        Ok(unsafe { core::slice::from_raw_parts(linear as *const u8, len) })
    }

    fn get_slice_mut(&mut self, addr: u64, len: usize) -> Result<&mut [u8], GuestMemoryError> {
        if len == 0 {
            if addr > self.guest_size {
                return Err(GuestMemoryError::OutOfBounds { addr, len });
            }
            // Safety: a 0-length slice may use a dangling pointer.
            return Ok(unsafe {
                core::slice::from_raw_parts_mut(core::ptr::NonNull::<u8>::dangling().as_ptr(), 0)
            });
        }

        let linear = self.validate_range(addr, len)?;
        // Safety: `validate_range` ensures the slice is fully within the guest RAM window.
        Ok(unsafe { core::slice::from_raw_parts_mut(linear as *mut u8, len) })
    }
}

struct AipcNetBackend {
    net_tx: SharedRingBuffer,
    net_rx: SharedRingBuffer,
}

impl NetBackend for AipcNetBackend {
    fn transmit(&mut self, packet: Vec<u8>) {
        // Best-effort: drop when full / oversized.
        let _ = self.net_tx.try_push(&packet);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        let record = self.net_rx.try_pop()?;
        let mut out = vec![0u8; record.length() as usize];
        record.copy_to(&mut out);
        Some(out)
    }
}

#[derive(Clone)]
struct LegacyIrqLatch {
    asserted: Rc<Cell<bool>>,
}

impl InterruptSink for LegacyIrqLatch {
    fn raise_legacy_irq(&mut self) {
        self.asserted.set(true);
    }

    fn lower_legacy_irq(&mut self) {
        self.asserted.set(false);
    }

    fn signal_msix(&mut self, _vector: u16) {
        // MSI-X is not currently surfaced through this bridge.
    }
}

#[wasm_bindgen]
pub struct VirtioNetPciBridge {
    mem: WasmGuestMemory,
    dev: VirtioPciDevice,
    irq_asserted: Rc<Cell<bool>>,
    legacy_io_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioNetPciTransportMode {
    ModernOnly,
    Transitional,
    LegacyOnly,
}

impl VirtioNetPciTransportMode {
    fn parse_js(value: Option<JsValue>) -> Result<Self, JsValue> {
        let Some(value) = value else {
            return Ok(Self::ModernOnly);
        };

        // Be permissive: accept both numeric and string modes so JS callers can use enums
        // without needing a specific exported type.
        if value.is_null() || value.is_undefined() {
            return Ok(Self::ModernOnly);
        }

        if let Some(b) = value.as_bool() {
            return Ok(if b {
                Self::Transitional
            } else {
                Self::ModernOnly
            });
        }

        if let Some(n) = value.as_f64() {
            let n = n as i32;
            return match n {
                0 => Ok(Self::ModernOnly),
                1 => Ok(Self::Transitional),
                2 => Ok(Self::LegacyOnly),
                _ => Err(js_error(format!("invalid virtio-net pci transport mode: {n}"))),
            };
        }

        if let Some(s) = value.as_string() {
            let s = s.trim().to_ascii_lowercase();
            return match s.as_str() {
                "" | "modern" | "modern-only" | "modern_only" => Ok(Self::ModernOnly),
                "transitional" => Ok(Self::Transitional),
                "legacy" | "legacy-only" | "legacy_only" => Ok(Self::LegacyOnly),
                _ => Err(js_error(format!(
                    "invalid virtio-net pci transport mode: {s}"
                ))),
            };
        }

        Err(js_error(
            "invalid virtio-net pci transport mode: expected string or number",
        ))
    }
}

#[wasm_bindgen]
impl VirtioNetPciBridge {
    /// Create a new virtio-net (virtio-pci, modern) bridge bound to the provided guest RAM mapping.
    ///
    /// - `guest_base` is the byte offset inside wasm linear memory where guest physical address 0
    ///   begins (see `guest_ram_layout`).
    /// - `guest_size` is the guest RAM size in bytes. Pass `0` to use "the remainder of linear
    ///   memory" as guest RAM (mirrors `UhciControllerBridge`).
    /// - `io_ipc_sab` is the browser runtime's `ioIpcSab` `SharedArrayBuffer` containing `NET_TX`
    ///   and `NET_RX` ring buffers.
    /// - `transport_mode` optionally selects the virtio-pci transport to expose:
    ///   - `"modern"` / `0` (default): modern-only (Aero Win7 virtio contract v1)
    ///   - `"transitional"` / `1`: modern + legacy I/O port BAR
    ///   - `"legacy"` / `2`: legacy I/O port BAR only (modern caps disabled)
    #[wasm_bindgen(constructor)]
    pub fn new(
        guest_base: u32,
        guest_size: u32,
        io_ipc_sab: SharedArrayBuffer,
        transport_mode: Option<JsValue>,
    ) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(js_error("guest_base must be non-zero"));
        }

        let mem_bytes = wasm_memory_byte_len();
        let guest_size_u64 = if guest_size == 0 {
            mem_bytes.saturating_sub(guest_base as u64)
        } else {
            guest_size as u64
        };
        // Keep guest RAM below the PCI MMIO aperture (see `guest_ram_layout` contract).
        let guest_size_u64 = guest_size_u64.min(crate::guest_layout::PCI_MMIO_BASE);

        let end = (guest_base as u64)
            .checked_add(guest_size_u64)
            .ok_or_else(|| js_error("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(js_error(format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        let net_tx = open_ring_by_kind(io_ipc_sab.clone(), NET_TX, 0)?;
        let net_rx = open_ring_by_kind(io_ipc_sab, NET_RX, 0)?;

        let backend = AipcNetBackend { net_tx, net_rx };

        // Deterministic locally-administered MAC.
        let net = VirtioNet::new(backend, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

        let asserted = Rc::new(Cell::new(false));
        let irq = LegacyIrqLatch {
            asserted: asserted.clone(),
        };

        let transport_mode = VirtioNetPciTransportMode::parse_js(transport_mode)?;

        let dev = match transport_mode {
            VirtioNetPciTransportMode::ModernOnly => {
                VirtioPciDevice::new(Box::new(net), Box::new(irq))
            }
            VirtioNetPciTransportMode::Transitional => {
                VirtioPciDevice::new_transitional(Box::new(net), Box::new(irq))
            }
            VirtioNetPciTransportMode::LegacyOnly => {
                VirtioPciDevice::new_legacy_only(Box::new(net), Box::new(irq))
            }
        };
        let legacy_io_size = dev.legacy_io_size().min(u64::from(u32::MAX)) as u32;

        Ok(Self {
            mem: WasmGuestMemory {
                guest_base,
                guest_size: guest_size_u64,
            },
            dev,
            irq_asserted: asserted,
            legacy_io_size,
        })
    }

    pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return 0xffff_ffff,
        };

        let mut buf = [0u8; 4];
        self.dev.bar0_read(offset as u64, &mut buf[..size]);
        u32::from_le_bytes(buf)
    }

    pub fn mmio_write(&mut self, offset: u32, size: u8, value: u32) {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };

        let bytes = value.to_le_bytes();
        self.dev.bar0_write(offset as u64, &bytes[..size]);
    }

    pub fn legacy_io_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return 0,
        };
        let end = offset.checked_add(size as u32).unwrap_or(u32::MAX);
        if self.legacy_io_size == 0 || end > self.legacy_io_size {
            return 0xffff_ffff;
        }
        let mut buf = [0u8; 4];
        self.dev.legacy_io_read(offset as u64, &mut buf[..size]);
        u32::from_le_bytes(buf)
    }

    pub fn legacy_io_write(&mut self, offset: u32, size: u8, value: u32) {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        let end = offset.checked_add(size as u32).unwrap_or(u32::MAX);
        if self.legacy_io_size == 0 || end > self.legacy_io_size {
            return;
        }
        let bytes = value.to_le_bytes();
        self.dev.legacy_io_write(offset as u64, &bytes[..size]);
        // Legacy queue notifications are expected to be "immediate" from the guest's
        // perspective (in real hardware, the kick causes the device to begin DMA). In the
        // browser runtime we have access to guest RAM in the WASM linear memory, so we can
        // service the notified virtqueue synchronously instead of requiring periodic polling.
        if offset as u64 == VIRTIO_PCI_LEGACY_QUEUE_NOTIFY {
            self.dev.process_notified_queues(&mut self.mem);
        }
    }

    /// Back-compat alias for `legacy_io_read` used by older JS runtimes.
    pub fn io_read(&mut self, offset: u32, size: u8) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return 0xffff_ffff;
        }
        self.legacy_io_read(offset, size)
    }

    /// Back-compat alias for `legacy_io_write` used by older JS runtimes.
    pub fn io_write(&mut self, offset: u32, size: u8, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        self.legacy_io_write(offset, size, value);
    }

    /// Process any pending queue work and host-driven events (e.g. `NET_RX` packets).
    pub fn poll(&mut self) {
        self.dev.poll(&mut self.mem);
    }

    /// Whether the PCI INTx line should be raised.
    pub fn irq_asserted(&self) -> bool {
        self.irq_asserted.get()
    }
}
