use crate::{AeroSparseDisk, Qcow2Disk, RawDisk, Result, StorageBackend, VhdDisk};

const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";
const AEROSPAR_MAGIC: [u8; 8] = *b"AEROSPAR";
const VHD_COOKIE: [u8; 8] = *b"conectix";
const VHD_FOOTER_SIZE: usize = 512;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DiskFormat {
    Raw,
    AeroSparse,
    Qcow2,
    Vhd,
}

/// Detect the on-disk image format from magic values.
///
/// Detection is conservative: unknown images fall back to [`DiskFormat::Raw`].
pub fn detect_format<B: StorageBackend>(backend: &mut B) -> Result<DiskFormat> {
    let len = backend.len()?;

    // QCOW2: check the magic and a plausible version field. A QCOW2 header is at least 72 bytes,
    // but we only need the first 8 bytes for conservative detection.
    if len >= 8 {
        let mut header8 = [0u8; 8];
        backend.read_at(0, &mut header8)?;
        if header8[..4] == QCOW2_MAGIC {
            let version = be_u32(&header8[4..8]);
            if version == 2 || version == 3 {
                return Ok(DiskFormat::Qcow2);
            }
        }
    }

    // VHD fixed disks have only a footer at the end; dynamic disks typically have a footer at
    // both the beginning and the end. Check both.
    if len >= VHD_FOOTER_SIZE as u64 {
        let mut footer = [0u8; VHD_FOOTER_SIZE];

        backend.read_at(len - VHD_FOOTER_SIZE as u64, &mut footer)?;
        if looks_like_vhd_footer(&footer, len) {
            return Ok(DiskFormat::Vhd);
        }

        backend.read_at(0, &mut footer)?;
        if looks_like_vhd_footer(&footer, len) {
            return Ok(DiskFormat::Vhd);
        }
    }

    if len >= 8 {
        let mut magic = [0u8; 8];
        backend.read_at(0, &mut magic)?;
        if magic == AEROSPAR_MAGIC {
            return Ok(DiskFormat::AeroSparse);
        }
    }

    Ok(DiskFormat::Raw)
}

fn looks_like_vhd_footer(footer: &[u8; VHD_FOOTER_SIZE], file_len: u64) -> bool {
    if footer[..8] != VHD_COOKIE {
        return false;
    }

    // The VHD footer is big-endian and has a fixed file format version.
    if be_u32(&footer[12..16]) != 0x0001_0000 {
        return false;
    }

    // Virtual disk size.
    let current_size = be_u64(&footer[48..56]);
    if current_size == 0 || current_size % (VHD_FOOTER_SIZE as u64) != 0 {
        return false;
    }

    let disk_type = be_u32(&footer[60..64]);
    if disk_type != 2 && disk_type != 3 {
        return false;
    }

    // Fixed: data_offset is 0xFFFF..FFFF.
    // Dynamic: data_offset points to the dynamic header and must be 512-byte aligned.
    let data_offset = be_u64(&footer[16..24]);
    match disk_type {
        2 => {
            if data_offset != u64::MAX {
                return false;
            }

            // A fixed VHD consists of the data region followed by a single 512-byte footer.
            let Some(required_len) = current_size.checked_add(VHD_FOOTER_SIZE as u64) else {
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
            if data_offset % (VHD_FOOTER_SIZE as u64) != 0 {
                return false;
            }
            // The footer copy occupies the first sector of the file.
            if data_offset < VHD_FOOTER_SIZE as u64 {
                return false;
            }
            // The dynamic header is 1024 bytes.
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

/// A convenience wrapper that can open multiple disk image formats from a single backend.
pub enum DiskImage<B> {
    Raw(RawDisk<B>),
    AeroSparse(AeroSparseDisk<B>),
    Qcow2(Qcow2Disk<B>),
    Vhd(Box<VhdDisk<B>>),
}

impl<B: StorageBackend> DiskImage<B> {
    pub fn format(&self) -> DiskFormat {
        match self {
            Self::Raw(_) => DiskFormat::Raw,
            Self::AeroSparse(_) => DiskFormat::AeroSparse,
            Self::Qcow2(_) => DiskFormat::Qcow2,
            Self::Vhd(_) => DiskFormat::Vhd,
        }
    }

    pub fn open_with_format(format: DiskFormat, backend: B) -> Result<Self> {
        match format {
            DiskFormat::Raw => Ok(Self::Raw(RawDisk::open(backend)?)),
            DiskFormat::AeroSparse => Ok(Self::AeroSparse(AeroSparseDisk::open(backend)?)),
            DiskFormat::Qcow2 => Ok(Self::Qcow2(Qcow2Disk::open(backend)?)),
            DiskFormat::Vhd => Ok(Self::Vhd(Box::new(VhdDisk::open(backend)?))),
        }
    }

    pub fn open_auto(mut backend: B) -> Result<Self> {
        let format = detect_format(&mut backend)?;
        Self::open_with_format(format, backend)
    }

    pub fn into_backend(self) -> B {
        match self {
            Self::Raw(d) => d.into_backend(),
            Self::AeroSparse(d) => d.into_backend(),
            Self::Qcow2(d) => d.into_backend(),
            Self::Vhd(d) => d.into_backend(),
        }
    }
}

impl<B: StorageBackend> crate::VirtualDisk for DiskImage<B> {
    fn capacity_bytes(&self) -> u64 {
        match self {
            Self::Raw(d) => d.capacity_bytes(),
            Self::AeroSparse(d) => d.capacity_bytes(),
            Self::Qcow2(d) => d.capacity_bytes(),
            Self::Vhd(d) => d.capacity_bytes(),
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        match self {
            Self::Raw(d) => d.read_at(offset, buf),
            Self::AeroSparse(d) => d.read_at(offset, buf),
            Self::Qcow2(d) => d.read_at(offset, buf),
            Self::Vhd(d) => d.read_at(offset, buf),
        }
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        match self {
            Self::Raw(d) => d.write_at(offset, buf),
            Self::AeroSparse(d) => d.write_at(offset, buf),
            Self::Qcow2(d) => d.write_at(offset, buf),
            Self::Vhd(d) => d.write_at(offset, buf),
        }
    }

    fn flush(&mut self) -> Result<()> {
        match self {
            Self::Raw(d) => d.flush(),
            Self::AeroSparse(d) => d.flush(),
            Self::Qcow2(d) => d.flush(),
            Self::Vhd(d) => d.flush(),
        }
    }
}
