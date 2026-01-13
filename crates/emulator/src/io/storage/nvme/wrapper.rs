use std::cell::RefCell;

use aero_devices::pci::PciDevice as _; // for `config_mut()`
use memory::{MemoryBus, MmioHandler};

use crate::io::pci::{MmioDevice, PciDevice};
use crate::io::storage::adapters::VirtualDiskFromEmuDiskBackend;
use crate::io::storage::disk::DiskBackend;
use crate::io::storage::pci_compat;

/// Emulator-facing NVMe controller wrapper.
pub struct NvmeController {
    inner: aero_devices_nvme::NvmeController,
}

impl NvmeController {
    /// NVMe BAR0 size (registers + doorbells).
    pub const BAR0_SIZE: u64 = aero_devices_nvme::NvmeController::bar0_len();

    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(disk: Box<dyn DiskBackend + Send>) -> Self {
        let vd = VirtualDiskFromEmuDiskBackend::new(disk);
        let inner = aero_devices_nvme::NvmeController::try_new_from_virtual_disk(Box::new(vd))
            .expect("failed to construct NVMe controller from emulator DiskBackend");
        Self { inner }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let vd = VirtualDiskFromEmuDiskBackend::new(disk);
        let inner = aero_devices_nvme::NvmeController::try_new_from_virtual_disk(Box::new(vd))
            .expect("failed to construct NVMe controller from emulator DiskBackend");
        Self { inner }
    }

    pub fn inner(&self) -> &aero_devices_nvme::NvmeController {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut aero_devices_nvme::NvmeController {
        &mut self.inner
    }

    pub fn into_inner(self) -> aero_devices_nvme::NvmeController {
        self.inner
    }
}

/// Emulator-facing PCI wrapper for an NVMe controller.
pub struct NvmePciDevice {
    inner: RefCell<aero_devices_nvme::NvmePciDevice>,
}

impl NvmePciDevice {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(disk: Box<dyn DiskBackend + Send>, bar0: u64) -> Self {
        let vd = VirtualDiskFromEmuDiskBackend::new(disk);
        let mut dev = aero_devices_nvme::NvmePciDevice::try_new_from_virtual_disk(Box::new(vd))
            .expect("failed to construct NVMe PCI device from emulator DiskBackend");

        // Preserve the legacy emulator API which passed a pre-assigned BAR0 base.
        dev.config_mut().write(0x10, 4, bar0 as u32);
        dev.config_mut().write(0x14, 4, (bar0 >> 32) as u32);

        Self {
            inner: RefCell::new(dev),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn new(disk: Box<dyn DiskBackend>, bar0: u64) -> Self {
        let vd = VirtualDiskFromEmuDiskBackend::new(disk);
        let mut dev = aero_devices_nvme::NvmePciDevice::try_new_from_virtual_disk(Box::new(vd))
            .expect("failed to construct NVMe PCI device from emulator DiskBackend");

        dev.config_mut().write(0x10, 4, bar0 as u32);
        dev.config_mut().write(0x14, 4, (bar0 >> 32) as u32);

        Self {
            inner: RefCell::new(dev),
        }
    }

    pub fn irq_level(&self) -> bool {
        self.inner.borrow().irq_level()
    }
}

impl PciDevice for NvmePciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        let mut inner = self.inner.borrow_mut();
        pci_compat::config_read(inner.config_mut(), offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        let mut inner = self.inner.borrow_mut();
        pci_compat::config_write(inner.config_mut(), offset, size, value);
    }
}

impl MmioDevice for NvmePciDevice {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }

        let mut inner = self.inner.borrow_mut();
        let value = inner.read(offset, size);

        // Legacy emulator behaviour: MMIO doorbell writes implicitly "tick" the controller so
        // commands are processed without requiring an explicit `process()` call by the platform.
        inner.process(mem);

        match size {
            1 => (value as u32) & 0xff,
            2 => (value as u32) & 0xffff,
            4 => value as u32,
            _ => 0,
        }
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }

        let mut inner = self.inner.borrow_mut();
        inner.write(offset, size, u64::from(value));
        inner.process(mem);
    }
}
