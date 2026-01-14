pub mod aerospar;
/// Legacy sparse disk format (`AEROSPRS`) kept for backward compatibility and migration.
///
/// New sparse images should use the canonical `AEROSPAR` format in `crates/aero-storage`.
pub mod aerosprs;
pub mod qcow2;
pub mod raw;
pub mod sparse;
pub mod vhd;

use crate::io::storage::adapters::aero_storage_disk_error_to_emulator;
use crate::io::storage::disk::DiskFormat;
use crate::io::storage::error::DiskResult;

pub use qcow2::Qcow2Disk;
pub use raw::RawDisk;
pub use sparse::SparseDisk;
pub use vhd::VhdDisk;

const AEROSPRS_MAGIC: [u8; 8] = *b"AEROSPRS";

pub fn detect_format<S: aero_storage::StorageBackend>(storage: &mut S) -> DiskResult<DiskFormat> {
    // Use the canonical `aero_storage` format detection logic for QCOW2/VHD/AEROSPAR.
    // The emulator-only legacy `AEROSPRS` case is handled below.
    let canonical =
        aero_storage::detect_format(storage).map_err(aero_storage_disk_error_to_emulator)?;

    let detected = match canonical {
        aero_storage::DiskFormat::Raw => DiskFormat::Raw,
        aero_storage::DiskFormat::Qcow2 => DiskFormat::Qcow2,
        aero_storage::DiskFormat::Vhd => DiskFormat::Vhd,
        aero_storage::DiskFormat::AeroSparse => DiskFormat::Sparse,
    };

    if detected != DiskFormat::Raw {
        return Ok(detected);
    }

    // Emulator-only legacy format: `AEROSPRS`.
    //
    // This older sparse format is not part of the canonical `aero_storage` crate, but we keep
    // detection for backwards compatibility/migration.
    let len = storage.len().map_err(aero_storage_disk_error_to_emulator)?;
    if len >= 8 {
        let mut magic = [0u8; 8];
        storage
            .read_at(0, &mut magic)
            .map_err(aero_storage_disk_error_to_emulator)?;
        if magic == AEROSPRS_MAGIC {
            // If the file is too small to contain a full header, still treat it as sparse so
            // `open_auto` returns a truncation/corruption error instead of silently opening raw.
            if len < 4096 {
                return Ok(DiskFormat::Sparse);
            }
            if len >= 12 {
                let mut version = [0u8; 4];
                storage
                    .read_at(8, &mut version)
                    .map_err(aero_storage_disk_error_to_emulator)?;
                if u32::from_le_bytes(version) == 1 {
                    return Ok(DiskFormat::Sparse);
                }
            }
        }
    }

    Ok(DiskFormat::Raw)
}
