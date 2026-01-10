use crate::memory::{
    read_u16_le, read_u32_le, read_u64_le, write_u16_le, write_u32_le, GuestMemory,
    GuestMemoryError,
};

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

pub const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;
pub const VIRTQ_USED_F_NO_NOTIFY: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtQueueError {
    GuestMemory(GuestMemoryError),
    QueueSizeZero,
    DescriptorIndexOutOfRange { index: u16, table_size: u16 },
    DescriptorChainLoop,
    IndirectDescriptorHasNext,
    NestedIndirectDescriptor,
    IndirectDescriptorLenNotMultipleOf16 { len: u32 },
    IndirectDescriptorTableTooLarge { count: u32 },
}

impl From<GuestMemoryError> for VirtQueueError {
    fn from(value: GuestMemoryError) -> Self {
        VirtQueueError::GuestMemory(value)
    }
}

/// Configuration for a split virtqueue (the legacy "vring" layout).
#[derive(Debug, Clone, Copy)]
pub struct VirtQueueConfig {
    pub size: u16,
    pub desc_addr: u64,
    pub avail_addr: u64,
    pub used_addr: u64,
}

#[derive(Debug, Clone, Copy)]
struct RawDescriptor {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

impl RawDescriptor {
    fn read_from<M: GuestMemory + ?Sized>(
        mem: &M,
        table_addr: u64,
        index: u16,
    ) -> Result<Self, VirtQueueError> {
        let base = table_addr + u64::from(index) * 16;
        let addr = read_u64_le(mem, base)?;
        let len = read_u32_le(mem, base + 8)?;
        let flags = read_u16_le(mem, base + 12)?;
        let next = read_u16_le(mem, base + 14)?;
        Ok(Self {
            addr,
            len,
            flags,
            next,
        })
    }
}

/// A parsed descriptor as presented to device models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

impl Descriptor {
    pub fn is_write_only(&self) -> bool {
        (self.flags & VIRTQ_DESC_F_WRITE) != 0
    }
}

/// A complete descriptor chain popped from a virtqueue.
#[derive(Debug, Clone)]
pub struct DescriptorChain {
    head_index: u16,
    descriptors: Vec<Descriptor>,
}

impl DescriptorChain {
    pub fn head_index(&self) -> u16 {
        self.head_index
    }

    pub fn descriptors(&self) -> &[Descriptor] {
        &self.descriptors
    }

    fn read_chain<M: GuestMemory + ?Sized>(
        mem: &M,
        table_addr: u64,
        table_size: u16,
        head_index: u16,
        allow_indirect: bool,
    ) -> Result<Vec<Descriptor>, VirtQueueError> {
        if table_size == 0 {
            return Err(VirtQueueError::QueueSizeZero);
        }
        if head_index >= table_size {
            return Err(VirtQueueError::DescriptorIndexOutOfRange {
                index: head_index,
                table_size,
            });
        }

        let mut visited = vec![false; table_size as usize];
        let mut out = Vec::new();
        let mut index = head_index;

        loop {
            if index >= table_size {
                return Err(VirtQueueError::DescriptorIndexOutOfRange { index, table_size });
            }
            if visited[index as usize] {
                return Err(VirtQueueError::DescriptorChainLoop);
            }
            visited[index as usize] = true;

            let raw = RawDescriptor::read_from(mem, table_addr, index)?;
            if (raw.flags & VIRTQ_DESC_F_INDIRECT) != 0 {
                if !allow_indirect {
                    return Err(VirtQueueError::NestedIndirectDescriptor);
                }
                if (raw.flags & VIRTQ_DESC_F_NEXT) != 0 {
                    return Err(VirtQueueError::IndirectDescriptorHasNext);
                }
                let indirect = Self::read_indirect(mem, raw.addr, raw.len)?;
                out.extend(indirect);
                break;
            }

            out.push(Descriptor {
                addr: raw.addr,
                len: raw.len,
                flags: raw.flags,
                next: raw.next,
            });

            if (raw.flags & VIRTQ_DESC_F_NEXT) == 0 {
                break;
            }
            index = raw.next;
        }
        Ok(out)
    }

    fn read_indirect<M: GuestMemory + ?Sized>(
        mem: &M,
        table_addr: u64,
        len: u32,
    ) -> Result<Vec<Descriptor>, VirtQueueError> {
        if len % 16 != 0 {
            return Err(VirtQueueError::IndirectDescriptorLenNotMultipleOf16 { len });
        }
        let count_u32 = len / 16;
        let count = u16::try_from(count_u32)
            .map_err(|_| VirtQueueError::IndirectDescriptorTableTooLarge { count: count_u32 })?;
        Self::read_chain(mem, table_addr, count, 0, false)
    }
}

/// A split virtqueue implementation operating over guest memory.
#[derive(Debug, Clone)]
pub struct VirtQueue {
    config: VirtQueueConfig,
    next_avail: u16,
    next_used: u16,
    event_idx: bool,
}

impl VirtQueue {
    pub fn new(config: VirtQueueConfig, event_idx: bool) -> Result<Self, VirtQueueError> {
        if config.size == 0 {
            return Err(VirtQueueError::QueueSizeZero);
        }
        Ok(Self {
            config,
            next_avail: 0,
            next_used: 0,
            event_idx,
        })
    }

    pub fn size(&self) -> u16 {
        self.config.size
    }

    pub fn set_event_idx(&mut self, enabled: bool) {
        self.event_idx = enabled;
    }

    pub fn pop_descriptor_chain<M: GuestMemory + ?Sized>(
        &mut self,
        mem: &M,
    ) -> Result<Option<DescriptorChain>, VirtQueueError> {
        let avail_idx = read_u16_le(mem, self.config.avail_addr + 2)?;
        if avail_idx == self.next_avail {
            return Ok(None);
        }

        let ring_index = self.next_avail % self.config.size;
        let elem_addr = self.config.avail_addr + 4 + u64::from(ring_index) * 2;
        let head = read_u16_le(mem, elem_addr)?;
        self.next_avail = self.next_avail.wrapping_add(1);

        let descriptors =
            DescriptorChain::read_chain(mem, self.config.desc_addr, self.config.size, head, true)?;
        Ok(Some(DescriptorChain {
            head_index: head,
            descriptors,
        }))
    }

    pub fn add_used<M: GuestMemory + ?Sized>(
        &mut self,
        mem: &mut M,
        head_index: u16,
        len: u32,
    ) -> Result<bool, VirtQueueError> {
        let old_used = self.next_used;
        let used_elem_index = old_used % self.config.size;
        let elem_addr = self.config.used_addr + 4 + u64::from(used_elem_index) * 8;
        write_u32_le(mem, elem_addr, u32::from(head_index))?;
        write_u32_le(mem, elem_addr + 4, len)?;

        self.next_used = self.next_used.wrapping_add(1);
        write_u16_le(mem, self.config.used_addr + 2, self.next_used)?;

        Ok(self.needs_interrupt(mem, old_used, self.next_used)?)
    }

    /// Update the `avail_event` field (end of the used ring) when
    /// `VIRTIO_F_RING_EVENT_IDX` is negotiated.
    ///
    /// This tells the guest when it should notify the device about new available
    /// buffers (guest → device notification suppression).
    pub fn update_avail_event<M: GuestMemory + ?Sized>(
        &self,
        mem: &mut M,
    ) -> Result<(), VirtQueueError> {
        if !self.event_idx {
            return Ok(());
        }
        let avail_event_addr = self.config.used_addr + 4 + u64::from(self.config.size) * 8;
        write_u16_le(mem, avail_event_addr, self.next_avail)?;
        Ok(())
    }

    fn needs_interrupt<M: GuestMemory + ?Sized>(
        &self,
        mem: &M,
        old_used: u16,
        new_used: u16,
    ) -> Result<bool, VirtQueueError> {
        if self.event_idx {
            // Virtio spec: vring_need_event(event_idx, new_idx, old_idx)
            // "used_event" lives after the avail ring and is written by the driver.
            let used_event_addr = self.config.avail_addr + 4 + u64::from(self.config.size) * 2;
            let event_idx = read_u16_le(mem, used_event_addr)?;
            Ok(vring_need_event(event_idx, new_used, old_used))
        } else {
            let flags = read_u16_le(mem, self.config.avail_addr)?;
            Ok((flags & VIRTQ_AVAIL_F_NO_INTERRUPT) == 0)
        }
    }
}

fn vring_need_event(event: u16, new: u16, old: u16) -> bool {
    new.wrapping_sub(event.wrapping_add(1)) < new.wrapping_sub(old)
}
