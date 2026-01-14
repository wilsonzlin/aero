use core::mem::offset_of;

use aero_protocol::aerogpu::aerogpu_ring as protocol_ring;
use memory::MemoryBus;

pub use protocol_ring::{
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC,
    AEROGPU_SUBMIT_FLAG_NO_IRQ, AEROGPU_SUBMIT_FLAG_PRESENT,
};

pub const AEROGPU_RING_HEADER_SIZE_BYTES: u64 = protocol_ring::AerogpuRingHeader::SIZE_BYTES as u64;
pub const AEROGPU_FENCE_PAGE_SIZE_BYTES: u64 = protocol_ring::AerogpuFencePage::SIZE_BYTES as u64;

pub const AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES: u32 =
    protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32;

pub const RING_MAGIC_OFFSET: u64 = offset_of!(protocol_ring::AerogpuRingHeader, magic) as u64;
pub const RING_ABI_VERSION_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuRingHeader, abi_version) as u64;
pub const RING_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuRingHeader, size_bytes) as u64;
pub const RING_ENTRY_COUNT_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuRingHeader, entry_count) as u64;
pub const RING_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuRingHeader, entry_stride_bytes) as u64;
pub const RING_FLAGS_OFFSET: u64 = offset_of!(protocol_ring::AerogpuRingHeader, flags) as u64;
pub const RING_HEAD_OFFSET: u64 = offset_of!(protocol_ring::AerogpuRingHeader, head) as u64;
pub const RING_TAIL_OFFSET: u64 = offset_of!(protocol_ring::AerogpuRingHeader, tail) as u64;

pub const SUBMIT_DESC_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, desc_size_bytes) as u64;
pub const SUBMIT_DESC_FLAGS_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, flags) as u64;
pub const SUBMIT_DESC_CONTEXT_ID_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, context_id) as u64;
pub const SUBMIT_DESC_ENGINE_ID_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, engine_id) as u64;
pub const SUBMIT_DESC_CMD_GPA_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, cmd_gpa) as u64;
pub const SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, cmd_size_bytes) as u64;
pub const SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, alloc_table_gpa) as u64;
pub const SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, alloc_table_size_bytes) as u64;
pub const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuSubmitDesc, signal_fence) as u64;

pub const ALLOC_TABLE_MAGIC_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuAllocTableHeader, magic) as u64;
pub const ALLOC_TABLE_ABI_VERSION_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuAllocTableHeader, abi_version) as u64;
pub const ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuAllocTableHeader, size_bytes) as u64;
pub const ALLOC_TABLE_ENTRY_COUNT_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuAllocTableHeader, entry_count) as u64;
pub const ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuAllocTableHeader, entry_stride_bytes) as u64;

pub const ALLOC_ENTRY_ALLOC_ID_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuAllocEntry, alloc_id) as u64;
pub const ALLOC_ENTRY_FLAGS_OFFSET: u64 = offset_of!(protocol_ring::AerogpuAllocEntry, flags) as u64;
pub const ALLOC_ENTRY_GPA_OFFSET: u64 = offset_of!(protocol_ring::AerogpuAllocEntry, gpa) as u64;
pub const ALLOC_ENTRY_SIZE_BYTES_OFFSET: u64 =
    offset_of!(protocol_ring::AerogpuAllocEntry, size_bytes) as u64;

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
    use aero_protocol::aerogpu::aerogpu_pci::{AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR};

    fn make_valid_header_with_abi(abi_version: u32) -> AeroGpuRingHeader {
        let entry_count = 8;
        let entry_stride_bytes = AeroGpuSubmitDesc::SIZE_BYTES;
        let size_bytes = (AEROGPU_RING_HEADER_SIZE_BYTES
            + (entry_count as u64 * entry_stride_bytes as u64)) as u32;

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

    #[derive(Clone, Debug)]
    struct VecMemory {
        data: Vec<u8>,
    }

    impl VecMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }
    }

    impl MemoryBus for VecMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = paddr as usize;
            let end = start + buf.len();
            buf.copy_from_slice(&self.data[start..end]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = paddr as usize;
            let end = start + buf.len();
            self.data[start..end].copy_from_slice(buf);
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

    #[test]
    fn ring_header_validation_accepts_mmio_size_larger_than_declared_size() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        let hdr = make_valid_header_with_abi(abi_version);

        // Forward-compat: the MMIO-programmed ring mapping may be larger than the ring header's
        // declared size (e.g. page rounding / extension space).
        assert!(hdr.is_valid(hdr.size_bytes + 4096));
    }

    #[test]
    fn ring_header_validation_rejects_declared_size_exceeding_mmio_size() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        let hdr = make_valid_header_with_abi(abi_version);

        assert!(!hdr.is_valid(hdr.size_bytes - 1));
    }

    #[test]
    fn ring_header_validation_checks_magic_and_abi_version() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        let mut hdr = make_valid_header_with_abi(abi_version);

        hdr.magic = 0;
        assert!(
            !hdr.is_valid(hdr.size_bytes),
            "wrong magic must be rejected"
        );

        hdr.magic = AEROGPU_RING_MAGIC;
        hdr.abi_version = 0;
        assert!(
            !hdr.is_valid(hdr.size_bytes),
            "wrong ABI version must be rejected"
        );

        hdr.abi_version = abi_version;
        assert!(hdr.is_valid(hdr.size_bytes));
    }

    #[test]
    fn ring_header_validation_rejects_bad_entry_count() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;

        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.entry_count = 0;
        assert!(!hdr.is_valid(0xFFFF));

        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.entry_count = 3; // not a power-of-two
                             // size_bytes must be >= required for validate_prefix to reach BadEntryCount.
        hdr.size_bytes = (AEROGPU_RING_HEADER_SIZE_BYTES
            + hdr.entry_count as u64 * hdr.entry_stride_bytes as u64)
            as u32;
        assert!(!hdr.is_valid(0xFFFF));
    }

    #[test]
    fn ring_header_validation_rejects_bad_stride_and_size() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;

        // Stride too small.
        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.entry_stride_bytes = AeroGpuSubmitDesc::SIZE_BYTES - 1;
        // size_bytes must be >= required for validate_prefix to reach BadStrideField.
        hdr.size_bytes = (AEROGPU_RING_HEADER_SIZE_BYTES
            + hdr.entry_count as u64 * hdr.entry_stride_bytes as u64)
            as u32;
        assert!(!hdr.is_valid(0xFFFF));

        // size_bytes too small for declared entry_count/stride.
        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.size_bytes = hdr.size_bytes - 1;
        assert!(!hdr.is_valid(0xFFFF));
    }

    #[test]
    fn ring_abi_matches_c_header() {
        use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

        assert_eq!(AEROGPU_RING_MAGIC, 0x474E_5241);
        assert_eq!(AEROGPU_ABI_VERSION_U32, 0x0001_0003);

        assert_eq!(AEROGPU_RING_HEADER_SIZE_BYTES, 64);
        assert_eq!(RING_MAGIC_OFFSET, 0);
        assert_eq!(RING_ABI_VERSION_OFFSET, 4);
        assert_eq!(RING_SIZE_BYTES_OFFSET, 8);
        assert_eq!(RING_ENTRY_COUNT_OFFSET, 12);
        assert_eq!(RING_ENTRY_STRIDE_BYTES_OFFSET, 16);
        assert_eq!(RING_FLAGS_OFFSET, 20);
        assert_eq!(RING_HEAD_OFFSET, 24);
        assert_eq!(RING_TAIL_OFFSET, 28);

        assert_eq!(AEROGPU_ALLOC_TABLE_MAGIC, 0x434F_4C41);
        assert_eq!(AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES, 24);
        assert_eq!(ALLOC_TABLE_MAGIC_OFFSET, 0);
        assert_eq!(ALLOC_TABLE_ABI_VERSION_OFFSET, 4);
        assert_eq!(ALLOC_TABLE_SIZE_BYTES_OFFSET, 8);
        assert_eq!(ALLOC_TABLE_ENTRY_COUNT_OFFSET, 12);
        assert_eq!(ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET, 16);

        assert_eq!(SUBMIT_DESC_SIZE_BYTES_OFFSET, 0);
        assert_eq!(SUBMIT_DESC_FLAGS_OFFSET, 4);
        assert_eq!(SUBMIT_DESC_CONTEXT_ID_OFFSET, 8);
        assert_eq!(SUBMIT_DESC_ENGINE_ID_OFFSET, 12);
        assert_eq!(SUBMIT_DESC_CMD_GPA_OFFSET, 16);
        assert_eq!(SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 24);
        assert_eq!(SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 32);
        assert_eq!(SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 40);
        assert_eq!(SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 48);

        assert_eq!(ALLOC_ENTRY_ALLOC_ID_OFFSET, 0);
        assert_eq!(ALLOC_ENTRY_FLAGS_OFFSET, 4);
        assert_eq!(ALLOC_ENTRY_GPA_OFFSET, 8);
        assert_eq!(ALLOC_ENTRY_SIZE_BYTES_OFFSET, 16);

        assert_eq!(AEROGPU_FENCE_PAGE_MAGIC, 0x434E_4546);
        assert_eq!(AEROGPU_FENCE_PAGE_SIZE_BYTES, 56);
        assert_eq!(FENCE_PAGE_MAGIC_OFFSET, 0);
        assert_eq!(FENCE_PAGE_ABI_VERSION_OFFSET, 4);
        assert_eq!(FENCE_PAGE_COMPLETED_FENCE_OFFSET, 8);
    }

    #[test]
    fn slot_index_wraps_by_entry_count() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        let hdr = make_valid_header_with_abi(abi_version);

        assert_eq!(hdr.slot_index(0), 0);
        assert_eq!(hdr.slot_index(hdr.entry_count - 1), hdr.entry_count - 1);
        assert_eq!(hdr.slot_index(hdr.entry_count), 0);
        assert_eq!(hdr.slot_index(hdr.entry_count + 1), 1);
    }

    #[test]
    fn write_head_writes_at_ring_head_offset() {
        let mut mem = VecMemory::new(0x1000);
        let ring_gpa = 0x100u64;
        AeroGpuRingHeader::write_head(&mut mem, ring_gpa, 0xDEAD_BEEF);
        assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0xDEAD_BEEF);
    }

    #[test]
    fn write_fence_page_writes_expected_fields() {
        let mut mem = VecMemory::new(0x1000);
        let fence_gpa = 0x200u64;
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        write_fence_page(&mut mem, fence_gpa, abi_version, 123);

        assert_eq!(
            mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
            AEROGPU_FENCE_PAGE_MAGIC
        );
        assert_eq!(
            mem.read_u32(fence_gpa + FENCE_PAGE_ABI_VERSION_OFFSET),
            abi_version
        );
        assert_eq!(
            mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
            123
        );
    }

    #[test]
    fn submit_desc_validate_prefix_rejects_too_small_size() {
        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: 0,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            cmd_gpa: 0,
            cmd_size_bytes: 0,
            alloc_table_gpa: 0,
            alloc_table_size_bytes: 0,
            signal_fence: 0,
        };

        assert!(matches!(
            desc.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadSizeField { .. })
        ));
    }

    #[test]
    fn ring_header_validate_prefix_reports_expected_errors() {
        use aero_protocol::aerogpu::aerogpu_pci::AerogpuAbiError;

        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;

        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.magic = 0;
        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadMagic { .. })
        ));

        let hdr = make_valid_header_with_abi(((AEROGPU_ABI_MAJOR + 1) << 16) | AEROGPU_ABI_MINOR);
        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::Abi(AerogpuAbiError::UnsupportedMajor { .. }))
        ));

        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.entry_count = 3;
        hdr.size_bytes = (AEROGPU_RING_HEADER_SIZE_BYTES
            + hdr.entry_count as u64 * hdr.entry_stride_bytes as u64) as u32;
        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadEntryCount { found: 3 })
        ));

        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.entry_stride_bytes = AeroGpuSubmitDesc::SIZE_BYTES - 1;
        hdr.size_bytes = (AEROGPU_RING_HEADER_SIZE_BYTES
            + hdr.entry_count as u64 * hdr.entry_stride_bytes as u64) as u32;
        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadStrideField { .. })
        ));

        let mut hdr = make_valid_header_with_abi(abi_version);
        hdr.size_bytes -= 1;
        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadSizeField { .. })
        ));
    }

    #[test]
    fn alloc_table_header_validate_prefix_rejects_wrong_magic() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        let entry_count = 1u32;
        let entry_stride = AeroGpuAllocEntry::SIZE_BYTES;
        let size_bytes = (protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32)
            + entry_count * entry_stride;
        let hdr = AeroGpuAllocTableHeader {
            magic: 0,
            abi_version,
            size_bytes,
            entry_count,
            entry_stride_bytes: entry_stride,
            reserved0: 0,
        };

        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadMagic { .. })
        ));
    }

    #[test]
    fn alloc_table_header_validate_prefix_rejects_bad_stride_and_size() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        let entry_count = 1u32;

        // Stride too small.
        let hdr = AeroGpuAllocTableHeader {
            magic: AEROGPU_ALLOC_TABLE_MAGIC,
            abi_version,
            size_bytes: protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32,
            entry_count,
            entry_stride_bytes: 1,
            reserved0: 0,
        };
        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadStrideField { .. })
        ));

        // size_bytes too small for declared entry_count/stride.
        let entry_stride = AeroGpuAllocEntry::SIZE_BYTES;
        let hdr = AeroGpuAllocTableHeader {
            magic: AEROGPU_ALLOC_TABLE_MAGIC,
            abi_version,
            size_bytes: protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32,
            entry_count,
            entry_stride_bytes: entry_stride,
            reserved0: 0,
        };
        assert!(matches!(
            hdr.validate_prefix(),
            Err(protocol_ring::AerogpuRingDecodeError::BadSizeField { .. })
        ));
    }

    #[test]
    fn read_from_decodes_ring_and_submit_and_alloc_structs() {
        let mut mem = VecMemory::new(0x2000);

        // Ring header.
        let ring_gpa = 0x100u64;
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;
        let entry_count = 8u32;
        let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
        let ring_size_bytes = (AEROGPU_RING_HEADER_SIZE_BYTES
            + entry_count as u64 * entry_stride as u64) as u32;

        mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
        mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, abi_version);
        mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size_bytes);
        mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
        mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
        mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0xAABB_CCDD);
        mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 5);
        mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 6);

        let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
        assert_eq!(ring.magic, AEROGPU_RING_MAGIC);
        assert_eq!(ring.abi_version, abi_version);
        assert_eq!(ring.size_bytes, ring_size_bytes);
        assert_eq!(ring.entry_count, entry_count);
        assert_eq!(ring.entry_stride_bytes, entry_stride);
        assert_eq!(ring.flags, 0xAABB_CCDD);
        assert_eq!(ring.head, 5);
        assert_eq!(ring.tail, 6);

        // Submit descriptor.
        let desc_gpa = 0x200u64;
        mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES);
        mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, AeroGpuSubmitDesc::FLAG_PRESENT);
        mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 123);
        mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 456);
        mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0xDEAD_BEEFu64);
        mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 0x1000);
        mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0xCAFE_BABEu64);
        mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0x2000);
        mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 0x1122_3344_5566_7788u64);

        let desc = AeroGpuSubmitDesc::read_from(&mut mem, desc_gpa);
        assert_eq!(desc.desc_size_bytes, AeroGpuSubmitDesc::SIZE_BYTES);
        assert_eq!(desc.flags, AeroGpuSubmitDesc::FLAG_PRESENT);
        assert_eq!(desc.context_id, 123);
        assert_eq!(desc.engine_id, 456);
        assert_eq!(desc.cmd_gpa, 0xDEAD_BEEFu64);
        assert_eq!(desc.cmd_size_bytes, 0x1000);
        assert_eq!(desc.alloc_table_gpa, 0xCAFE_BABEu64);
        assert_eq!(desc.alloc_table_size_bytes, 0x2000);
        assert_eq!(desc.signal_fence, 0x1122_3344_5566_7788u64);

        // Alloc table header.
        let alloc_hdr_gpa = 0x300u64;
        let alloc_entry_count = 1u32;
        let alloc_stride = AeroGpuAllocEntry::SIZE_BYTES;
        let alloc_size_bytes =
            (protocol_ring::AerogpuAllocTableHeader::SIZE_BYTES as u32) + alloc_entry_count * alloc_stride;
        mem.write_u32(alloc_hdr_gpa + ALLOC_TABLE_MAGIC_OFFSET, AEROGPU_ALLOC_TABLE_MAGIC);
        mem.write_u32(alloc_hdr_gpa + ALLOC_TABLE_ABI_VERSION_OFFSET, abi_version);
        mem.write_u32(alloc_hdr_gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET, alloc_size_bytes);
        mem.write_u32(alloc_hdr_gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET, alloc_entry_count);
        mem.write_u32(alloc_hdr_gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET, alloc_stride);
        mem.write_u32(alloc_hdr_gpa + 20, 0);

        let alloc_hdr = AeroGpuAllocTableHeader::read_from(&mut mem, alloc_hdr_gpa);
        assert_eq!(alloc_hdr.magic, AEROGPU_ALLOC_TABLE_MAGIC);
        assert_eq!(alloc_hdr.abi_version, abi_version);
        assert_eq!(alloc_hdr.size_bytes, alloc_size_bytes);
        assert_eq!(alloc_hdr.entry_count, alloc_entry_count);
        assert_eq!(alloc_hdr.entry_stride_bytes, alloc_stride);

        // Alloc entry.
        let alloc_entry_gpa = 0x400u64;
        mem.write_u32(alloc_entry_gpa + ALLOC_ENTRY_ALLOC_ID_OFFSET, 7);
        mem.write_u32(alloc_entry_gpa + ALLOC_ENTRY_FLAGS_OFFSET, 0x55AA);
        mem.write_u64(alloc_entry_gpa + ALLOC_ENTRY_GPA_OFFSET, 0x1000_0000);
        mem.write_u64(alloc_entry_gpa + ALLOC_ENTRY_SIZE_BYTES_OFFSET, 0x2000);

        let entry = AeroGpuAllocEntry::read_from(&mut mem, alloc_entry_gpa);
        assert_eq!(entry.alloc_id, 7);
        assert_eq!(entry.flags, 0x55AA);
        assert_eq!(entry.gpa, 0x1000_0000);
        assert_eq!(entry.size_bytes, 0x2000);
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
        let entry = protocol_ring::AerogpuAllocEntry::decode_from_le_bytes(&buf)
            .expect("buffer matches SIZE_BYTES");

        Self {
            alloc_id: entry.alloc_id,
            flags: entry.flags,
            gpa: entry.gpa,
            size_bytes: entry.size_bytes,
        }
    }
}

pub fn write_fence_page(
    mem: &mut dyn memory::MemoryBus,
    gpa: u64,
    abi_version: u32,
    completed_fence: u64,
) {
    debug_assert!(FENCE_PAGE_MAGIC_OFFSET + 4 <= AEROGPU_FENCE_PAGE_SIZE_BYTES);
    debug_assert!(FENCE_PAGE_ABI_VERSION_OFFSET + 4 <= AEROGPU_FENCE_PAGE_SIZE_BYTES);
    debug_assert!(FENCE_PAGE_COMPLETED_FENCE_OFFSET + 8 <= AEROGPU_FENCE_PAGE_SIZE_BYTES);

    mem.write_u32(gpa + FENCE_PAGE_MAGIC_OFFSET, AEROGPU_FENCE_PAGE_MAGIC);
    mem.write_u32(gpa + FENCE_PAGE_ABI_VERSION_OFFSET, abi_version);
    mem.write_u64(gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET, completed_fence);

    // Keep writes within the defined struct size; do not touch the rest of the page.
    let _ = AEROGPU_FENCE_PAGE_SIZE_BYTES;
}
