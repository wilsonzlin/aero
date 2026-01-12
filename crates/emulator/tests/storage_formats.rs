#![cfg(not(target_arch = "wasm32"))]

use emulator::io::storage::disk::ByteStorage;
use emulator::io::storage::formats::detect_format;
use emulator::io::storage::{DiskBackend, DiskError, DiskFormat, VirtualDrive, WriteCachePolicy};
use proptest::prelude::*;

const SECTOR_SIZE: usize = 512;
const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

#[derive(Default, Clone)]
struct MemStorage {
    data: Vec<u8>,
}

impl MemStorage {
    fn with_len(len: usize) -> Self {
        Self { data: vec![0; len] }
    }
}

impl ByteStorage for MemStorage {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> emulator::io::storage::DiskResult<()> {
        let offset = usize::try_from(offset).map_err(|_| DiskError::OutOfBounds)?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(DiskError::OutOfBounds)?;
        if end > self.data.len() {
            return Err(DiskError::Io("read past end".into()));
        }
        buf.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> emulator::io::storage::DiskResult<()> {
        let offset = usize::try_from(offset).map_err(|_| DiskError::OutOfBounds)?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(DiskError::OutOfBounds)?;
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> emulator::io::storage::DiskResult<()> {
        Ok(())
    }

    fn len(&mut self) -> emulator::io::storage::DiskResult<u64> {
        Ok(self.data.len() as u64)
    }

    fn set_len(&mut self, len: u64) -> emulator::io::storage::DiskResult<()> {
        let len = usize::try_from(len).map_err(|_| DiskError::OutOfBounds)?;
        self.data.resize(len, 0);
        Ok(())
    }
}

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn vhd_footer_checksum(raw: &[u8; 512]) -> u32 {
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

fn make_qcow2_empty(virtual_size: u64) -> MemStorage {
    assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);

    let cluster_bits = 16u32;
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;

    let file_len = cluster_size * 5;
    let mut storage = MemStorage::with_len(file_len as usize);

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
    storage.write_at(0, &header).unwrap();

    storage
        .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
        .unwrap();

    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    storage
        .write_at(l1_table_offset, &l1_entry.to_be_bytes())
        .unwrap();

    for cluster_index in 0u64..5 {
        let off = refcount_block_offset + cluster_index * 2;
        storage.write_at(off, &1u16.to_be_bytes()).unwrap();
    }

    storage
}

fn make_qcow2_with_pattern() -> MemStorage {
    let virtual_size = 2 * 1024 * 1024;
    let cluster_size = 1u64 << 16;

    let mut storage = make_qcow2_empty(virtual_size);
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;
    storage.set_len(cluster_size * 6).unwrap();

    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
    storage
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();

    let refcount_block_offset = cluster_size * 3;
    storage
        .write_at(refcount_block_offset + 5 * 2, &1u16.to_be_bytes())
        .unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    sector[..12].copy_from_slice(b"hello qcow2!");
    storage.write_at(data_cluster_offset, &sector).unwrap();

    storage
}

fn make_vhd_footer(virtual_size: u64, disk_type: u32, data_offset: u64) -> [u8; 512] {
    let mut footer = [0u8; 512];
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

fn make_vhd_fixed_with_pattern() -> MemStorage {
    let virtual_size = 1024 * 1024;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let mut storage = MemStorage::default();
    storage.write_at(0, &data).unwrap();
    storage.write_at(virtual_size, &footer).unwrap();
    storage
}

fn make_vhd_fixed_with_footer_copy() -> MemStorage {
    let virtual_size = 1024 * 1024u64;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);

    let mut storage = MemStorage::default();
    storage.write_at(0, &footer).unwrap();
    storage.write_at(512, &data).unwrap();
    storage.write_at(512 + virtual_size, &footer).unwrap();
    storage
}

fn make_vhd_dynamic_empty(virtual_size: u64, block_size: u32) -> MemStorage {
    assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);
    assert_eq!(block_size as usize % SECTOR_SIZE, 0);

    let dyn_header_offset = 512u64;
    let table_offset = 512u64 + 1024u64;
    let blocks = virtual_size.div_ceil(block_size as u64);
    let max_table_entries = blocks as u32;
    let bat_bytes = max_table_entries as u64 * 4;
    let bat_size = bat_bytes.div_ceil(512) * 512;

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    let file_len = 512 + 1024 + bat_size + 512;
    let mut storage = MemStorage::with_len(file_len as usize);

    storage.write_at(0, &footer).unwrap();
    storage.write_at(file_len - 512, &footer).unwrap();

    let mut dyn_header = [0u8; 1024];
    dyn_header[0..8].copy_from_slice(b"cxsparse");
    write_be_u64(&mut dyn_header, 8, u64::MAX);
    write_be_u64(&mut dyn_header, 16, table_offset);
    write_be_u32(&mut dyn_header, 24, 0x0001_0000);
    write_be_u32(&mut dyn_header, 28, max_table_entries);
    write_be_u32(&mut dyn_header, 32, block_size);
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    write_be_u32(&mut dyn_header, 36, checksum);
    storage.write_at(dyn_header_offset, &dyn_header).unwrap();

    let bat = vec![0xFFu8; bat_size as usize];
    storage.write_at(table_offset, &bat).unwrap();

    storage
}

fn make_vhd_dynamic_with_pattern() -> MemStorage {
    let virtual_size = 1024 * 1024;
    let block_size = 64 * 1024;
    let mut storage = make_vhd_dynamic_empty(virtual_size, block_size);

    let dyn_header_offset = 512u64;
    let table_offset = 512u64 + 1024u64;
    let bat_size = 512u64;
    let old_footer_offset = 512 + 1024 + bat_size;
    let bitmap_size = 512u64;
    let block_total_size = bitmap_size + block_size as u64;
    let new_footer_offset = old_footer_offset + block_total_size;

    storage.set_len(new_footer_offset + 512).unwrap();

    let bat_entry = (old_footer_offset / 512) as u32;
    storage
        .write_at(table_offset, &bat_entry.to_be_bytes())
        .unwrap();

    let mut bitmap = [0u8; 512];
    bitmap[0] = 0x80;
    storage.write_at(old_footer_offset, &bitmap).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    sector[..12].copy_from_slice(b"hello vhd-d!");
    let data_offset = old_footer_offset + bitmap_size;
    storage.write_at(data_offset, &sector).unwrap();

    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    storage.write_at(0, &footer).unwrap();
    storage.write_at(new_footer_offset, &footer).unwrap();

    storage
}

#[test]
fn detect_qcow2_and_vhd() {
    let mut qcow = make_qcow2_empty(1024 * 1024);
    assert_eq!(detect_format(&mut qcow).unwrap(), DiskFormat::Qcow2);

    let mut vhd = make_vhd_fixed_with_pattern();
    assert_eq!(detect_format(&mut vhd).unwrap(), DiskFormat::Vhd);
}

#[test]
fn detect_qcow2_bad_version_falls_back_to_raw() {
    let mut storage = MemStorage::with_len(8);
    let mut header = [0u8; 8];
    header[..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 1);
    storage.write_at(0, &header).unwrap();
    assert_eq!(detect_format(&mut storage).unwrap(), DiskFormat::Raw);
}

#[test]
fn detect_vhd_cookie_without_plausible_footer_is_raw() {
    let mut storage = MemStorage::with_len(512);
    let mut footer = [0u8; 512];
    footer[..8].copy_from_slice(b"conectix");
    write_be_u32(&mut footer, 12, 0x0001_0000);
    write_be_u64(&mut footer, 16, u64::MAX);
    write_be_u64(&mut footer, 48, 512);
    write_be_u32(&mut footer, 60, 2);
    storage.write_at(0, &footer).unwrap();
    assert_eq!(detect_format(&mut storage).unwrap(), DiskFormat::Raw);
}

#[test]
fn raw_disk_create_and_rw_roundtrip() {
    let storage = MemStorage::default();
    let mut disk = emulator::io::storage::formats::RawDisk::create(storage, 512, 8).unwrap();

    let data = vec![0xA5u8; SECTOR_SIZE];
    disk.write_sectors(1, &data).unwrap();

    let mut back = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn raw_disk_create_rejects_size_overflow() {
    // u64::MAX * 512 would overflow; `RawDisk::create` should reject instead of saturating.
    let storage = MemStorage::default();
    let res = emulator::io::storage::formats::RawDisk::create(storage, 512, u64::MAX);
    assert!(matches!(
        res,
        Err(DiskError::Unsupported("disk size overflow"))
    ));
}

#[test]
fn qcow2_unallocated_reads_zero() {
    let storage = make_qcow2_empty(1024 * 1024);
    let mut drive = VirtualDrive::open_auto(storage, 512, WriteCachePolicy::WriteThrough).unwrap();
    assert_eq!(drive.format(), DiskFormat::Qcow2);

    let mut buf = vec![0xAAu8; SECTOR_SIZE * 4];
    drive.read_sectors(0, &mut buf).unwrap();
    assert!(buf.iter().all(|b| *b == 0));
}

#[test]
fn qcow2_fixture_read_and_write() {
    let storage = make_qcow2_with_pattern();
    let mut drive = VirtualDrive::open_auto(storage, 512, WriteCachePolicy::WriteThrough).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    drive.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..12], b"hello qcow2!");

    let mut write_buf = vec![0u8; SECTOR_SIZE];
    write_buf[..14].copy_from_slice(b"write qcow2 ok");
    drive.write_sectors(10, &write_buf).unwrap();

    let mut read_back = vec![0u8; SECTOR_SIZE];
    drive.read_sectors(10, &mut read_back).unwrap();
    assert_eq!(read_back, write_buf);
}

#[test]
fn qcow2_write_persists_after_reopen() {
    let storage = make_qcow2_empty(1024 * 1024);
    let mut disk = emulator::io::storage::formats::Qcow2Disk::open(storage).unwrap();

    let data = vec![0x5Au8; SECTOR_SIZE * 2];
    disk.write_sectors(1, &data).unwrap();
    disk.flush().unwrap();

    let storage = disk.into_storage();
    let mut reopened = emulator::io::storage::formats::Qcow2Disk::open(storage).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE * 2];
    reopened.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn qcow2_truncated_l2_table_returns_corrupt_image() {
    let virtual_size = 1024 * 1024;
    let cluster_bits = 16u32;
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;

    // Leave the file too short to contain the entire L2 table cluster.
    let file_len = l2_table_offset + cluster_size / 2;
    let mut storage = MemStorage::with_len(file_len as usize);

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
    storage.write_at(0, &header).unwrap();

    storage
        .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
        .unwrap();

    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    storage
        .write_at(l1_table_offset, &l1_entry.to_be_bytes())
        .unwrap();

    for cluster_index in 0u64..5 {
        let off = refcount_block_offset + cluster_index * 2;
        storage.write_at(off, &1u16.to_be_bytes()).unwrap();
    }

    let mut disk = emulator::io::storage::formats::Qcow2Disk::open(storage).unwrap();
    let mut sector = [0u8; SECTOR_SIZE];
    let err = disk.read_sectors(0, &mut sector).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("qcow2 l2 table truncated")
    ));
}

#[test]
fn qcow2_rejects_oversized_l1_table() {
    const MAX_TABLE_BYTES: u64 = 128 * 1024 * 1024;

    let cluster_bits = 9u32; // 512-byte clusters
    let cluster_size = 1u64 << cluster_bits;
    let l2_entries_per_table = cluster_size / 8;

    // Choose a virtual size that requires an L1 table just barely larger than MAX_TABLE_BYTES.
    let required_l1_entries = (MAX_TABLE_BYTES / 8) + 1;
    let guest_clusters = required_l1_entries
        .checked_mul(l2_entries_per_table)
        .expect("guest_clusters overflow");
    let virtual_size = guest_clusters
        .checked_mul(cluster_size)
        .expect("virtual_size overflow");

    let l1_size =
        u32::try_from(required_l1_entries).expect("required_l1_entries too large for u32");

    let mut storage = MemStorage::with_len(104);
    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 3); // version
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, virtual_size);
    write_be_u32(&mut header, 36, l1_size);
    write_be_u64(&mut header, 40, cluster_size); // l1_table_offset
    write_be_u64(&mut header, 48, cluster_size * 2); // refcount_table_offset
    write_be_u32(&mut header, 56, 1); // refcount_table_clusters
    write_be_u64(&mut header, 72, 0); // incompatible_features
    write_be_u32(&mut header, 96, 4); // refcount_order (16-bit)
    write_be_u32(&mut header, 100, 104); // header_length
    storage.write_at(0, &header).unwrap();

    let res = emulator::io::storage::formats::Qcow2Disk::open(storage);
    assert!(matches!(
        res,
        Err(DiskError::Unsupported("qcow2 l1 table too large"))
    ));
}

#[test]
fn qcow2_rejects_oversized_refcount_table() {
    const MAX_TABLE_BYTES: u64 = 128 * 1024 * 1024;

    let cluster_bits = 16u32; // 64KiB clusters
    let cluster_size = 1u64 << cluster_bits;
    let refcount_table_clusters = (MAX_TABLE_BYTES / cluster_size) + 1;
    let refcount_table_clusters =
        u32::try_from(refcount_table_clusters).expect("refcount_table_clusters too large for u32");

    // Ensure the file is long enough for the (small) L1 table so we hit the refcount cap instead
    // of failing with a truncated L1 table.
    let file_len = cluster_size + 8;
    let mut storage = MemStorage::with_len(file_len as usize);

    let mut header = [0u8; 104];
    header[0..4].copy_from_slice(b"QFI\xfb");
    write_be_u32(&mut header, 4, 3); // version
    write_be_u32(&mut header, 20, cluster_bits);
    write_be_u64(&mut header, 24, cluster_size); // virtual_size
    write_be_u32(&mut header, 36, 1); // l1_size
    write_be_u64(&mut header, 40, cluster_size); // l1_table_offset
    write_be_u64(&mut header, 48, cluster_size * 2); // refcount_table_offset
    write_be_u32(&mut header, 56, refcount_table_clusters);
    write_be_u64(&mut header, 72, 0); // incompatible_features
    write_be_u32(&mut header, 96, 4); // refcount_order (16-bit)
    write_be_u32(&mut header, 100, 104); // header_length
    storage.write_at(0, &header).unwrap();

    let res = emulator::io::storage::formats::Qcow2Disk::open(storage);
    assert!(matches!(
        res,
        Err(DiskError::Unsupported("qcow2 refcount table too large"))
    ));
}

#[test]
fn vhd_fixed_fixture_read() {
    let storage = make_vhd_fixed_with_pattern();
    let mut drive = VirtualDrive::open_auto(storage, 512, WriteCachePolicy::WriteThrough).unwrap();
    assert_eq!(drive.format(), DiskFormat::Vhd);

    let mut sector = [0u8; SECTOR_SIZE];
    drive.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");
}

#[test]
fn vhd_fixed_footer_copy_is_supported() {
    let storage = make_vhd_fixed_with_footer_copy();
    let mut drive = VirtualDrive::open_auto(storage, 512, WriteCachePolicy::WriteThrough).unwrap();
    assert_eq!(drive.format(), DiskFormat::Vhd);

    let mut sector = [0u8; SECTOR_SIZE];
    drive.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..10], b"hello vhd!");

    let data = vec![0xCCu8; SECTOR_SIZE];
    drive.write_sectors(1, &data).unwrap();
    drive.flush().unwrap();

    let backend = drive.into_backend();
    let mut reopened =
        VirtualDrive::new(DiskFormat::Vhd, backend, WriteCachePolicy::WriteThrough).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_dynamic_unallocated_reads_zero_and_writes_allocate() {
    let storage = make_vhd_dynamic_empty(1024 * 1024, 64 * 1024);
    let mut drive = VirtualDrive::open_auto(storage, 512, WriteCachePolicy::WriteThrough).unwrap();

    let mut buf = vec![0xAAu8; SECTOR_SIZE * 8];
    drive.read_sectors(0, &mut buf).unwrap();
    assert!(buf.iter().all(|b| *b == 0));

    let data = vec![0x5Au8; SECTOR_SIZE * 2];
    drive.write_sectors(1, &data).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE * 2];
    drive.read_sectors(1, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_dynamic_fixture_read() {
    let storage = make_vhd_dynamic_with_pattern();
    let mut drive = VirtualDrive::open_auto(storage, 512, WriteCachePolicy::WriteThrough).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    drive.read_sectors(0, &mut sector).unwrap();
    assert_eq!(&sector[..12], b"hello vhd-d!");
}

#[test]
fn vhd_dynamic_write_persists_after_reopen() {
    let storage = make_vhd_dynamic_empty(1024 * 1024, 64 * 1024);
    let mut disk = emulator::io::storage::formats::VhdDisk::open(storage).unwrap();

    let data = vec![0xCCu8; SECTOR_SIZE];
    disk.write_sectors(3, &data).unwrap();
    disk.flush().unwrap();

    let storage = disk.into_storage();
    let mut reopened = emulator::io::storage::formats::VhdDisk::open(storage).unwrap();
    let mut back = vec![0u8; SECTOR_SIZE];
    reopened.read_sectors(3, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vhd_dynamic_rejects_bad_dynamic_header_checksum() {
    let mut storage = make_vhd_dynamic_empty(1024 * 1024, 64 * 1024);

    // Clobber the checksum field (offset 36) without changing the rest of the header.
    let dyn_header_offset = 512u64;
    storage.write_at(dyn_header_offset + 36, &0u32.to_be_bytes()).unwrap();

    let res = emulator::io::storage::formats::VhdDisk::open(storage);
    assert!(matches!(
        res,
        Err(DiskError::CorruptImage("vhd dynamic header checksum mismatch"))
    ));
}

#[test]
fn vhd_dynamic_rejects_bad_dynamic_header_data_offset() {
    let mut storage = make_vhd_dynamic_empty(1024 * 1024, 64 * 1024);

    // The dynamic header's `data_offset` is at 8..16 and must be 0xFFFF..FFFF.
    let dyn_header_offset = 512u64;
    storage.write_at(dyn_header_offset + 8, &0u64.to_be_bytes()).unwrap();

    let res = emulator::io::storage::formats::VhdDisk::open(storage);
    assert!(matches!(
        res,
        Err(DiskError::CorruptImage("vhd dynamic header data_offset invalid"))
    ));
}

#[test]
fn vhd_dynamic_rejects_oversized_bat() {
    const MAX_BAT_BYTES: u64 = 128 * 1024 * 1024;

    let block_size = 512u32;
    let required_entries = (MAX_BAT_BYTES / 4) + 1;
    let virtual_size = required_entries * block_size as u64;

    let dyn_header_offset = 512u64;
    let table_offset = dyn_header_offset + 1024;
    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);

    let file_len = 512 + 1024 + 512;
    let mut storage = MemStorage::with_len(file_len as usize);
    storage.write_at(0, &footer).unwrap();
    storage.write_at(file_len - 512, &footer).unwrap();

    let mut dyn_header = [0u8; 1024];
    dyn_header[0..8].copy_from_slice(b"cxsparse");
    write_be_u64(&mut dyn_header, 8, u64::MAX);
    write_be_u64(&mut dyn_header, 16, table_offset);
    write_be_u32(&mut dyn_header, 24, 0x0001_0000);
    write_be_u32(
        &mut dyn_header,
        28,
        u32::try_from(required_entries).expect("required_entries too large for u32"),
    );
    write_be_u32(&mut dyn_header, 32, block_size);
    let checksum = vhd_dynamic_header_checksum(&dyn_header);
    write_be_u32(&mut dyn_header, 36, checksum);
    storage.write_at(dyn_header_offset, &dyn_header).unwrap();

    let res = emulator::io::storage::formats::VhdDisk::open(storage);
    assert!(matches!(
        res,
        Err(DiskError::Unsupported("vhd bat too large"))
    ));
}

#[test]
fn vhd_dynamic_rejects_block_overlapping_metadata() {
    let virtual_size = 1024 * 1024u64;
    let block_size = 64 * 1024u32;

    let dyn_header_offset = 512u64;
    let table_offset = dyn_header_offset + 1024;

    let mut storage = make_vhd_dynamic_empty(virtual_size, block_size);

    // Point the first BAT entry at the dynamic header (sector 1).
    storage.write_at(table_offset, &1u32.to_be_bytes()).unwrap();

    // Extend the file so the invalid BAT entry doesn't get rejected only because it would overlap
    // the EOF footer.
    let bitmap_size = 512u64;
    let block_total_size = bitmap_size + block_size as u64;
    let new_footer_offset = dyn_header_offset + block_total_size;

    storage.set_len(new_footer_offset + 512).unwrap();
    let footer = make_vhd_footer(virtual_size, 3, dyn_header_offset);
    storage.write_at(new_footer_offset, &footer).unwrap();

    let mut drive = VirtualDrive::open_with_format(
        DiskFormat::Vhd,
        storage,
        512,
        WriteCachePolicy::WriteThrough,
    )
    .unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    let err = drive.read_sectors(0, &mut sector).unwrap_err();
    assert!(matches!(
        err,
        DiskError::CorruptImage("vhd block overlaps metadata")
    ));
}

#[derive(Clone, Debug)]
enum Op {
    Read { lba: u64, sectors: usize },
    Write { lba: u64, data: Vec<u8> },
}

fn ops_strategy(capacity_sectors: u64) -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(
        (0u64..capacity_sectors).prop_flat_map(move |lba| {
            let max = (capacity_sectors - lba) as usize;
            let max_sectors = max.clamp(1, 4);
            (1usize..=max_sectors).prop_flat_map(move |sectors| {
                prop_oneof![
                    3 => Just(Op::Read { lba, sectors }),
                    7 => proptest::collection::vec(any::<u8>(), sectors * SECTOR_SIZE)
                        .prop_map(move |data| Op::Write { lba, data }),
                ]
            })
        }),
        1..50,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn prop_qcow2_matches_reference(ops in ops_strategy(128)) {
        let capacity_sectors = 128u64;
        let virtual_size = capacity_sectors * SECTOR_SIZE as u64;
        let storage = make_qcow2_empty(virtual_size);
        let mut drive = VirtualDrive::open_with_format(
            DiskFormat::Qcow2,
            storage,
            512,
            WriteCachePolicy::WriteThrough,
        ).unwrap();
        let mut reference = vec![0u8; virtual_size as usize];

        for op in ops {
            match op {
                Op::Read { lba, sectors } => {
                    let mut buf = vec![0u8; sectors * SECTOR_SIZE];
                    drive.read_sectors(lba, &mut buf).unwrap();
                    let start = (lba * SECTOR_SIZE as u64) as usize;
                    prop_assert_eq!(&buf[..], &reference[start..start + buf.len()]);
                }
                Op::Write { lba, data } => {
                    drive.write_sectors(lba, &data).unwrap();
                    let start = (lba * SECTOR_SIZE as u64) as usize;
                    reference[start..start + data.len()].copy_from_slice(&data);
                }
            }
        }
    }

    #[test]
    fn prop_vhd_dynamic_matches_reference(ops in ops_strategy(128)) {
        let capacity_sectors = 128u64;
        let virtual_size = capacity_sectors * SECTOR_SIZE as u64;
        let storage = make_vhd_dynamic_empty(virtual_size, 64 * 1024);
        let mut drive = VirtualDrive::open_with_format(
            DiskFormat::Vhd,
            storage,
            512,
            WriteCachePolicy::WriteThrough,
        ).unwrap();
        let mut reference = vec![0u8; virtual_size as usize];

        for op in ops {
            match op {
                Op::Read { lba, sectors } => {
                    let mut buf = vec![0u8; sectors * SECTOR_SIZE];
                    drive.read_sectors(lba, &mut buf).unwrap();
                    let start = (lba * SECTOR_SIZE as u64) as usize;
                    prop_assert_eq!(&buf[..], &reference[start..start + buf.len()]);
                }
                Op::Write { lba, data } => {
                    drive.write_sectors(lba, &data).unwrap();
                    let start = (lba * SECTOR_SIZE as u64) as usize;
                    reference[start..start + data.len()].copy_from_slice(&data);
                }
            }
        }
    }
}
