use crate::memory::{read_u16_le, write_u16_le, GuestMemory, GuestMemoryError};

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;
pub const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// Maximum number of entries permitted in an indirect descriptor table.
///
/// Aero's virtio transport is designed to be deterministic and robust under
/// hostile/corrupted guests. Indirect descriptors can, in theory, describe very
/// large chains; bounding the table size prevents pathological O(N) behaviour
/// and large allocations when parsing a single descriptor chain.
pub const MAX_INDIRECT_DESC_TABLE_ENTRIES: u32 = 4096;

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
        let offset = u64::from(index) * 16;
        let base = table_addr
            .checked_add(offset)
            .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfBounds {
                addr: table_addr,
                len: 16,
            }))?;
        let bytes = mem.get_slice(base, 16)?;

        let addr = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let flags = u16::from_le_bytes(bytes[12..14].try_into().unwrap());
        let next = u16::from_le_bytes(bytes[14..16].try_into().unwrap());
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
        if !len.is_multiple_of(16) {
            return Err(VirtQueueError::IndirectDescriptorLenNotMultipleOf16 { len });
        }
        let count_u32 = len / 16;
        if count_u32 > MAX_INDIRECT_DESC_TABLE_ENTRIES {
            return Err(VirtQueueError::IndirectDescriptorTableTooLarge { count: count_u32 });
        }
        let count = u16::try_from(count_u32)
            .map_err(|_| VirtQueueError::IndirectDescriptorTableTooLarge { count: count_u32 })?;
        Self::read_chain(mem, table_addr, count, 0, false)
    }
}

/// Result of popping the next available descriptor chain.
///
/// `Invalid` indicates that the avail ring entry was consumed (the queue's `next_avail` advanced),
/// but the descriptor chain could not be parsed. Transports should still complete the chain with a
/// used entry (typically `used.len = 0`) so the guest can reclaim the descriptor.
#[derive(Debug, Clone)]
pub enum PoppedDescriptorChain {
    Chain(DescriptorChain),
    Invalid {
        head_index: u16,
        error: VirtQueueError,
    },
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

    /// Current device-side avail ring index.
    pub fn next_avail(&self) -> u16 {
        self.next_avail
    }

    /// Current device-side used ring index.
    pub fn next_used(&self) -> u16 {
        self.next_used
    }

    /// Whether `VIRTIO_F_RING_EVENT_IDX` is enabled for this queue.
    pub fn event_idx(&self) -> bool {
        self.event_idx
    }

    /// Restore the device-side progress counters for this virtqueue.
    ///
    /// This is used by VM snapshot/restore so already-consumed avail entries are not reprocessed
    /// after restore.
    pub fn restore_progress(&mut self, next_avail: u16, next_used: u16, event_idx: bool) {
        self.next_avail = next_avail;
        self.next_used = next_used;
        self.event_idx = event_idx;
    }

    pub fn pop_descriptor_chain<M: GuestMemory + ?Sized>(
        &mut self,
        mem: &M,
    ) -> Result<Option<PoppedDescriptorChain>, VirtQueueError> {
        let avail_idx_addr =
            self.config
                .avail_addr
                .checked_add(2)
                .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfBounds {
                    addr: self.config.avail_addr,
                    len: 2,
                }))?;
        let avail_idx = read_u16_le(mem, avail_idx_addr)?;
        if avail_idx == self.next_avail {
            return Ok(None);
        }

        let ring_index = self.next_avail % self.config.size;
        let elem_offset = 4 + u64::from(ring_index) * 2;
        let elem_addr =
            self.config
                .avail_addr
                .checked_add(elem_offset)
                .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfBounds {
                    addr: self.config.avail_addr,
                    len: 2,
                }))?;
        let head = read_u16_le(mem, elem_addr)?;
        self.next_avail = self.next_avail.wrapping_add(1);

        match DescriptorChain::read_chain(mem, self.config.desc_addr, self.config.size, head, true)
        {
            Ok(descriptors) => Ok(Some(PoppedDescriptorChain::Chain(DescriptorChain {
                head_index: head,
                descriptors,
            }))),
            Err(err) => Ok(Some(PoppedDescriptorChain::Invalid {
                head_index: head,
                error: err,
            })),
        }
    }

    pub fn add_used<M: GuestMemory + ?Sized>(
        &mut self,
        mem: &mut M,
        head_index: u16,
        len: u32,
    ) -> Result<bool, VirtQueueError> {
        let old_used = self.next_used;
        let used_elem_index = old_used % self.config.size;
        let elem_offset = 4 + u64::from(used_elem_index) * 8;
        let elem_addr =
            self.config
                .used_addr
                .checked_add(elem_offset)
                .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfBounds {
                    addr: self.config.used_addr,
                    len: 8,
                }))?;
        let mut elem_bytes = [0u8; 8];
        elem_bytes[0..4].copy_from_slice(&u32::from(head_index).to_le_bytes());
        elem_bytes[4..8].copy_from_slice(&len.to_le_bytes());
        mem.write(elem_addr, &elem_bytes)?;

        self.next_used = self.next_used.wrapping_add(1);
        let used_idx_addr =
            self.config
                .used_addr
                .checked_add(2)
                .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfBounds {
                    addr: self.config.used_addr,
                    len: 2,
                }))?;
        write_u16_le(mem, used_idx_addr, self.next_used)?;

        self.needs_interrupt(mem, old_used, self.next_used)
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
        let avail_event_offset = 4 + u64::from(self.config.size) * 8;
        let avail_event_addr = self
            .config
            .used_addr
            .checked_add(avail_event_offset)
            .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfBounds {
                addr: self.config.used_addr,
                len: 2,
            }))?;
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
            let used_event_offset = 4 + u64::from(self.config.size) * 2;
            let used_event_addr = self
                .config
                .avail_addr
                .checked_add(used_event_offset)
                .ok_or(VirtQueueError::GuestMemory(GuestMemoryError::OutOfBounds {
                    addr: self.config.avail_addr,
                    len: 2,
                }))?;
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
