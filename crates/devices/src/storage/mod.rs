use std::io;

use aero_storage_adapters::AeroVirtualDiskAsDeviceBackend;

/// Byte-addressed disk backend used by `aero-devices` device models (e.g. virtio-blk).
///
/// # Canonical trait note
///
/// Most of the Rust storage stack in this repo is converging on [`aero_storage::VirtualDisk`] as
/// the canonical synchronous disk trait (disk image formats, AHCI/IDE/NVMe controller wiring).
///
/// This `aero-devices` trait remains because some device models want:
/// - `std::io::Result` errors
/// - `&self` reads (interior mutability / locking inside the backend)
/// - a byte-addressed interface at the device boundary
///
/// Prefer passing `Box<dyn aero_storage::VirtualDisk>` through high-level wiring and adapt as
/// needed using [`aero_storage_adapters::AeroVirtualDiskAsDeviceBackend`].
///
/// See `docs/20-storage-trait-consolidation.md`.
pub trait DiskBackend: Send {
    /// Total disk size in bytes.
    fn len(&self) -> u64;
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

impl DiskBackend for AeroVirtualDiskAsDeviceBackend {
    fn len(&self) -> u64 {
        self.capacity_bytes()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.read_at_aligned(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.write_at_aligned(offset, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        AeroVirtualDiskAsDeviceBackend::flush(self)
    }
}

/// Adapter for treating a `aero-devices` disk backend as an `aero-storage` [`aero_storage::VirtualDisk`].
///
/// This is primarily useful for reusing `aero-storage` disk wrappers (e.g. caches or sparse formats)
/// on top of an existing device-model backend.
pub struct DeviceBackendAsAeroVirtualDisk {
    backend: Box<dyn DiskBackend>,
}

impl DeviceBackendAsAeroVirtualDisk {
    pub fn new(backend: Box<dyn DiskBackend>) -> Self {
        Self { backend }
    }

    pub fn into_inner(self) -> Box<dyn DiskBackend> {
        self.backend
    }

    fn map_backend_io_error(&self, err: io::Error) -> aero_storage::DiskError {
        // Prefer preserving the original `aero_storage::DiskError` when a backend (often an
        // adapter such as `AeroVirtualDiskAsDeviceBackend`) stored it inside `io::Error`.
        //
        // This keeps cross-crate error handling consistent and ensures that higher layers can
        // observe semantic failures like quota exhaustion or "backend in use".
        let kind = err.kind();
        let msg = err.to_string();
        if let Some(inner) = err.into_inner() {
            if let Ok(disk_err) = inner.downcast::<aero_storage::DiskError>() {
                return *disk_err;
            }
        }

        match kind {
            io::ErrorKind::StorageFull => aero_storage::DiskError::QuotaExceeded,
            io::ErrorKind::ResourceBusy => aero_storage::DiskError::InUse,
            io::ErrorKind::NotConnected => aero_storage::DiskError::BackendUnavailable,
            io::ErrorKind::BrokenPipe => aero_storage::DiskError::InvalidState(msg),
            io::ErrorKind::Unsupported => aero_storage::DiskError::NotSupported(msg),
            _ => aero_storage::DiskError::Io(msg),
        }
    }

    fn check_bounds(&self, offset: u64, len: usize) -> aero_storage::Result<()> {
        let len_u64 =
            u64::try_from(len).map_err(|_| aero_storage::DiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(len_u64)
            .ok_or(aero_storage::DiskError::OffsetOverflow)?;

        let capacity_bytes = self.backend.len();
        if end > capacity_bytes {
            return Err(aero_storage::DiskError::OutOfBounds {
                offset,
                len,
                capacity: capacity_bytes,
            });
        }
        Ok(())
    }
}

impl aero_storage::VirtualDisk for DeviceBackendAsAeroVirtualDisk {
    fn capacity_bytes(&self) -> u64 {
        self.backend.len()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.check_bounds(offset, buf.len())?;

        self.backend
            .read_at(offset, buf)
            .map_err(|e| self.map_backend_io_error(e))
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.check_bounds(offset, buf.len())?;

        self.backend
            .write_at(offset, buf)
            .map_err(|e| self.map_backend_io_error(e))
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.backend
            .flush()
            .map_err(|e| self.map_backend_io_error(e))
    }
}

pub struct VirtualDrive {
    sector_size: u32,
    backend: Box<dyn DiskBackend>,
}

impl VirtualDrive {
    pub fn new(sector_size: u32, backend: Box<dyn DiskBackend>) -> Self {
        Self {
            sector_size,
            backend,
        }
    }

    /// Wrap a boxed [`aero_storage::VirtualDisk`] as a `aero-devices` [`DiskBackend`].
    ///
    /// This is a convenience for the common case where the disk is already stored behind a
    /// trait object (`Box<dyn VirtualDisk>`). The adapter enforces 512-byte alignment and
    /// bounds checks at the device boundary.
    pub fn new_from_aero_virtual_disk(
        disk: Box<dyn aero_storage::VirtualDisk + Send>,
    ) -> Self {
        Self::new(
            512,
            Box::new(AeroVirtualDiskAsDeviceBackend::new(disk)),
        )
    }

    pub fn new_from_aero_storage<D>(disk: D) -> Self
    where
        D: aero_storage::VirtualDisk + Send + 'static,
    {
        Self::new_from_aero_virtual_disk(Box::new(disk))
    }

    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.backend.len()
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.backend.len() / u64::from(self.sector_size)
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.backend.read_at(offset, buf)
    }

    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.backend.write_at(offset, buf)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.backend.flush()
    }
}

impl std::fmt::Debug for VirtualDrive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualDrive")
            .field("sector_size", &self.sector_size)
            .field("capacity_bytes", &self.capacity_bytes())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_storage::VirtualDisk;

    struct VecBackend {
        data: Vec<u8>,
    }

    impl VecBackend {
        fn new(len: usize) -> Self {
            Self { data: vec![0; len] }
        }
    }

    impl DiskBackend for VecBackend {
        fn len(&self) -> u64 {
            self.data.len() as u64
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            let offset = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
            let end = offset
                .checked_add(buf.len())
                .ok_or(io::ErrorKind::InvalidInput)?;
            if end > self.data.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "oob"));
            }
            buf.copy_from_slice(&self.data[offset..end]);
            Ok(())
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
            let offset = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
            let end = offset
                .checked_add(buf.len())
                .ok_or(io::ErrorKind::InvalidInput)?;
            if end > self.data.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "oob"));
            }
            self.data[offset..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn device_backend_as_virtual_disk_allows_unaligned_reads() {
        let backend = Box::new(VecBackend::new(8));
        let mut disk = DeviceBackendAsAeroVirtualDisk::new(backend);

        disk.write_at(1, b"abc").unwrap();
        let mut out = [0u8; 3];
        disk.read_at(1, &mut out).unwrap();
        assert_eq!(&out, b"abc");
    }

    #[test]
    fn device_backend_as_virtual_disk_reports_out_of_bounds() {
        let backend = Box::new(VecBackend::new(4));
        let mut disk = DeviceBackendAsAeroVirtualDisk::new(backend);

        let mut out = [0u8; 1];
        let err = disk.read_at(4, &mut out).unwrap_err();
        assert!(matches!(err, aero_storage::DiskError::OutOfBounds { .. }));
    }

    #[test]
    fn device_backend_as_virtual_disk_reports_offset_overflow() {
        let backend = Box::new(VecBackend::new(4));
        let mut disk = DeviceBackendAsAeroVirtualDisk::new(backend);

        let mut out = [0u8; 1];
        let err = disk.read_at(u64::MAX, &mut out).unwrap_err();
        assert!(matches!(err, aero_storage::DiskError::OffsetOverflow));

        let err = disk.write_at(u64::MAX, &[0u8; 1]).unwrap_err();
        assert!(matches!(err, aero_storage::DiskError::OffsetOverflow));
    }

    #[test]
    fn device_backend_as_virtual_disk_preserves_embedded_disk_error() {
        struct ErrorBackend {
            err: fn() -> io::Error,
        }

        impl DiskBackend for ErrorBackend {
            fn len(&self) -> u64 {
                512
            }

            fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> io::Result<()> {
                Err((self.err)())
            }

            fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> io::Result<()> {
                Err((self.err)())
            }

            fn flush(&mut self) -> io::Result<()> {
                Err((self.err)())
            }
        }

        let cases: &[(
            fn() -> io::Error,
            fn(&aero_storage::DiskError) -> bool,
        )] = &[
            (
                || io::Error::new(io::ErrorKind::StorageFull, aero_storage::DiskError::QuotaExceeded),
                |e| matches!(e, aero_storage::DiskError::QuotaExceeded),
            ),
            (
                || io::Error::new(io::ErrorKind::ResourceBusy, aero_storage::DiskError::InUse),
                |e| matches!(e, aero_storage::DiskError::InUse),
            ),
            (
                || {
                    io::Error::new(
                        io::ErrorKind::NotConnected,
                        aero_storage::DiskError::BackendUnavailable,
                    )
                },
                |e| matches!(e, aero_storage::DiskError::BackendUnavailable),
            ),
            (
                || {
                    io::Error::new(
                        io::ErrorKind::Other,
                        aero_storage::DiskError::InvalidState("closed".to_string()),
                    )
                },
                |e| matches!(
                    e,
                    aero_storage::DiskError::InvalidState(msg) if msg == "closed"
                ),
            ),
            (
                || {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        aero_storage::DiskError::NotSupported("opfs".to_string()),
                    )
                },
                |e| matches!(
                    e,
                    aero_storage::DiskError::NotSupported(msg) if msg == "opfs"
                ),
            ),
            (
                || io::Error::new(io::ErrorKind::Other, aero_storage::DiskError::Io("boom".to_string())),
                |e| matches!(e, aero_storage::DiskError::Io(msg) if msg == "boom"),
            ),
        ];

        for (err_fn, is_expected) in cases {
            let backend = Box::new(ErrorBackend { err: *err_fn });
            let mut disk = DeviceBackendAsAeroVirtualDisk::new(backend);

            let mut buf = [0u8; 1];
            let err = disk.read_at(0, &mut buf).unwrap_err();
            assert!(is_expected(&err), "unexpected error: {err:?}");

            let err = disk.write_at(0, &[0u8; 1]).unwrap_err();
            assert!(is_expected(&err), "unexpected error: {err:?}");

            let err = disk.flush().unwrap_err();
            assert!(is_expected(&err), "unexpected error: {err:?}");
        }
    }

    #[test]
    fn device_backend_as_virtual_disk_maps_plain_io_error_kinds() {
        struct PlainErrBackend;

        impl DiskBackend for PlainErrBackend {
            fn len(&self) -> u64 {
                512
            }

            fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::StorageFull, "full"))
            }

            fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::ResourceBusy, "busy"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::NotConnected, "offline"))
            }
        }

        let backend = Box::new(PlainErrBackend);
        let mut disk = DeviceBackendAsAeroVirtualDisk::new(backend);

        let mut buf = [0u8; 1];
        let err = disk.read_at(0, &mut buf).unwrap_err();
        assert!(matches!(err, aero_storage::DiskError::QuotaExceeded));

        let err = disk.write_at(0, &[0u8; 1]).unwrap_err();
        assert!(matches!(err, aero_storage::DiskError::InUse));

        let err = disk.flush().unwrap_err();
        assert!(matches!(err, aero_storage::DiskError::BackendUnavailable));
    }
}
