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

    // QCOW2: check the magic and a plausible version field. A QCOW2 header is at least 72 bytes.
    //
    // For truncated images (< 8 bytes) that still match the magic, treat them as QCOW2 so callers
    // get a corruption error instead of silently falling back to raw. For non-truncated images,
    // keep detection conservative by only accepting v2/v3.
    if len >= 4 && len < 8 {
        let mut first4 = [0u8; 4];
        backend.read_at(0, &mut first4)?;
        if first4 == QCOW2_MAGIC {
            return Ok(DiskFormat::Qcow2);
        }
    }

    if len >= 8 {
        let mut first8 = [0u8; 8];
        backend.read_at(0, &mut first8)?;
        if first8[..4] == QCOW2_MAGIC {
            let version = be_u32(&first8[4..8]);
            if version == 2 || version == 3 {
                return Ok(DiskFormat::Qcow2);
            }
        }

        // AeroSparse: check magic plus a minimally plausible version field.
        //
        // We intentionally avoid fully validating the header here; `open_auto` should attempt to
        // open the image and report a corruption/unsupported error instead of silently treating an
        // AeroSparse-looking image as raw.
        if first8 == AEROSPAR_MAGIC {
            // If the file is too small to contain a complete header, still treat it as AeroSparse
            // so callers get a corruption error instead of silently falling back to raw.
            if len < 64 {
                return Ok(DiskFormat::AeroSparse);
            }

            let mut header = [0u8; 64];
            backend.read_at(0, &mut header)?;

            let version = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
            if version == 1 {
                return Ok(DiskFormat::AeroSparse);
            }
        }

        // VHD dynamic disks commonly store a footer copy at offset 0.
        if first8 == VHD_COOKIE {
            // If the file begins with the VHD cookie but is too small to contain a complete footer,
            // still treat it as a VHD so callers get a structured corruption error instead of
            // silently falling back to raw.
            if len < VHD_FOOTER_SIZE as u64 {
                return Ok(DiskFormat::Vhd);
            }

            let mut footer = [0u8; VHD_FOOTER_SIZE];
            backend.read_at(0, &mut footer)?;
            if looks_like_vhd_footer(&footer, len) {
                // For fixed disks, a valid footer at offset 0 implies the optional footer copy is
                // present, meaning the file must be large enough to contain:
                //   footer_copy (512) + data (current_size) + eof_footer (512)
                //
                // Without this check, a raw disk image whose first sector coincidentally resembles
                // a VHD footer could be misclassified as a VHD and then fail to open.
                let disk_type = be_u32(&footer[60..64]);
                if disk_type == 2 {
                    let current_size = be_u64(&footer[48..56]);
                    if let Some(required) = current_size.checked_add((VHD_FOOTER_SIZE as u64) * 2)
                    {
                        if len >= required {
                            return Ok(DiskFormat::Vhd);
                        }
                    }
                } else {
                    return Ok(DiskFormat::Vhd);
                }
            }
        }
    }

    // VHD fixed disks have only a footer at the end; dynamic disks typically have a footer at
    // both the beginning and the end. Check both.
    if len >= VHD_FOOTER_SIZE as u64 {
        let mut cookie = [0u8; 8];
        backend.read_at(len - VHD_FOOTER_SIZE as u64, &mut cookie)?;
        if cookie == VHD_COOKIE {
            let mut footer = [0u8; VHD_FOOTER_SIZE];
            backend.read_at(len - VHD_FOOTER_SIZE as u64, &mut footer)?;
            if looks_like_vhd_footer(&footer, len) {
                return Ok(DiskFormat::Vhd);
            }
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
    if current_size == 0 || !current_size.is_multiple_of(VHD_FOOTER_SIZE as u64) {
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
            if !data_offset.is_multiple_of(VHD_FOOTER_SIZE as u64) {
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
