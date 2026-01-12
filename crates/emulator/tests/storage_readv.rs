#![cfg(not(target_arch = "wasm32"))]

use emulator::io::storage::disk::MemDisk;
use emulator::io::storage::{
    DiskBackend as _, DiskError, DiskFormat, VirtualDrive, WriteCachePolicy,
};

#[test]
fn virtual_drive_readv_rejects_unaligned_buffer() {
    let backend = MemDisk::new(4);
    let mut drive = VirtualDrive::new(
        DiskFormat::Raw,
        Box::new(backend),
        WriteCachePolicy::WriteThrough,
    )
    .unwrap();

    let mut buf0 = vec![0u8; 512];
    let mut buf1 = [0u8; 1];
    let mut bufs: [&mut [u8]; 2] = [&mut buf0[..], &mut buf1[..]];
    let err = drive.readv_sectors(0, &mut bufs).unwrap_err();
    assert!(matches!(
        err,
        DiskError::UnalignedBuffer {
            len: 1,
            sector_size: 512
        }
    ));
}

#[test]
fn virtual_drive_writev_rejects_unaligned_buffer() {
    let backend = MemDisk::new(4);
    let mut drive = VirtualDrive::new(
        DiskFormat::Raw,
        Box::new(backend),
        WriteCachePolicy::WriteThrough,
    )
    .unwrap();

    let buf0 = vec![0u8; 512];
    let buf1 = [0u8; 1];
    let bufs: [&[u8]; 2] = [&buf0[..], &buf1[..]];
    let err = drive.writev_sectors(0, &bufs).unwrap_err();
    assert!(matches!(
        err,
        DiskError::UnalignedBuffer {
            len: 1,
            sector_size: 512
        }
    ));
}

#[test]
fn virtual_drive_readv_roundtrips() {
    let mut backend = MemDisk::new(4);
    backend.data_mut()[0..512].fill(0xAA);
    backend.data_mut()[512..1024].fill(0xBB);

    let mut drive = VirtualDrive::new(
        DiskFormat::Raw,
        Box::new(backend),
        WriteCachePolicy::WriteThrough,
    )
    .unwrap();

    let mut buf0 = vec![0u8; 512];
    let mut buf1 = vec![0u8; 512];
    let mut bufs: [&mut [u8]; 2] = [&mut buf0[..], &mut buf1[..]];
    drive.readv_sectors(0, &mut bufs).unwrap();
    assert!(buf0.iter().all(|b| *b == 0xAA));
    assert!(buf1.iter().all(|b| *b == 0xBB));
}
