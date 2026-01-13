//! Shared guest-memory `MemoryBus` implementation for wasm-side device bridges.
//!
//! Multiple wasm device bridges (UHCI/WebUSB/etc.) need to DMA into guest RAM stored inside the
//! module's linear memory. Guest physical address translation must account for the Q35 RAM layout
//! hole between the PCIe ECAM base and 4GiB (see [`crate::guest_phys`]).
//!
//! This module centralizes the translation + copy semantics to avoid drift across bridges.
//!
//! Semantics preserved from the historical per-bridge implementations:
//! - Reads from the PCI/MMIO hole return `0xFF` bytes (open bus).
//! - Reads out of bounds return `0x00` bytes.
//! - Writes to the hole or out of bounds are ignored.
//! - All arithmetic is overflow-checked; callers will never observe panics from malformed inputs.

use aero_usb::MemoryBus;

/// Guest RAM accessor backed by a contiguous region of linear memory.
///
/// In the browser runtime, `guest_base` is the byte offset inside wasm linear memory where guest
/// physical address 0 begins (see the `guest_ram_layout` contract). In host-side unit tests it may
/// be an actual host pointer value; the implementation treats it purely as an address.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GuestMemoryBus {
    guest_base: u64,
    ram_bytes: u64,
}

impl GuestMemoryBus {
    /// Create a new guest-memory bus with the given base offset/pointer and RAM size.
    pub(crate) fn new(guest_base: u32, ram_bytes: u64) -> Self {
        Self {
            guest_base: u64::from(guest_base),
            ram_bytes,
        }
    }

    /// Create a new guest-memory bus using a raw address for `guest_base`.
    ///
    /// This is intended for non-wasm tests/harnesses where guest RAM is backed by a native
    /// allocation (e.g. `Vec<u8>`). The address is still range-checked and never dereferenced if
    /// it cannot be represented safely.
    #[cfg(test)]
    pub(crate) fn new_raw(guest_base: usize, ram_bytes: u64) -> Self {
        Self {
            guest_base: guest_base as u64,
            ram_bytes,
        }
    }

    #[inline]
    fn linear_ptr(&self, ram_offset: u64, len: usize) -> Option<*const u8> {
        // Ensure the translated RAM range is within the configured guest RAM size.
        let end = ram_offset.checked_add(len as u64)?;
        if end > self.ram_bytes {
            return None;
        }

        // Translate RAM offset to a linear memory address.
        let linear = self.guest_base.checked_add(ram_offset)?;
        let linear_usize = usize::try_from(linear).ok()?;
        // Ensure `linear + len` does not overflow `usize` so pointer range math cannot wrap.
        let _ = linear_usize.checked_add(len)?;
        Some(linear_usize as *const u8)
    }

    #[inline]
    fn linear_ptr_mut(&self, ram_offset: u64, len: usize) -> Option<*mut u8> {
        Some(self.linear_ptr(ram_offset, len)? as *mut u8)
    }

    #[inline]
    fn fill_slice(buf: &mut [u8], start: usize, len: usize, value: u8) -> Result<(), ()> {
        let end = start.checked_add(len).ok_or(())?;
        if end > buf.len() {
            return Err(());
        }
        buf[start..end].fill(value);
        Ok(())
    }
}

impl MemoryBus for GuestMemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        let mut cur_paddr = paddr;
        let mut off = 0usize;

        while off < buf.len() {
            let remaining = buf.len().saturating_sub(off);
            if remaining == 0 {
                break;
            }

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

                    let end = match off.checked_add(len) {
                        Some(v) if v <= buf.len() => v,
                        _ => {
                            buf[off..].fill(0);
                            return;
                        }
                    };

                    // Safety: `linear_ptr` ensures the range fits within the configured guest RAM
                    // and that the computed linear address range cannot wrap `usize`.
                    unsafe {
                        core::ptr::copy_nonoverlapping(ptr, buf[off..end].as_mut_ptr(), len);
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::Hole { len } => {
                    if Self::fill_slice(buf, off, len, 0xFF).is_err() {
                        buf[off..].fill(0);
                        return;
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::OutOfBounds { len } => {
                    if Self::fill_slice(buf, off, len, 0).is_err() {
                        buf[off..].fill(0);
                        return;
                    }
                    len
                }
            };

            if chunk_len == 0 {
                break;
            }

            off = match off.checked_add(chunk_len) {
                Some(v) if v <= buf.len() => v,
                _ => {
                    // Internal arithmetic overflow; treat the rest as out-of-bounds.
                    buf.fill(0);
                    return;
                }
            };

            cur_paddr = match cur_paddr.checked_add(chunk_len as u64) {
                Some(v) => v,
                None => {
                    // Preserve legacy semantics: overflow while advancing the address terminates the
                    // transfer and zero-fills any remaining bytes.
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
            let remaining = buf.len().saturating_sub(off);
            if remaining == 0 {
                break;
            }

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

                    let end = match off.checked_add(len) {
                        Some(v) if v <= buf.len() => v,
                        _ => return,
                    };

                    // Safety: `linear_ptr_mut` ensures the range fits within the configured guest
                    // RAM and that the computed linear address range cannot wrap `usize`.
                    unsafe {
                        core::ptr::copy_nonoverlapping(buf[off..end].as_ptr(), ptr, len);
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::Hole { len } => {
                    // Open bus: writes are ignored.
                    len
                }
                crate::guest_phys::GuestRamChunk::OutOfBounds { len } => {
                    // Preserve historical semantics: ignore out-of-range writes.
                    len
                }
            };

            if chunk_len == 0 {
                break;
            }

            off = match off.checked_add(chunk_len) {
                Some(v) if v <= buf.len() => v,
                _ => return,
            };

            cur_paddr = match cur_paddr.checked_add(chunk_len as u64) {
                Some(v) => v,
                None => return,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GuestMemoryBus;

    use aero_usb::MemoryBus;

    #[test]
    fn guest_memory_bus_new_is_safe_without_valid_backing_memory() {
        // `GuestMemoryBus::new` is the constructor used by wasm-side callers where `guest_base` is a
        // linear-memory offset. In host tests we can still exercise it by using a zero-length RAM
        // region so the bus never dereferences the base address.
        let mut bus = GuestMemoryBus::new(0, 0);
        let mut buf = [0xAAu8; 4];
        bus.read_physical(0, &mut buf);
        assert_eq!(buf, [0u8; 4]);
    }

    #[test]
    fn guest_memory_bus_read_write_and_fill_semantics() {
        let mut backing = vec![0u8; 0x100];
        let base = backing.as_mut_ptr() as usize;

        let mut bus = GuestMemoryBus::new_raw(base, backing.len() as u64);

        // In-bounds write + read.
        bus.write_physical(0x10, &[1, 2, 3, 4]);
        assert_eq!(&backing[0x10..0x14], &[1, 2, 3, 4]);

        let mut tmp = [0u8; 4];
        bus.read_physical(0x10, &mut tmp);
        assert_eq!(tmp, [1, 2, 3, 4]);

        // Hole reads are open bus (0xFF).
        let mut hole = [0u8; 8];
        bus.read_physical(crate::guest_phys::PCIE_ECAM_BASE, &mut hole);
        assert_eq!(hole, [0xFF; 8]);

        // Out-of-bounds reads return 0.
        let mut oob = [0xAAu8; 8];
        bus.read_physical(backing.len() as u64 + 1, &mut oob);
        assert_eq!(oob, [0u8; 8]);
    }

    #[test]
    fn guest_memory_bus_does_not_panic_on_overflowing_address() {
        let mut backing = vec![0u8; 0x10];
        let base = backing.as_mut_ptr() as usize;
        let mut bus = GuestMemoryBus::new_raw(base, backing.len() as u64);

        let mut buf = [0xAAu8; 4];
        bus.read_physical(u64::MAX - 1, &mut buf);
        assert_eq!(buf, [0u8; 4]);
    }
}
