use std::io::Write;

use aero_image_chunker::{chunk_disk_to_vecs, ChecksumAlgorithm, ImageFormat};
use aero_storage::{AeroSparseConfig, AeroSparseDisk, MemBackend, VirtualDisk};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn persist_mem_backend(backend: MemBackend) -> NamedTempFile {
    let mut tmp = NamedTempFile::new().expect("tempfile");
    tmp.as_file_mut()
        .write_all(&backend.into_vec())
        .expect("write temp image");
    tmp.as_file_mut().flush().expect("flush temp image");
    tmp
}

#[test]
fn manifest_sha256_matches_chunk_bytes_for_aerosparse() {
    let disk_size = 16 * 1024u64;
    let chunk_size = 4 * 1024u64;

    // Create a sparse disk with only the first block allocated.
    let backend = MemBackend::default();
    let mut disk = AeroSparseDisk::create(
        backend,
        AeroSparseConfig {
            disk_size_bytes: disk_size,
            block_size_bytes: 4 * 1024,
        },
    )
    .expect("create aerosparse");
    disk.write_at(0, b"hello aerosparse!")
        .expect("write pattern");
    disk.flush().expect("flush");

    let tmp = persist_mem_backend(disk.into_backend());

    // Sanity check: physical file should be smaller than virtual disk so we'd catch accidental raw
    // reads.
    let physical_len = tmp.as_file().metadata().unwrap().len();
    assert!(
        physical_len < disk_size,
        "expected physical file ({physical_len}) < virtual size ({disk_size})"
    );

    let (manifest, chunks) = chunk_disk_to_vecs(
        tmp.path(),
        ImageFormat::Auto,
        chunk_size,
        ChecksumAlgorithm::Sha256,
    )
    .expect("chunk disk");

    assert_eq!(manifest.total_size, disk_size);
    assert_eq!(manifest.chunk_size, chunk_size);
    assert_eq!(manifest.chunk_count, disk_size / chunk_size);
    assert_eq!(chunks.len() as u64, manifest.chunk_count);
    assert_eq!(manifest.chunks.len() as u64, manifest.chunk_count);

    for (i, chunk_bytes) in chunks.iter().enumerate() {
        let expected = sha256_hex(chunk_bytes);
        let actual = manifest.chunks[i]
            .sha256
            .as_deref()
            .expect("sha256 in manifest");
        assert_eq!(actual, expected, "chunk {i} sha256 mismatch");
    }
}

