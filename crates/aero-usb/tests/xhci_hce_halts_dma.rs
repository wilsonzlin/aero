use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

#[derive(Default)]
struct CountingMem {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
}

impl CountingMem {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
            reads: 0,
            writes: 0,
        }
    }

    fn reset_counts(&mut self) {
        self.reads = 0;
        self.writes = 0;
    }
}

impl MemoryBus for CountingMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.reads += 1;
        let Ok(start) = usize::try_from(paddr) else {
            buf.fill(0);
            return;
        };
        let end = start.saturating_add(buf.len());
        if end > self.bytes.len() {
            buf.fill(0);
            return;
        }
        buf.copy_from_slice(&self.bytes[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.writes += 1;
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        let end = start.saturating_add(buf.len());
        if end > self.bytes.len() {
            return;
        }
        self.bytes[start..end].copy_from_slice(buf);
    }
}

fn write_erst_entry<M: MemoryBus + ?Sized>(mem: &mut M, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn xhci_step_1ms_does_not_dma_after_host_controller_error() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    // Force Host Controller Error via an invalid but in-bounds Event Ring configuration.
    let erstba = 0x1000u64;
    let ring_base = 0x2000u64;
    write_erst_entry(&mut mem, erstba, ring_base, 0);

    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    xhci.service_event_ring(&mut mem);

    let sts = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HCE, 0, "controller should latch HCE");

    // Once HCE is set, the guest must reset the controller. Further controller ticks should not
    // touch guest memory (avoid repeated open-bus DMAs on a broken configuration).
    mem.reset_counts();
    xhci.step_1ms(&mut mem);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

