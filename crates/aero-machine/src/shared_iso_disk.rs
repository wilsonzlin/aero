use std::io;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex, Weak};
#[cfg(target_arch = "wasm32")]
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::atapi::IsoBackend;
use aero_storage::{DiskError, VirtualDisk, SECTOR_SIZE};
use firmware::bios::{BlockDevice, CdromDevice, DiskError as BiosDiskError};

type SharedIsoDiskBackend = Box<dyn VirtualDisk>;

/// Cloneable handle to a read-only ISO (2048-byte sector) disk backend.
///
/// This adapter is intentionally defined in `aero-machine` so both of the following can share the
/// same underlying ISO image:
/// - the IDE/ATAPI CD-ROM device model (`aero_devices_storage::atapi::AtapiCdrom`)
/// - firmware BIOS boot code (El Torito) / INT dispatch
#[derive(Clone)]
pub struct SharedIsoDisk {
    #[cfg(target_arch = "wasm32")]
    inner: Rc<RefCell<SharedIsoDiskBackend>>,
    #[cfg(not(target_arch = "wasm32"))]
    inner: Arc<Mutex<SharedIsoDiskBackend>>,
    capacity_bytes: u64,
    sector_count: u32,
}

/// Weak reference to a [`SharedIsoDisk`] backend.
///
/// This is used by the canonical [`crate::Machine`] to avoid keeping the underlying ISO handle
/// alive after a guest-initiated eject: the ATAPI CD-ROM device holds the strong reference while
/// media is inserted, and the machine keeps only a weak reference for BIOS INT13 access.
#[derive(Clone)]
pub(crate) struct SharedIsoDiskWeak {
    #[cfg(target_arch = "wasm32")]
    inner: Weak<RefCell<SharedIsoDiskBackend>>,
    #[cfg(not(target_arch = "wasm32"))]
    inner: Weak<Mutex<SharedIsoDiskBackend>>,
    capacity_bytes: u64,
    sector_count: u32,
}

impl SharedIsoDisk {
    pub fn new(disk: SharedIsoDiskBackend) -> io::Result<Self> {
        let capacity_bytes = disk.capacity_bytes();
        if !capacity_bytes.is_multiple_of(AtapiCdrom::SECTOR_SIZE as u64) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ISO disk capacity is not a multiple of 2048-byte sectors",
            ));
        }

        let sector_count = capacity_bytes / AtapiCdrom::SECTOR_SIZE as u64;
        let sector_count = u32::try_from(sector_count).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "ISO disk capacity exceeds 32-bit sector count limit",
            )
        })?;

        #[cfg(target_arch = "wasm32")]
        let inner = Rc::new(RefCell::new(disk));
        #[cfg(not(target_arch = "wasm32"))]
        let inner = Arc::new(Mutex::new(disk));

        Ok(Self {
            inner,
            capacity_bytes,
            sector_count,
        })
    }

    pub(crate) fn downgrade(&self) -> SharedIsoDiskWeak {
        #[cfg(target_arch = "wasm32")]
        let inner = Rc::downgrade(&self.inner);
        #[cfg(not(target_arch = "wasm32"))]
        let inner = Arc::downgrade(&self.inner);

        SharedIsoDiskWeak {
            inner,
            capacity_bytes: self.capacity_bytes,
            sector_count: self.sector_count,
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn inner_mut(&self) -> std::cell::RefMut<'_, SharedIsoDiskBackend> {
        self.inner
            .try_borrow_mut()
            .expect("shared ISO disk refcell should not already be borrowed")
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn inner_mut(&self) -> std::sync::MutexGuard<'_, SharedIsoDiskBackend> {
        self.inner
            .lock()
            .expect("shared ISO disk mutex should not be poisoned")
    }
}

impl SharedIsoDiskWeak {
    pub(crate) fn upgrade(&self) -> Option<SharedIsoDisk> {
        let inner = self.inner.upgrade()?;
        Some(SharedIsoDisk {
            inner,
            capacity_bytes: self.capacity_bytes,
            sector_count: self.sector_count,
        })
    }
}

impl VirtualDisk for SharedIsoDisk {
    fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner_mut().read_at(offset, buf)
    }

    fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
        Err(DiskError::NotSupported(
            "writes to ISO media are not supported".to_string(),
        ))
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        // Even though the media is treated as read-only, forward flushes to the underlying backend
        // in case it buffers reads or maintains bookkeeping.
        self.inner_mut().flush()
    }
}

impl IsoBackend for SharedIsoDisk {
    fn sector_count(&self) -> u32 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        if !buf.len().is_multiple_of(AtapiCdrom::SECTOR_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned buffer length",
            ));
        }

        let offset = u64::from(lba)
            .checked_mul(AtapiCdrom::SECTOR_SIZE as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;

        self.inner_mut()
            .read_at(offset, buf)
            .map_err(io::Error::other)
    }
}

impl BlockDevice for SharedIsoDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), BiosDiskError> {
        self.inner_mut()
            .read_sectors(lba, buf)
            .map_err(|_err| BiosDiskError::OutOfRange)
    }

    fn size_in_sectors(&self) -> u64 {
        self.capacity_bytes / SECTOR_SIZE as u64
    }
}

impl CdromDevice for SharedIsoDisk {
    fn read_sector(
        &mut self,
        lba: u64,
        buf: &mut [u8; AtapiCdrom::SECTOR_SIZE],
    ) -> Result<(), BiosDiskError> {
        let offset = lba
            .checked_mul(AtapiCdrom::SECTOR_SIZE as u64)
            .ok_or(BiosDiskError::OutOfRange)?;
        self.inner_mut()
            .read_at(offset, buf)
            .map_err(|_err| BiosDiskError::OutOfRange)
    }

    fn size_in_sectors(&self) -> u64 {
        u64::from(self.sector_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Debug)]
    struct DropCounterDisk {
        len: u64,
        dropped: Arc<AtomicUsize>,
    }

    impl Drop for DropCounterDisk {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl VirtualDisk for DropCounterDisk {
        fn capacity_bytes(&self) -> u64 {
            self.len
        }

        fn read_at(&mut self, _offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
            buf.fill(0);
            Ok(())
        }

        fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
            Err(DiskError::NotSupported(
                "writes to drop-counter disk are not supported".to_string(),
            ))
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn weak_iso_ref_does_not_keep_backend_alive_after_atapi_eject() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let disk = DropCounterDisk {
            len: AtapiCdrom::SECTOR_SIZE as u64,
            dropped: dropped.clone(),
        };

        let iso = SharedIsoDisk::new(Box::new(disk)).expect("valid ISO backend");
        let weak = iso.downgrade();

        let mut dev = AtapiCdrom::new(Some(Box::new(iso)));
        assert!(
            weak.upgrade().is_some(),
            "weak ref should upgrade while ATAPI device still holds the backend"
        );
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "backend should not be dropped while referenced"
        );

        // Guest-initiated eject (START STOP UNIT with LOEJ and START=0) drops the backend.
        dev.eject_media();
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "backend should be dropped when ATAPI device releases last strong reference"
        );
        assert!(
            weak.upgrade().is_none(),
            "weak ref should not upgrade once backend is dropped"
        );
    }
}
