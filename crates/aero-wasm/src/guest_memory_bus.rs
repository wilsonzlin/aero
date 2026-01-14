//! Shared guest-memory `MemoryBus` implementation for wasm-side device bridges.
//!
//! UHCI (and other DMA-capable devices) need to read and write guest physical memory. In the web
//! runtime, guest RAM is stored as a contiguous byte buffer inside the module's WebAssembly linear
//! memory, with guest physical address 0 mapping to `guest_base` within the linear memory.
//!
//! This module centralizes the translation + copy semantics to avoid drift across bridges.
//!
//! Semantics preserved from the historical per-bridge implementations:
//! - Reads from the PCI/ECAM hole return `0xFF` bytes (open bus).
//! - Reads out of bounds return `0x00` bytes.
//! - Writes to the hole or out of bounds are ignored.
//! - Overflow handling uses `checked_add` and matches historical behavior.

use aero_usb::MemoryBus;

/// Abstract access to the underlying linear-memory backing store.
///
/// In wasm32 builds this is implemented by directly copying from/to the module's linear memory via
/// raw pointers. In unit tests we can provide an in-memory implementation.
pub(crate) trait LinearMemory {
    fn read(&self, linear: u32, dst: &mut [u8]);
    fn write(&mut self, linear: u32, src: &[u8]);
}

/// Generic guest-RAM-backed memory bus.
///
/// `guest_base` is a byte offset inside the linear memory where guest physical address 0 begins.
/// `ram_bytes` is the total guest RAM size in bytes (after any clamping in the caller).
#[derive(Debug, Clone)]
pub(crate) struct GuestMemoryBusImpl<M> {
    mem: M,
    guest_base: u32,
    ram_bytes: u64,
}

impl<M: Copy> Copy for GuestMemoryBusImpl<M> {}

impl<M> GuestMemoryBusImpl<M> {
    pub(crate) fn new_with_memory(mem: M, guest_base: u32, ram_bytes: u64) -> Self {
        Self {
            mem,
            guest_base,
            ram_bytes,
        }
    }

    #[inline]
    fn linear_u32(&self, ram_offset: u64, len: usize) -> Option<u32> {
        let end = ram_offset.checked_add(len as u64)?;
        if end > self.ram_bytes {
            return None;
        }
        let linear = (self.guest_base as u64).checked_add(ram_offset)?;
        u32::try_from(linear).ok()
    }
}

impl<M: LinearMemory> MemoryBus for GuestMemoryBusImpl<M> {
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
                    let Some(linear) = self.linear_u32(ram_offset, len) else {
                        buf[off..].fill(0);
                        return;
                    };
                    self.mem.read(linear, &mut buf[off..off + len]);
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
                    let Some(linear) = self.linear_u32(ram_offset, len) else {
                        return;
                    };
                    self.mem.write(linear, &buf[off..off + len]);
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

/// Memory bus used when PCI bus mastering (DMA) is disabled.
///
/// Reads return open bus (`0xFF`) and writes are ignored.
pub(crate) struct NoDmaMemory;

impl MemoryBus for NoDmaMemory {
    fn dma_enabled(&self) -> bool {
        false
    }

    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        buf.fill(0xFF);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
}

// -------------------------------------------------------------------------------------------------
// wasm32 backing implementation
// -------------------------------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct WasmLinearMemory;

#[cfg(target_arch = "wasm32")]
impl LinearMemory for WasmLinearMemory {
    #[inline]
    fn read(&self, linear: u32, dst: &mut [u8]) {
        // Safety: Callers validate `linear` is within the module's linear memory and that the
        // requested range lies within the configured guest RAM size.
        unsafe {
            core::ptr::copy_nonoverlapping(linear as *const u8, dst.as_mut_ptr(), dst.len());
        }
    }

    #[inline]
    fn write(&mut self, linear: u32, src: &[u8]) {
        // Safety: Callers validate `linear` is within the module's linear memory and that the
        // requested range lies within the configured guest RAM size.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), linear as *mut u8, src.len());
        }
    }
}

/// Canonical guest-RAM memory bus backed by the module's wasm linear memory.
#[cfg(target_arch = "wasm32")]
pub(crate) type GuestMemoryBus = GuestMemoryBusImpl<WasmLinearMemory>;

#[cfg(target_arch = "wasm32")]
impl GuestMemoryBus {
    pub(crate) fn new(guest_base: u32, ram_bytes: u64) -> Self {
        Self::new_with_memory(WasmLinearMemory, guest_base, ram_bytes)
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

// -------------------------------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::guest_phys::{HIGH_RAM_BASE, PCIE_ECAM_BASE};

    #[derive(Clone, Debug)]
    struct VecLinearMemory {
        buf: Vec<u8>,
        read_calls: std::cell::Cell<usize>,
        write_calls: std::cell::Cell<usize>,
    }

    impl VecLinearMemory {
        fn new(len: usize) -> Self {
            Self {
                buf: vec![0u8; len],
                read_calls: std::cell::Cell::new(0),
                write_calls: std::cell::Cell::new(0),
            }
        }
    }

    impl LinearMemory for VecLinearMemory {
        fn read(&self, linear: u32, dst: &mut [u8]) {
            self.read_calls.set(self.read_calls.get() + 1);
            let start = linear as usize;
            let end = start + dst.len();
            dst.copy_from_slice(&self.buf[start..end]);
        }

        fn write(&mut self, linear: u32, src: &[u8]) {
            self.write_calls.set(self.write_calls.get() + 1);
            let start = linear as usize;
            let end = start + src.len();
            self.buf[start..end].copy_from_slice(src);
        }
    }

    #[derive(Clone, Debug)]
    struct SparseLinearMemory {
        bytes: std::collections::HashMap<u32, u8>,
        write_calls: usize,
    }

    impl SparseLinearMemory {
        fn new() -> Self {
            Self {
                bytes: std::collections::HashMap::new(),
                write_calls: 0,
            }
        }

        fn set_range(&mut self, linear: u32, data: &[u8]) {
            for (i, b) in data.iter().enumerate() {
                self.bytes.insert(linear + i as u32, *b);
            }
        }

        fn get_range(&self, linear: u32, len: usize) -> Vec<u8> {
            (0..len)
                .map(|i| *self.bytes.get(&(linear + i as u32)).unwrap_or(&0))
                .collect()
        }
    }

    impl LinearMemory for SparseLinearMemory {
        fn read(&self, linear: u32, dst: &mut [u8]) {
            for (i, b) in dst.iter_mut().enumerate() {
                *b = *self.bytes.get(&(linear + i as u32)).unwrap_or(&0);
            }
        }

        fn write(&mut self, linear: u32, src: &[u8]) {
            self.write_calls += 1;
            for (i, b) in src.iter().enumerate() {
                self.bytes.insert(linear + i as u32, *b);
            }
        }
    }

    #[test]
    fn empty_buffers_are_no_ops() {
        let mem = VecLinearMemory::new(0x100);
        let mut bus = GuestMemoryBusImpl::new_with_memory(mem, 0, 0x100);

        bus.read_physical(0, &mut []);
        bus.write_physical(0, &[]);

        assert_eq!(bus.mem.read_calls.get(), 0);
        assert_eq!(bus.mem.write_calls.get(), 0);
    }

    #[test]
    fn no_dma_memory_reads_as_open_bus() {
        let mut mem = NoDmaMemory;
        let mut buf = [0u8; 4];
        mem.read_physical(0, &mut buf);
        assert_eq!(buf, [0xFF; 4]);
        mem.write_physical(0, &[1, 2, 3, 4]);
    }

    #[test]
    fn ram_to_out_of_bounds_read_fills_with_zero() {
        let mut mem = VecLinearMemory::new(0x2000);
        mem.buf[0x1FFE] = 0xAA;
        mem.buf[0x1FFF] = 0xBB;

        let mut bus = GuestMemoryBusImpl::new_with_memory(mem, 0, 0x2000);

        let mut buf = [0u8; 4];
        bus.read_physical(0x1FFE, &mut buf);
        assert_eq!(buf, [0xAA, 0xBB, 0x00, 0x00]);

        bus.write_physical(0x1FFE, &[1, 2, 3, 4]);
        assert_eq!(bus.mem.buf[0x1FFE], 1);
        assert_eq!(bus.mem.buf[0x1FFF], 2);
    }

    #[test]
    fn ram_to_hole_read_fills_with_ff_and_ignores_writes() {
        let mut mem = SparseLinearMemory::new();
        mem.set_range(PCIE_ECAM_BASE as u32 - 4, &[0x11, 0x22, 0x33, 0x44]);

        let ram_bytes = PCIE_ECAM_BASE + 0x1000;
        let mut bus = GuestMemoryBusImpl::new_with_memory(mem, 0, ram_bytes);

        let mut buf = [0u8; 8];
        bus.read_physical(PCIE_ECAM_BASE - 4, &mut buf);
        assert_eq!(buf, [0x11, 0x22, 0x33, 0x44, 0xFF, 0xFF, 0xFF, 0xFF]);

        bus.write_physical(
            PCIE_ECAM_BASE - 4,
            &[0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7],
        );
        assert_eq!(
            bus.mem.get_range(PCIE_ECAM_BASE as u32 - 4, 4),
            vec![0xA0, 0xA1, 0xA2, 0xA3]
        );
        // Ensure the hole write did not create entries for the hole region.
        assert!(bus.mem.bytes.get(&(PCIE_ECAM_BASE as u32)).is_none());
    }

    #[test]
    fn hole_to_high_ram_crosses_and_writes_only_to_ram() {
        let mut mem = SparseLinearMemory::new();
        // High RAM physical 4GiB maps to RAM offset PCIE_ECAM_BASE.
        mem.set_range(PCIE_ECAM_BASE as u32, &[0x55, 0x66, 0x77, 0x88]);

        let ram_bytes = PCIE_ECAM_BASE + 0x1000;
        let mut bus = GuestMemoryBusImpl::new_with_memory(mem, 0, ram_bytes);

        let mut buf = [0u8; 4];
        bus.read_physical(HIGH_RAM_BASE - 2, &mut buf);
        assert_eq!(buf, [0xFF, 0xFF, 0x55, 0x66]);

        bus.write_physical(HIGH_RAM_BASE - 2, &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(
            bus.mem.get_range(PCIE_ECAM_BASE as u32, 4),
            vec![0x03, 0x04, 0x77, 0x88]
        );
    }
}
