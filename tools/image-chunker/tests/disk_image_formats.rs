use std::io::Write;

use aero_image_chunker::{chunk_disk_to_vecs, ChecksumAlgorithm, ImageFormat};
use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, MemBackend, StorageBackend, VirtualDisk, SECTOR_SIZE,
};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

const QCOW2_OFLAG_COPIED: u64 = 1 << 63;
const VHD_DISK_TYPE_FIXED: u32 = 2;
const VHD_DISK_TYPE_DYNAMIC: u32 = 3;

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn persist_mem_backend(backend: MemBackend) -> NamedTempFile {
    let mut tmp = NamedTempFile::new().expect("tempfile");
    let bytes = backend.into_vec();
    tmp.as_file_mut()
        .write_all(&bytes)
        .expect("write temp image");
    tmp.as_file_mut().flush().expect("flush temp image");
    tmp
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn make_qcow2_with_pattern(virtual_size: u64) -> MemBackend {
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

    // Refcount table points at a single refcount block.
    backend
        .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
        .unwrap();

    // L1 table points at a single L2 table.
    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l1_table_offset, &l1_entry.to_be_bytes())
        .unwrap();

    // Mark metadata clusters as in-use: header, refcount table, L1 table, refcount block, L2 table.
    for cluster_index in 0u64..5 {
        let off = refcount_block_offset + cluster_index * 2;
        backend.write_at(off, &1u16.to_be_bytes()).unwrap();
    }

    // Allocate a single data cluster and map guest cluster 0 to it.
    let data_cluster_offset = cluster_size * 5;
    backend.set_len(cluster_size * 6).unwrap();

    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
    backend
        .write_at(l2_table_offset, &l2_entry.to_be_bytes())
        .unwrap();

    // Mark the new data cluster as allocated in the refcount block (cluster index 5).
    backend
        .write_at(refcount_block_offset + 5 * 2, &1u16.to_be_bytes())
        .unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    sector[..12].copy_from_slice(b"hello qcow2!");
    backend.write_at(data_cluster_offset, &sector).unwrap();

    backend
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

fn make_vhd_fixed_with_pattern(virtual_size: u64) -> MemBackend {
    assert_eq!(virtual_size % SECTOR_SIZE as u64, 0);

    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, VHD_DISK_TYPE_FIXED, u64::MAX);

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

    let footer = make_vhd_footer(virtual_size, VHD_DISK_TYPE_DYNAMIC, dyn_header_offset);
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

    let footer = make_vhd_footer(virtual_size, VHD_DISK_TYPE_DYNAMIC, dyn_header_offset);
    backend.write_at(0, &footer).unwrap();
    backend.write_at(new_footer_offset, &footer).unwrap();

    backend
}

#[test]
fn chunking_aerosparse_uses_virtual_disk_bytes() {
    let disk_size_bytes = 8 * 1024u64;
    let chunk_size = 1024u64;

    let backend = MemBackend::new();
    let mut disk = AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes,
            block_size_bytes: 4096,
        },
    )
    .unwrap();

    disk.write_at(0, b"hello").unwrap();
    disk.write_at(5000, &[1, 2, 3, 4]).unwrap();
    disk.flush().unwrap();

    let tmp = persist_mem_backend(disk.into_backend());

    let (manifest, chunks) = chunk_disk_to_vecs(
        tmp.path(),
        ImageFormat::Auto,
        chunk_size,
        ChecksumAlgorithm::Sha256,
    )
    .unwrap();

    assert_eq!(manifest.total_size, disk_size_bytes);
    assert_eq!(manifest.chunk_size, chunk_size);
    assert_eq!(manifest.chunk_count, disk_size_bytes / chunk_size);
    assert_eq!(chunks.len() as u64, manifest.chunk_count);
    assert!(manifest.chunks.iter().all(|c| c.size == chunk_size));
    assert!(manifest.chunks.iter().all(|c| c.sha256.is_some()));

    let mut expected = vec![0u8; disk_size_bytes as usize];
    expected[0..5].copy_from_slice(b"hello");
    expected[5000..5004].copy_from_slice(&[1, 2, 3, 4]);

    let actual: Vec<u8> = chunks.iter().flat_map(|c| c.iter()).copied().collect();
    assert_eq!(actual, expected);
}

#[test]
fn chunking_qcow2_uses_virtual_disk_bytes() {
    let disk_size_bytes = 16 * 1024u64;
    let chunk_size = 4096u64;

    let backend = make_qcow2_with_pattern(disk_size_bytes);
    let tmp = persist_mem_backend(backend);

    let (manifest, chunks) = chunk_disk_to_vecs(
        tmp.path(),
        ImageFormat::Auto,
        chunk_size,
        ChecksumAlgorithm::Sha256,
    )
    .unwrap();

    assert_eq!(manifest.total_size, disk_size_bytes);
    assert_eq!(manifest.chunk_size, chunk_size);
    assert_eq!(manifest.chunk_count, disk_size_bytes / chunk_size);
    assert_eq!(chunks.len() as u64, manifest.chunk_count);

    let mut expected = vec![0u8; disk_size_bytes as usize];
    expected[0..12].copy_from_slice(b"hello qcow2!");

    let actual: Vec<u8> = chunks.iter().flat_map(|c| c.iter()).copied().collect();
    assert_eq!(actual, expected);

    // Ensure per-chunk sha256 covers the expanded virtual disk bytes (not the container file bytes).
    for (i, chunk) in chunks.iter().enumerate() {
        let expected = sha256_hex(chunk);
        let actual = manifest.chunks[i]
            .sha256
            .as_deref()
            .expect("sha256 present");
        assert_eq!(actual, expected, "sha256 mismatch for qcow2 chunk {i}");
    }
}

#[test]
fn chunking_vhd_uses_virtual_disk_bytes() {
    let disk_size_bytes = 64 * 1024u64;
    let chunk_size = 4096u64;

    let backend = make_vhd_fixed_with_pattern(disk_size_bytes);
    let tmp = persist_mem_backend(backend);

    let (manifest, chunks) = chunk_disk_to_vecs(
        tmp.path(),
        ImageFormat::Auto,
        chunk_size,
        ChecksumAlgorithm::Sha256,
    )
    .unwrap();

    assert_eq!(manifest.total_size, disk_size_bytes);
    assert_eq!(manifest.chunk_size, chunk_size);
    assert_eq!(manifest.chunk_count, disk_size_bytes / chunk_size);
    assert_eq!(chunks.len() as u64, manifest.chunk_count);

    let mut expected = vec![0u8; disk_size_bytes as usize];
    expected[0..10].copy_from_slice(b"hello vhd!");

    let actual: Vec<u8> = chunks.iter().flat_map(|c| c.iter()).copied().collect();
    assert_eq!(actual, expected);

    for (i, chunk) in chunks.iter().enumerate() {
        let expected = sha256_hex(chunk);
        let actual = manifest.chunks[i]
            .sha256
            .as_deref()
            .expect("sha256 present");
        assert_eq!(actual, expected, "sha256 mismatch for vhd chunk {i}");
    }
}

#[test]
fn chunking_vhd_dynamic_uses_virtual_disk_bytes() {
    let disk_size_bytes = 64 * 1024u64;
    let chunk_size = 4096u64;

    let backend = make_vhd_dynamic_with_pattern();
    let tmp = persist_mem_backend(backend);

    let (manifest, chunks) = chunk_disk_to_vecs(
        tmp.path(),
        ImageFormat::Auto,
        chunk_size,
        ChecksumAlgorithm::Sha256,
    )
    .unwrap();

    assert_eq!(manifest.total_size, disk_size_bytes);
    assert_eq!(manifest.chunk_size, chunk_size);
    assert_eq!(manifest.chunk_count, disk_size_bytes / chunk_size);
    assert_eq!(chunks.len() as u64, manifest.chunk_count);

    let mut expected = vec![0u8; disk_size_bytes as usize];
    expected[0..12].copy_from_slice(b"hello vhd-d!");

    let actual: Vec<u8> = chunks.iter().flat_map(|c| c.iter()).copied().collect();
    assert_eq!(actual, expected);

    for (i, chunk) in chunks.iter().enumerate() {
        let expected = sha256_hex(chunk);
        let actual = manifest.chunks[i]
            .sha256
            .as_deref()
            .expect("sha256 present");
        assert_eq!(actual, expected, "sha256 mismatch for vhd-d chunk {i}");
    }
}

#[test]
fn chunking_raw_uses_file_bytes() {
    let disk_size_bytes = 4096u64;
    let chunk_size = 1024u64;

    let mut expected = vec![0u8; disk_size_bytes as usize];
    for (i, b) in expected.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }

    let mut backend = MemBackend::with_len(disk_size_bytes).unwrap();
    backend.write_at(0, &expected).unwrap();
    let tmp = persist_mem_backend(backend);

    let (manifest, chunks) = chunk_disk_to_vecs(
        tmp.path(),
        ImageFormat::Raw,
        chunk_size,
        ChecksumAlgorithm::Sha256,
    )
    .unwrap();

    assert_eq!(manifest.total_size, disk_size_bytes);
    assert_eq!(manifest.chunk_size, chunk_size);
    assert_eq!(manifest.chunk_count, disk_size_bytes / chunk_size);
    assert_eq!(chunks.len() as u64, manifest.chunk_count);

    let actual: Vec<u8> = chunks.iter().flat_map(|c| c.iter()).copied().collect();
    assert_eq!(actual, expected);

    for (i, chunk) in chunks.iter().enumerate() {
        let expected = sha256_hex(chunk);
        let actual = manifest.chunks[i]
            .sha256
            .as_deref()
            .expect("sha256 present");
        assert_eq!(actual, expected, "sha256 mismatch for raw chunk {i}");
    }
}
