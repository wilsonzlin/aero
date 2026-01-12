use aero_devices::pci::PciDevice as _;
use aero_devices_nvme::NvmePciDevice;
use memory::MmioHandler as _;

const NVME_CAP: u64 = 0x0000;
const NVME_CC: u64 = 0x0014;

#[test]
fn bar0_mmio_requires_pci_memory_space_enable() {
    let mut dev = NvmePciDevice::default();

    // Memory Space Enable (command bit 1) gates MMIO decoding: reads float high and writes are
    // ignored.
    dev.config_mut().set_command(0);
    assert_eq!(dev.read(NVME_CAP, 4), 0xFFFF_FFFF);
    assert_eq!(dev.read(NVME_CAP, 8), u64::MAX);

    // Try to enable the controller while MMIO decoding is disabled; this write should not take
    // effect.
    dev.write(NVME_CC, 4, 1);

    // Enable MMIO decoding and observe real register values again.
    dev.config_mut().set_command(0x0002); // MEM
    assert_ne!(dev.read(NVME_CAP, 4), 0xFFFF_FFFF);

    // CC.EN should still be clear because the earlier write was ignored.
    let cc = dev.read(NVME_CC, 4) as u32;
    assert_eq!(cc & 1, 0);

    // With MEM decoding enabled, writes should take effect again.
    dev.write(NVME_CC, 4, 1);
    let cc = dev.read(NVME_CC, 4) as u32;
    assert_eq!(cc & 1, 1);
}

#[test]
fn bar0_mmio_size0_is_noop() {
    let mut dev = NvmePciDevice::default();

    // Size-0 reads are treated as a no-op and return 0, regardless of whether MMIO decoding is
    // enabled in the PCI Command register.
    dev.config_mut().set_command(0);
    assert_eq!(dev.read(NVME_CAP, 0), 0);

    dev.config_mut().set_command(0x0002); // MEM
    assert_eq!(dev.read(NVME_CAP, 0), 0);

    // Size-0 writes are a no-op and must not change CC.EN.
    let cc_before = dev.read(NVME_CC, 4) as u32;
    assert_eq!(cc_before & 1, 0, "CC.EN should start cleared");

    dev.write(NVME_CC, 0, 1);

    let cc_after = dev.read(NVME_CC, 4) as u32;
    assert_eq!(cc_after, cc_before);
    assert_eq!(cc_after & 1, 0, "CC.EN must remain unchanged");
}

#[test]
fn nvme_irq_level_is_gated_by_pci_command_intx_disable() {
    let mut dev = NvmePciDevice::default();

    // Force a pending legacy interrupt without going through full queue processing.
    dev.controller_mut().intx_level = true;
    dev.config_mut().set_command(0x0002); // MEM

    assert!(dev.irq_level());

    // PCI command bit 10 disables legacy INTx assertion.
    dev.config_mut().set_command(0x0002 | (1 << 10));
    assert!(
        !dev.irq_level(),
        "IRQ must be suppressed when PCI COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx without touching controller state: the pending interrupt should become
    // visible again.
    dev.config_mut().set_command(0x0002);
    assert!(dev.irq_level());
}

#[test]
fn nvme_process_is_gated_by_pci_command_bus_master_enable() {
    use memory::MemoryBus as _;

    #[derive(Default)]
    struct CountingMem {
        buf: Vec<u8>,
        reads: u64,
        writes: u64,
    }

    impl CountingMem {
        fn new(size: usize) -> Self {
            Self {
                buf: vec![0u8; size],
                reads: 0,
                writes: 0,
            }
        }
    }

    impl memory::MemoryBus for CountingMem {
        fn read_physical(&mut self, paddr: u64, out: &mut [u8]) {
            self.reads += 1;
            let start = paddr as usize;
            let end = start + out.len();
            out.copy_from_slice(&self.buf[start..end]);
        }

        fn write_physical(&mut self, paddr: u64, data: &[u8]) {
            self.writes += 1;
            let start = paddr as usize;
            let end = start + data.len();
            self.buf[start..end].copy_from_slice(data);
        }
    }

    let mut dev = NvmePciDevice::default();
    let mut mem = CountingMem::new(2 * 1024 * 1024);

    // Enable MMIO decoding (MEM bit) so the controller register programming takes effect, but keep
    // Bus Master Enable clear so DMA is not allowed yet.
    dev.config_mut().set_command(0x0002); // MEM

    // Minimal controller enable with a small admin queue pair.
    let asq: u64 = 0x10000;
    let acq: u64 = 0x20000;
    // AQA: both admin SQ/CQ size = 16 entries (encoded as size-1).
    dev.write(0x0024, 4, 0x000f_000f);
    dev.write(0x0028, 8, asq);
    dev.write(0x0030, 8, acq);
    dev.write(NVME_CC, 4, 1); // CC.EN

    // Queue one invalid admin command so `process()` would need to DMA-read the SQ entry and
    // DMA-write a CQ completion.
    let mut cmd = [0u8; 64];
    cmd[0] = 0xFF; // invalid opcode
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    mem.write_physical(asq, &cmd);

    // Ring the SQ0 tail doorbell (admin SQ is qid=0; SQ tail doorbell at 0x1000).
    dev.write(0x1000, 4, 1);

    // With BME disabled, the device must not access guest memory.
    dev.process(&mut mem);
    assert_eq!(mem.reads, 0, "expected no DMA reads while BME is disabled");
    assert_eq!(mem.writes, 1, "expected only the test's own write_physical call");

    // Enable bus mastering and retry: now DMA should occur.
    dev.config_mut().set_command(0x0006); // MEM + BME
    dev.process(&mut mem);
    assert!(
        mem.reads > 0,
        "expected NVMe process() to DMA-read from guest memory once BME is enabled"
    );
    assert!(
        mem.writes > 1,
        "expected NVMe process() to DMA-write to guest memory once BME is enabled"
    );
}
