use core::mem::offset_of;

use aero_protocol::aerogpu::aerogpu_ring as protocol_ring;
use memory::MemoryBus;

pub use protocol_ring::{AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC};

pub const AEROGPU_RING_HEADER_SIZE_BYTES: u64 = protocol_ring::AerogpuRingHeader::SIZE_BYTES as u64;
pub const AEROGPU_FENCE_PAGE_SIZE_BYTES: u64 = protocol_ring::AerogpuFencePage::SIZE_BYTES as u64;

pub const AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES: u32 =
    protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32;

pub const AEROGPU_SUBMIT_FLAG_PRESENT: u32 = protocol_ring::AEROGPU_SUBMIT_FLAG_PRESENT;
pub const AEROGPU_SUBMIT_FLAG_NO_IRQ: u32 = protocol_ring::AEROGPU_SUBMIT_FLAG_NO_IRQ;

pub const RING_HEAD_OFFSET: u64 = offset_of!(protocol_ring::AerogpuRingHeader, head) as u64;
pub const RING_TAIL_OFFSET: u64 = offset_of!(protocol_ring::AerogpuRingHeader, tail) as u64;

pub const FENCE_PAGE_MAGIC_OFFSET: u64 = offset_of!(protocol_ring::AerogpuFencePage, magic) as u64;
pub const FENCE_PAGE_ABI_VERSION_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuFencePage, abi_version) as u64;
pub const FENCE_PAGE_COMPLETED_FENCE_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuFencePage, completed_fence) as u64;

#[derive(Clone, Debug)]
pub struct AeroGpuRingHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub entry_count: u32,
    pub entry_stride_bytes: u32,
    pub flags: u32,
    pub head: u32,
    pub tail: u32,
}

impl AeroGpuRingHeader {
    pub const SIZE_BYTES: u32 = protocol_ring::AerogpuRingHeader::SIZE_BYTES as u32;

    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let mut buf = [0u8; protocol_ring::AerogpuRingHeader::SIZE_BYTES];
        mem.read_physical(gpa, &mut buf);
        let hdr = protocol_ring::AerogpuRingHeader::decode_from_le_bytes(&buf)
            .expect("buffer matches AerogpuRingHeader::SIZE_BYTES");

        Self {
            magic: hdr.magic,
            abi_version: hdr.abi_version,
            size_bytes: hdr.size_bytes,
            entry_count: hdr.entry_count,
            entry_stride_bytes: hdr.entry_stride_bytes,
            flags: hdr.flags,
            head: hdr.head,
            tail: hdr.tail,
        }
    }

    pub fn validate_prefix(&self) -> Result<(), protocol_ring::AerogpuRingDecodeError> {
        let hdr = protocol_ring::AerogpuRingHeader {
            magic: self.magic,
            abi_version: self.abi_version,
            size_bytes: self.size_bytes,
            entry_count: self.entry_count,
            entry_stride_bytes: self.entry_stride_bytes,
            flags: self.flags,
            head: self.head,
            tail: self.tail,
            reserved0: 0,
            reserved1: 0,
            reserved2: [0; 3],
        };

        hdr.validate_prefix()
    }

    pub fn write_head(mem: &mut dyn MemoryBus, gpa: u64, head: u32) {
        mem.write_u32(gpa + RING_HEAD_OFFSET, head);
    }

    pub fn slot_index(&self, index: u32) -> u32 {
        // entry_count is validated as a power-of-two.
        index & (self.entry_count - 1)
    }

    pub fn is_valid(&self, mmio_ring_size_bytes: u32) -> bool {
        if self.validate_prefix().is_err() {
            return false;
        }

        // Caller has an MMIO mapping of size `mmio_ring_size_bytes`; the guest-declared ring size
        // must not exceed it.
        u64::from(self.size_bytes) <= u64::from(mmio_ring_size_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::aerogpu_regs::{AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR};

    fn make_valid_header_with_abi(abi_version: u32) -> AeroGpuRingHeader {
        let entry_count = 8;
        let entry_stride_bytes = AeroGpuSubmitDesc::SIZE_BYTES;
        let size_bytes =
            (AEROGPU_RING_HEADER_SIZE_BYTES + (entry_count as u64 * entry_stride_bytes as u64)) as u32;

        AeroGpuRingHeader {
            magic: AEROGPU_RING_MAGIC,
            abi_version,
            size_bytes,
            entry_count,
            entry_stride_bytes,
            flags: 0,
            head: 0,
            tail: 0,
        }
    }

    #[test]
    fn ring_header_validation_accepts_unknown_minor() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 999);
        let hdr = make_valid_header_with_abi(abi_version);
        assert!(hdr.is_valid(hdr.size_bytes));
    }

    #[test]
    fn ring_header_validation_rejects_unknown_major() {
        let abi_version = ((AEROGPU_ABI_MAJOR + 1) << 16) | AEROGPU_ABI_MINOR;
        let hdr = make_valid_header_with_abi(abi_version);
        assert!(!hdr.is_valid(hdr.size_bytes));
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuSubmitDesc {
    pub desc_size_bytes: u32,
    pub flags: u32,
    pub context_id: u32,
    pub engine_id: u32,
    pub cmd_gpa: u64,
    pub cmd_size_bytes: u32,
    pub alloc_table_gpa: u64,
    pub alloc_table_size_bytes: u32,
    pub signal_fence: u64,
}

impl AeroGpuSubmitDesc {
    pub const SIZE_BYTES: u32 = protocol_ring::AerogpuSubmitDesc::SIZE_BYTES as u32;

    pub const FLAG_PRESENT: u32 = protocol_ring::AEROGPU_SUBMIT_FLAG_PRESENT;
    pub const FLAG_NO_IRQ: u32 = protocol_ring::AEROGPU_SUBMIT_FLAG_NO_IRQ;

    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let mut buf = [0u8; protocol_ring::AerogpuSubmitDesc::SIZE_BYTES];
        mem.read_physical(gpa, &mut buf);
        let desc = protocol_ring::AerogpuSubmitDesc::decode_from_le_bytes(&buf)
            .expect("buffer matches AerogpuSubmitDesc::SIZE_BYTES");

        Self {
            desc_size_bytes: desc.desc_size_bytes,
            flags: desc.flags,
            context_id: desc.context_id,
            engine_id: desc.engine_id,
            cmd_gpa: desc.cmd_gpa,
            cmd_size_bytes: desc.cmd_size_bytes,
            alloc_table_gpa: desc.alloc_table_gpa,
            alloc_table_size_bytes: desc.alloc_table_size_bytes,
            signal_fence: desc.signal_fence,
        }
    }

    pub fn validate_prefix(&self) -> Result<(), protocol_ring::AerogpuRingDecodeError> {
        let desc = protocol_ring::AerogpuSubmitDesc {
            desc_size_bytes: self.desc_size_bytes,
            flags: self.flags,
            context_id: self.context_id,
            engine_id: self.engine_id,
            cmd_gpa: self.cmd_gpa,
            cmd_size_bytes: self.cmd_size_bytes,
            cmd_reserved0: 0,
            alloc_table_gpa: self.alloc_table_gpa,
            alloc_table_size_bytes: self.alloc_table_size_bytes,
            alloc_table_reserved0: 0,
            signal_fence: self.signal_fence,
            reserved0: 0,
        };

        desc.validate_prefix()
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuAllocTableHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub entry_count: u32,
    pub entry_stride_bytes: u32,
    pub reserved0: u32,
}

impl AeroGpuAllocTableHeader {
    pub const SIZE_BYTES: u32 = protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32;

    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let mut buf = [0u8; protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES];
        mem.read_physical(gpa, &mut buf);
        let hdr = protocol_ring::AerogpuAllocTableHeader::decode_from_le_bytes(&buf)
            .expect("buffer matches AerogpuAllocTableHeader::SIZE_BYTES");

        Self {
            magic: hdr.magic,
            abi_version: hdr.abi_version,
            size_bytes: hdr.size_bytes,
            entry_count: hdr.entry_count,
            entry_stride_bytes: hdr.entry_stride_bytes,
            reserved0: hdr.reserved0,
        }
    }

    pub fn validate_prefix(&self) -> Result<(), protocol_ring::AerogpuRingDecodeError> {
        let hdr = protocol_ring::AerogpuAllocTableHeader {
            magic: self.magic,
            abi_version: self.abi_version,
            size_bytes: self.size_bytes,
            entry_count: self.entry_count,
            entry_stride_bytes: self.entry_stride_bytes,
            reserved0: self.reserved0,
        };

        hdr.validate_prefix()
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuAllocEntry {
    pub alloc_id: u32,
    pub flags: u32,
    pub gpa: u64,
    pub size_bytes: u64,
}

impl AeroGpuAllocEntry {
    pub const SIZE_BYTES: u32 = protocol_ring::AerogpuAllocEntry::SIZE_BYTES as u32;

    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let mut buf = [0u8; protocol_ring::AerogpuAllocEntry::SIZE_BYTES];
        mem.read_physical(gpa, &mut buf);
        let entry =
            protocol_ring::AerogpuAllocEntry::decode_from_le_bytes(&buf).expect("buffer matches SIZE_BYTES");

        Self {
            alloc_id: entry.alloc_id,
            flags: entry.flags,
            gpa: entry.gpa,
            size_bytes: entry.size_bytes,
        }
    }
}

pub fn write_fence_page(mem: &mut dyn MemoryBus, gpa: u64, abi_version: u32, completed_fence: u64) {
    mem.write_u32(gpa + FENCE_PAGE_MAGIC_OFFSET, AEROGPU_FENCE_PAGE_MAGIC);
    mem.write_u32(gpa + FENCE_PAGE_ABI_VERSION_OFFSET, abi_version);
    mem.write_u64(gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET, completed_fence);

    // Keep writes within the defined struct size; do not touch the rest of the page.
    let _ = AEROGPU_FENCE_PAGE_SIZE_BYTES;
}
