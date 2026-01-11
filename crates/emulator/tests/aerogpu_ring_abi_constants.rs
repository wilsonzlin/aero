use aero_protocol::aerogpu::aerogpu_ring as proto;
use emulator::devices::aerogpu_ring as emu;

#[test]
fn aerogpu_ring_constants_match_aero_protocol() {
    // Magic values.
    assert_eq!(emu::AEROGPU_RING_MAGIC, proto::AEROGPU_RING_MAGIC);
    assert_eq!(emu::AEROGPU_ALLOC_TABLE_MAGIC, proto::AEROGPU_ALLOC_TABLE_MAGIC);
    assert_eq!(emu::AEROGPU_FENCE_PAGE_MAGIC, proto::AEROGPU_FENCE_PAGE_MAGIC);

    // Submit flags.
    assert_eq!(emu::AEROGPU_SUBMIT_FLAG_PRESENT, proto::AEROGPU_SUBMIT_FLAG_PRESENT);
    assert_eq!(emu::AEROGPU_SUBMIT_FLAG_NO_IRQ, proto::AEROGPU_SUBMIT_FLAG_NO_IRQ);
    assert_eq!(
        emu::AeroGpuSubmitDesc::FLAG_PRESENT,
        proto::AEROGPU_SUBMIT_FLAG_PRESENT
    );
    assert_eq!(
        emu::AeroGpuSubmitDesc::FLAG_NO_IRQ,
        proto::AEROGPU_SUBMIT_FLAG_NO_IRQ
    );

    // Struct sizes.
    assert_eq!(
        emu::AEROGPU_RING_HEADER_SIZE_BYTES,
        proto::AerogpuRingHeader::SIZE_BYTES as u64
    );
    assert_eq!(
        emu::AeroGpuRingHeader::SIZE_BYTES,
        proto::AerogpuRingHeader::SIZE_BYTES as u32
    );
    assert_eq!(
        emu::AeroGpuSubmitDesc::SIZE_BYTES,
        proto::AerogpuSubmitDesc::SIZE_BYTES as u32
    );
    assert_eq!(
        emu::AeroGpuAllocTableHeader::SIZE_BYTES,
        proto::AerogpuAllocTableHeader::SIZE_BYTES as u32
    );
    assert_eq!(
        emu::AeroGpuAllocEntry::SIZE_BYTES,
        proto::AerogpuAllocEntry::SIZE_BYTES as u32
    );
    assert_eq!(
        emu::AEROGPU_FENCE_PAGE_SIZE_BYTES,
        proto::AerogpuFencePage::SIZE_BYTES as u64
    );
}

