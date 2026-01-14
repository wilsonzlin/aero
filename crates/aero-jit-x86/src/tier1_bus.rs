//! Minimal memory bus trait used by the Tier-1 front-end.
//!
//! The Tier-1 pipeline is intentionally lightweight and is primarily used for
//! unit tests and early bring-up. It only needs byte-addressable linear memory.

use aero_types::Width;

pub trait Tier1Bus {
    fn read_u8(&self, addr: u64) -> u8;
    fn write_u8(&mut self, addr: u64, value: u8);

    /// Fetch the next 15 bytes of the instruction stream starting at `addr`.
    ///
    /// This is the architectural maximum instruction length and is used by the Tier-1
    /// decoder/block discovery logic. The default implementation performs 15 individual
    /// [`read_u8`] calls using `u64` wrapping arithmetic; implementations with fast contiguous RAM
    /// access can override it to avoid per-byte overhead and heap allocation.
    #[must_use]
    fn fetch_window(&self, addr: u64) -> [u8; 15] {
        let mut buf = [0u8; 15];
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = self.read_u8(addr.wrapping_add(i as u64));
        }
        buf
    }

    #[must_use]
    fn read(&self, addr: u64, width: Width) -> u64 {
        match width {
            Width::W8 => self.read_u8(addr) as u64,
            Width::W16 => {
                let b0 = self.read_u8(addr) as u64;
                let b1 = self.read_u8(addr.wrapping_add(1)) as u64;
                b0 | (b1 << 8)
            }
            Width::W32 => {
                let mut out = 0u64;
                for i in 0..4u64 {
                    out |= (self.read_u8(addr.wrapping_add(i)) as u64) << (i * 8);
                }
                out
            }
            Width::W64 => {
                let mut out = 0u64;
                for i in 0..8u64 {
                    out |= (self.read_u8(addr.wrapping_add(i)) as u64) << (i * 8);
                }
                out
            }
        }
    }

    fn write(&mut self, addr: u64, width: Width, value: u64) {
        let v = width.truncate(value);
        match width {
            Width::W8 => self.write_u8(addr, v as u8),
            Width::W16 => {
                self.write_u8(addr, v as u8);
                self.write_u8(addr.wrapping_add(1), (v >> 8) as u8);
            }
            Width::W32 => {
                for i in 0..4u64 {
                    self.write_u8(addr.wrapping_add(i), (v >> (i * 8)) as u8);
                }
            }
            Width::W64 => {
                for i in 0..8u64 {
                    self.write_u8(addr.wrapping_add(i), (v >> (i * 8)) as u8);
                }
            }
        }
    }

    #[must_use]
    fn fetch(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read_u8(addr.wrapping_add(i as u64));
        }
        buf
    }
}
