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
        let avail_idx = mem.read_u16_le(self.avail_ring + 2)?;
        if avail_idx == self.last_avail_idx {
            return Ok(None);
        }

        let ring_index = (self.last_avail_idx % self.size) as u64;
        let head_index = mem.read_u16_le(self.avail_ring + 4 + ring_index * 2)?;
        self.last_avail_idx = self.last_avail_idx.wrapping_add(1);

        Ok(Some(self.read_chain(mem, head_index)?))
    }

    pub fn push_used(
        &mut self,
        mem: &mut impl GuestMemory,
        chain: &DescriptorChain,
        len: u32,
    ) -> Result<bool, VirtQueueError> {
        let used_ring_index = (self.next_used_idx % self.size) as u64;
        let used_elem_addr = self.used_ring + 4 + used_ring_index * 8;

        mem.write_u32_le(used_elem_addr, chain.head_index as u32)?;
        mem.write_u32_le(used_elem_addr + 4, len)?;

        self.next_used_idx = self.next_used_idx.wrapping_add(1);
        mem.write_u16_le(self.used_ring + 2, self.next_used_idx)?;

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
        let base = self.desc_table + (index as u64) * 16;
        Ok(Descriptor {
            addr: mem.read_u64_le(base)?,
            len: mem.read_u32_le(base + 8)?,
            flags: mem.read_u16_le(base + 12)?,
            next: mem.read_u16_le(base + 14)?,
        })
    }
}
