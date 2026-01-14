use std::fs::{self, OpenOptions};
use std::path::Path;

use aero_disk_convert::{convert, ConvertOptions, OutputFormat, DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES};
use aero_storage::{
    DiskFormat, DiskImage, FileBackend, MemBackend, RawDisk, StorageBackend, VirtualDisk,
    SECTOR_SIZE,
};
use tempfile::tempdir;

const QCOW2_OFLAG_COPIED: u64 = 1 << 63;

fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn make_qcow2_with_pattern_bytes() -> (u64, Vec<u8>) {
    let virtual_size = 2 * 1024 * 1024u64;
    let cluster_bits = 12u32; // 4 KiB clusters
    let cluster_size = 1u64 << cluster_bits;

    let refcount_table_offset = cluster_size;
    let l1_table_offset = cluster_size * 2;
    let refcount_block_offset = cluster_size * 3;
    let l2_table_offset = cluster_size * 4;
    let data_cluster_offset = cluster_size * 5;

    let file_len = cluster_size * 6;
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

    // Refcount table points to the single refcount block.
    backend
        .write_at(refcount_table_offset, &refcount_block_offset.to_be_bytes())
        .unwrap();

    // L1 points to the single L2 table.
    let l1_entry = l2_table_offset | QCOW2_OFLAG_COPIED;
    backend.write_at(l1_table_offset, &l1_entry.to_be_bytes()).unwrap();

    // Mark header/refcount/L1/refcount-block/L2/data clusters as in-use.
    for cluster_index in 0u64..6 {
        let off = refcount_block_offset + cluster_index * 2;
        backend.write_at(off, &1u16.to_be_bytes()).unwrap();
    }

    // L2 entry maps guest cluster 0 to the data cluster.
    let l2_entry = data_cluster_offset | QCOW2_OFLAG_COPIED;
    backend.write_at(l2_table_offset, &l2_entry.to_be_bytes()).unwrap();

    let mut sector = [0u8; SECTOR_SIZE];
    sector[..12].copy_from_slice(b"hello qcow2!");
    backend.write_at(data_cluster_offset, &sector).unwrap();

    (virtual_size, backend.into_vec())
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

fn make_vhd_fixed_with_pattern_bytes() -> (u64, Vec<u8>) {
    let virtual_size = 64 * 1024u64;
    let mut data = vec![0u8; virtual_size as usize];
    data[0..10].copy_from_slice(b"hello vhd!");

    let footer = make_vhd_footer(virtual_size, 2, u64::MAX);
    data.extend_from_slice(&footer);
    (virtual_size, data)
}

fn open_disk_image(path: &Path) -> anyhow::Result<DiskImage<FileBackend>> {
    let backend = FileBackend::open_read_only(path)?;
    Ok(DiskImage::open_auto(backend)?)
}

#[test]
fn raw_to_aerosparse_preserves_bytes_and_sparsity() -> anyhow::Result<()> {
    let td = tempdir()?;
    let input = td.path().join("in.raw");
    let output = td.path().join("out.aerosparse");

    let capacity = 3 * 1024 * 1024u64;

    // Create a raw image with 3x 1MiB blocks, leaving the middle block all zeros.
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&input)?;
        let backend = FileBackend::from_file_with_path(file, &input);
        let mut disk = RawDisk::create(backend, capacity)?;
        disk.write_at(0, b"hello block0")?;
        disk.write_at(2 * 1024 * 1024 + 123, b"hello block2")?;
        disk.flush()?;
    }

    convert(ConvertOptions {
        input: input.clone(),
        output: output.clone(),
        output_format: OutputFormat::AeroSparse,
        block_size_bytes: DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES,
        progress: false,
        force: false,
    })?;

    let mut in_img = open_disk_image(&input)?;
    let mut out_img = open_disk_image(&output)?;

    assert_eq!(out_img.format(), DiskFormat::AeroSparse);
    assert_eq!(out_img.capacity_bytes(), capacity);

    let mut in_bytes = vec![0u8; capacity as usize];
    let mut out_bytes = vec![0u8; capacity as usize];
    in_img.read_at(0, &mut in_bytes)?;
    out_img.read_at(0, &mut out_bytes)?;
    assert_eq!(out_bytes, in_bytes);

    match &out_img {
        DiskImage::AeroSparse(d) => {
            assert_eq!(d.header().block_size_bytes, DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES);
            assert_eq!(d.header().allocated_blocks, 2);
        }
        other => panic!("expected AeroSparse output, got {:?}", other.format()),
    }

    Ok(())
}

#[test]
fn qcow2_to_raw_preserves_virtual_capacity_and_bytes() -> anyhow::Result<()> {
    let td = tempdir()?;
    let input = td.path().join("in.qcow2");
    let output = td.path().join("out.raw");

    let (virtual_size, bytes) = make_qcow2_with_pattern_bytes();
    fs::write(&input, bytes)?;

    convert(ConvertOptions {
        input: input.clone(),
        output: output.clone(),
        output_format: OutputFormat::Raw,
        block_size_bytes: DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES,
        progress: false,
        force: false,
    })?;

    let meta = fs::metadata(&output)?;
    assert_eq!(meta.len(), virtual_size);

    let mut out_img = open_disk_image(&output)?;
    assert_eq!(out_img.format(), DiskFormat::Raw);
    assert_eq!(out_img.capacity_bytes(), virtual_size);

    let mut expected = vec![0u8; virtual_size as usize];
    expected[..12].copy_from_slice(b"hello qcow2!");
    let mut actual = vec![0u8; virtual_size as usize];
    out_img.read_at(0, &mut actual)?;
    assert_eq!(actual, expected);

    Ok(())
}

#[test]
fn vhd_to_raw_preserves_virtual_capacity_and_bytes() -> anyhow::Result<()> {
    let td = tempdir()?;
    let input = td.path().join("in.vhd");
    let output = td.path().join("out.raw");

    let (virtual_size, bytes) = make_vhd_fixed_with_pattern_bytes();
    fs::write(&input, bytes)?;

    convert(ConvertOptions {
        input: input.clone(),
        output: output.clone(),
        output_format: OutputFormat::Raw,
        block_size_bytes: DEFAULT_AEROSPARSE_BLOCK_SIZE_BYTES,
        progress: false,
        force: false,
    })?;

    let meta = fs::metadata(&output)?;
    assert_eq!(meta.len(), virtual_size);

    let mut out_img = open_disk_image(&output)?;
    assert_eq!(out_img.format(), DiskFormat::Raw);
    assert_eq!(out_img.capacity_bytes(), virtual_size);

    let mut expected = vec![0u8; virtual_size as usize];
    expected[0..10].copy_from_slice(b"hello vhd!");
    let mut actual = vec![0u8; virtual_size as usize];
    out_img.read_at(0, &mut actual)?;
    assert_eq!(actual, expected);

    Ok(())
}
