use std::sync::{Arc, Mutex};

use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use firmware::bios::{BlockDevice, DiskError as BiosDiskError};

use crate::MachineError;

/// Cloneable handle to a single underlying virtual disk backend.
///
/// This adapter is intentionally defined in `aero-machine` so both:
/// - firmware BIOS INT13 (`firmware::bios::BlockDevice`), and
/// - PCI storage controllers (AHCI/NVMe/virtio-blk; `aero_storage::VirtualDisk`)
///   can operate on the *same* disk image when a guest transitions between them.
///
/// See `docs/20-storage-trait-consolidation.md`.
#[derive(Clone)]
pub struct SharedDisk {
    inner: Arc<Mutex<Box<dyn VirtualDisk + Send>>>,
}

impl SharedDisk {
    /// Construct a new shared disk wrapper around an existing [`VirtualDisk`] backend.
    pub fn new(backend: Box<dyn VirtualDisk + Send>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(backend)),
        }
    }

    /// Construct an in-memory shared disk from a raw disk image.
    ///
    /// The image must be a multiple of 512 bytes (BIOS sector size). An empty image is allowed and
    /// is treated as a single all-zero sector so BIOS boot attempts remain deterministic.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, MachineError> {
        Ok(Self::new(Self::virtual_disk_from_bytes(bytes)?))
    }

    /// Replace the underlying disk backend for **all** shared handles.
    pub fn set_backend(&self, backend: Box<dyn VirtualDisk + Send>) {
        *self
            .inner
            .lock()
            .expect("shared disk mutex should not be poisoned") = backend;
    }

    /// Replace the underlying disk image for **all** shared handles.
    ///
    /// This is a convenience wrapper for `Vec<u8>`-backed images used by
    /// [`crate::Machine::set_disk_image`].
    pub fn set_bytes(&self, bytes: Vec<u8>) -> Result<(), MachineError> {
        self.set_backend(Self::virtual_disk_from_bytes(bytes)?);
        Ok(())
    }

    fn virtual_disk_from_bytes(mut bytes: Vec<u8>) -> Result<Box<dyn VirtualDisk + Send>, MachineError> {
        if !bytes.len().is_multiple_of(SECTOR_SIZE) {
            return Err(MachineError::InvalidDiskSize(bytes.len()));
        }
        if bytes.is_empty() {
            bytes.resize(SECTOR_SIZE, 0);
        }

        let capacity_bytes: u64 = bytes.len().try_into().unwrap_or(u64::MAX);
        let mut disk = RawDisk::create(MemBackend::new(), capacity_bytes)
            .map_err(|e| MachineError::DiskBackend(e.to_string()))?;
        disk.write_at(0, &bytes)
            .map_err(|e| MachineError::DiskBackend(e.to_string()))?;
        Ok(Box::new(disk))
    }
}

impl VirtualDisk for SharedDisk {
    fn capacity_bytes(&self) -> u64 {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .flush()
    }
}

impl BlockDevice for SharedDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), BiosDiskError> {
        let offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(BiosDiskError::OutOfRange)?;
        let end = offset
            .checked_add(SECTOR_SIZE as u64)
            .ok_or(BiosDiskError::OutOfRange)?;

        let mut disk = self
            .inner
            .lock()
            .expect("shared disk mutex should not be poisoned");
        if end > disk.capacity_bytes() {
            return Err(BiosDiskError::OutOfRange);
        }
        disk.read_at(offset, buf)
            .map_err(|_err| BiosDiskError::OutOfRange)?;
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .capacity_bytes()
            / SECTOR_SIZE as u64
    }
}
