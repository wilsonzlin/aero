pub use aero_devices_gpu::ring::{
    write_fence_page, AeroGpuAllocEntry, AeroGpuAllocTableHeader, AeroGpuRingHeader,
    AeroGpuSubmitDesc, AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES, AEROGPU_ALLOC_TABLE_MAGIC,
    AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_FENCE_PAGE_SIZE_BYTES, AEROGPU_RING_HEADER_SIZE_BYTES,
    AEROGPU_RING_MAGIC, AEROGPU_SUBMIT_FLAG_NO_IRQ, AEROGPU_SUBMIT_FLAG_PRESENT,
    FENCE_PAGE_ABI_VERSION_OFFSET, FENCE_PAGE_COMPLETED_FENCE_OFFSET, FENCE_PAGE_MAGIC_OFFSET,
    RING_HEAD_OFFSET, RING_TAIL_OFFSET,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::aerogpu_regs::{AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR};

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

    #[test]
    fn ring_header_validation_accepts_unknown_minor() {
        let abi_version = (AEROGPU_ABI_MAJOR << 16) | (AEROGPU_ABI_MINOR + 1);
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
}
