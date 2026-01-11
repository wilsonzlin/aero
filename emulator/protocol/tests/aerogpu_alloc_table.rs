use aero_protocol::aerogpu::aerogpu_pci::{AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::{
    decode_alloc_table_le, lookup_alloc, AerogpuAllocEntry, AerogpuAllocTableDecodeError, AerogpuAllocTableHeader,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_ALLOC_FLAG_READONLY,
};

fn write_u32_le(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn write_u64_le(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn alloc_table_backing_u64(total_size_bytes: usize) -> Vec<u64> {
    assert_eq!(total_size_bytes % 8, 0);
    vec![0u64; total_size_bytes / 8]
}

fn backing_as_u8_mut(backing: &mut [u64]) -> &mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(backing.as_mut_ptr() as *mut u8, backing.len() * 8) }
}

#[test]
fn decode_alloc_table_valid() {
    let entry_count = 2u32;
    let total_size = AerogpuAllocTableHeader::SIZE_BYTES + (entry_count as usize * AerogpuAllocEntry::SIZE_BYTES);

    let mut backing = alloc_table_backing_u64(total_size);
    let buf = backing_as_u8_mut(&mut backing);

    // Header.
    write_u32_le(buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
    write_u32_le(buf, 4, AEROGPU_ABI_VERSION_U32);
    write_u32_le(buf, 8, total_size as u32);
    write_u32_le(buf, 12, entry_count);
    write_u32_le(buf, 16, AerogpuAllocEntry::SIZE_BYTES as u32);
    write_u32_le(buf, 20, 0);

    // Entry 0.
    let e0 = AerogpuAllocTableHeader::SIZE_BYTES;
    write_u32_le(buf, e0 + 0, 10);
    write_u32_le(buf, e0 + 4, AEROGPU_ALLOC_FLAG_READONLY);
    write_u64_le(buf, e0 + 8, 0x1122_3344_5566_7788);
    write_u64_le(buf, e0 + 16, 0x1000);
    write_u64_le(buf, e0 + 24, 0);

    // Entry 1.
    let e1 = AerogpuAllocTableHeader::SIZE_BYTES + AerogpuAllocEntry::SIZE_BYTES;
    write_u32_le(buf, e1 + 0, 20);
    write_u32_le(buf, e1 + 4, 0);
    write_u64_le(buf, e1 + 8, 0x8877_6655_4433_2211);
    write_u64_le(buf, e1 + 16, 0x2000);
    write_u64_le(buf, e1 + 24, 0);

    let decoded = decode_alloc_table_le(buf).unwrap();
    assert_eq!(decoded.header.magic, AEROGPU_ALLOC_TABLE_MAGIC);
    assert_eq!(decoded.header.abi_version, AEROGPU_ABI_VERSION_U32);
    assert_eq!(decoded.header.size_bytes, total_size as u32);
    assert_eq!(decoded.header.entry_count, entry_count);
    assert_eq!(decoded.header.entry_stride_bytes, AerogpuAllocEntry::SIZE_BYTES as u32);

    assert_eq!(decoded.entries.len(), 2);
    assert_eq!(decoded.entries[0].alloc_id, 10);
    assert_eq!(decoded.entries[0].flags, AEROGPU_ALLOC_FLAG_READONLY);
    assert_eq!(decoded.entries[0].gpa, 0x1122_3344_5566_7788);
    assert_eq!(decoded.entries[0].size_bytes, 0x1000);

    assert_eq!(decoded.entries[1].alloc_id, 20);
    assert_eq!(decoded.entries[1].flags, 0);
    assert_eq!(decoded.entries[1].gpa, 0x8877_6655_4433_2211);
    assert_eq!(decoded.entries[1].size_bytes, 0x2000);

    let entry = lookup_alloc(&decoded, 20).expect("alloc_id 20 should be present");
    assert_eq!(entry.gpa, 0x8877_6655_4433_2211);
}

#[test]
fn decode_alloc_table_rejects_short_buffer() {
    let buf = [0u8; AerogpuAllocTableHeader::SIZE_BYTES - 1];
    let err = decode_alloc_table_le(&buf).err().unwrap();
    assert!(matches!(err, AerogpuAllocTableDecodeError::BufferTooSmall));
}

#[test]
fn decode_alloc_table_rejects_bad_magic() {
    let mut buf = [0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    write_u32_le(&mut buf, 0, 0xDEAD_BEEF);
    let err = decode_alloc_table_le(&buf).err().unwrap();
    assert!(matches!(err, AerogpuAllocTableDecodeError::BadMagic { found: 0xDEAD_BEEF }));
}

#[test]
fn decode_alloc_table_rejects_bad_abi() {
    let mut buf = [0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    write_u32_le(&mut buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
    let bad_version = ((AEROGPU_ABI_MAJOR + 1) << 16) | AEROGPU_ABI_MINOR;
    write_u32_le(&mut buf, 4, bad_version);
    write_u32_le(&mut buf, 8, AerogpuAllocTableHeader::SIZE_BYTES as u32);
    write_u32_le(&mut buf, 12, 0);
    write_u32_le(&mut buf, 16, AerogpuAllocEntry::SIZE_BYTES as u32);
    write_u32_le(&mut buf, 20, 0);

    let err = decode_alloc_table_le(&buf).err().unwrap();
    assert!(matches!(err, AerogpuAllocTableDecodeError::Abi(_)));
}

#[test]
fn decode_alloc_table_rejects_bad_size() {
    // size_bytes < header size.
    {
        let mut buf = [0u8; AerogpuAllocTableHeader::SIZE_BYTES];
        write_u32_le(&mut buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
        write_u32_le(&mut buf, 4, AEROGPU_ABI_VERSION_U32);
        write_u32_le(&mut buf, 8, (AerogpuAllocTableHeader::SIZE_BYTES as u32) - 1);
        let err = decode_alloc_table_le(&buf).err().unwrap();
        assert!(matches!(err, AerogpuAllocTableDecodeError::BadSize { .. }));
    }

    // size_bytes > buffer length.
    {
        let mut buf = [0u8; AerogpuAllocTableHeader::SIZE_BYTES];
        write_u32_le(&mut buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
        write_u32_le(&mut buf, 4, AEROGPU_ABI_VERSION_U32);
        write_u32_le(&mut buf, 8, (AerogpuAllocTableHeader::SIZE_BYTES as u32) + 4);
        let err = decode_alloc_table_le(&buf).err().unwrap();
        assert!(matches!(err, AerogpuAllocTableDecodeError::BadSize { .. }));
    }
}

#[test]
fn decode_alloc_table_rejects_bad_stride() {
    let mut buf = [0u8; AerogpuAllocTableHeader::SIZE_BYTES];
    write_u32_le(&mut buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
    write_u32_le(&mut buf, 4, AEROGPU_ABI_VERSION_U32);
    write_u32_le(&mut buf, 8, AerogpuAllocTableHeader::SIZE_BYTES as u32);
    write_u32_le(&mut buf, 12, 0);
    write_u32_le(&mut buf, 16, 16);

    let err = decode_alloc_table_le(&buf).err().unwrap();
    assert!(matches!(err, AerogpuAllocTableDecodeError::BadStride { .. }));
}

#[test]
fn decode_alloc_table_rejects_entry_count_out_of_bounds() {
    let mut backing = alloc_table_backing_u64(AerogpuAllocTableHeader::SIZE_BYTES);
    let buf = backing_as_u8_mut(&mut backing);

    write_u32_le(buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
    write_u32_le(buf, 4, AEROGPU_ABI_VERSION_U32);
    write_u32_le(buf, 8, AerogpuAllocTableHeader::SIZE_BYTES as u32);
    write_u32_le(buf, 12, 1); // no space for even one entry
    write_u32_le(buf, 16, AerogpuAllocEntry::SIZE_BYTES as u32);
    write_u32_le(buf, 20, 0);

    let err = decode_alloc_table_le(buf).err().unwrap();
    assert!(matches!(err, AerogpuAllocTableDecodeError::CountOutOfBounds));
}

#[test]
fn decode_alloc_table_rejects_misaligned_entries() {
    let entry_count = 1u32;
    let total_size = AerogpuAllocTableHeader::SIZE_BYTES + (entry_count as usize * AerogpuAllocEntry::SIZE_BYTES);
    assert_eq!(total_size % 8, 0);

    // Add 1 byte of padding so that the returned slice is misaligned.
    let mut backing = vec![0u64; (total_size + 8) / 8];
    let raw = backing_as_u8_mut(&mut backing);
    let buf = &mut raw[1..1 + total_size];

    write_u32_le(buf, 0, AEROGPU_ALLOC_TABLE_MAGIC);
    write_u32_le(buf, 4, AEROGPU_ABI_VERSION_U32);
    write_u32_le(buf, 8, total_size as u32);
    write_u32_le(buf, 12, entry_count);
    write_u32_le(buf, 16, AerogpuAllocEntry::SIZE_BYTES as u32);
    write_u32_le(buf, 20, 0);

    let err = decode_alloc_table_le(buf).err().unwrap();
    assert!(matches!(err, AerogpuAllocTableDecodeError::Misaligned));
}
