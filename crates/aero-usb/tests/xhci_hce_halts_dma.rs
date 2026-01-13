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

fn force_hce(xhci: &mut XhciController, mem: &mut CountingMem) {
    // Force Host Controller Error via an invalid but in-bounds Event Ring configuration.
    let erstba = 0x1000u64;
    let ring_base = 0x2000u64;
    write_erst_entry(mem, erstba, ring_base, 0);

    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erstba >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, ring_base);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, ring_base >> 32);

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    xhci.service_event_ring(mem);

    let sts = xhci.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_ne!(sts & regs::USBSTS_HCE, 0, "controller should latch HCE");
}

#[test]
fn xhci_step_1ms_does_not_dma_after_host_controller_error() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    force_hce(&mut xhci, &mut mem);

    // Once HCE is set, the guest must reset the controller. Further controller ticks should not
    // touch guest memory (avoid repeated open-bus DMAs on a broken configuration).
    mem.reset_counts();
    xhci.step_1ms(&mut mem);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_run_does_not_dma_after_host_controller_error() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    force_hce(&mut xhci, &mut mem);

    // Setting RUN should not perform the DMA-on-RUN probe while HCE is latched.
    mem.reset_counts();
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_doorbell_does_not_dma_after_host_controller_error() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    force_hce(&mut xhci, &mut mem);

    // Endpoint doorbells attempt to run transfer execution and drain the event ring immediately.
    // With HCE latched, we should not touch guest memory.
    let doorbell1 = u64::from(regs::DBOFF_VALUE) + u64::from(regs::doorbell::DOORBELL_STRIDE);
    mem.reset_counts();
    xhci.mmio_write(doorbell1, 4, 2);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_mmio_read_does_not_dma_after_host_controller_error() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    force_hce(&mut xhci, &mut mem);

    // MMIO reads run `maybe_process_command_ring()` first. Ensure that path does not DMA after HCE,
    // even if we set `cmd_kick` by ringing doorbell 0.
    let doorbell0 = u64::from(regs::DBOFF_VALUE);
    xhci.mmio_write(doorbell0, 4, 0);

    mem.reset_counts();
    let _ = xhci.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_hce_sets_hchalted_even_when_run_set() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    // With RUN set, HCHalted should normally clear.
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    let sts = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_eq!(
        sts & regs::USBSTS_HCHALTED,
        0,
        "setting RUN should clear HCHalted"
    );

    force_hce(&mut xhci, &mut mem);

    // Real hardware halts the controller on fatal errors. Mirror that behaviour by reporting
    // HCHalted even if USBCMD.RUN remains set.
    let sts = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HCE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
}
