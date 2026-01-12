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

    if len >= 8 {
        let mut first8 = [0u8; 8];
        storage.read_at(0, &mut first8)?;
        if first8[..4] == QCOW2_MAGIC {
            let version = be_u32(&first8[4..8]);
            if version == 2 || version == 3 {
                return Ok(DiskFormat::Qcow2);
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
                let Some(required) = current_size.checked_add(1024) else {
                    return Ok(DiskFormat::Raw);
                };
                if len >= required {
                    return Ok(DiskFormat::Vhd);
                }
            } else {
                return Ok(DiskFormat::Vhd);
            }
        }
    }

    if len >= 8 {
        let mut magic = [0u8; 8];
        storage.read_at(0, &mut magic)?;
        if magic == AEROSPAR_MAGIC {
            // If the file is too small to contain a full header, still treat it as sparse so
            // `open_auto` returns a corruption error rather than silently falling back to raw.
            if len < 64 {
                return Ok(DiskFormat::Sparse);
            }

            let mut header = [0u8; 64];
            storage.read_at(0, &mut header)?;

            let version = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
            let header_size =
                u32::from_le_bytes([header[12], header[13], header[14], header[15]]) as u64;
            let table_offset = u64::from_le_bytes([
                header[32], header[33], header[34], header[35], header[36], header[37], header[38],
                header[39],
            ]);

            if version == 1 && header_size == 64 && table_offset == 64 {
                return Ok(DiskFormat::Sparse);
            }
        }
        if magic == AEROSPRS_MAGIC {
            // If the file is too small to contain a full header, still treat it as sparse so
            // `open_auto` returns a corruption error rather than silently falling back to raw.
            if len < 4096 {
                return Ok(DiskFormat::Sparse);
            }

            let mut header = [0u8; 4096];
            storage.read_at(0, &mut header)?;
            if aerosprs::SparseHeader::decode(&header).is_ok() {
                return Ok(DiskFormat::Sparse);
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
