use aero_storage::{
    DiskError, MemBackend, Qcow2Disk, StorageBackend, VhdDisk, VirtualDisk, SECTOR_SIZE,
};

const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn vhd_footer_checksum(raw: &[u8; SECTOR_SIZE]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in raw.iter().enumerate() {
        if (64..68).contains(&i) {
            continue;
        }
        sum = sum.wrapping_add(*b as u32);
    }
    !sum
}

fn make_qcow2_empty(virtual_size: u64) -> MemBackend {
    assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);

    // Keep fixtures small while still exercising the full metadata path.
    let cluster_bits = 12u32; // 4 KiB clusters
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;

    let file_len = cluster_size * 5;
    let mut backend = MemBackend::with_len(file_len).unwrap();

    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 3); // version
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, virtual_size);
    write_be_u32(&mut header, 36, 1); // l1_size
    write_be_u64(&mut header, 40, l1_table_offset);
    write_be_u64(&mut header, 48, refcount_table_offset);
    write_be_u32(&mut header, 56, 1); // refcount_table_clusters
    write_be_u64(&mut header, 72, 0); // incompatible_features
    write_be_u64(&mut header, 80, 0); // compatible_features
    write_be_u64(&mut header, 88, 0); // autoclear_features
    write_be_u32(&mut header, 96, 4); // refcount_order (16-bit)
    write_be_u32(&mut header, 100, 104); // header_length
    backend.write_at(0, &header).unwrap();

    backend
        .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
        .unwrap();

    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l1_table_offset, &l1_entry.to_be_bytes())
        .unwrap();

    for cluster_index in 0u64..5 {
        let off = refcount_block_offset + cluster_index * 2;
        backend.write_at(off, &1u16.to_be_bytes()).unwrap();
    }

    backend
}

fn make_qcow2_with_pattern() -> MemBackend {
    let virtual_size = 2 * 1024 * 1024;
    let cluster_size = 1u64 << 12;

    let mut backend = make_qcow2_empty(virtual_size);
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;
    backend.set_len(cluster_size * 6).unwrap();

    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();

    let refcount_block_offset = cluster_size * 3;
    backend
        .write_at(refcount_block_offset + 5 * 2, &1u16.to_be_bytes())
        .unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    sector[..12].copy_from_slice(b"hello qcow2!");
    backend.write_at(data_cluster_offset, &sector).unwrap();

    backend
}

fn make_vhd_footer(virtual_size: u64, disk_type: u32, data_offset: u64) -> [u8; SECTOR_SIZE] {
    let mut footer = [0u8; SECTOR_SIZE];
    footer[0..8].copy_from_slice(b"conectix");
    write_be_u32(&mut footer, 8, 2); // features
    write_be_u32(&mut footer, 12, 0x0001_0000); // file_format_version
    write_be_u64(&mut footer, 16, data_offset);
    write_be_u64(&mut footer, 40, virtual_size); // original_size
    write_be_u64(&mut footer, 48, virtual_size); // current_size
    write_be_u32(&mut footer, 60, disk_type);
    let checksum = vhd_footer_checksum(&footer);
    write_be_u32(&mut footer, 64, checksum);
    footer
}

fn make_vhd_fixed_with_pattern() -> MemBackend {
    let virtual_size = 64 * 1024;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let mut backend = MemBackend::default();
    backend.write_at(0, &data).unwrap();
    backend.write_at(virtual_size, &footer).unwrap();
    backend
}

fn make_vhd_dynamic_empty(virtual_size: u64, block_size: u32) -> MemBackend {
    assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);
    assert_eq!(block_size as usize % SECTOR_SIZE, 0);

    let dyn_header_offset = SECTOR_SIZE as u64;
    let table_offset = dyn_header_offset + 1024u64;
    let blocks = virtual_size.div_ceil(block_size as u64);
    let max_table_entries = blocks as u32;
    let bat_bytes = max_table_entries as u64 * 4;
    let bat_size = bat_bytes.div_ceil(SECTOR_SIZE as u64) * SECTOR_SIZE as u64;

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    let file_len = (SECTOR_SIZE as u64) + 1024 + bat_size + (SECTOR_SIZE as u64);
    let mut backend = MemBackend::with_len(file_len).unwrap();

    backend.write_at(0, &footer).unwrap();
    backend.write_at(file_len - SECTOR_SIZE as u64, &footer).unwrap();

    let mut dyn_header = [0u8; 1024];
    dyn_header[0..8].copy_from_slice(b"cxsparse");
    write_be_u64(&mut dyn_header, 8, u64::MAX);
    write_be_u64(&mut dyn_header, 16, table_offset);
    write_be_u32(&mut dyn_header, 24, 0x0001_0000);
    write_be_u32(&mut dyn_header, 28, max_table_entries);
    write_be_u32(&mut dyn_header, 32, block_size);
    backend.write_at(dyn_header_offset, &dyn_header).unwrap();

    let bat = vec![0xFFu8; bat_size as usize];
    backend.write_at(table_offset, &bat).unwrap();

    backend
}

fn make_vhd_dynamic_with_pattern() -> MemBackend {
    let virtual_size = 64 * 1024;
    let block_size = 16 * 1024;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let dyn_header_offset = SECTOR_SIZE as u64;
    let table_offset = dyn_header_offset + 1024u64;
    let bat_size = SECTOR_SIZE as u64; // 4 entries padded to 512
    let old_footer_offset = (SECTOR_SIZE as u64) + 1024 + bat_size;
    let bitmap_size = SECTOR_SIZE as u64; // sectors_per_block=32 => bitmap_bytes=4 => 512 aligned
    let block_total_size = bitmap_size + block_size as u64;
    let new_footer_offset = old_footer_offset + block_total_size;

    backend.set_len(new_footer_offset + SECTOR_SIZE as u64).unwrap();

    let bat_entry = (old_footer_offset / SECTOR_SIZE as u64) as u32;
    backend
        .write_at(table_offset, &bat_entry.to_be_bytes())
        .unwrap();

    let mut bitmap = [0u8; SECTOR_SIZE];
    bitmap[0] = 0x80;
    backend.write_at(old_footer_offset, &bitmap).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    sector[..12].copy_from_slice(b"hello vhd-d!");
    let data_offset = old_footer_offset + bitmap_size;
    backend.write_at(data_offset, &sector).unwrap();

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    backend.write_at(0, &footer).unwrap();
    backend.write_at(new_footer_offset, &footer).unwrap();

    backend
}

#[test]
fn qcow2_unallocated_reads_zero() {
    let backend = make_qcow2_empty(64 * 1024);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    let mut buf = vec![0xAAu8; SECTOR_SIZE * 4];
    disk.read_sectors(0, &mut buf).unwrap();
    assert!(buf.iter().all(|b| *b == 0));
}

#[test]
fn qcow2_fixture_read_and_write() {
    let backend = make_qcow2_with_pattern();
    let mut disk = Qcow2Disk::open(backend).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..12], b"hello qcow2!");

    let mut write_buf = vec![0u8; SECTOR_SIZE];
    write_buf[..14].copy_from_slice(b"write qcow2 ok");
    disk.write_sectors(10, &write_buf).unwrap();

    let mut read_back = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(10, &mut read_back).unwrap();
    assert_eq!(read_back, write_buf);
}

#[test]
fn qcow2_write_persists_after_reopen() {
    let backend = make_qcow2_empty(64 * 1024);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    let data = vec![0x5Au8; SECTOR_SIZE * 2];
    disk.write_sectors(1, &data).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = Qcow2Disk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE * 2];
    reopened.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn qcow2_rejects_corrupt_magic() {
    let mut backend = MemBackend::with_len(104).unwrap();
    backend.write_at(0, b"NOPE").unwrap();
    match Qcow2Disk::open(backend) {
        Ok(_) => panic!("expected qcow2 open to fail"),
        Err(err) => assert!(matches!(err, DiskError::CorruptImage(_))),
    }
}

#[test]
fn qcow2_zero_writes_do_not_allocate_clusters() {
    let mut backend = make_qcow2_empty(64 * 1024);
    let initial_len = backend.len().unwrap();
    let cluster_size = 1u64 << 12;

    let mut disk = Qcow2Disk::open(backend).unwrap();

    let zeros = vec![0u8; SECTOR_SIZE];
    disk.write_sectors(0, &zeros).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();
    assert_eq!(final_len, initial_len);
    assert!(final_len.is_multiple_of(cluster_size));
}

#[test]
fn qcow2_nonzero_write_allocates_cluster_and_grows_file() {
    let mut backend = make_qcow2_empty(64 * 1024);
    let initial_len = backend.len().unwrap();
    let cluster_size = 1u64 << 12;

    let mut disk = Qcow2Disk::open(backend).unwrap();

    let data = vec![0xA5u8; SECTOR_SIZE];
    disk.write_sectors(0, &data).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();
    assert!(final_len > initial_len);
    assert!(final_len.is_multiple_of(cluster_size));
}

#[test]
fn vhd_fixed_fixture_read() {
    let backend = make_vhd_fixed_with_pattern();
    let mut disk = VhdDisk::open(backend).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");
}

#[test]
fn vhd_fixed_write_last_sector_persists_and_footer_remains_valid() {
    let virtual_size = 64 * 1024u64;
    let backend = make_vhd_fixed_with_pattern();
    let mut disk = VhdDisk::open(backend).unwrap();

    let last_lba = (virtual_size / SECTOR_SIZE as u64) - 1;
    let data = vec![0xDDu8; SECTOR_SIZE];
    disk.write_sectors(last_lba, &data).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(last_lba, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_dynamic_unallocated_reads_zero_and_writes_allocate() {
    let backend = make_vhd_dynamic_empty(64 * 1024, 16 * 1024);
    let mut disk = VhdDisk::open(backend).unwrap();

    let mut buf = vec![0xAAu8; SECTOR_SIZE * 8];
    disk.read_sectors(0, &mut buf).unwrap();
    assert!(buf.iter().all(|b| *b == 0));

    let data = vec![0x5Au8; SECTOR_SIZE * 2];
    disk.write_sectors(1, &data).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE * 2];
    disk.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_dynamic_zero_writes_do_not_allocate_blocks() {
    let mut backend = make_vhd_dynamic_empty(64 * 1024, 16 * 1024);
    let initial_len = backend.len().unwrap();

    let mut disk = VhdDisk::open(backend).unwrap();
    let zeros = vec![0u8; SECTOR_SIZE];
    disk.write_sectors(0, &zeros).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();
    assert_eq!(final_len, initial_len);
}

#[test]
fn vhd_dynamic_nonzero_write_allocates_block_and_grows_file() {
    let mut backend = make_vhd_dynamic_empty(64 * 1024, 16 * 1024);
    let initial_len = backend.len().unwrap();

    let mut disk = VhdDisk::open(backend).unwrap();
    let data = vec![0x11u8; SECTOR_SIZE];
    disk.write_sectors(0, &data).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();
    assert!(final_len > initial_len);
}

#[test]
fn vhd_dynamic_fixture_read() {
    let backend = make_vhd_dynamic_with_pattern();
    let mut disk = VhdDisk::open(backend).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..12], b"hello vhd-d!");
}

#[test]
fn vhd_dynamic_write_persists_after_reopen() {
    let backend = make_vhd_dynamic_empty(64 * 1024, 16 * 1024);
    let mut disk = VhdDisk::open(backend).unwrap();

    let data = vec![0xCCu8; SECTOR_SIZE];
    disk.write_sectors(3, &data).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(3, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_rejects_bad_footer_checksum() {
    let mut backend = make_vhd_fixed_with_pattern();
    let mut last = [0u8; 1];
    backend.read_at((64 * 1024) + (SECTOR_SIZE as u64) - 1, &mut last).unwrap();
    last[0] ^= 0xFF;
    backend
        .write_at((64 * 1024) + (SECTOR_SIZE as u64) - 1, &last)
        .unwrap();

    match VhdDisk::open(backend) {
        Ok(_) => panic!("expected vhd open to fail"),
        Err(err) => assert!(matches!(err, DiskError::CorruptImage(_))),
    }
}
