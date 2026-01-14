#![cfg(feature = "legacy-usb-xhci")]

use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::Trb;
use emulator::io::pci::{MmioDevice, PciDevice};
use emulator::io::usb::xhci::{regs, XhciController, XhciPciDevice};
use memory::MemoryBus;

#[test]
fn xhci_pci_identity_matches_canonical_profile() {
    let dev = XhciPciDevice::new(XhciController::new(), 0xfebf_0000);

    assert_eq!(dev.config_read(0x00, 2) as u16, 0x1b36); // QEMU/Red Hat
    assert_eq!(dev.config_read(0x02, 2) as u16, 0x000d); // qemu-xhci
    assert_eq!(dev.config_read(0x08, 1) as u8, 0x01); // revision

    // Class code: 0c/03/30 (serial bus / USB / xHCI).
    assert_eq!(dev.config_read(0x0b, 1) as u8, 0x0c);
    assert_eq!(dev.config_read(0x0a, 1) as u8, 0x03);
    assert_eq!(dev.config_read(0x09, 1) as u8, 0x30);

    // Subsystem IDs are set to match the base VID/DID (canonical qemu-xhci identity).
    assert_eq!(dev.config_read(0x2c, 2) as u16, 0x1b36);
    assert_eq!(dev.config_read(0x2e, 2) as u16, 0x000d);

    // INTx pin should be INTA#; line is derived from the canonical PCI routing swizzle.
    assert_eq!(dev.config_read(0x3d, 1) as u8, 0x01);
    assert_eq!(dev.config_read(0x3c, 1) as u8, 0x0b);
}

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
    assert_eq!(
        dev.mmio_read(&mut mem, regs::REG_USBCMD, 4) & regs::USBCMD_RUN,
        0
    );

    // Writes should apply once MEM is enabled.
    dev.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert_ne!(
        dev.mmio_read(&mut mem, regs::REG_USBCMD, 4) & regs::USBCMD_RUN,
        0
    );
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
fn tick_1ms_services_event_ring_only_when_bus_master_enabled() {
    let mut dev = XhciPciDevice::new(XhciController::new(), 0xfebf_0000);
    let mut mem = VecMemory::new(0x10_000);

    // Enable MMIO decoding so we can program interrupter registers, but leave bus mastering off.
    dev.config_write(0x04, 2, 1 << 1);

    // Configure a minimal event ring segment table in guest memory.
    let erstba: u64 = 0x1000; // 64-byte aligned.
    let seg_base: u64 = 0x2000; // 16-byte aligned.
    let seg_size_trbs: u32 = 4;

    mem.write_physical(erstba, &seg_base.to_le_bytes());
    mem.write_physical(erstba + 8, &seg_size_trbs.to_le_bytes());
    mem.write_physical(erstba + 12, &0u32.to_le_bytes());

    // Program interrupter 0 registers.
    dev.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);
    dev.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    dev.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    dev.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERSTBA_HI,
        4,
        (erstba >> 32) as u32,
    );
    dev.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, seg_base as u32);
    dev.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (seg_base >> 32) as u32,
    );

    // Queue a deterministic event TRB in host memory.
    let trb = Trb::new(0x1122_3344_5566_7788, 0x99aa_bbcc, 0xddee_ff00);
    dev.controller.post_event(trb);
    assert!(dev.controller.irq_pending());

    // With Bus Master Enable clear, the controller must not DMA into the event ring.
    dev.tick_1ms(&mut mem);
    assert!(
        dev.controller.irq_pending(),
        "event should remain pending while DMA is disabled"
    );
    let seg_base = usize::try_from(seg_base).unwrap();
    assert_eq!(&mem.data[seg_base..seg_base + 16], &[0u8; 16]);

    // Enable bus mastering and confirm the event ring gets populated.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.tick_1ms(&mut mem);

    assert!(
        !dev.controller.irq_pending(),
        "event should be consumed once DMA is enabled"
    );

    let mut expected = trb;
    expected.set_cycle(true);
    assert_eq!(&mem.data[seg_base..seg_base + 16], &expected.to_bytes());
    assert_ne!(dev.mmio_read(&mut mem, regs::REG_INTR0_IMAN, 4) & 0x1, 0);
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

#[test]
fn pci_bar0_probe_reports_xhci_mmio_window_size() {
    // Use a deliberately misaligned base to ensure the wrapper applies BAR-size alignment.
    let mut dev = XhciPciDevice::new(XhciController::new(), 0xfebf_1234);
    assert_eq!(dev.config_read(0x10, 4), 0xfebf_0000);

    // Standard PCI BAR size probing: write all 1s and read back the size mask.
    dev.config_write(0x10, 4, 0xffff_ffff);
    assert_eq!(
        dev.config_read(0x10, 4),
        !(XhciController::MMIO_SIZE - 1) & 0xffff_fff0
    );

    // BAR programming should be masked to the window size.
    dev.config_write(0x10, 4, 0xfec0_1234);
    assert_eq!(dev.config_read(0x10, 4), 0xfec0_0000);
}
