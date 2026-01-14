use std::io::Write;

use aero_image_chunker::{chunk_disk_to_vecs, ChecksumAlgorithm, ImageFormat};
use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, MemBackend, StorageBackend, VirtualDisk, SECTOR_SIZE,
};
use tempfile::NamedTempFile;

const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

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

    let actual: Vec<u8> = chunks.into_iter().flatten().collect();
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
        ChecksumAlgorithm::None,
    )
    .unwrap();

    assert_eq!(manifest.total_size, disk_size_bytes);
    assert_eq!(manifest.chunk_size, chunk_size);
    assert_eq!(manifest.chunk_count, disk_size_bytes / chunk_size);
    assert_eq!(chunks.len() as u64, manifest.chunk_count);

    let mut expected = vec![0u8; disk_size_bytes as usize];
    expected[0..12].copy_from_slice(b"hello qcow2!");

    let actual: Vec<u8> = chunks.into_iter().flatten().collect();
    assert_eq!(actual, expected);
}
