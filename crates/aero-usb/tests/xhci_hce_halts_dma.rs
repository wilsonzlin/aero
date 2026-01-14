use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader};
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::{
    ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult,
};

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

fn write_erst_entry<M: MemoryBus + ?Sized>(
    mem: &mut M,
    erstba: u64,
    seg_base: u64,
    seg_size_trbs: u32,
) {
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

    // Program CRCR so `step_1ms` has a valid tick-driven DMA target while RUN is set.
    let crcr_addr = 0x3000u64;
    MemoryBus::write_u32(&mut mem, crcr_addr, 0x1122_3344);
    xhci.mmio_write(regs::REG_CRCR_LO, 4, crcr_addr);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, crcr_addr >> 32);
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Sanity check: without HCE, `step_1ms` should touch guest memory via the tick-driven DMA path.
    mem.reset_counts();
    xhci.step_1ms(&mut mem);
    assert!(
        mem.reads > 0 || mem.writes > 0,
        "expected tick-driven DMA before HCE is latched"
    );

    force_hce(&mut xhci, &mut mem);

    // Once HCE is set, the guest must reset the controller. Further controller ticks should not
    // touch guest memory (avoid repeated open-bus DMAs on a broken configuration).
    mem.reset_counts();
    xhci.step_1ms(&mut mem);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_step_1ms_does_not_dma_after_host_controller_error_even_with_pending_command_ring_work() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    // Enable RUN so command-ring execution is possible.
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    // Clear the deferred dma-on-RUN probe so any subsequent DMA reads come from command-ring
    // execution (not the RUN edge probe).
    xhci.tick_1ms_with_dma(&mut mem);

    // Configure a host-side command ring with a runnable TRB and ring doorbell 0 so `step_1ms`
    // would normally DMA-read the TRB from guest memory.
    let cmd_ring = 0x3000u64;
    xhci.set_command_ring(cmd_ring, true);
    let mut cmd = Trb::new(0, 0, 0);
    cmd.set_trb_type(TrbType::NoOpCommand);
    cmd.set_cycle(true);
    cmd.write_to(&mut mem, cmd_ring);
    xhci.mmio_write(u64::from(regs::DBOFF_VALUE), 4, 0);

    mem.reset_counts();
    xhci.step_1ms(&mut mem);
    assert!(
        mem.reads > 0,
        "expected command-ring processing to DMA-read guest memory before HCE is latched"
    );

    // Re-ring doorbell 0 so there is still pending command-ring work when we enter HCE.
    xhci.mmio_write(u64::from(regs::DBOFF_VALUE), 4, 0);

    force_hce(&mut xhci, &mut mem);

    // With HCE latched, the controller must not touch guest memory even if the command ring is
    // kicked and runnable.
    mem.reset_counts();
    xhci.step_1ms(&mut mem);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_tick_1ms_with_dma_does_not_dma_after_host_controller_error_even_with_run_set() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    // Program CRCR so the tick path has a valid DMA target.
    let crcr_addr = 0x3000u64;
    MemoryBus::write_u32(&mut mem, crcr_addr, 0x1122_3344);
    xhci.mmio_write(regs::REG_CRCR_LO, 4, crcr_addr);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, crcr_addr >> 32);
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Sanity check: without HCE, tick-driven DMA should touch guest memory.
    mem.reset_counts();
    xhci.tick_1ms_with_dma(&mut mem);
    assert!(
        mem.reads > 0 || mem.writes > 0,
        "expected tick-driven DMA before HCE is latched"
    );

    force_hce(&mut xhci, &mut mem);

    // With HCE latched, even `tick_1ms_with_dma` must not touch guest memory (DMA-on-RUN probe +
    // CRCR dword read are suppressed).
    mem.reset_counts();
    xhci.tick_1ms_with_dma(&mut mem);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_reset_clears_host_controller_error_and_allows_dma_again() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    // Program CRCR so the tick-driven DMA path has a valid target while RUN is set.
    let crcr_addr = 0x3000u64;
    MemoryBus::write_u32(&mut mem, crcr_addr, 0x1122_3344);
    xhci.mmio_write(regs::REG_CRCR_LO, 4, crcr_addr);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, crcr_addr >> 32);
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Sanity check: without HCE, the tick path should DMA.
    mem.reset_counts();
    xhci.tick_1ms_with_dma(&mut mem);
    assert!(
        mem.reads > 0 || mem.writes > 0,
        "expected tick-driven DMA before HCE is latched"
    );

    force_hce(&mut xhci, &mut mem);

    // Controller reset should clear the fatal error state.
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_HCRST));
    let sts = xhci.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_eq!(
        sts & regs::USBSTS_HCE,
        0,
        "controller reset should clear HCE"
    );

    // After reset, reprogram RUN + CRCR and ensure DMA works again.
    MemoryBus::write_u32(&mut mem, crcr_addr, 0x5566_7788);
    xhci.mmio_write(regs::REG_CRCR_LO, 4, crcr_addr);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, crcr_addr >> 32);
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    mem.reset_counts();
    xhci.tick_1ms_with_dma(&mut mem);
    assert!(
        mem.reads > 0 || mem.writes > 0,
        "expected tick-driven DMA after clearing HCE via controller reset"
    );
}

#[test]
fn xhci_service_event_ring_does_not_dma_after_host_controller_error() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    // Configure a valid Event Ring, enqueue a dummy event, and ensure delivery performs DMA.
    let erstba = 0x1000u64;
    let ring_base = 0x2000u64;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erstba >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, ring_base);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, ring_base >> 32);

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);

    mem.reset_counts();
    xhci.service_event_ring(&mut mem);
    assert!(
        mem.writes > 0,
        "expected event ring delivery to DMA-write guest memory before HCE is latched"
    );
    assert_eq!(xhci.pending_event_count(), 0, "event should be consumed");

    // Latch Host Controller Error and ensure `service_event_ring` does not touch guest memory or
    // consume pending events.
    force_hce(&mut xhci, &mut mem);
    while xhci.pop_pending_event().is_some() {}

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    assert_eq!(xhci.pending_event_count(), 1, "expected one queued event");

    mem.reset_counts();
    xhci.service_event_ring(&mut mem);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
    assert_eq!(
        xhci.pending_event_count(),
        1,
        "events must remain queued while in HCE state"
    );
}

#[test]
fn xhci_tick_does_not_dma_after_host_controller_error_even_with_active_endpoint() {
    #[derive(Default)]
    struct NakDevice;

    impl UsbDeviceModel for NakDevice {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Ack
        }

        fn handle_in_transfer(&mut self, _ep_addr: u8, _max_len: usize) -> UsbInResult {
            UsbInResult::Nak
        }

        fn handle_out_transfer(&mut self, _ep_addr: u8, _data: &[u8]) -> UsbOutResult {
            UsbOutResult::Nak
        }
    }

    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::with_port_count(1);

    xhci.attach_device(0, Box::new(NakDevice));
    while xhci.pop_pending_event().is_some() {}

    // Enable a slot (requires DCBAAP), then clear DCBAAP so the transfer engine uses only the
    // controller-local ring cursor (test harness convenience).
    xhci.set_dcbaap(0x1000);
    let slot_id = xhci.enable_slot(&mut mem).slot_id;
    xhci.set_dcbaap(0);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    assert_eq!(
        xhci.address_device(slot_id, slot_ctx).completion_code,
        aero_usb::xhci::CommandCompletionCode::Success,
        "address device"
    );

    // Transfer-ring execution is gated by RUN in the controller tick path.
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Queue an active bulk/interrupt endpoint with a single runnable TRB so `tick()` would normally
    // DMA.
    let ring_addr = 0x4000u64;
    let mut trb = Trb::new(0, 0, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(true);
    trb.write_to(&mut mem, ring_addr);
    xhci.set_endpoint_ring(slot_id, 2, ring_addr, true);
    xhci.ring_doorbell(slot_id, 2);

    // Sanity check: the controller should have recorded at least one active endpoint.
    const TAG_ACTIVE_ENDPOINTS: u16 = 22;
    let snapshot = xhci.save_state();
    let r = SnapshotReader::parse(&snapshot, *b"XHCI").expect("parse snapshot");
    let active = r
        .bytes(TAG_ACTIVE_ENDPOINTS)
        .expect("missing active endpoints field");
    let count = u32::from_le_bytes(active[0..4].try_into().unwrap());
    assert_eq!(count, 1, "expected one queued active endpoint");

    // Sanity check: with RUN set and without HCE, the transfer tick should DMA-read the queued TRB
    // (even though the device NAKs the transfer, so the endpoint remains active).
    mem.reset_counts();
    xhci.tick(&mut mem);
    assert!(
        mem.reads > 0 || mem.writes > 0,
        "expected transfer-ring DMA before HCE is latched"
    );

    force_hce(&mut xhci, &mut mem);

    // With HCE latched, the controller must not touch guest memory even if work is queued.
    mem.reset_counts();
    xhci.tick(&mut mem);
    assert_eq!(mem.reads, 0, "unexpected DMA reads while in HCE state");
    assert_eq!(mem.writes, 0, "unexpected DMA writes while in HCE state");
}

#[test]
fn xhci_mmio_read_does_not_dma_after_host_controller_error() {
    let mut mem = CountingMem::new(0x20_000);
    let mut xhci = XhciController::new();

    // MMIO reads should never DMA, even if we ring doorbell 0 (setting `cmd_kick`) before the
    // controller enters the fatal error state.
    let doorbell0 = u64::from(regs::DBOFF_VALUE);
    xhci.mmio_write(doorbell0, 4, 0);

    force_hce(&mut xhci, &mut mem);

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
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    let sts = xhci.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_eq!(
        sts & regs::USBSTS_HCHALTED,
        0,
        "setting RUN should clear HCHalted"
    );

    force_hce(&mut xhci, &mut mem);

    // Real hardware halts the controller on fatal errors. Mirror that behaviour by reporting
    // HCHalted even if USBCMD.RUN remains set.
    let sts = xhci.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_ne!(sts & regs::USBSTS_HCE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
}
