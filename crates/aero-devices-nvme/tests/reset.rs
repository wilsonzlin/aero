use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_devices::pci::PciDevice as _;
use aero_devices_nvme::NvmePciDevice;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::MmioHandler as _;

struct DropDetectDisk {
    inner: RawDisk<MemBackend>,
    dropped: Arc<AtomicBool>,
}

impl Drop for DropDetectDisk {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl VirtualDisk for DropDetectDisk {
    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.flush()
    }
}

#[test]
fn reset_clears_registers_and_irq_but_preserves_attached_disk_backend() {
    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    let mut dev = NvmePciDevice::try_new_from_virtual_disk(Box::new(disk)).unwrap();

    // Enable MMIO decoding so register writes take effect.
    dev.config_mut().set_command(0x0006); // MEM + BME

    // Mutate a few registers and assert an interrupt so we can observe reset clearing them.
    dev.write(0x000c, 4, 1); // INTMS
    dev.write(0x0014, 4, 1); // CC.EN
    dev.controller_mut().intx_level = true;
    assert!(dev.irq_level());

    // Reset the device model. This should *not* drop the attached backend.
    dev.reset();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "reset dropped the attached disk backend"
    );
    assert_eq!(dev.config().command(), 0, "reset should clear PCI command");

    // Re-enable MMIO decoding after reset so we can observe register state.
    dev.config_mut().set_command(0x0002); // MEM
    assert_eq!(
        dev.read(0x0014, 4),
        0,
        "reset should clear NVMe CC register"
    );
    assert_eq!(
        dev.read(0x001c, 4),
        0,
        "reset should clear NVMe CSTS register"
    );
    assert_eq!(
        dev.read(0x000c, 4),
        0,
        "reset should clear NVMe interrupt mask register"
    );
    assert!(!dev.irq_level(), "reset should deassert legacy INTx");

    // Dropping the device should drop the backend (sanity check).
    drop(dev);
    assert!(
        dropped.load(Ordering::SeqCst),
        "dropping the NVMe device should drop the disk backend"
    );
}

