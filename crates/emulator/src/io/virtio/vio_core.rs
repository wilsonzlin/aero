//! Minimal virtio "split ring" implementation used by device models.
//!
//! The project-wide name for this module is **VIO-CORE**.

use core::fmt;
use memory::{GuestMemory, GuestMemoryError};

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;
pub const VRING_DESC_F_INDIRECT: u16 = 4;

pub const VRING_AVAIL_F_NO_INTERRUPT: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DescriptorChain {
    pub head_index: u16,
    pub descriptors: Vec<Descriptor>,
}

impl fmt::Debug for DescriptorChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DescriptorChain")
            .field("head_index", &self.head_index)
            .field("descriptors_len", &self.descriptors.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtQueueError {
    GuestMemory(GuestMemoryError),
    IndirectDescriptorsNotSupported,
    DescriptorChainLoop { head: u16 },
    DescriptorIndexOutOfRange { index: u16, size: u16 },
    DescriptorChainTooShort { requested: usize },
}

impl fmt::Display for VirtQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VirtQueueError::GuestMemory(err) => write!(f, "{err}"),
            VirtQueueError::IndirectDescriptorsNotSupported => {
                write!(f, "virtqueue uses indirect descriptors (unsupported)")
            }
            VirtQueueError::DescriptorChainLoop { head } => {
                write!(f, "virtqueue descriptor chain loop detected: head={head}")
            }
            VirtQueueError::DescriptorIndexOutOfRange { index, size } => write!(
                f,
                "virtqueue descriptor index out of range: index={index} size={size}"
            ),
            VirtQueueError::DescriptorChainTooShort { requested } => write!(
                f,
                "virtqueue descriptor chain too short: requested {requested} bytes"
            ),
        }
    }
}

impl std::error::Error for VirtQueueError {}

impl From<GuestMemoryError> for VirtQueueError {
    fn from(value: GuestMemoryError) -> Self {
        Self::GuestMemory(value)
    }
}

fn checked_offset(base: u64, offset: u64, len: usize, size: u64) -> Result<u64, VirtQueueError> {
    base.checked_add(offset).ok_or_else(|| {
        VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange { paddr: base, len, size })
    })
}

#[derive(Debug, Clone)]
pub struct VirtQueue {
    pub size: u16,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,

    last_avail_idx: u16,
    next_used_idx: u16,
}

impl VirtQueue {
    pub fn new(size: u16, desc_table: u64, avail_ring: u64, used_ring: u64) -> Self {
        Self {
            size,
            desc_table,
            avail_ring,
            used_ring,
            last_avail_idx: 0,
            next_used_idx: 0,
        }
    }

    pub fn pop_available(
        &mut self,
        mem: &impl GuestMemory,
    ) -> Result<Option<DescriptorChain>, VirtQueueError> {
        let avail_idx_addr = checked_offset(self.avail_ring, 2, 2, mem.size())?;
        let avail_idx = mem.read_u16_le(avail_idx_addr)?;
        if avail_idx == self.last_avail_idx {
            return Ok(None);
        }

        let ring_index = (self.last_avail_idx % self.size) as u64;
        let head_index_addr =
            checked_offset(self.avail_ring, 4 + ring_index * 2, 2, mem.size())?;
        let head_index = mem.read_u16_le(head_index_addr)?;
        self.last_avail_idx = self.last_avail_idx.wrapping_add(1);

        Ok(Some(self.read_chain(mem, head_index)?))
    }

    /// Returns the next available descriptor chain without consuming it.
    ///
    /// This can be used by devices that need to validate whether they can
    /// service the chain (e.g. ensure sufficient buffer capacity) before
    /// advancing the available index.
    pub fn peek_available(
        &self,
        mem: &impl GuestMemory,
    ) -> Result<Option<DescriptorChain>, VirtQueueError> {
        let avail_idx_addr = checked_offset(self.avail_ring, 2, 2, mem.size())?;
        let avail_idx = mem.read_u16_le(avail_idx_addr)?;
        if avail_idx == self.last_avail_idx {
            return Ok(None);
        }

        let ring_index = (self.last_avail_idx % self.size) as u64;
        let head_index_addr =
            checked_offset(self.avail_ring, 4 + ring_index * 2, 2, mem.size())?;
        let head_index = mem.read_u16_le(head_index_addr)?;
        Ok(Some(self.read_chain(mem, head_index)?))
    }

    /// Advances the available index by one entry, consuming the same chain
    /// returned by [`peek_available`].
    ///
    /// Returns `true` if an entry was consumed.
    pub fn consume_available(&mut self, mem: &impl GuestMemory) -> Result<bool, VirtQueueError> {
        let avail_idx_addr = checked_offset(self.avail_ring, 2, 2, mem.size())?;
        let avail_idx = mem.read_u16_le(avail_idx_addr)?;
        if avail_idx == self.last_avail_idx {
            return Ok(false);
        }

        self.last_avail_idx = self.last_avail_idx.wrapping_add(1);
        Ok(true)
    }

    pub fn push_used(
        &mut self,
        mem: &mut impl GuestMemory,
        chain: &DescriptorChain,
        len: u32,
    ) -> Result<bool, VirtQueueError> {
        let mem_size = mem.size();
        let used_ring_index = (self.next_used_idx % self.size) as u64;
        let used_elem_addr = checked_offset(self.used_ring, 4 + used_ring_index * 8, 8, mem_size)?;

        mem.write_u32_le(used_elem_addr, chain.head_index as u32)?;
        let used_elem_len_addr = checked_offset(used_elem_addr, 4, 4, mem_size)?;
        mem.write_u32_le(used_elem_len_addr, len)?;

        self.next_used_idx = self.next_used_idx.wrapping_add(1);
        let used_idx_addr = checked_offset(self.used_ring, 2, 2, mem_size)?;
        mem.write_u16_le(used_idx_addr, self.next_used_idx)?;

        self.device_should_interrupt(mem)
    }

    pub fn device_should_interrupt(&self, mem: &impl GuestMemory) -> Result<bool, VirtQueueError> {
        let avail_flags = mem.read_u16_le(self.avail_ring)?;
        Ok(avail_flags & VRING_AVAIL_F_NO_INTERRUPT == 0)
    }

    fn read_chain(
        &self,
        mem: &impl GuestMemory,
        head_index: u16,
    ) -> Result<DescriptorChain, VirtQueueError> {
        if head_index >= self.size {
            return Err(VirtQueueError::DescriptorIndexOutOfRange {
                index: head_index,
                size: self.size,
            });
        }

        let mut descriptors = Vec::new();
        let mut next_index = head_index;

        for _ in 0..self.size {
            if next_index >= self.size {
                return Err(VirtQueueError::DescriptorIndexOutOfRange {
                    index: next_index,
                    size: self.size,
                });
            }

            let desc = self.read_descriptor(mem, next_index)?;
            if desc.flags & VRING_DESC_F_INDIRECT != 0 {
                return Err(VirtQueueError::IndirectDescriptorsNotSupported);
            }

            descriptors.push(desc);

            if desc.flags & VRING_DESC_F_NEXT == 0 {
                return Ok(DescriptorChain {
                    head_index,
                    descriptors,
                });
            }

            next_index = desc.next;
        }

        Err(VirtQueueError::DescriptorChainLoop { head: head_index })
    }

    fn read_descriptor(
        &self,
        mem: &impl GuestMemory,
        index: u16,
    ) -> Result<Descriptor, VirtQueueError> {
        let base = checked_offset(self.desc_table, (index as u64) * 16, 16, mem.size())?;
        let len_addr = checked_offset(base, 8, 4, mem.size())?;
        let flags_addr = checked_offset(base, 12, 2, mem.size())?;
        let next_addr = checked_offset(base, 14, 2, mem.size())?;
        Ok(Descriptor {
            addr: mem.read_u64_le(base)?,
            len: mem.read_u32_le(len_addr)?,
            flags: mem.read_u16_le(flags_addr)?,
            next: mem.read_u16_le(next_addr)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory::DenseMemory;

    fn write_desc(mem: &mut DenseMemory, base: u64, index: u16, desc: Descriptor) {
        let off = base + (index as u64) * 16;
        mem.write_u64_le(off, desc.addr).unwrap();
        mem.write_u32_le(off + 8, desc.len).unwrap();
        mem.write_u16_le(off + 12, desc.flags).unwrap();
        mem.write_u16_le(off + 14, desc.next).unwrap();
    }

    fn init_avail(mem: &mut DenseMemory, avail: u64, flags: u16, idx: u16, head: u16) {
        mem.write_u16_le(avail, flags).unwrap();
        mem.write_u16_le(avail + 2, idx).unwrap();
        mem.write_u16_le(avail + 4, head).unwrap();
    }

    #[test]
    fn peek_available_does_not_consume_and_consume_available_advances() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: 0x80,
                len: 4,
                flags: 0,
                next: 0,
            },
        );
        init_avail(&mut mem, avail, 0, 1, 0);

        let mut vq = VirtQueue::new(8, desc_table, avail, used);
        let first = vq.peek_available(&mem).unwrap().unwrap();
        let second = vq.peek_available(&mem).unwrap().unwrap();
        assert_eq!(first, second);

        assert!(vq.consume_available(&mem).unwrap());
        assert!(vq.peek_available(&mem).unwrap().is_none());
        assert!(!vq.consume_available(&mem).unwrap());
    }

    #[test]
    fn peek_then_pop_available_returns_same_chain() {
        let mut mem = DenseMemory::new(0x4000).unwrap();

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        write_desc(
            &mut mem,
            desc_table,
            0,
            Descriptor {
                addr: 0x80,
                len: 4,
                flags: 0,
                next: 0,
            },
        );
        init_avail(&mut mem, avail, 0, 1, 0);

        let mut vq = VirtQueue::new(8, desc_table, avail, used);
        let peeked = vq.peek_available(&mem).unwrap().unwrap();
        let popped = vq.pop_available(&mem).unwrap().unwrap();
        assert_eq!(peeked, popped);
        assert!(vq.peek_available(&mem).unwrap().is_none());
    }

    #[test]
    fn virtqueue_addresses_do_not_panic_on_overflow() {
        let mem = DenseMemory::new(0x100).unwrap();
        let mut vq = VirtQueue::new(8, 0, u64::MAX - 1, 0);
        let err = vq.pop_available(&mem).unwrap_err();
        assert!(matches!(
            err,
            VirtQueueError::GuestMemory(GuestMemoryError::OutOfRange { .. })
        ));
    }
}
