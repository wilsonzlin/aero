use aero_storage::{MemBackend, RawDisk, StorageBackend as _, VhdDisk, VirtualDisk, SECTOR_SIZE};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

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

fn vhd_dynamic_header_checksum(raw: &[u8; 1024]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in raw.iter().enumerate() {
        if (36..40).contains(&i) {
            continue;
        }
        sum = sum.wrapping_add(*b as u32);
    }
    !sum
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

fn make_vhd_differencing_empty(virtual_size: u64, block_size: u32) -> MemBackend {
    assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);
    assert_eq!(block_size as usize % SECTOR_SIZE, 0);

    let dyn_header_offset = SECTOR_SIZE as u64;
    let table_offset = dyn_header_offset + 1024u64;
    let blocks = virtual_size.div_ceil(block_size as u64);
    let max_table_entries = blocks as u32;
    let bat_bytes = max_table_entries as u64 * 4;
    let bat_size = bat_bytes.div_ceil(SECTOR_SIZE as u64) * SECTOR_SIZE as u64;

    let footer = make_vhd_footer(virtual_size, 4, dyn_header_offset);
    let file_len = (SECTOR_SIZE as u64) + 1024 + bat_size + (SECTOR_SIZE as u64);
    let mut backend = MemBackend::with_len(file_len).unwrap();

    // Required footer copy at offset 0 and footer at EOF.
    backend.write_at(0, &footer).unwrap();
    backend
        .write_at(file_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    let mut dyn_header = [0u8; 1024];
    dyn_header[0..8].copy_from_slice(b"cxsparse");
    write_be_u64(&mut dyn_header, 8, u64::MAX);
    write_be_u64(&mut dyn_header, 16, table_offset);
    write_be_u32(&mut dyn_header, 24, 0x0001_0000);
    write_be_u32(&mut dyn_header, 28, max_table_entries);
    write_be_u32(&mut dyn_header, 32, block_size);
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    write_be_u32(&mut dyn_header, 36, checksum);
    backend.write_at(dyn_header_offset, &dyn_header).unwrap();

    // BAT: all entries unallocated.
    let bat = vec![0xFFu8; bat_size as usize];
    backend.write_at(table_offset, &bat).unwrap();

    backend
}

struct CountingDisk<D> {
    inner: D,
    reads: Arc<AtomicU64>,
}

impl<D> CountingDisk<D> {
    fn new(inner: D, reads: Arc<AtomicU64>) -> Self {
        Self { inner, reads }
    }
}

impl<D: VirtualDisk> VirtualDisk for CountingDisk<D> {
    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.flush()
    }
}

#[test]
fn vhd_differencing_unallocated_reads_from_parent() {
    let virtual_size = 16 * 1024u64;
    let block_size = 4 * 1024u32;

    let mut base = RawDisk::create(MemBackend::new(), virtual_size).unwrap();
    let pattern: Vec<u8> = (0..virtual_size as usize).map(|i| (i & 0xFF) as u8).collect();
    base.write_at(0, &pattern).unwrap();

    let backend = make_vhd_differencing_empty(virtual_size, block_size);
    let mut disk = VhdDisk::open_with_parent(backend, Box::new(base)).unwrap();

    // Read across a VHD block boundary to ensure the fallback happens for multiple blocks.
    let start = (block_size as usize) - 100;
    let mut buf = vec![0u8; 300];
    disk.read_at(start as u64, &mut buf).unwrap();
    assert_eq!(&buf, &pattern[start..start + buf.len()]);
}

#[test]
fn vhd_differencing_partial_write_allocates_and_preserves_parent_bytes() {
    let virtual_size = 16 * 1024u64;
    let block_size = 4 * 1024u32;

    let mut base = RawDisk::create(MemBackend::new(), virtual_size).unwrap();
    let pattern: Vec<u8> = (0..virtual_size as usize).map(|i| (i & 0xFF) as u8).collect();
    base.write_at(0, &pattern).unwrap();

    let mut backend = make_vhd_differencing_empty(virtual_size, block_size);
    let initial_len = backend.len().unwrap();

    let mut disk = VhdDisk::open_with_parent(backend, Box::new(base)).unwrap();

    // Unaligned write that requires seeding bytes from the parent.
    disk.write_at(3, &[9, 8, 7, 6]).unwrap();

    let mut back = [0u8; 16];
    disk.read_at(0, &mut back).unwrap();
    let mut expected = pattern[0..16].to_vec();
    expected[3..7].copy_from_slice(&[9, 8, 7, 6]);
    assert_eq!(&back, expected.as_slice());

    // Another sector in the same newly-allocated block that was not written should still read
    // from the parent.
    let mut sector1_prefix = [0u8; 16];
    disk.read_at(SECTOR_SIZE as u64, &mut sector1_prefix).unwrap();
    assert_eq!(
        &sector1_prefix,
        &pattern[SECTOR_SIZE..SECTOR_SIZE + sector1_prefix.len()]
    );

    disk.flush().unwrap();
    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();
    assert!(final_len > initial_len);
}

#[test]
fn vhd_differencing_full_block_write_does_not_read_parent() {
    let virtual_size = 16 * 1024u64;
    let block_size = 4 * 1024u32;

    let mut base = RawDisk::create(MemBackend::new(), virtual_size).unwrap();
    base.write_at(0, &vec![0x55u8; virtual_size as usize]).unwrap();

    let reads = Arc::new(AtomicU64::new(0));
    let parent = CountingDisk::new(base, reads.clone());

    let backend = make_vhd_differencing_empty(virtual_size, block_size);
    let mut disk = VhdDisk::open_with_parent(backend, Box::new(parent)).unwrap();

    // Overwrite the entire first VHD block; should not require consulting the parent.
    let data = vec![0xAAu8; block_size as usize];
    disk.write_at(0, &data).unwrap();

    assert_eq!(reads.load(Ordering::Relaxed), 0);

    // Sanity check: read back the start of the block.
    let mut back = vec![0u8; 32];
    disk.read_at(0, &mut back).unwrap();
    assert_eq!(&back, &data[..back.len()]);
}

