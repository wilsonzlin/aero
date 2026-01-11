use memory::MemoryBus;

use aero_protocol::aerogpu::aerogpu_pci::parse_and_validate_abi_version_u32;

// Constants mirrored from `drivers/aerogpu/protocol/aerogpu_ring.h`.

pub const AEROGPU_RING_MAGIC: u32 = 0x474E_5241; // "ARNG" little-endian
pub const AEROGPU_RING_HEADER_SIZE_BYTES: u64 = 64;

pub const AEROGPU_ALLOC_TABLE_MAGIC: u32 = 0x434F_4C41; // "ALOC"
pub const AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES: u32 = 24;

pub const AEROGPU_FENCE_PAGE_MAGIC: u32 = 0x434E_4546; // "FENC"
pub const AEROGPU_FENCE_PAGE_SIZE_BYTES: u64 = 56;

pub const RING_HEAD_OFFSET: u64 = 24;
pub const RING_TAIL_OFFSET: u64 = 28;

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
        let magic = mem.read_u32(gpa + 0);
        let abi_version = mem.read_u32(gpa + 4);
        let size_bytes = mem.read_u32(gpa + 8);
        let entry_count = mem.read_u32(gpa + 12);
        let entry_stride_bytes = mem.read_u32(gpa + 16);
        let flags = mem.read_u32(gpa + 20);
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
        if parse_and_validate_abi_version_u32(self.abi_version).is_err() {
            return false;
        }
        if self.entry_count == 0 || !self.entry_count.is_power_of_two() {
            return false;
        }
        if self.entry_stride_bytes == 0 {
            return false;
        }
        if self.entry_stride_bytes < AeroGpuSubmitDesc::SIZE_BYTES {
            return false;
        }
        let required =
            match u64::from(self.entry_count).checked_mul(u64::from(self.entry_stride_bytes)) {
                Some(bytes) => match AEROGPU_RING_HEADER_SIZE_BYTES.checked_add(bytes) {
                    Some(total) => total,
                    None => return false,
                },
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
    pub const SIZE_BYTES: u32 = 64;

    pub const FLAG_PRESENT: u32 = 1 << 0;
    pub const FLAG_NO_IRQ: u32 = 1 << 1;

    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let desc_size_bytes = mem.read_u32(gpa + 0);
        let flags = mem.read_u32(gpa + 4);
        let context_id = mem.read_u32(gpa + 8);
        let engine_id = mem.read_u32(gpa + 12);
        let cmd_gpa = mem.read_u64(gpa + 16);
        let cmd_size_bytes = mem.read_u32(gpa + 24);
        let alloc_table_gpa = mem.read_u64(gpa + 32);
        let alloc_table_size_bytes = mem.read_u32(gpa + 40);
        let signal_fence = mem.read_u64(gpa + 48);

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
        let magic = mem.read_u32(gpa + 0);
        let abi_version = mem.read_u32(gpa + 4);
        let size_bytes = mem.read_u32(gpa + 8);
        let entry_count = mem.read_u32(gpa + 12);
        let entry_stride_bytes = mem.read_u32(gpa + 16);
        let reserved0 = mem.read_u32(gpa + 20);

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
    pub const SIZE_BYTES: u32 = 32;

    pub fn read_from(mem: &mut dyn MemoryBus, gpa: u64) -> Self {
        let alloc_id = mem.read_u32(gpa + 0);
        let flags = mem.read_u32(gpa + 4);
        let gpa_val = mem.read_u64(gpa + 8);
        let size_bytes = mem.read_u64(gpa + 16);
        let _reserved0 = mem.read_u64(gpa + 24);

        Self {
            alloc_id,
            flags,
            gpa: gpa_val,
            size_bytes,
        }
    }
}
