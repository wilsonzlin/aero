pub mod aerospar;
pub mod aerosprs;
pub mod qcow2;
pub mod raw;
pub mod sparse;
pub mod vhd;

use crate::io::storage::disk::{ByteStorage, DiskFormat};
use crate::io::storage::error::DiskResult;

pub use qcow2::Qcow2Disk;
pub use raw::RawDisk;
pub use sparse::SparseDisk;
pub use vhd::VhdDisk;

const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";
const AEROSPAR_MAGIC: [u8; 8] = *b"AEROSPAR";
const AEROSPRS_MAGIC: [u8; 8] = *b"AEROSPRS";
const VHD_COOKIE: [u8; 8] = *b"conectix";

pub fn detect_format<S: ByteStorage>(storage: &mut S) -> DiskResult<DiskFormat> {
    let len = storage.len()?;

    // QCOW2: check the magic and a plausible version field. A QCOW2 header is at least 72 bytes.
    //
    // For truncated images (< 8 bytes) that still match the magic, treat them as QCOW2 so callers
    // get a corruption error instead of silently falling back to raw.
    if (4..8).contains(&len) {
        let mut first4 = [0u8; 4];
        storage.read_at(0, &mut first4)?;
        if first4 == QCOW2_MAGIC {
            return Ok(DiskFormat::Qcow2);
        }
    }

    // Read the first 12 bytes when possible so we can check both the QCOW2 version field
    // (big-endian at offset 4) and the sparse-format version field (little-endian at offset 8)
    // without issuing multiple small reads.
    let mut first8: Option<[u8; 8]> = None;
    let mut le_version_at_8: u32 = 0;
    if len >= 8 {
        if len >= 12 {
            let mut first12 = [0u8; 12];
            storage.read_at(0, &mut first12)?;
            let mut buf8 = [0u8; 8];
            buf8.copy_from_slice(&first12[..8]);
            first8 = Some(buf8);
            le_version_at_8 = u32::from_le_bytes([first12[8], first12[9], first12[10], first12[11]]);
        } else {
            let mut buf8 = [0u8; 8];
            storage.read_at(0, &mut buf8)?;
            first8 = Some(buf8);
        }
    }

    if let Some(first8) = first8 {
        if first8[..4] == QCOW2_MAGIC {
            let version = be_u32(&first8[4..8]);
            if version == 2 || version == 3 {
                return Ok(DiskFormat::Qcow2);
            }
        }

        // Dynamic VHDs store a footer copy at offset 0. If the file begins with the VHD cookie but
        // is too small to contain a complete footer, still treat it as a VHD so `open_auto`
        // returns a corruption error instead of silently falling back to raw.
        if len < 512 && first8 == VHD_COOKIE {
            return Ok(DiskFormat::Vhd);
        }

        if first8 == AEROSPAR_MAGIC {
            // If the file is too small to contain a full header, still treat it as sparse so
            // `open_auto` returns a corruption error rather than silently falling back to raw.
            if len < 64 {
                return Ok(DiskFormat::Sparse);
            }
            if le_version_at_8 == 1 {
                return Ok(DiskFormat::Sparse);
            }
        }

        if first8 == AEROSPRS_MAGIC {
            // If the file is too small to contain a full header, still treat it as sparse so
            // `open_auto` returns a corruption error rather than silently falling back to raw.
            if len < 4096 {
                return Ok(DiskFormat::Sparse);
            }
            if le_version_at_8 == 1 {
                return Ok(DiskFormat::Sparse);
            }
        }
    }

    if len >= 512 {
        // Check footer at EOF first (fixed + dynamic VHDs).
        let mut footer = [0u8; 512];
        storage.read_at(len - 512, &mut footer)?;
        if looks_like_vhd_footer(&footer, len) {
            return Ok(DiskFormat::Vhd);
        }
        // Dynamic VHDs (and some fixed disks) store a footer copy at offset 0.
        storage.read_at(0, &mut footer)?;
        if looks_like_vhd_footer(&footer, len) {
            // For fixed disks, a valid footer at offset 0 implies an optional footer copy, meaning
            // the file must be large enough to contain:
            //   footer_copy (512) + data (current_size) + eof_footer (512)
            //
            // Without this check, a raw disk image whose first sector coincidentally resembles a
            // VHD footer could be misclassified as VHD and then fail to open.
            let disk_type = be_u32(&footer[60..64]);
            if disk_type == 2 {
                let current_size = be_u64(&footer[48..56]);
                if let Some(required) = current_size.checked_add(1024) {
                    if len >= required {
                        return Ok(DiskFormat::Vhd);
                    }
                }
            } else {
                return Ok(DiskFormat::Vhd);
            }
        }
    }

    Ok(DiskFormat::Raw)
}

fn looks_like_vhd_footer(footer: &[u8; 512], file_len: u64) -> bool {
    if footer[..8] != VHD_COOKIE {
        return false;
    }
    if be_u32(&footer[12..16]) != 0x0001_0000 {
        return false;
    }

    let current_size = be_u64(&footer[48..56]);
    if current_size == 0 || !current_size.is_multiple_of(512) {
        return false;
    }
    let disk_type = be_u32(&footer[60..64]);
    if disk_type != 2 && disk_type != 3 {
        return false;
    }
    let data_offset = be_u64(&footer[16..24]);
    match disk_type {
        2 => {
            if data_offset != u64::MAX {
                return false;
            }
            let Some(required_len) = current_size.checked_add(512) else {
                return false;
            };
            if file_len < required_len {
                return false;
            }
        }
        3 => {
            if data_offset == u64::MAX {
                return false;
            }
            if !data_offset.is_multiple_of(512) {
                return false;
            }
            if data_offset < 512 {
                return false;
            }
            let Some(end) = data_offset.checked_add(1024) else {
                return false;
            };
            if end > file_len {
                return false;
            }
        }
        _ => return false,
    }
    true
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn be_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}
