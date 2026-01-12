use crate::{AeroSparseDisk, Qcow2Disk, RawDisk, Result, StorageBackend, VhdDisk};

const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";
const AEROSPAR_MAGIC: [u8; 8] = *b"AEROSPAR";
const VHD_COOKIE: [u8; 8] = *b"conectix";

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

    if len >= 4 {
        let mut magic = [0u8; 4];
        backend.read_at(0, &mut magic)?;
        if magic == QCOW2_MAGIC {
            return Ok(DiskFormat::Qcow2);
        }
    }

    // VHD fixed disks have only a footer at the end; dynamic disks typically have a footer at
    // both the beginning and the end. Check both.
    if len >= 512 {
        let mut cookie = [0u8; 8];
        backend.read_at(len - 512, &mut cookie)?;
        if cookie == VHD_COOKIE {
            return Ok(DiskFormat::Vhd);
        }
        backend.read_at(0, &mut cookie)?;
        if cookie == VHD_COOKIE {
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
