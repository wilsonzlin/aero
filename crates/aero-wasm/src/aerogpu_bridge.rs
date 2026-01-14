//! WASM-side bridge for exposing the AeroGPU PCI/MMIO device model.
//!
//! This is intended for browser integrations that:
//! - run the PCI device model in the CPU worker (inside the `aero-wasm` module), and
//! - forward drained submissions to a GPU worker for execution.

use wasm_bindgen::prelude::*;

use js_sys::{Array, BigInt, Object, Reflect, Uint8Array};

use aero_devices::pci::PciDevice as _;
use aero_devices_gpu::{
    AeroGpuDeviceConfig, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode, AeroGpuPciDevice,
};
use memory::{MemoryBus, MmioHandler};

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

fn validate_mmio_size(size: u32) -> usize {
    match size {
        1 | 2 | 4 => size as usize,
        _ => 0,
    }
}

/// Guest physical memory backed by the module's linear memory.
///
/// Guest physical address 0 maps to `guest_base` in linear memory and spans `ram_bytes` bytes.
#[derive(Clone, Copy)]
struct LinearGuestMemory {
    guest_base: u32,
    ram_bytes: u64,
}

impl LinearGuestMemory {
    #[inline]
    fn linear_ptr(&self, ram_offset: u64, len: usize) -> Option<*const u8> {
        let end = ram_offset.checked_add(len as u64)?;
        if end > self.ram_bytes {
            return None;
        }
        let linear = (self.guest_base as u64).checked_add(ram_offset)?;
        let linear_u32 = u32::try_from(linear).ok()?;
        Some(core::ptr::with_exposed_provenance(linear_u32 as usize))
    }

    #[inline]
    fn linear_ptr_mut(&self, ram_offset: u64, len: usize) -> Option<*mut u8> {
        Some(self.linear_ptr(ram_offset, len)? as *mut u8)
    }
}

impl MemoryBus for LinearGuestMemory {
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

                    // Shared-memory (threaded wasm) builds: use atomic byte reads to avoid Rust
                    // data-race UB when the guest RAM lives in a shared `WebAssembly.Memory`.
                    #[cfg(feature = "wasm-threaded")]
                    {
                        use core::sync::atomic::{AtomicU8, Ordering};
                        let src = ptr as *const AtomicU8;
                        for (i, slot) in buf[off..off + len].iter_mut().enumerate() {
                            // Safety: `translate_guest_paddr_chunk` bounds-checks against
                            // `ram_bytes` and `AtomicU8` has alignment 1.
                            *slot = unsafe { (&*src.add(i)).load(Ordering::Relaxed) };
                        }
                    }

                    // Non-threaded wasm builds: linear memory is not shared across threads, so memcpy
                    // is fine.
                    #[cfg(not(feature = "wasm-threaded"))]
                    unsafe {
                        // Safety: `translate_guest_paddr_chunk` bounds-checks against `ram_bytes`
                        // and the guest region is bounds-checked against the wasm linear memory
                        // size in `AerogpuBridge::new`.
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

                    // Shared-memory (threaded wasm) builds: use atomic byte stores to avoid Rust
                    // data-race UB when the guest RAM lives in a shared `WebAssembly.Memory`.
                    #[cfg(feature = "wasm-threaded")]
                    {
                        use core::sync::atomic::{AtomicU8, Ordering};
                        let dst = ptr as *const AtomicU8;
                        for (i, byte) in buf[off..off + len].iter().copied().enumerate() {
                            // Safety: `translate_guest_paddr_chunk` bounds-checks against
                            // `ram_bytes` and `AtomicU8` has alignment 1.
                            unsafe { (&*dst.add(i)).store(byte, Ordering::Relaxed) };
                        }
                    }

                    // Non-threaded wasm builds: linear memory is not shared across threads, so memcpy
                    // is fine.
                    #[cfg(not(feature = "wasm-threaded"))]
                    unsafe {
                        // Safety: `translate_guest_paddr_chunk` bounds-checks against `ram_bytes`
                        // and the guest region is bounds-checked against the wasm linear memory
                        // size in `AerogpuBridge::new`.
                        core::ptr::copy_nonoverlapping(buf[off..].as_ptr(), ptr, len);
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::Hole { len } => len,
                crate::guest_phys::GuestRamChunk::OutOfBounds { len } => len,
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

fn bigint_to_u64(v: &BigInt) -> Result<u64, JsValue> {
    let s = v
        .to_string(10)
        .map_err(|_| js_error("BigInt::to_string failed"))?
        .as_string()
        .ok_or_else(|| js_error("BigInt::to_string returned non-string"))?;
    s.parse::<u64>()
        .map_err(|_| js_error("fence BigInt is not a valid u64"))
}

#[wasm_bindgen]
pub struct AerogpuBridge {
    dev: AeroGpuPciDevice,
    mem: LinearGuestMemory,
    now_ns: u64,
}

#[wasm_bindgen]
impl AerogpuBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32, vblank_hz: Option<u32>) -> Result<Self, JsValue> {
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

        let dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig {
            vblank_hz,
            executor: AeroGpuExecutorConfig {
                fence_completion: AeroGpuFenceCompletionMode::Deferred,
                ..Default::default()
            },
        });

        Ok(Self {
            dev,
            mem: LinearGuestMemory {
                guest_base,
                ram_bytes: guest_size_u64,
            },
            now_ns: 0,
        })
    }

    /// Mirror the guest-written PCI command register (offset 0x04, low 16 bits) into the device model.
    ///
    /// The AeroGPU model gates all DMA (ring reads, fence page writes) on COMMAND.BME (bit 2).
    pub fn set_pci_command(&mut self, command: u32) {
        self.dev.config_mut().set_command(command as u16);
    }

    pub fn mmio_read(&mut self, offset: u32, size: u32) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0;
        }

        let end = offset.saturating_add(size as u32);
        if end as u64 > aero_devices_gpu::regs::AEROGPU_PCI_BAR0_SIZE_BYTES {
            return 0xFFFF_FFFF;
        }

        self.dev.read(offset as u64, size) as u32
    }

    pub fn mmio_write(&mut self, offset: u32, size: u32, value: u32) {
        let size = validate_mmio_size(size);
        if size == 0 {
            return;
        }

        let end = offset.saturating_add(size as u32);
        if end as u64 > aero_devices_gpu::regs::AEROGPU_PCI_BAR0_SIZE_BYTES {
            return;
        }

        self.dev.write(offset as u64, size, value as u64);

        // Doorbells and other state transitions may schedule deferred work (ring parsing, fence
        // page writes). Drive that work immediately with a 0ns tick so the guest observes progress
        // without requiring an explicit `tick()` call on every MMIO write.
        self.dev.tick(&mut self.mem, self.now_ns);
    }

    /// Advance the device model by `delta_ns` (vblank + deferred fence processing).
    pub fn tick(&mut self, delta_ns: u64) {
        self.now_ns = self.now_ns.saturating_add(delta_ns);
        self.dev.tick(&mut self.mem, self.now_ns);
    }

    pub fn irq_level(&self) -> bool {
        self.dev.irq_level()
    }

    /// Drain any queued submissions since the last drain.
    ///
    /// Returns an array of objects:
    /// `{ context_id: number, engine_id: number, signal_fence: BigInt, cmd_stream: Uint8Array, alloc_table: Uint8Array | null }`.
    pub fn aerogpu_drain_submissions(&mut self) -> JsValue {
        let submissions = self.dev.drain_pending_submissions();
        let arr = Array::new();

        for sub in submissions {
            let obj = Object::new();
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("context_id"),
                &JsValue::from(sub.context_id),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("engine_id"),
                &JsValue::from(sub.engine_id),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("signal_fence"),
                &BigInt::from(sub.signal_fence).into(),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("cmd_stream"),
                &Uint8Array::from(sub.cmd_stream.as_slice()).into(),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("alloc_table"),
                &sub.alloc_table
                    .as_ref()
                    .map(|bytes| Uint8Array::from(bytes.as_slice()).into())
                    .unwrap_or(JsValue::NULL),
            );
            let _ = Reflect::set(&obj, &JsValue::from_str("flags"), &JsValue::from(sub.flags));

            arr.push(&obj);
        }

        arr.into()
    }

    pub fn aerogpu_complete_fence(&mut self, fence: BigInt) -> Result<(), JsValue> {
        let fence = bigint_to_u64(&fence)?;
        self.dev.complete_fence(&mut self.mem, fence);
        Ok(())
    }

    /// Debug: read the guest-programmed scanout framebuffer and return it as RGBA8.
    ///
    /// Returns `null` if the scanout is disabled or misconfigured.
    pub fn aerogpu_read_presented_scanout_rgba8(&mut self, scanout_id: u32) -> JsValue {
        if let Some((width, height, rgba8)) = self.dev.read_presented_scanout_rgba8(scanout_id) {
            let obj = Object::new();
            let _ = Reflect::set(&obj, &JsValue::from_str("width"), &JsValue::from(width));
            let _ = Reflect::set(&obj, &JsValue::from_str("height"), &JsValue::from(height));
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("rgba8"),
                &Uint8Array::from(rgba8.as_slice()).into(),
            );
            return obj.into();
        }

        if scanout_id != 0 {
            return JsValue::NULL;
        }

        let Some(rgba8) = self.dev.regs.scanout0.read_rgba(&mut self.mem) else {
            return JsValue::NULL;
        };

        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("width"),
            &JsValue::from(self.dev.regs.scanout0.width),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("height"),
            &JsValue::from(self.dev.regs.scanout0.height),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rgba8"),
            &Uint8Array::from(rgba8.as_slice()).into(),
        );
        obj.into()
    }
}
