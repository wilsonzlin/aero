#![cfg(not(target_arch = "wasm32"))]

use emulator::io::pci::{MmioDevice, PciDevice};
use emulator::io::storage::disk::MemDisk;
use emulator::io::storage::nvme::NvmePciDevice;
use memory::MemoryBus;

#[derive(Default)]
struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn with_len(len: usize) -> Self {
        Self { data: vec![0; len] }
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            buf.fill(0);
            return;
        }
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[start..end].copy_from_slice(buf);
    }
}

const NVME_REG_CC: u64 = 0x0014;
const NVME_REG_CSTS: u64 = 0x001c;
const NVME_REG_AQA: u64 = 0x0024;
const NVME_REG_ASQ: u64 = 0x0028;
const NVME_REG_ASQ_HI: u64 = 0x002c;
const NVME_REG_ACQ: u64 = 0x0030;
const NVME_REG_ACQ_HI: u64 = 0x0034;

const CC_EN: u32 = 1 << 0;
const CSTS_RDY: u32 = 1 << 0;

#[test]
fn nvme_enable_rejects_pathological_aqa_without_panic() {
    let disk = Box::new(MemDisk::new(8));
    let mut dev = NvmePciDevice::new(disk, 0xfebf_0000);
    let mut mem = TestMem::with_len(0x10_000);

    // Enable MMIO decode (PCI command MEM bit) so register writes take effect.
    dev.config_write(0x04, 2, 1 << 1);

    // Set up a valid admin queue base so enable failure is attributable to AQA validation.
    dev.mmio_write(&mut mem, NVME_REG_ASQ, 4, 0x1000);
    dev.mmio_write(&mut mem, NVME_REG_ASQ_HI, 4, 0);
    dev.mmio_write(&mut mem, NVME_REG_ACQ, 4, 0x2000);
    dev.mmio_write(&mut mem, NVME_REG_ACQ_HI, 4, 0);

    // AQA with maximum values (ASQS/ACQS = 0x0fff) must be rejected (queue size > controller max).
    dev.mmio_write(&mut mem, NVME_REG_AQA, 4, 0xffff_ffff);
    dev.mmio_write(&mut mem, NVME_REG_CC, 4, CC_EN);

    let csts = dev.mmio_read(&mut mem, NVME_REG_CSTS, 4);
    assert_eq!(csts & CSTS_RDY, 0);
}

#[test]
fn nvme_enable_without_asq_acq_is_rejected_without_panic() {
    let disk = Box::new(MemDisk::new(8));
    let mut dev = NvmePciDevice::new(disk, 0xfebf_0000);
    let mut mem = TestMem::with_len(0x10_000);

    dev.config_write(0x04, 2, 1 << 1);

    // Leaving ASQ/ACQ unset must not crash and must not set CSTS.RDY.
    dev.mmio_write(&mut mem, NVME_REG_AQA, 4, 0x0003_0003);
    dev.mmio_write(&mut mem, NVME_REG_CC, 4, CC_EN);

    let csts = dev.mmio_read(&mut mem, NVME_REG_CSTS, 4);
    assert_eq!(csts & CSTS_RDY, 0);
}
