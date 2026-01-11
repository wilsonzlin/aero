use crate::io::virtio::core::GuestMemory;
use crate::io::virtio::core::GuestMemoryError;
use std::fmt;

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

pub const VRING_AVAIL_F_NO_INTERRUPT: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VringDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescChain {
    pub head_index: u16,
    pub descs: Vec<VringDesc>,
}

#[derive(Debug)]
pub enum VirtQueueError {
    GuestMemory(GuestMemoryError),
    DescriptorIndexOutOfRange { index: u16, size: u16 },
    DescriptorLoop { head_index: u16 },
    IndirectDescriptorUnsupported,
}

impl fmt::Display for VirtQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VirtQueueError::GuestMemory(e) => write!(f, "{e}"),
            VirtQueueError::DescriptorIndexOutOfRange { index, size } => write!(
                f,
                "virtqueue descriptor index out of range: index={index}, size={size}"
            ),
            VirtQueueError::DescriptorLoop { head_index } => {
                write!(
                    f,
                    "virtqueue descriptor loop detected: head_index={head_index}"
                )
            }
            VirtQueueError::IndirectDescriptorUnsupported => {
                write!(f, "virtqueue indirect descriptors are not supported")
            }
        }
    }
}

impl std::error::Error for VirtQueueError {}

impl From<GuestMemoryError> for VirtQueueError {
    fn from(value: GuestMemoryError) -> Self {
        VirtQueueError::GuestMemory(value)
    }
}

/// A minimal split virtqueue implementation (descriptor/avail/used rings).
///
/// This intentionally only implements the pieces needed for device-side
/// request processing and unit tests.
#[derive(Debug, Clone)]
pub struct VirtQueue {
    size: u16,
    desc_addr: u64,
    avail_addr: u64,
    used_addr: u64,
    next_avail_idx: u16,
    next_used_idx: u16,
}

impl VirtQueue {
    pub fn new(size: u16, desc_addr: u64, avail_addr: u64, used_addr: u64) -> Self {
        Self {
            size,
            desc_addr,
            avail_addr,
            used_addr,
            next_avail_idx: 0,
            next_used_idx: 0,
        }
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    fn read_desc(&self, mem: &dyn GuestMemory, index: u16) -> Result<VringDesc, VirtQueueError> {
        if index >= self.size {
            return Err(VirtQueueError::DescriptorIndexOutOfRange {
                index,
                size: self.size,
            });
        }

        let base = self.desc_addr + (index as u64) * 16;
        Ok(VringDesc {
            addr: mem.read_u64_le(base)?,
            len: mem.read_u32_le(base + 8)?,
            flags: mem.read_u16_le(base + 12)?,
            next: mem.read_u16_le(base + 14)?,
        })
    }

    fn read_avail_idx(&self, mem: &dyn GuestMemory) -> Result<u16, VirtQueueError> {
        Ok(mem.read_u16_le(self.avail_addr + 2)?)
    }

    fn read_avail_flags(&self, mem: &dyn GuestMemory) -> Result<u16, VirtQueueError> {
        Ok(mem.read_u16_le(self.avail_addr)?)
    }

    fn read_avail_ring(
        &self,
        mem: &dyn GuestMemory,
        ring_index: u16,
    ) -> Result<u16, VirtQueueError> {
        let ring_addr = self.avail_addr + 4 + (ring_index as u64) * 2;
        Ok(mem.read_u16_le(ring_addr)?)
    }

    fn write_used_idx(&self, mem: &mut dyn GuestMemory, idx: u16) -> Result<(), VirtQueueError> {
        mem.write_u16_le(self.used_addr + 2, idx)?;
        Ok(())
    }

    fn write_used_elem(
        &self,
        mem: &mut dyn GuestMemory,
        ring_index: u16,
        id: u32,
        len: u32,
    ) -> Result<(), VirtQueueError> {
        let elem_addr = self.used_addr + 4 + (ring_index as u64) * 8;
        mem.write_u32_le(elem_addr, id)?;
        mem.write_u32_le(elem_addr + 4, len)?;
        Ok(())
    }

    /// Returns the next available descriptor chain, if any.
    pub fn pop_available(
        &mut self,
        mem: &dyn GuestMemory,
    ) -> Result<Option<DescChain>, VirtQueueError> {
        let avail_idx = self.read_avail_idx(mem)?;
        if self.next_avail_idx == avail_idx {
            return Ok(None);
        }

        let ring_index = self.next_avail_idx % self.size;
        let head = self.read_avail_ring(mem, ring_index)?;
        self.next_avail_idx = self.next_avail_idx.wrapping_add(1);

        let mut descs = Vec::new();
        let mut seen = vec![false; self.size as usize];
        let mut cur = head;

        loop {
            if cur >= self.size {
                return Err(VirtQueueError::DescriptorIndexOutOfRange {
                    index: cur,
                    size: self.size,
                });
            }
            if seen[cur as usize] {
                return Err(VirtQueueError::DescriptorLoop { head_index: head });
            }
            seen[cur as usize] = true;

            let d = self.read_desc(mem, cur)?;
            if d.flags & VIRTQ_DESC_F_INDIRECT != 0 {
                return Err(VirtQueueError::IndirectDescriptorUnsupported);
            }
            descs.push(d);

            if d.flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            cur = d.next;
        }

        Ok(Some(DescChain {
            head_index: head,
            descs,
        }))
    }

    /// Pushes a used-ring completion for the provided head descriptor index.
    pub fn push_used(
        &mut self,
        mem: &mut dyn GuestMemory,
        head_index: u16,
        len: u32,
    ) -> Result<(), VirtQueueError> {
        let ring_index = self.next_used_idx % self.size;
        self.write_used_elem(mem, ring_index, head_index as u32, len)?;
        self.next_used_idx = self.next_used_idx.wrapping_add(1);
        self.write_used_idx(mem, self.next_used_idx)?;
        Ok(())
    }

    /// Returns true when the guest has not disabled interrupts for this queue.
    pub fn should_notify(&self, mem: &dyn GuestMemory) -> Result<bool, VirtQueueError> {
        Ok(self.read_avail_flags(mem)? & VRING_AVAIL_F_NO_INTERRUPT == 0)
    }
}
