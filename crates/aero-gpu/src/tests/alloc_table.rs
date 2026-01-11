use crate::aerogpu_executor::{AllocTable, ExecutorError};
use crate::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AerogpuAllocTableHeader, AEROGPU_ALLOC_TABLE_MAGIC};

fn build_alloc_table_bytes(
    size_bytes: u32,
    entry_count: u32,
    entry_stride_bytes: u32,
    entries: &[(u32, u64, u64)],
) -> Vec<u8> {
    let mut buf = vec![0u8; size_bytes as usize];

    fn write_u32(buf: &mut [u8], offset: usize, v: u32) {
        buf[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn write_u64(buf: &mut [u8], offset: usize, v: u64) {
        buf[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
    }

    write_u32(&mut buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
    write_u32(&mut buf, 4, AEROGPU_ABI_VERSION_U32);
    write_u32(&mut buf, 8, size_bytes);
    write_u32(&mut buf, 12, entry_count);
    write_u32(&mut buf, 16, entry_stride_bytes);
    write_u32(&mut buf, 20, 0);

    for (i, (alloc_id, gpa, size)) in entries.iter().copied().enumerate() {
        let base =
            AerogpuAllocTableHeader::SIZE_BYTES + i * (entry_stride_bytes as usize);
        write_u32(&mut buf, base + 0, alloc_id);
        write_u32(&mut buf, base + 4, 0); // flags
        write_u64(&mut buf, base + 8, gpa);
        write_u64(&mut buf, base + 16, size);
        write_u64(&mut buf, base + 24, 0);
    }

    buf
}

#[test]
fn alloc_table_duplicate_alloc_id_is_rejected() {
    let gpa = 0x1000u64;
    let entry_stride = AerogpuAllocEntry::SIZE_BYTES as u32;
    let size_bytes = (AerogpuAllocTableHeader::SIZE_BYTES + 2 * AerogpuAllocEntry::SIZE_BYTES) as u32;

    let bytes = build_alloc_table_bytes(
        size_bytes,
        2,
        entry_stride,
        &[
            (1, 0x2000, 0x100),
            // Same alloc_id, different backing address -> collision.
            (1, 0x3000, 0x100),
        ],
    );

    let mem = VecGuestMemory::new(0x4000);
    mem.write(gpa, &bytes).unwrap();

    let err = AllocTable::decode_from_guest_memory(&mem, gpa, size_bytes)
        .expect_err("duplicate alloc_id must be rejected");
    let ExecutorError::Validation(msg) = err else {
        panic!("expected validation error, got {err:?}");
    };
    assert!(
        msg.contains("duplicate alloc_id=1"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn alloc_table_header_stride_too_small_is_rejected() {
    let gpa = 0x1000u64;
    let size_bytes = AerogpuAllocTableHeader::SIZE_BYTES as u32;
    let bytes = build_alloc_table_bytes(size_bytes, 1, 0, &[]);

    let mem = VecGuestMemory::new(0x2000);
    mem.write(gpa, &bytes).unwrap();

    let err =
        AllocTable::decode_from_guest_memory(&mem, gpa, size_bytes).expect_err("must fail");
    let ExecutorError::Validation(msg) = err else {
        panic!("expected validation error, got {err:?}");
    };
    assert!(
        msg.contains("BadStrideField"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn alloc_table_header_size_too_small_for_entry_count_is_rejected() {
    let gpa = 0x1000u64;
    // size_bytes claims only the header, but entry_count expects at least one entry.
    let size_bytes = AerogpuAllocTableHeader::SIZE_BYTES as u32;
    let entry_stride = AerogpuAllocEntry::SIZE_BYTES as u32;
    let bytes = build_alloc_table_bytes(size_bytes, 1, entry_stride, &[]);

    let mem = VecGuestMemory::new(0x2000);
    mem.write(gpa, &bytes).unwrap();

    let err =
        AllocTable::decode_from_guest_memory(&mem, gpa, size_bytes).expect_err("must fail");
    let ExecutorError::Validation(msg) = err else {
        panic!("expected validation error, got {err:?}");
    };
    assert!(
        msg.contains("BadSizeField"),
        "unexpected error message: {msg}"
    );
}
