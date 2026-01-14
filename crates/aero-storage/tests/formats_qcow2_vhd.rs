#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    detect_format, DiskError, DiskFormat, DiskImage, MemBackend, Qcow2Disk, RawDisk,
    StorageBackend, VhdDisk, VirtualDisk, SECTOR_SIZE,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

const QCOW2_OFLAG_COPIED: u64 = 1 << 63;
const QCOW2_OFLAG_COMPRESSED: u64 = 1 << 62;
const QCOW2_OFLAG_ZERO: u64 = 1 << 0;

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

fn make_qcow2_empty_with_backing(virtual_size: u64) -> MemBackend {
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

    let backing_name = b"backing.img";
    let backing_file_offset = 104u64; // immediately after v3 header

    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 3); // version
    write_be_u64(&mut header, 8, backing_file_offset);
    write_be_u32(&mut header, 16, backing_name.len() as u32);
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
    backend.write_at(backing_file_offset, backing_name).unwrap();

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

fn make_qcow2_v2_empty(virtual_size: u64) -> MemBackend {
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

    // QCOW2 v2 header is 72 bytes (big-endian).
    let mut header = [0u8; 72];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 2); // version
                                     // backing file offset/size are zero
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, virtual_size);
    // crypt_method is zero
    write_be_u32(&mut header, 36, 1); // l1_size
    write_be_u64(&mut header, 40, l1_table_offset);
    write_be_u64(&mut header, 48, refcount_table_offset);
    write_be_u32(&mut header, 56, 1); // refcount_table_clusters
                                      // nb_snapshots and snapshots_offset are zero
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

fn make_qcow2_empty_without_l2(virtual_size: u64) -> MemBackend {
    assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);

    let cluster_bits = 12u32; // 4 KiB clusters
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;

    // No L2 table cluster is allocated yet.
    let file_len = cluster_size * 4;
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

    // Mark metadata clusters as in-use: header, refcount table, L1 table, refcount block.
    for cluster_index in 0u64..4 {
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

fn make_qcow2_two_contiguous_data_clusters() -> MemBackend {
    let cluster_bits = 12u32;
    let cluster_size = 1u64 << cluster_bits;
    let virtual_size = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;
    let data0_offset = cluster_size * 5;
    let data1_offset = cluster_size * 6;

    let mut backend = make_qcow2_empty(virtual_size);
    backend.set_len(cluster_size * 7).unwrap();

    let l2_entry0 = data0_offset | QCOW2_OFLAG_COPIED;
    let l2_entry1 = data1_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l2_table_offset, &l2_entry0.to_be_bytes())
        .unwrap();
    backend
        .write_at(l2_table_offset + 8, &l2_entry1.to_be_bytes())
        .unwrap();

    let cluster0 = vec![0xA5u8; cluster_size as usize];
    let cluster1 = vec![0x5Au8; cluster_size as usize];
    backend.write_at(data0_offset, &cluster0).unwrap();
    backend.write_at(data1_offset, &cluster1).unwrap();

    backend
}

fn make_qcow2_shared_data_cluster(pattern: u8) -> MemBackend {
    let cluster_bits = 12u32;
    let cluster_size = 1u64 << cluster_bits;
    let virtual_size = cluster_size * 2;

    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;

    let mut backend = make_qcow2_empty(virtual_size);
    backend.set_len(cluster_size * 6).unwrap();

    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
    // Two guest clusters point at the same physical data cluster.
    backend
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();
    backend
        .write_at(l2_table_offset + 8, &l2_entry.to_be_bytes())
        .unwrap();

    // Cluster index 5 corresponds to `data_cluster_offset`.
    backend
        .write_at(refcount_block_offset + 5 * 2, &2u16.to_be_bytes())
        .unwrap();

    let cluster = vec![pattern; cluster_size as usize];
    backend.write_at(data_cluster_offset, &cluster).unwrap();

    backend
}

struct CountingBackend {
    inner: MemBackend,
    reads: Arc<AtomicU64>,
}

impl CountingBackend {
    fn new(inner: MemBackend, reads: Arc<AtomicU64>) -> Self {
        Self { inner, reads }
    }
}

impl StorageBackend for CountingBackend {
    fn len(&mut self) -> aero_storage::Result<u64> {
        self.inner.len()
    }

    fn set_len(&mut self, len: u64) -> aero_storage::Result<()> {
        self.inner.set_len(len)
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

struct WriteTraceBackend {
    inner: MemBackend,
    writes: Arc<Mutex<Vec<(u64, usize)>>>,
}

impl WriteTraceBackend {
    fn new(inner: MemBackend, writes: Arc<Mutex<Vec<(u64, usize)>>>) -> Self {
        Self { inner, writes }
    }
}

impl StorageBackend for WriteTraceBackend {
    fn len(&mut self) -> aero_storage::Result<u64> {
        self.inner.len()
    }

    fn set_len(&mut self, len: u64) -> aero_storage::Result<()> {
        self.inner.set_len(len)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.writes.lock().unwrap().push((offset, buf.len()));
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.flush()
    }
}

struct FailOnWriteBackend {
    inner: MemBackend,
    fail_offset: u64,
    fail_len: usize,
}

impl FailOnWriteBackend {
    fn new(inner: MemBackend, fail_offset: u64, fail_len: usize) -> Self {
        Self {
            inner,
            fail_offset,
            fail_len,
        }
    }
}

impl StorageBackend for FailOnWriteBackend {
    fn len(&mut self) -> aero_storage::Result<u64> {
        self.inner.len()
    }

    fn set_len(&mut self, len: u64) -> aero_storage::Result<()> {
        self.inner.set_len(len)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        if offset == self.fail_offset && buf.len() == self.fail_len {
            return Err(DiskError::Io("injected write failure".to_string()));
        }
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.flush()
    }
}

#[derive(Clone)]
struct SharedReadOnlyDisk<D> {
    inner: Arc<Mutex<D>>,
}

impl<D: VirtualDisk> VirtualDisk for SharedReadOnlyDisk<D> {
    fn capacity_bytes(&self) -> u64 {
        self.inner.lock().unwrap().capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner.lock().unwrap().read_at(offset, buf)
    }

    fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
        Err(DiskError::Unsupported("parent disk is read-only"))
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.lock().unwrap().flush()
    }
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

fn make_vhd_fixed_with_footer_copy() -> MemBackend {
    let virtual_size = 64 * 1024u64;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let mut backend = MemBackend::default();
    backend.write_at(0, &footer).unwrap(); // footer copy
    backend.write_at(SECTOR_SIZE as u64, &data).unwrap();
    backend
        .write_at(SECTOR_SIZE as u64 + virtual_size, &footer)
        .unwrap();
    backend
}

fn make_vhd_fixed_with_footer_copy_non_identical() -> MemBackend {
    let virtual_size = 64 * 1024u64;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);
    let mut footer_copy = footer;
    // Mutate a non-structural field (timestamp) so the footer copy differs from the EOF footer
    // while remaining a valid footer with a correct checksum.
    write_be_u32(&mut footer_copy, 24, 1234);
    let checksum = vhd_footer_checksum(&footer_copy);
    write_be_u32(&mut footer_copy, 64, checksum);

    let mut backend = MemBackend::default();
    backend.write_at(0, &footer_copy).unwrap(); // footer copy
    backend.write_at(SECTOR_SIZE as u64, &data).unwrap();
    backend
        .write_at(SECTOR_SIZE as u64 + virtual_size, &footer)
        .unwrap();
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

    let bat = vec![0xFFu8; bat_size as usize];
    backend.write_at(table_offset, &bat).unwrap();

    backend
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

    backend
        .set_len(new_footer_offset + SECTOR_SIZE as u64)
        .unwrap();

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
fn qcow2_open_rejects_backing_file_without_explicit_parent() {
    let backend = make_qcow2_empty_with_backing(64 * 1024);
    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported("qcow2 backing file")));
}

#[test]
fn disk_image_open_with_parent_supports_qcow2_backing() {
    let virtual_size = 64 * 1024u64;

    // Backing disk with known data.
    let mut backing_backend = MemBackend::with_len(virtual_size).unwrap();
    let mut backing_sector0 = [0u8; SECTOR_SIZE];
    backing_sector0[..15].copy_from_slice(b"backing sector0");
    backing_backend.write_at(0, &backing_sector0).unwrap();

    let backing_disk = RawDisk::open(backing_backend).unwrap();
    let qcow2_backend = make_qcow2_empty_with_backing(virtual_size);
    let mut disk =
        DiskImage::open_with_parent(DiskFormat::Qcow2, qcow2_backend, Box::new(backing_disk))
            .unwrap();

    let mut buf0 = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut buf0).unwrap();
    assert_eq!(buf0, backing_sector0);
}

#[test]
fn disk_image_open_auto_with_parent_supports_qcow2_backing() {
    let virtual_size = 64 * 1024u64;

    let mut backing_backend = MemBackend::with_len(virtual_size).unwrap();
    let mut backing_sector0 = [0u8; SECTOR_SIZE];
    backing_sector0[..15].copy_from_slice(b"backing sector0");
    backing_backend.write_at(0, &backing_sector0).unwrap();

    let backing_disk = RawDisk::open(backing_backend).unwrap();
    let qcow2_backend = make_qcow2_empty_with_backing(virtual_size);
    let mut disk = DiskImage::open_auto_with_parent(qcow2_backend, Box::new(backing_disk)).unwrap();
    assert_eq!(disk.format(), DiskFormat::Qcow2);

    let mut buf0 = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut buf0).unwrap();
    assert_eq!(buf0, backing_sector0);
}

#[test]
fn qcow2_with_backing_reads_fall_back_and_writes_are_copy_on_write() {
    let virtual_size = 64 * 1024u64;

    // Backing disk with known data.
    let mut backing_backend = MemBackend::with_len(virtual_size).unwrap();
    let mut backing_sector0 = [0u8; SECTOR_SIZE];
    backing_sector0[..15].copy_from_slice(b"backing sector0");
    backing_backend.write_at(0, &backing_sector0).unwrap();

    let mut backing_sector1 = [0u8; SECTOR_SIZE];
    backing_sector1[..15].copy_from_slice(b"backing sector1");
    backing_backend
        .write_at(SECTOR_SIZE as u64, &backing_sector1)
        .unwrap();

    // First sector of guest cluster 1 (cluster_size=4096 => lba 8).
    let cluster_size = 1u64 << 12;
    let cluster1_lba = cluster_size / SECTOR_SIZE as u64;
    let mut backing_cluster1_sector0 = [0u8; SECTOR_SIZE];
    backing_cluster1_sector0[..16].copy_from_slice(b"backing cluster1");
    backing_backend
        .write_at(cluster1_lba * SECTOR_SIZE as u64, &backing_cluster1_sector0)
        .unwrap();

    let backing_disk = RawDisk::open(backing_backend).unwrap();
    let qcow2_backend = make_qcow2_empty_with_backing(virtual_size);
    let mut disk = Qcow2Disk::open_with_parent(qcow2_backend, Box::new(backing_disk)).unwrap();

    // Unallocated clusters should read from the backing disk.
    let mut buf0 = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut buf0).unwrap();
    assert_eq!(buf0, backing_sector0);
    let mut buf1 = [0u8; SECTOR_SIZE];
    disk.read_sectors(1, &mut buf1).unwrap();
    assert_eq!(buf1, backing_sector1);

    let mut buf_cluster1 = [0u8; SECTOR_SIZE];
    disk.read_sectors(cluster1_lba, &mut buf_cluster1).unwrap();
    assert_eq!(buf_cluster1, backing_cluster1_sector0);

    // Write into an unallocated cluster (cluster 0). This should allocate a cluster in the qcow2
    // child and seed untouched bytes from the backing disk.
    let mut overlay_sector0 = [0u8; SECTOR_SIZE];
    overlay_sector0[..15].copy_from_slice(b"overlay sector0");
    disk.write_sectors(0, &overlay_sector0).unwrap();

    let mut read0 = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut read0).unwrap();
    assert_eq!(read0, overlay_sector0);

    // Sector 1 shares the same qcow2 guest cluster as sector 0; it should still match the backing
    // disk due to copy-on-write seeding.
    let mut read1 = [0u8; SECTOR_SIZE];
    disk.read_sectors(1, &mut read1).unwrap();
    assert_eq!(read1, backing_sector1);

    // Guest cluster 1 should remain unallocated in the qcow2 child; it must still fall back to
    // the backing disk.
    let mut read_cluster1 = [0u8; SECTOR_SIZE];
    disk.read_sectors(cluster1_lba, &mut read_cluster1).unwrap();
    assert_eq!(read_cluster1, backing_cluster1_sector0);

    disk.flush().unwrap();

    // Writes must never affect the backing disk.
    let (mut child_backend, backing_opt) = disk.into_backend_and_backing();
    let mut backing = backing_opt.unwrap();

    let mut backing0_after = [0u8; SECTOR_SIZE];
    backing.read_sectors(0, &mut backing0_after).unwrap();
    assert_eq!(backing0_after, backing_sector0);

    let mut backing1_after = [0u8; SECTOR_SIZE];
    backing.read_sectors(1, &mut backing1_after).unwrap();
    assert_eq!(backing1_after, backing_sector1);

    // The qcow2 image must have grown due to cluster allocation.
    let final_len = child_backend.len().unwrap();
    assert_eq!(final_len, cluster_size * 6);
}

#[test]
fn qcow2_backing_zero_cluster_flag_falls_back_to_parent() {
    let virtual_size = 64 * 1024u64;
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;

    let mut backing_backend = MemBackend::with_len(virtual_size).unwrap();
    let mut backing_sector0 = [0u8; SECTOR_SIZE];
    backing_sector0[..15].copy_from_slice(b"backing sector0");
    backing_backend.write_at(0, &backing_sector0).unwrap();
    let mut backing_sector1 = [0u8; SECTOR_SIZE];
    backing_sector1[..15].copy_from_slice(b"backing sector1");
    backing_backend
        .write_at(SECTOR_SIZE as u64, &backing_sector1)
        .unwrap();

    let backing_disk = RawDisk::open(backing_backend).unwrap();

    let mut qcow2_backend = make_qcow2_empty_with_backing(virtual_size);
    // Mark guest cluster 0 as a v3 "zero cluster". The implementation treats this as unallocated,
    // and with a backing disk it should therefore fall back to the parent.
    qcow2_backend
        .write_at(l2_table_offset, &QCOW2_OFLAG_ZERO.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open_with_parent(qcow2_backend, Box::new(backing_disk)).unwrap();

    let mut read0 = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut read0).unwrap();
    assert_eq!(read0, backing_sector0);

    // Writes should allocate a real data cluster and preserve backing bytes for sectors we don't
    // overwrite.
    let data = vec![0xCCu8; SECTOR_SIZE];
    disk.write_sectors(0, &data).unwrap();

    let mut read_back = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut read_back).unwrap();
    assert_eq!(read_back, data);

    let mut read1 = [0u8; SECTOR_SIZE];
    disk.read_sectors(1, &mut read1).unwrap();
    assert_eq!(read1, backing_sector1);
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
fn qcow2_rejects_backing_file() {
    let mut backend = make_qcow2_empty(64 * 1024);
    // backing_file_offset is at offset 8 in the header.
    backend.write_at(8, &1u64.to_be_bytes()).unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported("qcow2 backing file")));
}

#[test]
fn qcow2_rejects_encryption() {
    let mut backend = make_qcow2_empty(64 * 1024);
    // crypt_method is at offset 32 in the header.
    backend.write_at(32, &1u32.to_be_bytes()).unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported("qcow2 encryption")));
}

#[test]
fn qcow2_rejects_internal_snapshots() {
    let mut backend = make_qcow2_empty(64 * 1024);
    // nb_snapshots is at offset 60 in the header.
    backend.write_at(60, &1u32.to_be_bytes()).unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::Unsupported("qcow2 internal snapshots")
    ));
}

#[test]
fn qcow2_rejects_metadata_tables_overlapping() {
    let cluster_size = 1u64 << 12;
    let mut backend = make_qcow2_empty(64 * 1024);

    // Set l1_table_offset to overlap the refcount table (both at cluster 1).
    backend.write_at(40, &cluster_size.to_be_bytes()).unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 metadata tables overlap")
    ));
}

#[test]
fn qcow2_rejects_table_offset_overlapping_header() {
    let mut backend = make_qcow2_empty(64 * 1024);
    // Set l1_table_offset to 0 (overlaps the header).
    backend.write_at(40, &0u64.to_be_bytes()).unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 table overlaps header")
    ));
}

#[test]
fn qcow2_rejects_table_offset_not_cluster_aligned() {
    let cluster_size = 1u64 << 12;
    let mut backend = make_qcow2_empty(64 * 1024);

    // Set l1_table_offset to a value that's 8-byte aligned but not cluster aligned.
    let bad = cluster_size * 2 + 8;
    backend.write_at(40, &bad.to_be_bytes()).unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 table offset not cluster aligned")
    ));
}

#[test]
fn qcow2_rejects_l2_table_overlapping_refcount_table() {
    let cluster_size = 1u64 << 12;
    let l1_table_offset = cluster_size * 2;
    let refcount_table_offset = cluster_size;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_l1_entry = refcount_table_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l1_table_offset, &bad_l1_entry.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 cluster overlaps refcount table")
    ));
}

#[test]
fn qcow2_rejects_data_cluster_overlapping_l1_table() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;
    let l1_table_offset = cluster_size * 2;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_l2_entry = l1_table_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l2_table_offset, &bad_l2_entry.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let mut buf = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 cluster overlaps l1 table")
    ));
}

#[test]
fn qcow2_rejects_data_cluster_overlapping_refcount_block() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;
    let refcount_block_offset = cluster_size * 3;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_l2_entry = refcount_block_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l2_table_offset, &bad_l2_entry.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let mut buf = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 data cluster overlaps metadata")
    ));
}

#[test]
fn qcow2_rejects_compressed_l1_entry() {
    let cluster_size = 1u64 << 12;
    let l1_table_offset = cluster_size * 2;
    let l2_table_offset = cluster_size * 4;

    let mut backend = make_qcow2_empty(64 * 1024);
    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED | QCOW2_OFLAG_COMPRESSED;
    backend
        .write_at(l1_table_offset, &l1_entry.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported("qcow2 compressed l1")));
}

#[test]
fn qcow2_rejects_refcount_block_overlapping_l2_table() {
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;
    let l2_table_offset = cluster_size * 4;

    let mut backend = make_qcow2_empty(64 * 1024);
    backend
        .write_at(refcount_table_offset, &l2_table_offset.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 metadata clusters overlap")
    ));
}

#[test]
fn qcow2_rejects_compressed_l2_entry() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;

    let mut backend = make_qcow2_empty(64 * 1024);
    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COMPRESSED;
    backend
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let mut buf = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        DiskError::Unsupported("qcow2 compressed cluster")
    ));
}

#[test]
fn qcow2_rejects_unaligned_l1_entry() {
    let cluster_size = 1u64 << 12;
    let l1_table_offset = cluster_size * 2;
    let l2_table_offset = cluster_size * 4;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED | 2;
    backend
        .write_at(l1_table_offset, &bad_l1_entry.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 unaligned l1 entry")
    ));
}

#[test]
fn qcow2_rejects_unaligned_l2_entry() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED | 2;
    backend
        .write_at(l2_table_offset, &bad_l2_entry.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let mut buf = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 unaligned l2 entry")
    ));
}

#[test]
fn qcow2_write_rejects_data_cluster_pointing_past_eof() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_data_offset = cluster_size * 100;
    let l2_entry = bad_data_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let data = vec![0x11u8; SECTOR_SIZE];
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 data cluster truncated")
    ));
}

#[test]
fn qcow2_write_rejects_shared_data_cluster() {
    // Legacy test name kept for continuity: shared clusters should now trigger copy-on-write
    // rather than being rejected.
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;
    let refcount_block_offset = cluster_size * 3;
    let old_data_cluster_offset = cluster_size * 5;

    let mut backend = make_qcow2_shared_data_cluster(0xA5);
    let initial_len = backend.len().unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let data = vec![0x11u8; SECTOR_SIZE];
    disk.write_sectors(0, &data).unwrap();
    disk.flush().unwrap();

    // Guest cluster A (cluster index 0) should see the new write.
    let mut back_a = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut back_a).unwrap();
    assert_eq!(back_a, data);

    // Guest cluster B (cluster index 1) should still read the original data.
    let sectors_per_cluster = (cluster_size as usize / SECTOR_SIZE) as u64;
    let mut back_b = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(sectors_per_cluster, &mut back_b).unwrap();
    assert_eq!(back_b, vec![0xA5u8; SECTOR_SIZE]);

    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();
    assert_eq!(final_len, initial_len + cluster_size);

    // L2 entry for cluster A should now point at the newly allocated cluster.
    let new_data_cluster_offset = initial_len;
    let mut l2_bytes = [0u8; 16];
    backend.read_at(l2_table_offset, &mut l2_bytes).unwrap();
    let l2_entry_a = u64::from_be_bytes(l2_bytes[0..8].try_into().unwrap());
    let l2_entry_b = u64::from_be_bytes(l2_bytes[8..16].try_into().unwrap());
    assert_eq!(l2_entry_a, new_data_cluster_offset | QCOW2_OFLAG_COPIED);
    assert_eq!(l2_entry_b, old_data_cluster_offset | QCOW2_OFLAG_COPIED);

    // Refcount for old cluster should now be 1, and the new cluster should be 1.
    let mut rc_old = [0u8; 2];
    backend
        .read_at(refcount_block_offset + 5 * 2, &mut rc_old)
        .unwrap();
    assert_eq!(u16::from_be_bytes(rc_old), 1);

    let mut rc_new = [0u8; 2];
    backend
        .read_at(refcount_block_offset + 6 * 2, &mut rc_new)
        .unwrap();
    assert_eq!(u16::from_be_bytes(rc_new), 1);
}

#[test]
fn qcow2_shared_data_cluster_partial_write_is_copied_before_write() {
    let cluster_size = 1u64 << 12;
    let sectors_per_cluster = (cluster_size as usize / SECTOR_SIZE) as u64;

    let backend = make_qcow2_shared_data_cluster(0xEE);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    // Write a small slice inside cluster A.
    let offset_in_cluster: usize = 123;
    disk.write_at(offset_in_cluster as u64, &[1, 2, 3, 4])
        .unwrap();
    disk.flush().unwrap();

    // Cluster A should contain the original bytes except for the written slice.
    let mut cluster_a = vec![0u8; cluster_size as usize];
    disk.read_at(0, &mut cluster_a).unwrap();
    assert!(cluster_a[..offset_in_cluster].iter().all(|b| *b == 0xEE));
    assert_eq!(
        &cluster_a[offset_in_cluster..offset_in_cluster + 4],
        &[1, 2, 3, 4]
    );
    assert!(cluster_a[offset_in_cluster + 4..]
        .iter()
        .all(|b| *b == 0xEE));

    // Cluster B should still read the original bytes.
    let mut cluster_b = vec![0u8; cluster_size as usize];
    disk.read_at(cluster_size, &mut cluster_b).unwrap();
    assert!(cluster_b.iter().all(|b| *b == 0xEE));

    // Sanity check: the LBA-based read path should also still see the original pattern for B.
    let mut sector_b0 = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(sectors_per_cluster, &mut sector_b0)
        .unwrap();
    assert_eq!(sector_b0, vec![0xEEu8; SECTOR_SIZE]);
}

#[test]
fn qcow2_rejects_unaligned_refcount_block_entry() {
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;
    let refcount_block_offset = cluster_size * 3;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_refcount_entry = refcount_block_offset | 2;
    backend
        .write_at(refcount_table_offset, &bad_refcount_entry.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 unaligned refcount block entry")
    ));
}

#[test]
fn qcow2_rejects_compressed_refcount_block_entry() {
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;
    let refcount_block_offset = cluster_size * 3;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_refcount_entry = refcount_block_offset | QCOW2_OFLAG_COMPRESSED;
    backend
        .write_at(refcount_table_offset, &bad_refcount_entry.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::Unsupported("qcow2 compressed refcount block")
    ));
}

#[test]
fn qcow2_rejects_nonzero_refcount_entry_with_zero_offset() {
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_refcount_entry = QCOW2_OFLAG_COPIED; // non-zero, but offset=0
    backend
        .write_at(refcount_table_offset, &bad_refcount_entry.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 invalid refcount block entry")
    ));
}

#[test]
fn qcow2_rejects_refcount_block_overlapping_l1_table() {
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;

    let mut backend = make_qcow2_empty(64 * 1024);
    backend
        .write_at(refcount_table_offset, &l1_table_offset.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 cluster overlaps l1 table")
    ));
}

#[test]
fn qcow2_rejects_refcount_block_overlapping_refcount_table() {
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;

    let mut backend = make_qcow2_empty(64 * 1024);
    backend
        .write_at(refcount_table_offset, &refcount_table_offset.to_be_bytes())
        .unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 cluster overlaps refcount table")
    ));
}

#[test]
fn qcow2_rejects_invalid_zero_cluster_entry() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;

    let mut backend = make_qcow2_empty(64 * 1024);
    // ZERO flag plus another low bit is invalid.
    let bad_zero_entry = QCOW2_OFLAG_ZERO | 2;
    backend
        .write_at(l2_table_offset, &bad_zero_entry.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let mut buf = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 invalid zero cluster entry")
    ));
}

#[test]
fn qcow2_rejects_zero_cluster_entry_with_offset_bits_set() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;

    let mut backend = make_qcow2_empty(64 * 1024);
    let bad_zero_entry = data_cluster_offset | QCOW2_OFLAG_ZERO;
    backend
        .write_at(l2_table_offset, &bad_zero_entry.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let mut buf = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(0, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 invalid zero cluster entry")
    ));
}

#[test]
fn qcow2_v2_open_write_and_reopen_roundtrip() {
    let backend = make_qcow2_v2_empty(64 * 1024);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    let data = vec![0xA5u8; SECTOR_SIZE];
    disk.write_sectors(2, &data).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = Qcow2Disk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(2, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn qcow2_rejects_absurd_l1_table_size() {
    // Ensure we fail fast without attempting to allocate a huge L1 table.
    let cluster_bits = 9u32; // 512-byte clusters (smallest allowed)
    let virtual_size = 1u64 << 40; // 1 TiB -> requires an enormous L1 table at 512B clusters

    let mut backend = MemBackend::with_len(104).unwrap();
    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 3); // version
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, virtual_size);
    write_be_u32(&mut header, 36, 0x0200_0000); // l1_size must be >= required_l1 (33,554,432)
    write_be_u64(&mut header, 40, SECTOR_SIZE as u64); // l1_table_offset (won't be read on failure)
    write_be_u64(&mut header, 48, SECTOR_SIZE as u64); // refcount_table_offset (won't be read on failure)
    write_be_u32(&mut header, 56, 1); // refcount_table_clusters
    write_be_u64(&mut header, 72, 0); // incompatible_features
    write_be_u32(&mut header, 96, 4); // refcount_order
    write_be_u32(&mut header, 100, 104); // header_length
    backend.write_at(0, &header).unwrap();

    let err = Qcow2Disk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported(_)));
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
fn qcow2_allocates_l2_table_when_missing() {
    let virtual_size = 64 * 1024;
    let cluster_size = 1u64 << 12;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;

    let mut backend = make_qcow2_empty_without_l2(virtual_size);
    let initial_len = backend.len().unwrap();
    assert_eq!(initial_len, cluster_size * 4);

    let mut disk = Qcow2Disk::open(backend).unwrap();

    let data = vec![0xABu8; SECTOR_SIZE];
    disk.write_sectors(0, &data).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();

    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;
    assert_eq!(final_len, cluster_size * 6);

    // L1 entry should now point at the newly allocated L2 table.
    let mut l1_entry_bytes = [0u8; 8];
    backend
        .read_at(l1_table_offset, &mut l1_entry_bytes)
        .unwrap();
    let l1_entry = u64::from_be_bytes(l1_entry_bytes);
    assert_eq!(l1_entry, l2_table_offset | QCOW2_OFLAG_COPIED);

    // L2 entry 0 should now point at the newly allocated data cluster.
    let mut l2_entry_bytes = [0u8; 8];
    backend
        .read_at(l2_table_offset, &mut l2_entry_bytes)
        .unwrap();
    let l2_entry = u64::from_be_bytes(l2_entry_bytes);
    assert_eq!(l2_entry, data_cluster_offset | QCOW2_OFLAG_COPIED);

    // Refcount block should mark the new clusters (L2 + data) as in-use.
    let mut refcounts = [0u8; 4];
    backend
        .read_at(refcount_block_offset + 4 * 2, &mut refcounts)
        .unwrap();
    assert_eq!(refcounts, [0, 1, 0, 1]);

    // Persistence check.
    let mut reopened = Qcow2Disk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(0, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn qcow2_allocates_new_refcount_block_when_needed() {
    let virtual_size = 64 * 1024;
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;
    let l2_table_offset = cluster_size * 4;

    // Create a file whose physical size forces the next allocation into cluster_index=2048
    // (which requires allocating a new refcount block at block_index=1).
    let mut backend = make_qcow2_empty(virtual_size);
    backend.set_len(cluster_size * 2048).unwrap();
    let initial_len = backend.len().unwrap();
    assert_eq!(initial_len, cluster_size * 2048);

    let mut disk = Qcow2Disk::open(backend).unwrap();
    let data = vec![0x5Au8; SECTOR_SIZE];
    disk.write_sectors(0, &data).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();
    let final_len = backend.len().unwrap();

    let data_cluster_offset = cluster_size * 2048;
    let new_refcount_block_offset = cluster_size * 2049;
    assert_eq!(final_len, cluster_size * 2050);

    // L2 entry 0 should now point at the newly allocated (very high offset) data cluster.
    let mut l2_entry_bytes = [0u8; 8];
    backend
        .read_at(l2_table_offset, &mut l2_entry_bytes)
        .unwrap();
    let l2_entry = u64::from_be_bytes(l2_entry_bytes);
    assert_eq!(l2_entry, data_cluster_offset | QCOW2_OFLAG_COPIED);

    // Refcount table entry 1 should now point at the newly allocated refcount block.
    let mut refcount_table_entry_bytes = [0u8; 8];
    backend
        .read_at(refcount_table_offset + 8, &mut refcount_table_entry_bytes)
        .unwrap();
    let refcount_table_entry = u64::from_be_bytes(refcount_table_entry_bytes);
    assert_eq!(refcount_table_entry, new_refcount_block_offset);

    // The new refcount block must mark:
    // - entry 0 (cluster_index=2048) as in-use (data cluster)
    // - entry 1 (cluster_index=2049) as in-use (the refcount block itself)
    let mut refcounts = [0u8; 4];
    backend
        .read_at(new_refcount_block_offset, &mut refcounts)
        .unwrap();
    assert_eq!(refcounts, [0, 1, 0, 1]);
}

#[test]
fn qcow2_read_at_merges_contiguous_data_clusters() {
    let backend = make_qcow2_two_contiguous_data_clusters();

    let reads = Arc::new(AtomicU64::new(0));
    let backend = CountingBackend::new(backend, reads.clone());
    let mut disk = Qcow2Disk::open(backend).unwrap();

    // Force the L2 table to be loaded/cached by reading an unallocated cluster.
    // This should not trigger any data cluster reads.
    reads.store(0, Ordering::Relaxed);
    let mut tmp = [0u8; 1];
    disk.read_at((1u64 << 12) * 2, &mut tmp).unwrap();
    reads.store(0, Ordering::Relaxed);

    // Read two full guest clusters at once. The fixture arranges for those two clusters to be
    // backed by contiguous physical clusters, so the implementation should be able to merge
    // them into a single backend `read_at` call.
    let mut buf = vec![0u8; (1 << 12) * 2];
    disk.read_at(0, &mut buf).unwrap();
    assert_eq!(reads.load(Ordering::Relaxed), 1);

    assert!(buf[..(1 << 12)].iter().all(|b| *b == 0xA5));
    assert!(buf[(1 << 12)..].iter().all(|b| *b == 0x5A));
}

#[test]
fn qcow2_failed_l2_entry_write_does_not_leave_cached_mapping() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;

    // Fail the 8-byte L2 entry update for guest cluster 0.
    let backend = make_qcow2_empty(64 * 1024);
    let backend = FailOnWriteBackend::new(backend, l2_table_offset, 8);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    let data = vec![0x55u8; SECTOR_SIZE];

    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Retry should still fail (the failed metadata update must not have been cached).
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Since the L2 entry was never persisted, reads must still return zeros.
    let mut back = vec![0xAAu8; SECTOR_SIZE];
    disk.read_sectors(0, &mut back).unwrap();
    assert!(back.iter().all(|b| *b == 0));
}

#[test]
fn qcow2_failed_l1_entry_write_does_not_leave_cached_l1_entry() {
    let cluster_size = 1u64 << 12;
    let l1_table_offset = cluster_size * 2;

    // Fail the 8-byte L1 entry update for guest cluster 0 (allocating the missing L2 table).
    let backend = make_qcow2_empty_without_l2(64 * 1024);
    let backend = FailOnWriteBackend::new(backend, l1_table_offset, 8);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    let data = vec![0x11u8; SECTOR_SIZE];

    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Retry should still fail (the failed L1 update must not have been cached).
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Since the L1 entry was never persisted, reads must still return zeros.
    let mut back = vec![0xAAu8; SECTOR_SIZE];
    disk.read_sectors(0, &mut back).unwrap();
    assert!(back.iter().all(|b| *b == 0));
}

#[test]
fn qcow2_failed_refcount_table_entry_write_does_not_leave_cached_entry() {
    let cluster_size = 1u64 << 12;
    let refcount_table_offset = cluster_size;

    // Force the next allocation into cluster_index=2048, which requires allocating a new
    // refcount block at refcount_table index 1.
    let mut backend = make_qcow2_empty(64 * 1024);
    backend.set_len(cluster_size * 2048).unwrap();

    // Fail the 8-byte refcount table entry update for block_index=1.
    let backend = FailOnWriteBackend::new(backend, refcount_table_offset + 8, 8);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    let data = vec![0x22u8; SECTOR_SIZE];
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Retry should still fail (the failed table entry must not have been cached).
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Since the mapping never completed, reads must still return zeros.
    let mut back = vec![0xAAu8; SECTOR_SIZE];
    disk.read_sectors(0, &mut back).unwrap();
    assert!(back.iter().all(|b| *b == 0));
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
fn vhd_fixed_footer_copy_is_supported() {
    let backend = make_vhd_fixed_with_footer_copy();
    let mut disk = VhdDisk::open(backend).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");

    // Writes should also be offset correctly and persist.
    let data = vec![0xCCu8; SECTOR_SIZE];
    disk.write_sectors(1, &data).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_fixed_footer_copy_non_identical_is_supported() {
    let backend = make_vhd_fixed_with_footer_copy_non_identical();
    let mut disk = VhdDisk::open(backend).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");

    // Writes should also be offset correctly and persist.
    let data = vec![0xCCu8; SECTOR_SIZE];
    disk.write_sectors(1, &data).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn qcow2_unaligned_write_at_roundtrip_and_zero_fill() {
    let cluster_size = 1u64 << 12;

    let mut backend = make_qcow2_empty(64 * 1024);
    let initial_len = backend.len().unwrap();
    assert_eq!(initial_len, cluster_size * 5);

    let mut disk = Qcow2Disk::open(backend).unwrap();

    disk.write_at(123, &[1, 2, 3, 4]).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = Qcow2Disk::open(backend).unwrap();

    let mut back = [0u8; 4];
    reopened.read_at(123, &mut back).unwrap();
    assert_eq!(back, [1, 2, 3, 4]);

    // The remainder of the allocated cluster should still read as zero.
    let mut surrounding = [0xFFu8; 32];
    reopened
        .read_at(cluster_size - 16, &mut surrounding)
        .unwrap();
    assert!(surrounding.iter().all(|b| *b == 0));
}

#[test]
fn qcow2_write_at_spanning_clusters_roundtrip() {
    let cluster_size = 1u64 << 12;

    let backend = make_qcow2_empty(64 * 1024);
    let mut disk = Qcow2Disk::open(backend).unwrap();

    // Write across a cluster boundary (last 10 bytes of cluster 0, first 10 of cluster 1).
    let mut pattern = [0u8; 20];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = i as u8;
    }
    disk.write_at(cluster_size - 10, &pattern).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = Qcow2Disk::open(backend).unwrap();

    let mut back = [0u8; 20];
    reopened.read_at(cluster_size - 10, &mut back).unwrap();
    assert_eq!(back, pattern);

    // Verify surrounding bytes remain zero.
    let mut window = [0u8; 40];
    reopened.read_at(cluster_size - 20, &mut window).unwrap();
    assert!(window[0..10].iter().all(|b| *b == 0));
    assert_eq!(&window[10..30], &pattern);
    assert!(window[30..].iter().all(|b| *b == 0));
}

#[test]
fn qcow2_zero_cluster_flag_is_treated_as_unallocated_and_can_be_written() {
    let cluster_size = 1u64 << 12;
    let l2_table_offset = cluster_size * 4;

    let mut backend = make_qcow2_empty(64 * 1024);
    // Mark guest cluster 0 as a v3 "zero cluster".
    backend
        .write_at(l2_table_offset, &QCOW2_OFLAG_ZERO.to_be_bytes())
        .unwrap();

    let mut disk = Qcow2Disk::open(backend).unwrap();

    // Reads should still return zeros.
    let mut sector = [0xAAu8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector).unwrap();
    assert!(sector.iter().all(|b| *b == 0));

    // Writes should allocate a real data cluster and clear the zero flag.
    let data = vec![0xCCu8; SECTOR_SIZE];
    disk.write_sectors(0, &data).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();
    let expected_data_cluster_offset = cluster_size * 5;

    let mut new_l2_entry_bytes = [0u8; 8];
    backend
        .read_at(l2_table_offset, &mut new_l2_entry_bytes)
        .unwrap();
    let new_l2_entry = u64::from_be_bytes(new_l2_entry_bytes);
    assert_eq!(
        new_l2_entry,
        expected_data_cluster_offset | QCOW2_OFLAG_COPIED
    );

    let mut reopened = Qcow2Disk::open(backend).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(0, &mut back).unwrap();
    assert_eq!(back, data);
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
fn vhd_fixed_rejects_bad_file_format_version() {
    let virtual_size = 64 * 1024u64;
    let mut backend = make_vhd_fixed_with_pattern();

    let footer_offset = virtual_size;
    let mut footer = [0u8; SECTOR_SIZE];
    backend.read_at(footer_offset, &mut footer).unwrap();

    // Corrupt file_format_version at offset 12..16 and fix up checksum.
    footer[12..16].copy_from_slice(&0u32.to_be_bytes());
    let checksum = vhd_footer_checksum(&footer);
    footer[64..68].copy_from_slice(&checksum.to_be_bytes());
    backend.write_at(footer_offset, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::Unsupported("vhd file format version")
    ));
}

#[test]
fn vhd_fixed_rejects_data_offset_not_max() {
    let virtual_size = 64 * 1024u64;
    let mut backend = make_vhd_fixed_with_pattern();

    let footer_offset = virtual_size;
    let mut footer = [0u8; SECTOR_SIZE];
    backend.read_at(footer_offset, &mut footer).unwrap();

    // Set data_offset to 0 (invalid for fixed) and fix up checksum.
    footer[16..24].copy_from_slice(&0u64.to_be_bytes());
    let checksum = vhd_footer_checksum(&footer);
    footer[64..68].copy_from_slice(&checksum.to_be_bytes());
    backend.write_at(footer_offset, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd fixed data_offset invalid")
    ));
}

#[test]
fn vhd_rejects_unsupported_disk_type() {
    let virtual_size = 64 * 1024u64;

    // disk_type values other than 2/3/4 are unsupported.
    let footer = make_vhd_footer(virtual_size, 5, 0);
    let mut backend = MemBackend::with_len(SECTOR_SIZE as u64).unwrap();
    backend.write_at(0, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported("vhd disk type")));
}

#[test]
fn vhd_rejects_file_too_small_to_contain_footer() {
    let backend = MemBackend::default();
    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::CorruptImage("vhd file too small")));
}

#[test]
fn vhd_rejects_footer_truncated_when_backend_len_is_stale() {
    // Simulate a backend that reports a length large enough for a footer, but then fails reads with
    // OutOfBounds (e.g. file shrank between len() and read_at()).
    struct StaleLenBackend {
        reported_len: u64,
    }

    impl StorageBackend for StaleLenBackend {
        fn len(&mut self) -> aero_storage::Result<u64> {
            Ok(self.reported_len)
        }

        fn set_len(&mut self, _len: u64) -> aero_storage::Result<()> {
            Err(DiskError::NotSupported(
                "set_len not supported for stale-len test backend".into(),
            ))
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
            Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: 0,
            })
        }

        fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
            Err(DiskError::NotSupported(
                "write_at not supported for stale-len test backend".into(),
            ))
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            Ok(())
        }
    }

    let backend = StaleLenBackend {
        reported_len: SECTOR_SIZE as u64,
    };
    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd footer truncated")
    ));
}

#[test]
fn vhd_fixed_rejects_truncated_disk_missing_data_region() {
    // Footer claims a 1KiB fixed disk but the file only contains the footer sector.
    let virtual_size = 2 * SECTOR_SIZE as u64;
    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);
    let mut backend = MemBackend::with_len(SECTOR_SIZE as u64).unwrap();
    backend.write_at(0, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd fixed disk truncated")
    ));
}

#[test]
fn vhd_fixed_rejects_current_size_overflow() {
    // Construct a footer where the advertised disk size causes `current_size + footer_len` to
    // overflow u64 during open-time validation.
    let virtual_size = u64::MAX - (SECTOR_SIZE as u64) + 1; // 2^64 - 512, sector aligned
    assert!(virtual_size.is_multiple_of(SECTOR_SIZE as u64));

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);
    let mut backend = MemBackend::with_len(SECTOR_SIZE as u64).unwrap();
    backend.write_at(0, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd current_size overflow")
    ));
}

#[test]
fn vhd_rejects_footer_cookie_mismatch() {
    let virtual_size = 64 * 1024u64;
    let mut backend = make_vhd_fixed_with_pattern();

    // Corrupt the EOF footer cookie without touching any other fields.
    backend.write_at(virtual_size, b"BADFOOT!").unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd footer cookie mismatch")
    ));
}

#[test]
fn vhd_rejects_invalid_current_size_field() {
    let virtual_size = 64 * 1024u64;
    let mut backend = make_vhd_fixed_with_pattern();

    // current_size is at 48..56 in the footer.
    let footer_offset = virtual_size;
    let mut footer = [0u8; SECTOR_SIZE];
    backend.read_at(footer_offset, &mut footer).unwrap();
    footer[48..56].copy_from_slice(&0u64.to_be_bytes());
    let checksum = vhd_footer_checksum(&footer);
    footer[64..68].copy_from_slice(&checksum.to_be_bytes());
    backend.write_at(footer_offset, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd current_size invalid")
    ));
}

#[test]
fn vhd_dynamic_rejects_invalid_dynamic_header_offset_in_footer() {
    let virtual_size = 64 * 1024u64;
    let footer = make_vhd_footer(virtual_size, 3, u64::MAX);
    let mut backend = MemBackend::with_len(SECTOR_SIZE as u64).unwrap();
    backend.write_at(0, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header offset invalid")
    ));
}

#[test]
fn vhd_dynamic_rejects_truncated_dynamic_header() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Point the dynamic header at the EOF footer so the required 1024-byte header extends past EOF.
    let file_len = backend.len().unwrap();
    let footer_offset = file_len - SECTOR_SIZE as u64;
    let footer = make_vhd_footer(virtual_size, 3, footer_offset);
    backend.write_at(0, &footer).unwrap();
    backend.write_at(footer_offset, &footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header truncated")
    ));
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
fn vhd_dynamic_unaligned_write_at_roundtrip() {
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(64 * 1024, block_size);
    let initial_len = backend.len().unwrap();

    let mut disk = VhdDisk::open(backend).unwrap();
    disk.write_at(3, &[9, 8, 7, 6]).unwrap();
    disk.flush().unwrap();

    let backend = disk.into_backend();
    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back = [0u8; 4];
    reopened.read_at(3, &mut back).unwrap();
    assert_eq!(back, [9, 8, 7, 6]);

    // Image must have grown due to block allocation.
    let mut backend = reopened.into_backend();
    assert!(backend.len().unwrap() > initial_len);
}

#[test]
fn vhd_dynamic_write_at_spanning_two_sectors_sets_bitmap_bits() {
    let block_size = 16 * 1024u32;
    let backend = make_vhd_dynamic_empty(64 * 1024, block_size);
    let mut disk = VhdDisk::open(backend).unwrap();

    // Write across sector 0 and 1.
    let data = vec![0x11u8; 200];
    disk.write_at((SECTOR_SIZE as u64) - 100, &data).unwrap();
    disk.flush().unwrap();

    let mut backend = disk.into_backend();

    // The first allocation for this fixture should start at the old footer offset.
    let dyn_header_offset = SECTOR_SIZE as u64;
    let table_offset = dyn_header_offset + 1024;
    let bat_size = SECTOR_SIZE as u64; // 4 entries padded to 512
    let block_start = (SECTOR_SIZE as u64) + 1024 + bat_size; // old footer offset

    let mut bat_entry_bytes = [0u8; 4];
    backend.read_at(table_offset, &mut bat_entry_bytes).unwrap();
    let bat_entry = u32::from_be_bytes(bat_entry_bytes);
    assert_eq!(bat_entry, (block_start / SECTOR_SIZE as u64) as u32);

    // Bitmap should have sectors 0 and 1 marked as present (bits 7 and 6).
    let mut bitmap_first = [0u8; 1];
    backend.read_at(block_start, &mut bitmap_first).unwrap();
    assert_eq!(bitmap_first[0], 0xC0);
}

#[test]
fn vhd_dynamic_write_updates_single_bitmap_byte() {
    let backend = make_vhd_dynamic_with_pattern();

    let writes = Arc::new(Mutex::new(Vec::new()));
    let backend = WriteTraceBackend::new(backend, writes.clone());
    let mut disk = VhdDisk::open(backend).unwrap();

    // Fixture layout (see `make_vhd_dynamic_with_pattern`):
    // - dynamic header at 512
    // - BAT at 1536 (padded to 512 bytes)
    // - first allocated block placed at the old footer offset: 2048
    // - bitmap size is 512 bytes, so data starts at 2560.
    let block_start = (SECTOR_SIZE as u64) + 1024 + (SECTOR_SIZE as u64);

    // Write to sector 1, which is in the already-allocated block and will require flipping a
    // bitmap bit. The bitmap update should write back only the single changed bitmap byte.
    let data = vec![0x11u8; SECTOR_SIZE];
    disk.write_sectors(1, &data).unwrap();
    disk.flush().unwrap();

    let writes = writes.lock().unwrap();
    assert!(
        writes
            .iter()
            .any(|(off, len)| *off == block_start && *len == 1),
        "expected a 1-byte bitmap update write at block_start; writes={writes:?}"
    );
    assert!(
        !writes
            .iter()
            .any(|(off, len)| *off == block_start && *len == SECTOR_SIZE),
        "bitmap should not be rewritten fully; writes={writes:?}"
    );
}

#[test]
fn vhd_dynamic_failed_bitmap_write_rolls_back_cached_bit() {
    let backend = make_vhd_dynamic_with_pattern();

    // Fixture layout (see `make_vhd_dynamic_with_pattern`):
    // - dynamic header at 512
    // - BAT at 1536 (padded to 512 bytes)
    // - first allocated block placed at the old footer offset: 2048
    // - bitmap size is 512 bytes, so data starts at 2560.
    let block_start = (SECTOR_SIZE as u64) + 1024 + (SECTOR_SIZE as u64);

    // Fail specifically the 1-byte bitmap write at the start of the bitmap.
    let backend = FailOnWriteBackend::new(backend, block_start, 1);
    let mut disk = VhdDisk::open(backend).unwrap();

    let data = vec![0x55u8; SECTOR_SIZE];
    let err = disk.write_sectors(1, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Even though the data write may have succeeded, the bitmap bit was not persisted and should
    // also have been rolled back in the in-memory cache, so reads must still return zeros.
    let mut back = vec![0xAAu8; SECTOR_SIZE];
    disk.read_sectors(1, &mut back).unwrap();
    assert!(back.iter().all(|b| *b == 0));
}

#[test]
fn vhd_dynamic_failed_bat_write_does_not_leave_cached_bat_entry() {
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(64 * 1024, block_size);
    let initial_len = backend.len().unwrap();

    // Fixture layout (see `make_vhd_dynamic_empty`):
    // - dynamic header at 512
    // - BAT at 1536 (padded to 512 bytes)
    let table_offset = (SECTOR_SIZE as u64) + 1024u64;

    // Fail specifically the 4-byte BAT entry update for block 0.
    let backend = FailOnWriteBackend::new(backend, table_offset, 4);
    let mut disk = VhdDisk::open(backend).unwrap();

    let data = vec![0x11u8; SECTOR_SIZE];
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Retry should still fail (the failed BAT write must not have been cached).
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // Reads must still return zeros because the block is still unallocated in-memory.
    let mut back = vec![0xAAu8; SECTOR_SIZE];
    disk.read_sectors(0, &mut back).unwrap();
    assert!(back.iter().all(|b| *b == 0));

    // The failed allocation must not corrupt the VHD footer at EOF: the image should remain
    // openable even after the error.
    let mut backend = disk.into_backend();
    // Failed allocation should also not permanently grow the image: we can roll back because the
    // BAT entry update never committed.
    assert_eq!(backend.len().unwrap(), initial_len);
    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back2 = vec![0xAAu8; SECTOR_SIZE];
    reopened.read_sectors(0, &mut back2).unwrap();
    assert!(back2.iter().all(|b| *b == 0));
}

#[test]
fn vhd_dynamic_failed_bitmap_init_rolls_back_allocation() {
    // Use a large block_size so bitmap_size > 512 and we can inject a failure that doesn't also
    // trip the footer-restore write (which is exactly 512 bytes).
    let block_size = 4 * 1024 * 1024u32; // 4 MiB => bitmap_size=1024 bytes

    let mut backend = make_vhd_dynamic_empty(64 * 1024, block_size);
    let initial_len = backend.len().unwrap();
    let old_footer_offset = initial_len - (SECTOR_SIZE as u64);
    let sectors_per_block = (block_size as u64) / SECTOR_SIZE as u64;
    let bitmap_bytes = sectors_per_block.div_ceil(8);
    let bitmap_size = bitmap_bytes.div_ceil(SECTOR_SIZE as u64) * SECTOR_SIZE as u64;
    let bitmap_size: usize = bitmap_size.try_into().unwrap();

    // Fail the bitmap initialization write.
    let backend = FailOnWriteBackend::new(backend, old_footer_offset, bitmap_size);
    let mut disk = VhdDisk::open(backend).unwrap();

    let data = vec![0x11u8; SECTOR_SIZE];
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    let mut backend = disk.into_backend();
    assert_eq!(backend.len().unwrap(), initial_len);

    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back = vec![0xAAu8; SECTOR_SIZE];
    reopened.read_sectors(0, &mut back).unwrap();
    assert!(back.iter().all(|b| *b == 0));
}

#[test]
fn vhd_dynamic_failed_footer_write_rolls_back_resize() {
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(64 * 1024, block_size);
    let initial_len = backend.len().unwrap();

    // Compute where the new EOF footer would be written for the first allocated block.
    let old_footer_offset = initial_len - (SECTOR_SIZE as u64);
    let sectors_per_block = (block_size as u64) / SECTOR_SIZE as u64;
    let bitmap_bytes = sectors_per_block.div_ceil(8);
    let bitmap_size = bitmap_bytes.div_ceil(SECTOR_SIZE as u64) * SECTOR_SIZE as u64;
    let new_footer_offset = old_footer_offset + bitmap_size + block_size as u64;

    // Fail the 512-byte footer write to the new end-of-file.
    let backend = FailOnWriteBackend::new(backend, new_footer_offset, SECTOR_SIZE);
    let mut disk = VhdDisk::open(backend).unwrap();

    let data = vec![0x11u8; SECTOR_SIZE];
    let err = disk.write_sectors(0, &data).unwrap_err();
    assert!(matches!(err, DiskError::Io(_)));

    // The allocation should have rolled back the file resize so the original footer remains
    // at EOF and the image is still openable.
    let mut backend = disk.into_backend();
    assert_eq!(backend.len().unwrap(), initial_len);

    let mut reopened = VhdDisk::open(backend).unwrap();
    let mut back = vec![0xAAu8; SECTOR_SIZE];
    reopened.read_sectors(0, &mut back).unwrap();
    assert!(back.iter().all(|b| *b == 0));
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

    // Verify BAT + bitmap were updated for block 0.
    let dyn_header_offset = SECTOR_SIZE as u64;
    let table_offset = dyn_header_offset + 1024;
    let bat_size = SECTOR_SIZE as u64; // 4 entries padded to 512
    let old_footer_offset = (SECTOR_SIZE as u64) + 1024 + bat_size;
    let bitmap_size = SECTOR_SIZE as u64;
    let block_total_size = bitmap_size + 16 * 1024u64;
    let new_footer_offset = old_footer_offset + block_total_size;

    assert_eq!(final_len, new_footer_offset + SECTOR_SIZE as u64);

    let mut bat_entry_bytes = [0u8; 4];
    backend.read_at(table_offset, &mut bat_entry_bytes).unwrap();
    let bat_entry = u32::from_be_bytes(bat_entry_bytes);
    assert_eq!(bat_entry, (old_footer_offset / SECTOR_SIZE as u64) as u32);

    let mut bitmap_first = [0u8; 1];
    backend
        .read_at(old_footer_offset, &mut bitmap_first)
        .unwrap();
    assert_eq!(bitmap_first[0], 0x80);

    // Reading another sector in the same allocated block that is not marked present must yield
    // zeros (bitmap bit is still 0).
    let mut disk = VhdDisk::open(backend).unwrap();
    let mut sector1 = [0xAAu8; SECTOR_SIZE];
    disk.read_sectors(1, &mut sector1).unwrap();
    assert!(sector1.iter().all(|b| *b == 0));
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
fn vhd_differencing_reads_fall_back_to_parent_and_writes_overlay() {
    let virtual_size = 64 * 1024u64;

    let mut parent_backend = make_vhd_fixed_with_pattern();
    parent_backend
        .write_at(SECTOR_SIZE as u64, b"parent-s1")
        .unwrap();

    let parent_disk = VhdDisk::open(parent_backend).unwrap();
    let parent = Arc::new(Mutex::new(parent_disk));

    let child_backend = make_vhd_differencing_empty(virtual_size, 16 * 1024);
    let parent_view = SharedReadOnlyDisk {
        inner: parent.clone(),
    };
    let mut disk = VhdDisk::open_differencing(child_backend, Box::new(parent_view)).unwrap();

    // Before any allocations, all reads come from the parent.
    let mut sector0 = [0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut sector0).unwrap();
    assert_eq!(&sector0[..10], b"hello vhd!");

    let mut sector1 = [0u8; SECTOR_SIZE];
    disk.read_sectors(1, &mut sector1).unwrap();
    assert_eq!(&sector1[..9], b"parent-s1");

    // Write an overlay sector. This should allocate in the child only.
    let mut write0 = vec![0u8; SECTOR_SIZE];
    write0[..14].copy_from_slice(b"child-overlay!");
    disk.write_sectors(0, &write0).unwrap();

    let mut back0 = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut back0).unwrap();
    assert_eq!(back0, write0);

    // Another sector in the same allocated block is still unallocated in the child and must fall
    // back to the parent (not zeros).
    let mut back1 = [0u8; SECTOR_SIZE];
    disk.read_sectors(1, &mut back1).unwrap();
    assert_eq!(&back1[..9], b"parent-s1");

    // Parent must remain unchanged.
    let mut p0 = [0u8; SECTOR_SIZE];
    let mut p1 = [0u8; SECTOR_SIZE];
    {
        let mut parent_guard = parent.lock().unwrap();
        parent_guard.read_sectors(0, &mut p0).unwrap();
        parent_guard.read_sectors(1, &mut p1).unwrap();
    }
    assert_eq!(&p0[..10], b"hello vhd!");
    assert_eq!(&p1[..9], b"parent-s1");

    // Writes must persist in the differencing layer after reopen.
    disk.flush().unwrap();
    let backend = disk.into_backend();
    let parent_view = SharedReadOnlyDisk {
        inner: parent.clone(),
    };
    let mut reopened = VhdDisk::open_differencing(backend, Box::new(parent_view)).unwrap();

    let mut back0_re = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(0, &mut back0_re).unwrap();
    assert_eq!(back0_re, write0);

    let mut back1_re = [0u8; SECTOR_SIZE];
    reopened.read_sectors(1, &mut back1_re).unwrap();
    assert_eq!(&back1_re[..9], b"parent-s1");
}

#[test]
fn vhd_differencing_open_auto_is_rejected_with_hint() {
    let virtual_size = 64 * 1024u64;
    let mut backend = make_vhd_differencing_empty(virtual_size, 16 * 1024);
    assert_eq!(detect_format(&mut backend).unwrap(), DiskFormat::Vhd);

    let err = DiskImage::open_auto(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::Unsupported(
            "vhd differencing disks require explicit parent (use VhdDisk::open_with_parent/open_differencing)"
        )
    ));
}

#[test]
fn vhd_open_with_parent_rejects_non_differencing_disk() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let parent = RawDisk::create(MemBackend::new(), virtual_size).unwrap();
    let err = VhdDisk::open_with_parent(backend, Box::new(parent))
        .err()
        .expect("expected error");
    assert!(matches!(
        err,
        DiskError::InvalidConfig("vhd open_with_parent/open_differencing requires disk_type=4")
    ));
}

#[test]
fn vhd_open_with_parent_rejects_parent_capacity_mismatch() {
    let virtual_size = 64 * 1024u64;
    let backend = make_vhd_differencing_empty(virtual_size, 16 * 1024);

    let parent = RawDisk::create(MemBackend::new(), virtual_size - SECTOR_SIZE as u64).unwrap();
    let err = VhdDisk::open_with_parent(backend, Box::new(parent))
        .err()
        .expect("expected error");
    assert!(matches!(
        err,
        DiskError::InvalidConfig("vhd parent capacity does not match differencing disk size")
    ));
}

#[test]
fn vhd_differencing_rejects_bat_entry_pointing_into_metadata() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_differencing_empty(virtual_size, block_size);

    // Grow the file so a block starting at offset 0 would fit before the footer at EOF.
    // This ensures the failure is due to "metadata overlap", not "block overlaps footer".
    let bitmap_size = SECTOR_SIZE as u64;
    let new_len = bitmap_size + block_size as u64 + SECTOR_SIZE as u64;
    let dyn_header_offset = SECTOR_SIZE as u64;
    let footer = make_vhd_footer(virtual_size, 4, dyn_header_offset);
    backend.set_len(new_len).unwrap();
    backend.write_at(0, &footer).unwrap();
    backend
        .write_at(new_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    // Point block 0 at the start of the file (overlapping the footer copy / dynamic header / BAT).
    let table_offset = dyn_header_offset + 1024u64;
    backend.write_at(table_offset, &0u32.to_be_bytes()).unwrap();

    let parent_backend = make_vhd_fixed_with_pattern();
    let parent_disk = VhdDisk::open(parent_backend).unwrap();
    let err = VhdDisk::open_differencing(backend, Box::new(parent_disk))
        .err()
        .expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd block overlaps metadata")
    ));
}

#[test]
fn vhd_rejects_absurd_bat_size() {
    // Ensure we fail fast without allocating a huge BAT.
    let virtual_size = 20u64 * 1024 * 1024 * 1024; // 20 GiB virtual disk
    let dyn_header_offset = SECTOR_SIZE as u64;
    let table_offset = dyn_header_offset + 1024u64;
    let file_len = table_offset + SECTOR_SIZE as u64; // footer stored at EOF (overlaps table_offset)
    let block_size = SECTOR_SIZE as u32; // smallest block size -> BAT grows with virtual size
    let required_entries = virtual_size / SECTOR_SIZE as u64;
    assert!(required_entries * 4 > 128 * 1024 * 1024);

    let mut backend = MemBackend::with_len(file_len).unwrap();

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    backend.write_at(0, &footer).unwrap();
    backend
        .write_at(file_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    let mut dyn_header = [0u8; 1024];
    dyn_header[0..8].copy_from_slice(b"cxsparse");
    write_be_u64(&mut dyn_header, 8, u64::MAX);
    write_be_u64(&mut dyn_header, 16, table_offset);
    write_be_u32(&mut dyn_header, 24, 0x0001_0000);
    write_be_u32(&mut dyn_header, 28, required_entries as u32); // max_table_entries
    write_be_u32(&mut dyn_header, 32, block_size); // block_size
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    write_be_u32(&mut dyn_header, 36, checksum);
    backend.write_at(dyn_header_offset, &dyn_header).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::Unsupported("vhd bat too large")));
}

#[test]
fn vhd_rejects_bad_footer_checksum() {
    let mut backend = make_vhd_fixed_with_pattern();
    let mut last = [0u8; 1];
    backend
        .read_at((64 * 1024) + (SECTOR_SIZE as u64) - 1, &mut last)
        .unwrap();
    last[0] ^= 0xFF;
    backend
        .write_at((64 * 1024) + (SECTOR_SIZE as u64) - 1, &last)
        .unwrap();

    let err = VhdDisk::open(backend)
        .err()
        .expect("expected vhd open to fail");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd footer checksum mismatch")
    ));
}

#[test]
fn vhd_dynamic_rejects_footer_copy_mismatch() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Corrupt the required footer copy at offset 0 while leaving the footer at EOF intact.
    let bad_footer = make_vhd_footer(virtual_size, 3, (SECTOR_SIZE as u64) * 2);
    backend.write_at(0, &bad_footer).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd footer copy mismatch")
    ));
}

#[test]
fn vhd_rejects_misaligned_file_length() {
    let backend = MemBackend::with_len((SECTOR_SIZE as u64) + 1).unwrap();
    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd file length misaligned")
    ));
}

#[test]
fn vhd_dynamic_rejects_block_overlapping_footer() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Force a bogus BAT entry that points at the current end-of-file footer (but the file has not
    // been grown to fit a whole block there).
    let table_offset = (SECTOR_SIZE as u64) + 1024u64;
    let file_len = backend.len().unwrap();
    let footer_offset = file_len - SECTOR_SIZE as u64;
    let bat_entry = (footer_offset / SECTOR_SIZE as u64) as u32;
    backend
        .write_at(table_offset, &bat_entry.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd block overlaps footer")
    ));
}

#[test]
fn vhd_dynamic_rejects_bat_entry_pointing_into_metadata() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Grow the file so a block starting at offset 0 would fit before the footer at EOF.
    // This ensures the failure is due to "metadata overlap", not "block overlaps footer".
    let bitmap_size = SECTOR_SIZE as u64; // for 16 KiB blocks (32 sectors) => 512-aligned bitmap
    let new_len = bitmap_size + block_size as u64 + SECTOR_SIZE as u64;
    let dyn_header_offset = SECTOR_SIZE as u64;
    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    backend.set_len(new_len).unwrap();
    backend
        .write_at(new_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    // Point block 0 at the start of the file (overlapping the footer copy / dynamic header / BAT).
    let table_offset = dyn_header_offset + 1024u64;
    backend.write_at(table_offset, &0u32.to_be_bytes()).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd block overlaps metadata")
    ));
}

#[test]
fn vhd_dynamic_rejects_overlapping_blocks() {
    let mut backend = make_vhd_dynamic_with_pattern();

    // Duplicate the BAT entry for block 0 into block 1. This makes two virtual blocks alias the
    // same on-disk block region, which must be treated as corruption.
    let table_offset = (SECTOR_SIZE as u64) + 1024u64;
    let mut entry0 = [0u8; 4];
    backend.read_at(table_offset, &mut entry0).unwrap();
    backend.write_at(table_offset + 4, &entry0).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::CorruptImage("vhd blocks overlap")));
}

#[test]
fn vhd_dynamic_zero_writes_do_not_hide_corrupt_bat_entries() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let bitmap_size = SECTOR_SIZE as u64;
    let new_len = bitmap_size + block_size as u64 + SECTOR_SIZE as u64;
    let dyn_header_offset = SECTOR_SIZE as u64;
    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    backend.set_len(new_len).unwrap();
    backend
        .write_at(new_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    let table_offset = dyn_header_offset + 1024u64;
    backend.write_at(table_offset, &0u32.to_be_bytes()).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd block overlaps metadata")
    ));
}

#[test]
fn vhd_dynamic_rejects_truncated_bat_when_max_table_entries_exceeds_file() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Inflate `max_table_entries` so the declared BAT region would extend into the EOF footer.
    let dyn_header_offset = SECTOR_SIZE as u64;
    backend
        .write_at(dyn_header_offset + 28, &130u32.to_be_bytes())
        .unwrap();
    // Update the dynamic header checksum so we exercise the intended BAT truncation path.
    let mut dyn_header = [0u8; 1024];
    backend.read_at(dyn_header_offset, &mut dyn_header).unwrap();
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    backend
        .write_at(dyn_header_offset + 36, &checksum.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::CorruptImage("vhd bat truncated")));
}

#[test]
fn vhd_dynamic_rejects_bat_too_small_when_max_table_entries_is_insufficient() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Reduce max_table_entries below the required number of blocks for the advertised virtual size.
    // This should be rejected before any BAT reads.
    let dyn_header_offset = SECTOR_SIZE as u64;
    let mut dyn_header = [0u8; 1024];
    backend.read_at(dyn_header_offset, &mut dyn_header).unwrap();
    write_be_u32(&mut dyn_header, 28, 3); // required_entries is 4 for 64KiB/16KiB
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    write_be_u32(&mut dyn_header, 36, checksum);
    backend.write_at(dyn_header_offset, &dyn_header).unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(err, DiskError::CorruptImage("vhd bat too small")));
}

#[test]
fn vhd_dynamic_rejects_bat_overlapping_dynamic_header() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Point the BAT at the start of the dynamic header (overlapping it).
    let dyn_header_offset = SECTOR_SIZE as u64;
    backend
        .write_at(dyn_header_offset + 16, &dyn_header_offset.to_be_bytes())
        .unwrap();
    let mut dyn_header = [0u8; 1024];
    backend.read_at(dyn_header_offset, &mut dyn_header).unwrap();
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    backend
        .write_at(dyn_header_offset + 36, &checksum.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd bat overlaps dynamic header")
    ));
}

#[test]
fn vhd_dynamic_rejects_bat_overlapping_footer_copy() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Point the BAT at the start of the file (overlapping the required footer copy).
    let dyn_header_offset = SECTOR_SIZE as u64;
    backend
        .write_at(dyn_header_offset + 16, &0u64.to_be_bytes())
        .unwrap();
    let mut dyn_header = [0u8; 1024];
    backend.read_at(dyn_header_offset, &mut dyn_header).unwrap();
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    backend
        .write_at(dyn_header_offset + 36, &checksum.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd bat overlaps footer copy")
    ));
}

#[test]
fn vhd_dynamic_rejects_bad_dynamic_header_cookie() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let dyn_header_offset = SECTOR_SIZE as u64;
    backend.write_at(dyn_header_offset, b"BADHDR!!").unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header cookie mismatch")
    ));
}

#[test]
fn vhd_dynamic_rejects_bad_dynamic_header_version() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let dyn_header_offset = SECTOR_SIZE as u64;
    // dynamic header version is at offset 24 in the dynamic header.
    backend
        .write_at(dyn_header_offset + 24, &0u32.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::Unsupported("vhd dynamic header version")
    ));
}

#[test]
fn vhd_dynamic_rejects_bad_dynamic_header_data_offset() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let dyn_header_offset = SECTOR_SIZE as u64;
    // `data_offset` is at 8..16 in the dynamic header and must be 0xFFFF..FFFF.
    backend
        .write_at(dyn_header_offset + 8, &0u64.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header data_offset invalid")
    ));
}

#[test]
fn vhd_dynamic_rejects_bad_dynamic_header_checksum() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // The dynamic header checksum is at offset 36 in the 1024-byte header. Corrupt it without
    // changing any other fields so parsing reaches the checksum validation path.
    let dyn_header_offset = SECTOR_SIZE as u64;
    let mut checksum = [0u8; 4];
    backend
        .read_at(dyn_header_offset + 36, &mut checksum)
        .unwrap();
    let bad = u32::from_be_bytes(checksum) ^ 1;
    backend
        .write_at(dyn_header_offset + 36, &bad.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header checksum mismatch")
    ));
}

#[test]
fn vhd_dynamic_rejects_misaligned_bat_offset() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // BAT offset is at 16..24 in the dynamic header. Make it intentionally misaligned.
    let dyn_header_offset = SECTOR_SIZE as u64;
    let mut table_offset = [0u8; 8];
    backend
        .read_at(dyn_header_offset + 16, &mut table_offset)
        .unwrap();
    let table_offset = u64::from_be_bytes(table_offset);
    backend
        .write_at(dyn_header_offset + 16, &(table_offset + 1).to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd bat offset misaligned")
    ));
}

#[test]
fn vhd_dynamic_rejects_zero_max_table_entries() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // max_table_entries is at 28..32 in the dynamic header.
    let dyn_header_offset = SECTOR_SIZE as u64;
    backend
        .write_at(dyn_header_offset + 28, &0u32.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd max_table_entries is zero")
    ));
}

#[test]
fn vhd_dynamic_rejects_invalid_block_size() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Dynamic header's block_size is at offset 32.
    let dyn_header_offset = SECTOR_SIZE as u64;
    backend
        .write_at(dyn_header_offset + 32, &123u32.to_be_bytes())
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd block_size invalid")
    ));
}

#[test]
fn vhd_dynamic_rejects_block_size_too_large() {
    let virtual_size = 64 * 1024u64;
    // 64MiB is the maximum supported dynamic VHD block size; request something larger.
    let block_size = 64 * 1024 * 1024 + SECTOR_SIZE as u32;
    let backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::Unsupported("vhd block_size too large")
    ));
}

#[test]
fn vhd_dynamic_rejects_dynamic_header_overlapping_footer_copy() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let footer = make_vhd_footer(virtual_size, 3, 0);
    let file_len = backend.len().unwrap();
    backend.write_at(0, &footer).unwrap();
    backend
        .write_at(file_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header overlaps footer copy")
    ));
}

#[test]
fn vhd_dynamic_rejects_misaligned_dynamic_header_offset() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    let footer = make_vhd_footer(virtual_size, 3, 1);
    let file_len = backend.len().unwrap();
    backend.write_at(0, &footer).unwrap();
    backend
        .write_at(file_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header offset misaligned")
    ));
}

#[test]
fn vhd_dynamic_rejects_dynamic_header_overlapping_footer() {
    let virtual_size = 64 * 1024u64;
    let block_size = 16 * 1024u32;
    let mut backend = make_vhd_dynamic_empty(virtual_size, block_size);

    // Place the dynamic header at the BAT offset so it extends into the EOF footer.
    let dyn_header_offset = (SECTOR_SIZE as u64) + 1024;
    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    let file_len = backend.len().unwrap();
    backend.write_at(0, &footer).unwrap();
    backend
        .write_at(file_len - SECTOR_SIZE as u64, &footer)
        .unwrap();

    let err = VhdDisk::open(backend).err().expect("expected error");
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd dynamic header overlaps footer")
    ));
}
