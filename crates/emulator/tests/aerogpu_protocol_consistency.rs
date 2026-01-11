use aero_protocol::aerogpu::aerogpu_ring;

use emulator::devices::aerogpu_ring as emu_ring;

#[test]
fn a3a0_protocol_constants_match_aero_protocol_crate() {
    assert_eq!(
        emu_ring::AEROGPU_ALLOC_TABLE_MAGIC,
        aerogpu_ring::AEROGPU_ALLOC_TABLE_MAGIC
    );
    assert_eq!(
        emu_ring::AEROGPU_RING_MAGIC,
        aerogpu_ring::AEROGPU_RING_MAGIC
    );
    assert_eq!(
        emu_ring::AEROGPU_FENCE_PAGE_MAGIC,
        aerogpu_ring::AEROGPU_FENCE_PAGE_MAGIC
    );

    assert_eq!(
        emu_ring::AeroGpuSubmitDesc::SIZE_BYTES as usize,
        aerogpu_ring::AerogpuSubmitDesc::SIZE_BYTES
    );
    assert_eq!(
        emu_ring::AeroGpuSubmitDesc::FLAG_PRESENT,
        aerogpu_ring::AEROGPU_SUBMIT_FLAG_PRESENT
    );
    assert_eq!(
        emu_ring::AeroGpuSubmitDesc::FLAG_NO_IRQ,
        aerogpu_ring::AEROGPU_SUBMIT_FLAG_NO_IRQ
    );

    assert_eq!(
        emu_ring::AEROGPU_RING_HEADER_SIZE_BYTES as usize,
        aerogpu_ring::AerogpuRingHeader::SIZE_BYTES
    );
    assert_eq!(
        emu_ring::AEROGPU_FENCE_PAGE_SIZE_BYTES as usize,
        aerogpu_ring::AerogpuFencePage::SIZE_BYTES
    );

    assert_eq!(
        emu_ring::RING_HEAD_OFFSET as usize,
        core::mem::offset_of!(aerogpu_ring::AerogpuRingHeader, head)
    );
    assert_eq!(
        emu_ring::RING_TAIL_OFFSET as usize,
        core::mem::offset_of!(aerogpu_ring::AerogpuRingHeader, tail)
    );
    assert_eq!(
        emu_ring::FENCE_PAGE_MAGIC_OFFSET as usize,
        core::mem::offset_of!(aerogpu_ring::AerogpuFencePage, magic)
    );
    assert_eq!(
        emu_ring::FENCE_PAGE_ABI_VERSION_OFFSET as usize,
        core::mem::offset_of!(aerogpu_ring::AerogpuFencePage, abi_version)
    );
    assert_eq!(
        emu_ring::FENCE_PAGE_COMPLETED_FENCE_OFFSET as usize,
        core::mem::offset_of!(aerogpu_ring::AerogpuFencePage, completed_fence)
    );
}
