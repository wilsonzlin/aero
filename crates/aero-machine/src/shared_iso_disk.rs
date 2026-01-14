use std::io;
#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, rc::Rc};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex};

use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::atapi::IsoBackend;
use aero_storage::{DiskError, VirtualDisk, SECTOR_SIZE};
use firmware::bios::{BlockDevice, CdromDevice, DiskError as BiosDiskError};

#[cfg(target_arch = "wasm32")]
type SharedIsoDiskBackend = Box<dyn VirtualDisk>;

#[cfg(not(target_arch = "wasm32"))]
type SharedIsoDiskBackend = Box<dyn VirtualDisk + Send>;

/// Cloneable handle to a read-only ISO (2048-byte sector) disk backend.
///
/// This adapter is intentionally defined in `aero-machine` so both:
/// - the IDE/ATAPI CD-ROM device model (`aero_devices_storage::atapi::AtapiCdrom`), and
/// - firmware BIOS boot code (El Torito) / INT dispatch
/// can share the same underlying ISO image.
#[derive(Clone)]
pub struct SharedIsoDisk {
    #[cfg(target_arch = "wasm32")]
    inner: Rc<RefCell<SharedIsoDiskBackend>>,
    #[cfg(not(target_arch = "wasm32"))]
    inner: Arc<Mutex<SharedIsoDiskBackend>>,
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
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), BiosDiskError> {
        self.inner_mut()
            .read_sectors(lba, buf)
            .map_err(|_err| BiosDiskError::OutOfRange)
    }

    fn size_in_sectors(&self) -> u64 {
        self.capacity_bytes / SECTOR_SIZE as u64
    }
}

impl CdromDevice for SharedIsoDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 2048]) -> Result<(), BiosDiskError> {
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
