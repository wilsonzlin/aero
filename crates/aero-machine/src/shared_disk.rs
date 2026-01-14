#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex};

use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use firmware::bios::{BlockDevice, DiskError as BiosDiskError};

use crate::MachineError;

type SharedDiskBackend = Box<dyn VirtualDisk>;

/// Cloneable handle to a single underlying virtual disk backend.
///
/// This adapter is intentionally defined in `aero-machine` so both:
/// - firmware BIOS INT13 (`firmware::bios::BlockDevice`), and
/// - PCI storage controllers that accept an `aero_storage::VirtualDisk` backend (AHCI, NVMe,
///   virtio-blk)
///   can operate on the *same* disk image when a guest transitions between them.
///
/// See `docs/20-storage-trait-consolidation.md`.
#[derive(Clone)]
pub struct SharedDisk {
    #[cfg(target_arch = "wasm32")]
    inner: Rc<RefCell<SharedDiskBackend>>,
    #[cfg(not(target_arch = "wasm32"))]
    inner: Arc<Mutex<SharedDiskBackend>>,
}

impl SharedDisk {
    /// Construct a new shared disk wrapper around an existing [`VirtualDisk`] backend.
    #[cfg(target_arch = "wasm32")]
    pub fn new(backend: SharedDiskBackend) -> Self {
        Self {
            inner: Rc::new(RefCell::new(backend)),
        }
    }

    /// Construct a new shared disk wrapper around an existing [`VirtualDisk`] backend.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(backend: SharedDiskBackend) -> Self {
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
    ///
    /// Note: for [`crate::Machine`], prefer [`crate::Machine::set_disk_backend`] so any storage
    /// controllers that derive ATA IDENTIFY geometry from disk capacity can be rebuilt when the
    /// backend changes.
    #[cfg(target_arch = "wasm32")]
    pub fn set_backend(&self, backend: SharedDiskBackend) {
        *self
            .inner
            .try_borrow_mut()
            .expect("shared disk refcell should not already be borrowed") = backend;
    }

    /// Replace the underlying disk backend for **all** shared handles.
    ///
    /// Note: for [`crate::Machine`], prefer [`crate::Machine::set_disk_backend`] so any storage
    /// controllers that derive ATA IDENTIFY geometry from disk capacity can be rebuilt when the
    /// backend changes.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_backend(&self, backend: SharedDiskBackend) {
        let mut guard = self
            .inner
            .lock()
            .expect("shared disk mutex should not be poisoned");
        *guard = backend;
    }

    /// Replace the underlying disk image for **all** shared handles.
    ///
    /// This is a convenience wrapper for `Vec<u8>`-backed images used by
    /// [`crate::Machine::set_disk_image`].
    pub fn set_bytes(&self, bytes: Vec<u8>) -> Result<(), MachineError> {
        self.set_backend(Self::virtual_disk_from_bytes(bytes)?);
        Ok(())
    }

    fn virtual_disk_from_bytes(mut bytes: Vec<u8>) -> Result<SharedDiskBackend, MachineError> {
        if !bytes.len().is_multiple_of(SECTOR_SIZE) {
            return Err(MachineError::InvalidDiskSize(bytes.len()));
        }
        if bytes.is_empty() {
            bytes.resize(SECTOR_SIZE, 0);
        }

        let disk = RawDisk::open(MemBackend::from_vec(bytes))
            .map_err(|e| MachineError::DiskBackend(e.to_string()))?;
        Ok(Box::new(disk))
    }
}

#[cfg(target_arch = "wasm32")]
impl VirtualDisk for SharedDisk {
    fn capacity_bytes(&self) -> u64 {
        self.inner
            .try_borrow()
            .expect("shared disk refcell should not already be mutably borrowed")
            .capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner
            .try_borrow_mut()
            .expect("shared disk refcell should not already be borrowed")
            .read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.inner
            .try_borrow_mut()
            .expect("shared disk refcell should not already be borrowed")
            .write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner
            .try_borrow_mut()
            .expect("shared disk refcell should not already be borrowed")
            .flush()
    }

    fn discard_range(&mut self, offset: u64, len: u64) -> aero_storage::Result<()> {
        self.inner
            .try_borrow_mut()
            .expect("shared disk refcell should not already be borrowed")
            .discard_range(offset, len)
    }
}

#[cfg(not(target_arch = "wasm32"))]
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

    fn discard_range(&mut self, offset: u64, len: u64) -> aero_storage::Result<()> {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .discard_range(offset, len)
    }
}

#[cfg(target_arch = "wasm32")]
impl BlockDevice for SharedDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), BiosDiskError> {
        self.inner
            .try_borrow_mut()
            .expect("shared disk refcell should not already be borrowed")
            .read_sectors(lba, buf)
            .map_err(|_err| BiosDiskError::OutOfRange)
    }

    fn size_in_sectors(&self) -> u64 {
        self.inner
            .try_borrow()
            .expect("shared disk refcell should not already be mutably borrowed")
            .capacity_bytes()
            / SECTOR_SIZE as u64
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl BlockDevice for SharedDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), BiosDiskError> {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .read_sectors(lba, buf)
            .map_err(|_err| BiosDiskError::OutOfRange)
    }

    fn size_in_sectors(&self) -> u64 {
        self.inner
            .lock()
            .expect("shared disk mutex should not be poisoned")
            .capacity_bytes()
            / SECTOR_SIZE as u64
    }
}

#[cfg(test)]
mod tests {
    use super::SharedDisk;
    use aero_storage::{AeroSparseConfig, AeroSparseDisk, MemBackend, VirtualDisk, SECTOR_SIZE};

    #[test]
    fn shared_disk_forwards_discard_range_to_inner_disk() {
        let disk = AeroSparseDisk::create(
            MemBackend::new(),
            AeroSparseConfig {
                disk_size_bytes: 8192,
                block_size_bytes: 4096,
            },
        )
        .unwrap();

        let mut shared = SharedDisk::new(Box::new(disk));

        // Allocate and write a non-zero sector so we can observe it being discarded.
        shared.write_at(0, &[0x5A; SECTOR_SIZE]).unwrap();

        // Discard the entire first sparse block (4096 bytes). Reads should return zeros.
        shared.discard_range(0, 4096).unwrap();

        let mut buf = [0xCCu8; SECTOR_SIZE];
        shared.read_at(0, &mut buf).unwrap();
        assert_eq!(buf, [0u8; SECTOR_SIZE]);
    }
}
