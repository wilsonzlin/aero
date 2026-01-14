use crate::io::storage::adapters::aero_storage_disk_error_to_emulator;
use crate::io::storage::disk::{DiskBackend, MaybeSend};
use crate::io::storage::error::{DiskError, DiskResult};

use super::aerospar::AerosparDisk;
use super::aerosprs::SparseDisk as AerosprsDisk;

const AEROSPAR_MAGIC: [u8; 8] = *b"AEROSPAR";
const AEROSPRS_MAGIC: [u8; 8] = *b"AEROSPRS";

/// Sparse disk images supported by the emulator.
///
/// - `AEROSPAR`: current sparse disk format (also used by the wasm32 OPFS backend in
///   `crates/aero-opfs`)
/// - `AEROSPRS`: legacy format (kept for backwards compatibility + migration)
///
/// See also: `docs/20-storage-trait-consolidation.md` (authoritative format status/migration notes).
pub enum SparseDisk<S> {
    Aerospar(AerosparDisk<S>),
    Aerosprs(AerosprsDisk<S>),
}

impl<S: aero_storage::StorageBackend> SparseDisk<S> {
    /// Create a new sparse disk image in the current `AEROSPAR` format.
    ///
    /// Note: this does **not** create legacy `AEROSPRS` images; that format is supported only for
    /// opening/migrating older images.
    pub fn create(
        storage: S,
        sector_size: u32,
        total_sectors: u64,
        block_size: u32,
    ) -> DiskResult<Self> {
        Ok(Self::Aerospar(AerosparDisk::create(
            storage,
            sector_size,
            total_sectors,
            block_size,
        )?))
    }

    /// Open a sparse disk image, auto-selecting between `AEROSPAR` and legacy `AEROSPRS` based on
    /// the magic header.
    pub fn open(mut storage: S) -> DiskResult<Self> {
        let len = storage.len().map_err(aero_storage_disk_error_to_emulator)?;
        if len < 8 {
            return Err(DiskError::CorruptImage("sparse header truncated"));
        }

        let mut magic = [0u8; 8];
        storage
            .read_at(0, &mut magic)
            .map_err(aero_storage_disk_error_to_emulator)?;

        if magic == AEROSPAR_MAGIC {
            return Ok(Self::Aerospar(AerosparDisk::open(storage)?));
        }
        if magic == AEROSPRS_MAGIC {
            return Ok(Self::Aerosprs(AerosprsDisk::open(storage)?));
        }

        Err(DiskError::CorruptImage("sparse magic mismatch"))
    }

    pub fn into_storage(self) -> S {
        match self {
            Self::Aerospar(disk) => disk.into_storage(),
            Self::Aerosprs(disk) => disk.into_storage(),
        }
    }
}

impl<S: aero_storage::StorageBackend + MaybeSend> DiskBackend for SparseDisk<S> {
    fn sector_size(&self) -> u32 {
        match self {
            Self::Aerospar(disk) => disk.sector_size(),
            Self::Aerosprs(disk) => disk.sector_size(),
        }
    }

    fn total_sectors(&self) -> u64 {
        match self {
            Self::Aerospar(disk) => disk.total_sectors(),
            Self::Aerosprs(disk) => disk.total_sectors(),
        }
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        match self {
            Self::Aerospar(disk) => disk.read_sectors(lba, buf),
            Self::Aerosprs(disk) => disk.read_sectors(lba, buf),
        }
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        match self {
            Self::Aerospar(disk) => disk.write_sectors(lba, buf),
            Self::Aerosprs(disk) => disk.write_sectors(lba, buf),
        }
    }

    fn flush(&mut self) -> DiskResult<()> {
        match self {
            Self::Aerospar(disk) => disk.flush(),
            Self::Aerosprs(disk) => disk.flush(),
        }
    }
}
