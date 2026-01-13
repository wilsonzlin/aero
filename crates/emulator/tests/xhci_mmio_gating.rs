use emulator::io::pci::{MmioDevice, PciDevice};
use emulator::io::usb::xhci::{regs, XhciController, XhciPciDevice};
use memory::MemoryBus;

struct PanicMem;

impl MemoryBus for PanicMem {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write");
    }
}

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large for VecMemory");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let range = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[range]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let range = self.range(paddr, buf.len());
        self.data[range].copy_from_slice(buf);
    }
}

#[test]
fn pci_command_mem_bit_gates_xhci_mmio() {
    let mut dev = XhciPciDevice::new(XhciController::new(), 0xfebf_0000);
    let mut mem = PanicMem;

    // COMMAND.MEM is clear by default: reads float high, writes ignored.
    assert_eq!(
        dev.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION, 4),
        u32::MAX
    );
    dev.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // Enable MMIO decoding and verify the earlier write did not take effect.
    dev.config_write(0x04, 2, 1 << 1);
    assert_ne!(
        dev.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION, 4),
        u32::MAX
    );
    assert_eq!(dev.mmio_read(&mut mem, regs::REG_USBCMD, 4) & regs::USBCMD_RUN, 0);

    // Writes should apply once MEM is enabled.
    dev.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_ne!(dev.mmio_read(&mut mem, regs::REG_USBCMD, 4) & regs::USBCMD_RUN, 0);
}

#[test]
fn pci_command_bme_bit_gates_xhci_dma() {
    let mut dev = XhciPciDevice::new(XhciController::new(), 0xfebf_0000);

    // Enable MMIO decoding so we can program registers, but leave bus mastering disabled.
    dev.config_write(0x04, 2, 1 << 1);

    // Program a non-zero CRCR so the controller has something to DMA from.
    dev.mmio_write(&mut PanicMem, regs::REG_CRCR_LO, 4, 0x1000);
    dev.mmio_write(&mut PanicMem, regs::REG_CRCR_HI, 4, 0);

    // With BME clear, starting the controller must not DMA (PanicMem would panic if touched).
    dev.mmio_write(&mut PanicMem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // Enable bus mastering and verify the DMA path is now reachable.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.mmio_write(&mut PanicMem, regs::REG_USBCMD, 4, 0); // clear RUN

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dev.mmio_write(&mut PanicMem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    }));
    assert!(err.is_err());
}

#[test]
fn pci_command_intx_disable_bit_masks_irq_level() {
    let mut dev = XhciPciDevice::new(XhciController::new(), 0xfebf_0000);
    let mut mem = VecMemory::new(0x4000);

    // Enable MMIO decoding + bus mastering so the DMA path runs and asserts an IRQ.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, 0x1000);
    dev.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    dev.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    assert!(dev.controller.irq_level());
    assert!(dev.irq_level());

    // Disable legacy INTx delivery via PCI command bit 10.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2) | (1 << 10));
    assert!(dev.controller.irq_level());
    assert!(!dev.irq_level());

    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    assert!(dev.irq_level());
}

