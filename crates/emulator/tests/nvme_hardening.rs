#![cfg(not(target_arch = "wasm32"))]

use emulator::io::pci::MmioDevice;
use emulator::io::storage::disk::MemDisk;
use emulator::io::storage::nvme::NvmeController;
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

const CC_EN: u32 = 1 << 0;
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

#[test]
fn nvme_enable_rejects_unsupported_page_size() {
    let disk = Box::new(MemDisk::new(8));
    let mut ctrl = NvmeController::new(disk);
    let mut mem = TestMem::default();

    // MPS=15 => page_size=4096<<15 = 128MiB, which we should reject.
    let cc = CC_EN | (15u32 << 7);
    ctrl.mmio_write(&mut mem, NVME_REG_CC, 4, cc);

    let csts = ctrl.mmio_read(&mut mem, NVME_REG_CSTS, 4);
    assert_eq!(csts & CSTS_RDY, 0);
    assert_ne!(csts & CSTS_CFS, 0);
}

#[test]
fn nvme_enable_with_large_aqa_does_not_panic() {
    let disk = Box::new(MemDisk::new(8));
    let mut ctrl = NvmeController::new(disk);
    let mut mem = TestMem::with_len(4096);

    // Historically this could overflow in debug builds due to u16 + 1.
    ctrl.mmio_write(&mut mem, NVME_REG_AQA, 4, 0xffff_ffff);

    // Enable with default page size (MPS=0 => 4KiB).
    ctrl.mmio_write(&mut mem, NVME_REG_CC, 4, CC_EN);

    let csts = ctrl.mmio_read(&mut mem, NVME_REG_CSTS, 4);
    assert_ne!(csts & CSTS_RDY, 0);
    assert_eq!(csts & CSTS_CFS, 0);
}
