use aero_storage::{MemBackend, RawDisk, VirtualDisk as _};

use emulator::io::storage::adapters::{
    aero_storage_disk_error_to_emulator, emulator_disk_error_to_aero_storage,
    EmuDiskBackendFromVirtualDisk, VirtualDiskFromEmuDiskBackend,
};
use emulator::io::storage::disk::MemDisk as EmuMemDisk;
use emulator::io::storage::{DiskBackend as _, DiskError};

#[test]
fn aero_raw_disk_wrapped_as_emulator_disk_backend() {
    let sectors: u64 = 64;
    let backend = MemBackend::new();
    let raw = RawDisk::create(backend, sectors * 512).unwrap();

    let mut emu = EmuDiskBackendFromVirtualDisk::new(raw);
    assert_eq!(emu.sector_size(), 512);
    assert_eq!(emu.total_sectors(), sectors);

    let mut write_buf = vec![0u8; 512 * 4];
    for (i, b) in write_buf.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }

    emu.write_sectors(10, &write_buf).unwrap();
    emu.flush().unwrap();

    let mut read_buf = vec![0u8; write_buf.len()];
    emu.read_sectors(10, &mut read_buf).unwrap();
    assert_eq!(read_buf, write_buf);

    // Error mapping: unaligned buffer length.
    let mut unaligned = [0u8; 1];
    let err = emu.read_sectors(0, &mut unaligned).unwrap_err();
    assert!(matches!(err, DiskError::UnalignedBuffer { .. }));

    // Error mapping: out-of-range.
    let mut oob = vec![0u8; 512 * 2];
    let err = emu.read_sectors(sectors - 1, &mut oob).unwrap_err();
    assert!(matches!(err, DiskError::OutOfRange { .. }));
}

#[test]
fn error_mapping_preserves_offset_overflow_roundtrip() {
    let err = aero_storage::DiskError::OffsetOverflow;
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::Unsupported("offset overflow"));

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(back, aero_storage::DiskError::OffsetOverflow));
}

#[test]
fn error_mapping_preserves_corrupt_image_roundtrip() {
    let err = aero_storage::DiskError::CorruptImage("bad");
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::CorruptImage("bad"));

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(back, aero_storage::DiskError::CorruptImage("bad")));
}

#[test]
fn error_mapping_preserves_unsupported_roundtrip() {
    let err = aero_storage::DiskError::Unsupported("feature");
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::Unsupported("feature"));

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(
        back,
        aero_storage::DiskError::Unsupported("feature")
    ));
}

#[test]
fn error_mapping_preserves_not_supported_roundtrip() {
    let err = aero_storage::DiskError::NotSupported("opfs".to_string());
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::NotSupported("opfs".to_string()));

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(
        back,
        aero_storage::DiskError::NotSupported(msg) if msg == "opfs"
    ));
}

#[test]
fn error_mapping_preserves_quota_exceeded_roundtrip() {
    let err = aero_storage::DiskError::QuotaExceeded;
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::QuotaExceeded);

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(back, aero_storage::DiskError::QuotaExceeded));
}

#[test]
fn error_mapping_preserves_in_use_roundtrip() {
    let err = aero_storage::DiskError::InUse;
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::InUse);

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(back, aero_storage::DiskError::InUse));
}

#[test]
fn error_mapping_preserves_invalid_state_roundtrip() {
    let err = aero_storage::DiskError::InvalidState("closed".to_string());
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::InvalidState("closed".to_string()));

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(
        back,
        aero_storage::DiskError::InvalidState(msg) if msg == "closed"
    ));
}

#[test]
fn error_mapping_preserves_backend_unavailable_roundtrip() {
    let err = aero_storage::DiskError::BackendUnavailable;
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::BackendUnavailable);

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(back, aero_storage::DiskError::BackendUnavailable));
}

#[test]
fn error_mapping_preserves_io_roundtrip() {
    let err = aero_storage::DiskError::Io("boom".to_string());
    let emu = aero_storage_disk_error_to_emulator(err);
    assert_eq!(emu, DiskError::Io("boom".to_string()));

    let back = emulator_disk_error_to_aero_storage(emu, None, None, None);
    assert!(matches!(
        back,
        aero_storage::DiskError::Io(msg) if msg == "boom"
    ));
}

#[test]
fn virtual_disk_from_emu_disk_backend_supports_unaligned_reads() {
    let mut backend = EmuMemDisk::new(2);
    backend.data_mut()[0..512].fill(0xAA);
    backend.data_mut()[512..1024].fill(0xBB);

    let mut disk = VirtualDiskFromEmuDiskBackend(backend);

    // Span two sectors.
    let mut buf = [0u8; 2];
    disk.read_at(511, &mut buf).unwrap();
    assert_eq!(buf, [0xAA, 0xBB]);

    // Within one sector.
    let mut buf = [0u8; 3];
    disk.read_at(1, &mut buf).unwrap();
    assert_eq!(buf, [0xAA, 0xAA, 0xAA]);
}

#[test]
fn virtual_disk_from_emu_disk_backend_supports_unaligned_writes() {
    let mut backend = EmuMemDisk::new(2);
    backend.data_mut()[0..512].fill(0xAA);
    backend.data_mut()[512..1024].fill(0xBB);

    let mut disk = VirtualDiskFromEmuDiskBackend(backend);

    // Within a sector.
    disk.write_at(1, &[1, 2, 3]).unwrap();
    assert_eq!(disk.0.data()[0], 0xAA);
    assert_eq!(&disk.0.data()[1..4], &[1, 2, 3]);
    assert_eq!(disk.0.data()[4], 0xAA);

    // Span two sectors.
    disk.write_at(511, &[0x11, 0x22]).unwrap();
    assert_eq!(disk.0.data()[510], 0xAA);
    assert_eq!(disk.0.data()[511], 0x11);
    assert_eq!(disk.0.data()[512], 0x22);
    assert_eq!(disk.0.data()[513], 0xBB);
}

#[test]
fn virtual_disk_from_emu_disk_backend_reports_out_of_bounds() {
    let backend = EmuMemDisk::new(1);
    let mut disk = VirtualDiskFromEmuDiskBackend(backend);

    let mut buf = [0u8; 1];
    let err = disk.read_at(512, &mut buf).unwrap_err();
    assert!(matches!(
        err,
        aero_storage::DiskError::OutOfBounds {
            offset: 512,
            len: 1,
            capacity: 512,
        }
    ));

    let err = disk.write_at(512, &[0u8; 1]).unwrap_err();
    assert!(matches!(
        err,
        aero_storage::DiskError::OutOfBounds {
            offset: 512,
            len: 1,
            capacity: 512,
        }
    ));
}

#[test]
fn virtual_disk_from_emu_disk_backend_reports_offset_overflow() {
    let backend = EmuMemDisk::new(1);
    let mut disk = VirtualDiskFromEmuDiskBackend(backend);

    let mut buf = [0u8; 1];
    let err = disk.read_at(u64::MAX, &mut buf).unwrap_err();
    assert!(matches!(err, aero_storage::DiskError::OffsetOverflow));

    let err = disk.write_at(u64::MAX, &[0u8; 1]).unwrap_err();
    assert!(matches!(err, aero_storage::DiskError::OffsetOverflow));
}

#[test]
fn virtual_disk_from_emu_disk_backend_supports_4096_sector_backends() {
    let mut backend = EmuMemDisk::new_with_sector_size(2, 4096);
    backend.data_mut()[0..4096].fill(0xAA);
    backend.data_mut()[4096..8192].fill(0xBB);
    let mut disk = VirtualDiskFromEmuDiskBackend(backend);

    let mut buf = [0u8; 1];
    disk.read_at(4095, &mut buf).unwrap();
    assert_eq!(buf, [0xAA]);

    let mut buf = [0u8; 2];
    disk.read_at(4095, &mut buf).unwrap();
    assert_eq!(buf, [0xAA, 0xBB]);

    disk.write_at(4095, &[0x11, 0x22]).unwrap();
    assert_eq!(disk.0.data()[4094], 0xAA);
    assert_eq!(disk.0.data()[4095], 0x11);
    assert_eq!(disk.0.data()[4096], 0x22);
    assert_eq!(disk.0.data()[4097], 0xBB);
}
