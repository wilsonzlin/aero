pub mod qcow2;
pub mod raw;
pub mod sparse;
pub mod vhd;

use crate::io::storage::disk::{ByteStorage, DiskFormat};
use crate::io::storage::error::DiskResult;

pub use qcow2::Qcow2Disk;
pub use raw::RawDisk;
pub use sparse::{SparseDisk, SparseHeader};
pub use vhd::VhdDisk;

const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";
const SPARSE_MAGIC: [u8; 8] = *b"AEROSPRS";
const VHD_COOKIE: [u8; 8] = *b"conectix";

pub fn detect_format<S: ByteStorage>(storage: &mut S) -> DiskResult<DiskFormat> {
    let len = storage.len()?;

    if len >= 4 {
        let mut magic = [0u8; 4];
        storage.read_at(0, &mut magic)?;
        if magic == QCOW2_MAGIC {
            return Ok(DiskFormat::Qcow2);
        }
    }

    if len >= 512 {
        let mut cookie = [0u8; 8];
        storage.read_at(len - 512, &mut cookie)?;
        if cookie == VHD_COOKIE {
            return Ok(DiskFormat::Vhd);
        }
        storage.read_at(0, &mut cookie)?;
        if cookie == VHD_COOKIE {
            return Ok(DiskFormat::Vhd);
        }
    }

    if len >= 8 {
        let mut magic = [0u8; 8];
        storage.read_at(0, &mut magic)?;
        if magic == SPARSE_MAGIC {
            return Ok(DiskFormat::Sparse);
        }
    }

    Ok(DiskFormat::Raw)
}
