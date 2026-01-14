#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::io::{Seek, SeekFrom, Write};

use aero_storage::{
    AeroSparseDisk, DiskImage, MemBackend, StorageBackend, VirtualDisk, SECTOR_SIZE,
};
use tempfile::tempdir;

const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
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

    // Mark metadata clusters as in-use: header, refcount table, L1 table, refcount block, L2 table.
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

#[test]
fn qcow2_to_aerospar_roundtrip() {
    let dir = tempdir().unwrap();
    let in_path = dir.path().join("in.qcow2");
    let out_path = dir.path().join("out.aerospar");

    let input = make_qcow2_with_pattern().into_vec();
    fs::write(&in_path, &input).unwrap();

    assert_cmd::cargo::cargo_bin_cmd!("aero-disk-convert")
        .args([
            "convert",
            "--input",
            in_path.to_str().unwrap(),
            "--output",
            out_path.to_str().unwrap(),
            "--output-format",
            "aerosparse",
            "--block-size-bytes",
            "4096",
        ])
        .assert()
        .success();

    let input_disk_bytes = fs::read(&in_path).unwrap();
    let output_disk_bytes = fs::read(&out_path).unwrap();

    let mut input_disk = DiskImage::open_auto(MemBackend::from_vec(input_disk_bytes)).unwrap();
    let mut output_disk = AeroSparseDisk::open(MemBackend::from_vec(output_disk_bytes)).unwrap();

    assert_eq!(input_disk.capacity_bytes(), output_disk.capacity_bytes());

    let cap = input_disk.capacity_bytes();
    let mut buf_in = vec![0u8; 4096];
    let mut buf_out = vec![0u8; 4096];

    let mut off = 0u64;
    while off < cap {
        let len = ((cap - off) as usize).min(buf_in.len());
        input_disk.read_at(off, &mut buf_in[..len]).unwrap();
        output_disk.read_at(off, &mut buf_out[..len]).unwrap();
        assert_eq!(&buf_in[..len], &buf_out[..len], "mismatch at offset {off}");
        off += len as u64;
    }
}

#[test]
fn raw_to_aerospar_is_smaller_when_sparse() {
    let dir = tempdir().unwrap();
    let in_path = dir.path().join("in.img");
    let out_path = dir.path().join("out.aerospar");

    let disk_size = 2 * 1024 * 1024u64;
    let mut f = fs::File::create(&in_path).unwrap();
    f.set_len(disk_size).unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(b"hello raw!").unwrap();
    f.sync_all().unwrap();

    assert_cmd::cargo::cargo_bin_cmd!("aero-disk-convert")
        .args([
            "convert",
            "--input",
            in_path.to_str().unwrap(),
            "--output",
            out_path.to_str().unwrap(),
            "--output-format",
            "aerosparse",
            "--block-size-bytes",
            "4096",
        ])
        .assert()
        .success();

    let in_len = fs::metadata(&in_path).unwrap().len();
    let out_len = fs::metadata(&out_path).unwrap().len();
    assert!(
        out_len < in_len,
        "expected sparse output to be smaller (out={out_len} in={in_len})"
    );

    let out_bytes = fs::read(&out_path).unwrap();
    let out_disk = AeroSparseDisk::open(MemBackend::from_vec(out_bytes)).unwrap();
    assert_eq!(out_disk.header().allocated_blocks, 1);

    let mut first = [0u8; 16];
    let mut disk = out_disk;
    disk.read_at(0, &mut first).unwrap();
    assert_eq!(&first[..10], b"hello raw!");
}
