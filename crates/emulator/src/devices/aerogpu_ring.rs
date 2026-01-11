use aero_protocol::aerogpu::{aerogpu_pci, aerogpu_ring as protocol_ring};
use memory::MemoryBus;

pub use protocol_ring::{AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC};

pub const AEROGPU_RING_HEADER_SIZE_BYTES: u64 = protocol_ring::AerogpuRingHeader::SIZE_BYTES as u64;
pub const AEROGPU_FENCE_PAGE_SIZE_BYTES: u64 = protocol_ring::AerogpuFencePage::SIZE_BYTES as u64;

pub const RING_HEAD_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuRingHeader, head) as u64;
pub const RING_TAIL_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuRingHeader, tail) as u64;

pub const AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES: u32 =
    protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32;

pub const FENCE_PAGE_MAGIC_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuFencePage, magic) as u64;
pub const FENCE_PAGE_ABI_VERSION_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuFencePage, abi_version) as u64;
pub const FENCE_PAGE_COMPLETED_FENCE_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuFencePage, completed_fence) as u64;

const RING_MAGIC_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuRingHeader, magic) as u64;
const RING_ABI_VERSION_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuRingHeader, abi_version) as u64;
const RING_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuRingHeader, size_bytes) as u64;
const RING_ENTRY_COUNT_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuRingHeader, entry_count) as u64;
const RING_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuRingHeader, entry_stride_bytes) as u64;
const RING_FLAGS_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuRingHeader, flags) as u64;

const SUBMIT_DESC_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, desc_size_bytes) as u64;
const SUBMIT_DESC_FLAGS_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, flags) as u64;
const SUBMIT_DESC_CONTEXT_ID_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, context_id) as u64;
const SUBMIT_DESC_ENGINE_ID_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, engine_id) as u64;
const SUBMIT_DESC_CMD_GPA_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, cmd_gpa) as u64;
const SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, cmd_size_bytes) as u64;
const SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, alloc_table_gpa) as u64;
const SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, alloc_table_size_bytes) as u64;
const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuSubmitDesc, signal_fence) as u64;

const ALLOC_TABLE_MAGIC_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocTableHeader, magic) as u64;
const ALLOC_TABLE_ABI_VERSION_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocTableHeader, abi_version) as u64;
const ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocTableHeader, size_bytes) as u64;
const ALLOC_TABLE_ENTRY_COUNT_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocTableHeader, entry_count) as u64;
const ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocTableHeader, entry_stride_bytes) as u64;
const ALLOC_TABLE_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocTableHeader, reserved0) as u64;

const ALLOC_ENTRY_ALLOC_ID_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocEntry, alloc_id) as u64;
const ALLOC_ENTRY_FLAGS_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuAllocEntry, flags) as u64;
const ALLOC_ENTRY_GPA_OFFSET: u64 = core::mem::offset_of!(protocol_ring::AerogpuAllocEntry, gpa) as u64;
const ALLOC_ENTRY_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocEntry, size_bytes) as u64;
const ALLOC_ENTRY_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(protocol_ring::AerogpuAllocEntry, reserved0) as u64;

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
    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let magic = mem.read_u32(gpa + RING_MAGIC_OFFSET);
        let abi_version = mem.read_u32(gpa + RING_ABI_VERSION_OFFSET);
        let size_bytes = mem.read_u32(gpa + RING_SIZE_BYTES_OFFSET);
        let entry_count = mem.read_u32(gpa + RING_ENTRY_COUNT_OFFSET);
        let entry_stride_bytes = mem.read_u32(gpa + RING_ENTRY_STRIDE_BYTES_OFFSET);
        let flags = mem.read_u32(gpa + RING_FLAGS_OFFSET);
        let head = mem.read_u32(gpa + RING_HEAD_OFFSET);
        let tail = mem.read_u32(gpa + RING_TAIL_OFFSET);

        Self {
            magic,
            abi_version,
            size_bytes,
            entry_count,
            entry_stride_bytes,
            flags,
            head,
            tail,
        }
    }

    pub fn write_head(mem: &mut dyn MemoryBus, gpa: u64, head: u32) {
        mem.write_u32(gpa + RING_HEAD_OFFSET, head);
    }

    pub fn slot_index(&self, index: u32) -> u32 {
        // entry_count is validated as a power-of-two.
        index & (self.entry_count - 1)
    }

    pub fn is_valid(&self, mmio_ring_size_bytes: u32) -> bool {
        if self.magic != AEROGPU_RING_MAGIC {
            return false;
        }
        if aerogpu_pci::parse_and_validate_abi_version_u32(self.abi_version).is_err() {
            return false;
        }
        if self.entry_count == 0 || !self.entry_count.is_power_of_two() {
            return false;
        }
        if self.entry_stride_bytes == 0 || self.entry_stride_bytes < AeroGpuSubmitDesc::SIZE_BYTES {
            return false;
        }
        let required = match u64::from(self.entry_count)
            .checked_mul(u64::from(self.entry_stride_bytes))
            .and_then(|bytes| AEROGPU_RING_HEADER_SIZE_BYTES.checked_add(bytes))
        {
            Some(total) => total,
            None => return false,
        };
        let size_bytes = u64::from(self.size_bytes);
        let mmio_size = u64::from(mmio_ring_size_bytes);
        required <= size_bytes && size_bytes <= mmio_size
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
        let desc_size_bytes = mem.read_u32(gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET);
        let flags = mem.read_u32(gpa + SUBMIT_DESC_FLAGS_OFFSET);
        let context_id = mem.read_u32(gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET);
        let engine_id = mem.read_u32(gpa + SUBMIT_DESC_ENGINE_ID_OFFSET);
        let cmd_gpa = mem.read_u64(gpa + SUBMIT_DESC_CMD_GPA_OFFSET);
        let cmd_size_bytes = mem.read_u32(gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET);
        let alloc_table_gpa = mem.read_u64(gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET);
        let alloc_table_size_bytes = mem.read_u32(gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET);
        let signal_fence = mem.read_u64(gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET);

        Self {
            desc_size_bytes,
            flags,
            context_id,
            engine_id,
            cmd_gpa,
            cmd_size_bytes,
            alloc_table_gpa,
            alloc_table_size_bytes,
            signal_fence,
        }
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
    pub const SIZE_BYTES: u32 = AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES;

    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let magic = mem.read_u32(gpa + ALLOC_TABLE_MAGIC_OFFSET);
        let abi_version = mem.read_u32(gpa + ALLOC_TABLE_ABI_VERSION_OFFSET);
        let size_bytes = mem.read_u32(gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET);
        let entry_count = mem.read_u32(gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET);
        let entry_stride_bytes = mem.read_u32(gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET);
        let reserved0 = mem.read_u32(gpa + ALLOC_TABLE_RESERVED0_OFFSET);

        Self {
            magic,
            abi_version,
            size_bytes,
            entry_count,
            entry_stride_bytes,
            reserved0,
        }
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
        let alloc_id = mem.read_u32(gpa + ALLOC_ENTRY_ALLOC_ID_OFFSET);
        let flags = mem.read_u32(gpa + ALLOC_ENTRY_FLAGS_OFFSET);
        let gpa_val = mem.read_u64(gpa + ALLOC_ENTRY_GPA_OFFSET);
        let size_bytes = mem.read_u64(gpa + ALLOC_ENTRY_SIZE_BYTES_OFFSET);
        let _reserved0 = mem.read_u64(gpa + ALLOC_ENTRY_RESERVED0_OFFSET);

        Self {
            alloc_id,
            flags,
            gpa: gpa_val,
            size_bytes,
        }
    }
}
