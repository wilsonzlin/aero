use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, MemBackend, StorageBackend as _, VirtualDisk,
};

const BLOCK_SIZE: u32 = 4096;

fn make_disk() -> AeroSparseDisk<MemBackend> {
    AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: 16 * 1024,
            block_size_bytes: BLOCK_SIZE,
        },
    )
    .unwrap()
}

#[test]
fn sparse_zero_write_full_block_does_not_allocate() {
    let mut disk = make_disk();
    let initial_len = disk.header().data_offset;

    let zeros = vec![0u8; BLOCK_SIZE as usize];
    disk.write_at(0, &zeros).unwrap();

    assert_eq!(disk.header().allocated_blocks, 0);

    let mut backend = disk.into_backend();
    assert_eq!(backend.len().unwrap(), initial_len);
}

#[test]
fn sparse_zero_write_partial_does_not_allocate() {
    let mut disk = make_disk();
    let initial_len = disk.header().data_offset;

    disk.write_at(123, &[0u8; 200]).unwrap();

    assert_eq!(disk.header().allocated_blocks, 0);

    let mut backend = disk.into_backend();
    assert_eq!(backend.len().unwrap(), initial_len);
}

#[test]
fn sparse_zero_write_multiple_blocks_does_not_allocate() {
    let mut disk = make_disk();
    let initial_len = disk.header().data_offset;

    let zeros = vec![0u8; (BLOCK_SIZE as usize) * 3];
    disk.write_at(0, &zeros).unwrap();

    assert_eq!(disk.header().allocated_blocks, 0);

    let mut backend = disk.into_backend();
    assert_eq!(backend.len().unwrap(), initial_len);
}

#[test]
fn sparse_zero_write_mixed_block_allocates_once_and_preserves_data() {
    let mut disk = make_disk();

    let mut data = vec![0u8; (BLOCK_SIZE as usize) * 2];
    let non_zero_idx = (BLOCK_SIZE as usize) + 17;
    data[non_zero_idx] = 0xAB;

    disk.write_at(0, &data).unwrap();

    assert_eq!(disk.header().allocated_blocks, 1);
    assert!(!disk.is_block_allocated(0));
    assert!(disk.is_block_allocated(1));

    let mut out = vec![0u8; data.len()];
    disk.read_at(0, &mut out).unwrap();
    assert_eq!(out, data);
}

#[test]
fn sparse_zero_write_into_allocated_block_overwrites_data() {
    let mut disk = make_disk();

    disk.write_at(0, &[1, 2, 3, 4]).unwrap();
    assert_eq!(disk.header().allocated_blocks, 1);

    disk.write_at(0, &[0, 0, 0, 0]).unwrap();
    assert_eq!(disk.header().allocated_blocks, 1);

    let mut out = [0xFFu8; 4];
    disk.read_at(0, &mut out).unwrap();
    assert_eq!(out, [0, 0, 0, 0]);
}
