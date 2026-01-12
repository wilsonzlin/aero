pub mod aerospar;
pub mod aerosprs;
pub mod qcow2;
pub mod raw;
pub mod sparse;
pub mod vhd;

use aero_storage::AeroSparseHeader;

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
            return Ok(DiskFormat::Vhd);
        }
    }

    if len >= 8 {
        let mut magic = [0u8; 8];
        storage.read_at(0, &mut magic)?;
        if magic == AEROSPAR_MAGIC && len >= 64 {
            let mut header = [0u8; 64];
            storage.read_at(0, &mut header)?;
            if AeroSparseHeader::decode(&header).is_ok() {
                return Ok(DiskFormat::Sparse);
            }
        }
        if magic == AEROSPRS_MAGIC && len >= 4096 {
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
    if current_size == 0 || current_size % 512 != 0 {
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
            if data_offset % 512 != 0 {
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
