use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

#[derive(Default)]
struct PanicMem;

impl MemoryBus for PanicMem {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write");
    }
}

#[derive(Default)]
struct CountingMem {
    data: Vec<u8>,
    reads: usize,
    writes: usize,
}

impl CountingMem {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            reads: 0,
            writes: 0,
        }
    }
}

impl MemoryBus for CountingMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.reads += 1;
        let start = usize::try_from(paddr).expect("paddr should fit in usize");
        let end = start + buf.len();
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.writes += 1;
        let start = usize::try_from(paddr).expect("paddr should fit in usize");
        let end = start + buf.len();
        self.data[start..end].copy_from_slice(buf);
    }
}

#[test]
fn xhci_controller_caplength_hciversion_reads() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION, 4),
        (0x0100u32 << 16) | 0x40
    );

    // Byte/word reads should match the LE layout.
    assert_eq!(ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION, 1), 0x40);
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION + 2, 2),
        0x0100
    );
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION + 3, 1),
        0x01
    );
}

#[test]
fn xhci_controller_run_triggers_dma_and_w1c_clears_irq() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    // Seed the DMA target.
    mem.data[0x1000..0x1004].copy_from_slice(&[1, 2, 3, 4]);

    // Program CRCR and start the controller: first RUN transition should DMA once.
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1000);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    assert_eq!(mem.reads, 0);

    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_eq!(mem.reads, 1);
    assert!(ctrl.irq_level());
    assert_ne!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_EINT,
        0
    );

    // Writing RUN again should not DMA (no rising edge).
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_eq!(mem.reads, 1);

    // Stop then start again -> second rising edge DMA.
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, 0);
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_eq!(mem.reads, 2);

    // USBSTS is RW1C: writing 1 clears the pending interrupt.
    ctrl.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!ctrl.irq_level());
    assert_eq!(
        ctrl.mmio_read(&mut mem, regs::REG_USBSTS, 4) & regs::USBSTS_EINT,
        0
    );
}

#[test]
fn xhci_controller_snapshot_roundtrip_preserves_regs() {
    let mut ctrl = XhciController::new();
    let mut mem = CountingMem::new(0x4000);

    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1234);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    let bytes = ctrl.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    assert_eq!(restored.mmio_read(&mut mem, regs::REG_USBCMD, 4), regs::USBCMD_RUN);
    assert_eq!(restored.mmio_read(&mut mem, regs::REG_CRCR_LO, 4), 0x1234);
    assert!(restored.irq_level());
}

