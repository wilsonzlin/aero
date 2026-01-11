use aero_gpu::aerogpu_executor::AllocTable;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuAllocEntry as ProtocolAllocEntry, AerogpuAllocTableHeader as ProtocolAllocTableHeader,
    AEROGPU_ALLOC_TABLE_MAGIC,
};

const ALLOC_TABLE_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolAllocTableHeader, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[test]
fn alloc_table_capacity_bytes_may_exceed_header_size_bytes() {
    let mut guest = VecGuestMemory::new(0x10_000);

    let table_gpa = 0x1000u64;
    let alloc_id = 1u32;

    let mut table_bytes = Vec::new();
    // aerogpu_alloc_table_header (24 bytes)
    push_u32(&mut table_bytes, AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut table_bytes, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut table_bytes, 0); // size_bytes (patch later)
    push_u32(&mut table_bytes, 1); // entry_count
    push_u32(&mut table_bytes, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
    push_u32(&mut table_bytes, 0); // reserved0

    // aerogpu_alloc_entry (32 bytes)
    push_u32(&mut table_bytes, alloc_id);
    push_u32(&mut table_bytes, 0); // flags
    push_u64(&mut table_bytes, 0x2000); // gpa
    push_u64(&mut table_bytes, 0x1000); // size_bytes
    push_u64(&mut table_bytes, 0); // reserved0

    let size_bytes = table_bytes.len() as u32;
    table_bytes[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());

    guest
        .write(table_gpa, &table_bytes)
        .expect("write alloc table bytes");

    // Forward-compat: `alloc_table_size_bytes` is the backing buffer size; the header's
    // `size_bytes` is the number of bytes used. Allow the backing buffer to exceed the used bytes
    // (e.g. page rounding / reuse).
    let backing_size_bytes = (16 * 1024 * 1024) + 1;
    let table =
        AllocTable::decode_from_guest_memory(&mut guest, table_gpa, backing_size_bytes).unwrap();
    assert!(table.get(alloc_id).is_some());
}
