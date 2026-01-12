//! Compatibility adapters between the emulator storage traits (`ByteStorage`, `DiskBackend`)
//! and the canonical `aero_storage` traits (`StorageBackend`, `VirtualDisk`).
//!
//! These adapters are intentionally lightweight and aim to preserve as much error information
//! as possible when crossing crate boundaries.
//!
//! See `docs/20-storage-trait-consolidation.md`.

use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

/// Wrap an emulator [`ByteStorage`] and expose it as an [`aero_storage::StorageBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorageBackendFromByteStorage<S>(pub S);

impl<S> StorageBackendFromByteStorage<S> {
    pub fn new(inner: S) -> Self {
        Self(inner)
    }

    pub fn into_inner(self) -> S {
        self.0
    }
}

/// Wrap an [`aero_storage::StorageBackend`] and expose it as an emulator [`ByteStorage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteStorageFromStorageBackend<B>(pub B);

impl<B> ByteStorageFromStorageBackend<B> {
    pub fn new(inner: B) -> Self {
        Self(inner)
    }

    pub fn into_inner(self) -> B {
        self.0
    }
}

/// Wrap an [`aero_storage::VirtualDisk`] and expose it as an emulator [`DiskBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmuDiskBackendFromVirtualDisk<D>(pub D);

impl<D> EmuDiskBackendFromVirtualDisk<D> {
    pub fn new(inner: D) -> Self {
        Self(inner)
    }

    pub fn into_inner(self) -> D {
        self.0
    }
}

/// Wrap an emulator [`DiskBackend`] and expose it as an [`aero_storage::VirtualDisk`].
///
/// This adapter is only correct when the emulator backend uses 512-byte sectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtualDiskFromEmuDiskBackend<B>(pub B);

impl<B> VirtualDiskFromEmuDiskBackend<B> {
    pub fn new(inner: B) -> Self {
        Self(inner)
    }

    pub fn into_inner(self) -> B {
        self.0
    }
}

/// Convert an emulator [`DiskError`] into an [`aero_storage::DiskError`].
///
/// Some `aero-storage` variants (notably `OutOfBounds`) require additional context; callers
/// should provide `offset` / `len` / `capacity` when available.
pub fn emulator_disk_error_to_aero_storage(
    err: DiskError,
    offset: Option<u64>,
    len: Option<usize>,
    capacity: Option<u64>,
) -> aero_storage::DiskError {
    match err {
        DiskError::UnalignedBuffer { len, sector_size } => {
            aero_storage::DiskError::UnalignedLength {
                len,
                alignment: sector_size as usize,
            }
        }
        DiskError::InvalidBufferLength => aero_storage::DiskError::UnalignedLength {
            len: len.unwrap_or(0),
            alignment: 1,
        },
        DiskError::OutOfBounds | DiskError::OutOfRange { .. } => {
            aero_storage::DiskError::OutOfBounds {
                offset: offset.unwrap_or(0),
                len: len.unwrap_or(0),
                capacity: capacity.unwrap_or(0),
            }
        }
        DiskError::CorruptImage(msg) => aero_storage::DiskError::CorruptImage(msg),
        DiskError::Unsupported("offset overflow") => aero_storage::DiskError::OffsetOverflow,
        DiskError::Unsupported(msg) => aero_storage::DiskError::Unsupported(msg),
        DiskError::NotSupported(msg) => aero_storage::DiskError::NotSupported(msg),
        DiskError::QuotaExceeded => aero_storage::DiskError::QuotaExceeded,
        DiskError::InUse => aero_storage::DiskError::InUse,
        DiskError::InvalidState(msg) => aero_storage::DiskError::InvalidState(msg),
        DiskError::BackendUnavailable => aero_storage::DiskError::BackendUnavailable,
        DiskError::Io(msg) => aero_storage::DiskError::Io(msg),
    }
}

/// Convert an [`aero_storage::DiskError`] into an emulator [`DiskError`].
pub fn aero_storage_disk_error_to_emulator(err: aero_storage::DiskError) -> DiskError {
    match err {
        aero_storage::DiskError::UnalignedLength { len, alignment } => DiskError::UnalignedBuffer {
            len,
            sector_size: alignment.try_into().unwrap_or(512),
        },
        aero_storage::DiskError::OutOfBounds { .. } => DiskError::OutOfBounds,
        aero_storage::DiskError::OffsetOverflow => DiskError::Unsupported("offset overflow"),
        aero_storage::DiskError::CorruptImage(msg) => DiskError::CorruptImage(msg),
        aero_storage::DiskError::Unsupported(msg) => DiskError::Unsupported(msg),
        aero_storage::DiskError::InvalidSparseHeader(msg) => DiskError::CorruptImage(msg),
        aero_storage::DiskError::InvalidConfig(msg) => DiskError::Unsupported(msg),
        aero_storage::DiskError::CorruptSparseImage(msg) => DiskError::CorruptImage(msg),
        aero_storage::DiskError::NotSupported(msg) => DiskError::NotSupported(msg),
        aero_storage::DiskError::QuotaExceeded => DiskError::QuotaExceeded,
        aero_storage::DiskError::InUse => DiskError::InUse,
        aero_storage::DiskError::InvalidState(msg) => DiskError::InvalidState(msg),
        aero_storage::DiskError::BackendUnavailable => DiskError::BackendUnavailable,
        aero_storage::DiskError::Io(msg) => DiskError::Io(msg),
    }
}

/// Convert an [`aero_storage::DiskError`] into an emulator [`DiskError`], preserving disk-range
/// context when possible.
pub fn aero_storage_disk_error_to_emulator_with_sector_context(
    err: aero_storage::DiskError,
    lba: u64,
    sectors: u64,
    capacity_sectors: u64,
) -> DiskError {
    match err {
        aero_storage::DiskError::OutOfBounds { .. } | aero_storage::DiskError::OffsetOverflow => {
            DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            }
        }
        other => aero_storage_disk_error_to_emulator(other),
    }
}

impl<S: ByteStorage> aero_storage::StorageBackend for StorageBackendFromByteStorage<S> {
    fn len(&mut self) -> aero_storage::Result<u64> {
        self.0
            .len()
            .map_err(|e| emulator_disk_error_to_aero_storage(e, None, None, None))
    }

    fn set_len(&mut self, len: u64) -> aero_storage::Result<()> {
        self.0
            .set_len(len)
            .map_err(|e| emulator_disk_error_to_aero_storage(e, None, None, Some(len)))
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        match self.0.read_at(offset, buf) {
            Ok(()) => Ok(()),
            Err(err) => {
                // Best-effort capacity reporting: if the underlying backend supports `len()`,
                // include it in the aero-storage out-of-bounds error.
                let capacity = self.0.len().ok();
                Err(emulator_disk_error_to_aero_storage(
                    err,
                    Some(offset),
                    Some(buf.len()),
                    capacity,
                ))
            }
        }
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        match self.0.write_at(offset, buf) {
            Ok(()) => Ok(()),
            Err(err) => {
                let capacity = self.0.len().ok();
                Err(emulator_disk_error_to_aero_storage(
                    err,
                    Some(offset),
                    Some(buf.len()),
                    capacity,
                ))
            }
        }
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.0
            .flush()
            .map_err(|e| emulator_disk_error_to_aero_storage(e, None, None, None))
    }
}

impl<B: aero_storage::StorageBackend> ByteStorage for ByteStorageFromStorageBackend<B> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.0
            .read_at(offset, buf)
            .map_err(aero_storage_disk_error_to_emulator)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()> {
        self.0
            .write_at(offset, buf)
            .map_err(aero_storage_disk_error_to_emulator)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.0.flush().map_err(aero_storage_disk_error_to_emulator)
    }

    fn len(&mut self) -> DiskResult<u64> {
        self.0.len().map_err(aero_storage_disk_error_to_emulator)
    }

    fn set_len(&mut self, len: u64) -> DiskResult<()> {
        self.0
            .set_len(len)
            .map_err(aero_storage_disk_error_to_emulator)
    }
}

impl<D: aero_storage::VirtualDisk> DiskBackend for EmuDiskBackendFromVirtualDisk<D> {
    fn sector_size(&self) -> u32 {
        aero_storage::SECTOR_SIZE as u32
    }

    fn total_sectors(&self) -> u64 {
        self.0.capacity_bytes() / aero_storage::SECTOR_SIZE as u64
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        let sector_size = self.sector_size();
        if !buf.len().is_multiple_of(sector_size as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: buf.len(),
                sector_size,
            });
        }
        let sectors = (buf.len() / sector_size as usize) as u64;
        let capacity_sectors = self.total_sectors();
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors,
        })?;
        if end > capacity_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            });
        }

        let offset = lba
            .checked_mul(sector_size as u64)
            .ok_or(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            })?;

        self.0.read_at(offset, buf).map_err(|err| {
            aero_storage_disk_error_to_emulator_with_sector_context(
                err,
                lba,
                sectors,
                capacity_sectors,
            )
        })
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        let sector_size = self.sector_size();
        if !buf.len().is_multiple_of(sector_size as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: buf.len(),
                sector_size,
            });
        }
        let sectors = (buf.len() / sector_size as usize) as u64;
        let capacity_sectors = self.total_sectors();
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors,
        })?;
        if end > capacity_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            });
        }

        let offset = lba
            .checked_mul(sector_size as u64)
            .ok_or(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors,
            })?;

        self.0.write_at(offset, buf).map_err(|err| {
            aero_storage_disk_error_to_emulator_with_sector_context(
                err,
                lba,
                sectors,
                capacity_sectors,
            )
        })
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.0.flush().map_err(aero_storage_disk_error_to_emulator)
    }
}

impl<B: DiskBackend> aero_storage::VirtualDisk for VirtualDiskFromEmuDiskBackend<B> {
    fn capacity_bytes(&self) -> u64 {
        self.0
            .total_sectors()
            .saturating_mul(self.0.sector_size() as u64)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }

        let sector_size = self.0.sector_size();
        if sector_size != aero_storage::SECTOR_SIZE as u32 {
            return Err(aero_storage::DiskError::InvalidConfig(
                "VirtualDiskFromEmuDiskBackend requires 512-byte sectors",
            ));
        }

        let capacity = self
            .0
            .total_sectors()
            .checked_mul(sector_size as u64)
            .ok_or(aero_storage::DiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(aero_storage::DiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(aero_storage::DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity,
            });
        }

        let sector_size_u64 = sector_size as u64;
        let mut remaining = buf;
        let mut cur_offset = offset;

        // Handle an initial partial sector.
        let first_off = (cur_offset % sector_size_u64) as usize;
        if first_off != 0 {
            let lba = cur_offset / sector_size_u64;
            let mut sector_buf = [0u8; aero_storage::SECTOR_SIZE];
            self.0.read_sectors(lba, &mut sector_buf).map_err(|e| {
                emulator_disk_error_to_aero_storage(
                    e,
                    Some(cur_offset),
                    Some(remaining.len()),
                    Some(capacity),
                )
            })?;
            let to_copy = remaining.len().min(aero_storage::SECTOR_SIZE - first_off);
            remaining[..to_copy].copy_from_slice(&sector_buf[first_off..first_off + to_copy]);
            remaining = &mut remaining[to_copy..];
            cur_offset = cur_offset
                .checked_add(to_copy as u64)
                .ok_or(aero_storage::DiskError::OffsetOverflow)?;
        }

        // Middle aligned sectors.
        if !remaining.is_empty() {
            let aligned_len =
                remaining.len() / aero_storage::SECTOR_SIZE * aero_storage::SECTOR_SIZE;
            if aligned_len > 0 {
                let lba = cur_offset / sector_size_u64;
                self.0
                    .read_sectors(lba, &mut remaining[..aligned_len])
                    .map_err(|e| {
                        emulator_disk_error_to_aero_storage(
                            e,
                            Some(cur_offset),
                            Some(aligned_len),
                            Some(capacity),
                        )
                    })?;
                remaining = &mut remaining[aligned_len..];
                cur_offset = cur_offset
                    .checked_add(aligned_len as u64)
                    .ok_or(aero_storage::DiskError::OffsetOverflow)?;
            }
        }

        // Trailing partial sector.
        if !remaining.is_empty() {
            let lba = cur_offset / sector_size_u64;
            let mut sector_buf = [0u8; aero_storage::SECTOR_SIZE];
            self.0.read_sectors(lba, &mut sector_buf).map_err(|e| {
                emulator_disk_error_to_aero_storage(
                    e,
                    Some(cur_offset),
                    Some(remaining.len()),
                    Some(capacity),
                )
            })?;
            remaining.copy_from_slice(&sector_buf[..remaining.len()]);
        }

        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }

        let sector_size = self.0.sector_size();
        if sector_size != aero_storage::SECTOR_SIZE as u32 {
            return Err(aero_storage::DiskError::InvalidConfig(
                "VirtualDiskFromEmuDiskBackend requires 512-byte sectors",
            ));
        }

        let capacity = self
            .0
            .total_sectors()
            .checked_mul(sector_size as u64)
            .ok_or(aero_storage::DiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(aero_storage::DiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(aero_storage::DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity,
            });
        }

        let sector_size_u64 = sector_size as u64;
        let mut remaining = buf;
        let mut cur_offset = offset;

        // Initial partial sector.
        let first_off = (cur_offset % sector_size_u64) as usize;
        if first_off != 0 {
            let lba = cur_offset / sector_size_u64;
            let mut sector_buf = [0u8; aero_storage::SECTOR_SIZE];
            self.0.read_sectors(lba, &mut sector_buf).map_err(|e| {
                emulator_disk_error_to_aero_storage(
                    e,
                    Some(cur_offset),
                    Some(remaining.len()),
                    Some(capacity),
                )
            })?;
            let to_copy = remaining.len().min(aero_storage::SECTOR_SIZE - first_off);
            sector_buf[first_off..first_off + to_copy].copy_from_slice(&remaining[..to_copy]);
            self.0.write_sectors(lba, &sector_buf).map_err(|e| {
                emulator_disk_error_to_aero_storage(
                    e,
                    Some(cur_offset),
                    Some(to_copy),
                    Some(capacity),
                )
            })?;
            remaining = &remaining[to_copy..];
            cur_offset = cur_offset
                .checked_add(to_copy as u64)
                .ok_or(aero_storage::DiskError::OffsetOverflow)?;
        }

        // Middle aligned sectors.
        if !remaining.is_empty() {
            let aligned_len =
                remaining.len() / aero_storage::SECTOR_SIZE * aero_storage::SECTOR_SIZE;
            if aligned_len > 0 {
                let lba = cur_offset / sector_size_u64;
                self.0
                    .write_sectors(lba, &remaining[..aligned_len])
                    .map_err(|e| {
                        emulator_disk_error_to_aero_storage(
                            e,
                            Some(cur_offset),
                            Some(aligned_len),
                            Some(capacity),
                        )
                    })?;
                remaining = &remaining[aligned_len..];
                cur_offset = cur_offset
                    .checked_add(aligned_len as u64)
                    .ok_or(aero_storage::DiskError::OffsetOverflow)?;
            }
        }

        // Trailing partial sector.
        if !remaining.is_empty() {
            let lba = cur_offset / sector_size_u64;
            let mut sector_buf = [0u8; aero_storage::SECTOR_SIZE];
            self.0.read_sectors(lba, &mut sector_buf).map_err(|e| {
                emulator_disk_error_to_aero_storage(
                    e,
                    Some(cur_offset),
                    Some(remaining.len()),
                    Some(capacity),
                )
            })?;
            sector_buf[..remaining.len()].copy_from_slice(remaining);
            self.0.write_sectors(lba, &sector_buf).map_err(|e| {
                emulator_disk_error_to_aero_storage(
                    e,
                    Some(cur_offset),
                    Some(remaining.len()),
                    Some(capacity),
                )
            })?;
        }

        Ok(())
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        let sector_size = self.0.sector_size();
        if sector_size != aero_storage::SECTOR_SIZE as u32 {
            return Err(aero_storage::DiskError::InvalidConfig(
                "VirtualDiskFromEmuDiskBackend requires 512-byte sectors",
            ));
        }
        self.0
            .flush()
            .map_err(|e| emulator_disk_error_to_aero_storage(e, None, None, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aero_storage_disk_error_to_emulator_maps_browser_variants() {
        assert_eq!(
            aero_storage_disk_error_to_emulator(aero_storage::DiskError::NotSupported(
                "opfs missing".into()
            )),
            DiskError::NotSupported("opfs missing".into())
        );
        assert_eq!(
            aero_storage_disk_error_to_emulator(aero_storage::DiskError::QuotaExceeded),
            DiskError::QuotaExceeded
        );
        assert_eq!(
            aero_storage_disk_error_to_emulator(aero_storage::DiskError::InUse),
            DiskError::InUse
        );
        assert_eq!(
            aero_storage_disk_error_to_emulator(aero_storage::DiskError::InvalidState(
                "closed".into()
            )),
            DiskError::InvalidState("closed".into())
        );
        assert_eq!(
            aero_storage_disk_error_to_emulator(aero_storage::DiskError::BackendUnavailable),
            DiskError::BackendUnavailable
        );
        assert_eq!(
            aero_storage_disk_error_to_emulator(aero_storage::DiskError::Io("boom".into())),
            DiskError::Io("boom".into())
        );
    }

    #[test]
    fn emulator_to_aero_storage_to_emulator_roundtrip_preserves_browser_variants() {
        let cases = [
            DiskError::NotSupported("opfs missing".into()),
            DiskError::QuotaExceeded,
            DiskError::InUse,
            DiskError::InvalidState("closed".into()),
            DiskError::BackendUnavailable,
            DiskError::Io("boom".into()),
        ];

        for case in cases {
            let storage = emulator_disk_error_to_aero_storage(case.clone(), None, None, None);
            let back = aero_storage_disk_error_to_emulator(storage);
            assert_eq!(back, case);
        }
    }
}
