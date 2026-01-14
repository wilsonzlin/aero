use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::pci::{msi::PCI_CAP_ID_MSI, msix::PCI_CAP_ID_MSIX, PciDevice as _};
use aero_devices_nvme::NvmePciDevice;
use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};
use memory::{MemoryBus as _, MmioHandler as _};

#[derive(Clone, Default)]
struct RecordingMsi {
    log: Rc<RefCell<Vec<MsiMessage>>>,
}

impl MsiTrigger for RecordingMsi {
    fn trigger_msi(&mut self, message: MsiMessage) {
        self.log.borrow_mut().push(message);
    }
}

struct TestMem {
    buf: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self {
            buf: vec![0u8; size],
        }
    }
}

impl memory::MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, out: &mut [u8]) {
        let start = paddr as usize;
        let end = start + out.len();
        out.copy_from_slice(&self.buf[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, data: &[u8]) {
        let start = paddr as usize;
        let end = start + data.len();
        self.buf[start..end].copy_from_slice(data);
    }
}

fn enable_msi(dev: &mut NvmePciDevice, address: u64, data: u16) {
    let cap_offset = dev
        .config_mut()
        .find_capability(PCI_CAP_ID_MSI)
        .expect("NVMe device should expose MSI capability") as u16;

    dev.config_mut()
        .write(cap_offset + 0x04, 4, address as u32);
    dev.config_mut()
        .write(cap_offset + 0x08, 4, (address >> 32) as u32);
    dev.config_mut()
        .write(cap_offset + 0x0c, 2, u32::from(data));

    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));
}

fn disable_msi(dev: &mut NvmePciDevice) {
    let cap_offset = dev
        .config_mut()
        .find_capability(PCI_CAP_ID_MSI)
        .expect("NVMe device should expose MSI capability") as u16;
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl & !0x0001));
}

fn enable_msix(dev: &mut NvmePciDevice) {
    let cap_offset = dev
        .config_mut()
        .find_capability(PCI_CAP_ID_MSIX)
        .expect("NVMe device should expose MSI-X capability") as u16;
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));
}

fn program_msix_table_entry0(dev: &mut NvmePciDevice, address: u64, data: u32) {
    // MSI-X table entry layout (16 bytes):
    // - +0x00: Message Address Low
    // - +0x04: Message Address High
    // - +0x08: Message Data
    // - +0x0c: Vector Control (bit 0 = mask)
    let table_base = 0x3000u64;
    dev.write(table_base + 0x00, 4, address as u32 as u64);
    dev.write(table_base + 0x04, 4, (address >> 32) as u64);
    dev.write(table_base + 0x08, 4, data as u64);
    dev.write(table_base + 0x0c, 4, 0); // unmasked
}

fn trigger_completion(dev: &mut NvmePciDevice, mem: &mut TestMem) {
    // Minimal controller enable with a small admin queue pair.
    let asq: u64 = 0x10000;
    let acq: u64 = 0x20000;
    // AQA: both admin SQ/CQ size = 16 entries (encoded as size-1).
    dev.write(0x0024, 4, 0x000f_000f);
    dev.write(0x0028, 8, asq);
    dev.write(0x0030, 8, acq);
    dev.write(0x0014, 4, 1); // CC.EN

    // Queue one invalid admin command so `process()` DMA-reads the SQ entry and DMA-writes a CQ
    // completion, which raises an interrupt.
    let mut cmd = [0u8; 64];
    cmd[0] = 0xFF; // invalid opcode
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    mem.write_physical(asq, &cmd);

    // Ring the SQ0 tail doorbell (admin SQ is qid=0; SQ tail doorbell at 0x1000).
    dev.write(0x1000, 4, 1);

    dev.process(mem);
}

#[test]
fn nvme_delivers_msi_when_enabled() {
    let mut dev = NvmePciDevice::default();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    // Enable MMIO decoding and bus mastering so controller programming takes effect and `process()`
    // is allowed to DMA.
    dev.config_mut().set_command(0x0006); // MEM + BME

    let log = Rc::new(RefCell::new(Vec::new()));
    dev.set_msi_target(Some(Box::new(RecordingMsi { log: log.clone() })));

    let vector: u8 = 0x45;
    enable_msi(&mut dev, 0xFEE0_0000, vector as u16);

    trigger_completion(&mut dev, &mut mem);

    let msgs = log.borrow();
    assert_eq!(msgs.len(), 1, "expected exactly one MSI delivery");
    assert_eq!(msgs[0].vector(), vector);
}

#[test]
fn nvme_delivers_msix_when_enabled() {
    let mut dev = NvmePciDevice::default();
    let mut mem = TestMem::new(2 * 1024 * 1024);

    // Enable MMIO decoding and bus mastering so controller programming takes effect and `process()`
    // is allowed to DMA.
    dev.config_mut().set_command(0x0006); // MEM + BME

    let log = Rc::new(RefCell::new(Vec::new()));
    dev.set_msi_target(Some(Box::new(RecordingMsi { log: log.clone() })));

    // Program MSI as well so we can assert MSI-X is preferred when both are enabled.
    let msi_vector: u8 = 0x44;
    enable_msi(&mut dev, 0xFEE0_0000, msi_vector as u16);

    let msix_vector: u8 = 0x46;
    enable_msix(&mut dev);
    program_msix_table_entry0(&mut dev, 0xFEE0_0000, msix_vector as u32);

    trigger_completion(&mut dev, &mut mem);

    let msgs = log.borrow();
    assert_eq!(msgs.len(), 1, "expected exactly one MSI-X delivery");
    assert_eq!(
        msgs[0].vector(),
        msix_vector,
        "MSI-X must be preferred over MSI when both are enabled"
    );
    drop(msgs);

    // Verify legacy INTx is suppressed due to MSI-X (even if MSI is disabled).
    disable_msi(&mut dev);
    assert!(
        !dev.irq_level(),
        "legacy INTx must be suppressed while MSI-X is active"
    );
}

