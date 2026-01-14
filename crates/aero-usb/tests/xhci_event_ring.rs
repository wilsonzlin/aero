use aero_usb::xhci::interrupter::{ERDP_EHB, IMAN_IE, IMAN_IP};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController, EVENT_ENQUEUE_BUDGET_PER_TICK};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

#[derive(Default)]
struct DummyDevice;

impl UsbDeviceModel for DummyDevice {
    fn reset(&mut self) {}

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn event_ring_enqueue_writes_trb_and_sets_interrupt_pending() {
    let mut mem = TestMemory::new(0x20_000);

    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut evt = Trb::default();
    evt.parameter = 0x1234_5678;
    evt.set_trb_type(TrbType::PortStatusChangeEvent);

    xhci.post_event(evt);
    xhci.tick_1ms_and_service_event_ring(&mut mem);

    let got = Trb::read_from(&mut mem, ring_base);
    assert!(got.cycle(), "controller should set the producer cycle bit");
    assert_eq!(got.trb_type(), TrbType::PortStatusChangeEvent);
    assert_eq!(got.parameter, 0x1234_5678);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    // IMAN.IE gates IRQ assertion while preserving the pending latch.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, 0);
    assert!(xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);
    assert!(xhci.irq_level());

    // Verify USBSTS.EINT is W1C and can be used to acknowledge interrupter 0.
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());

    // Generate another event and verify IMAN.IP is also W1C.
    xhci.post_event(evt);
    xhci.service_event_ring(&mut mem);
    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    // Verify IMAN.IP is W1C.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IP | IMAN_IE);
    assert!(!xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());
}

#[test]
fn event_ring_erdp_ehb_write_clears_interrupt_pending() {
    let mut mem = TestMemory::new(0x20_000);

    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut evt = Trb::default();
    evt.parameter = 0x1234_5678;
    evt.set_trb_type(TrbType::PortStatusChangeEvent);

    xhci.post_event(evt);
    xhci.service_event_ring(&mut mem);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    // Many xHCI drivers acknowledge interrupts by writing ERDP with EHB set.
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_LO,
        4,
        (ring_base as u32) | (ERDP_EHB as u32),
    );
    assert!(!xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());
}

#[test]
fn event_ring_erdp_ehb_byte_write_clears_interrupt_pending() {
    let mut mem = TestMemory::new(0x20_000);

    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut evt = Trb::default();
    evt.parameter = 0x1234_5678;
    evt.set_trb_type(TrbType::PortStatusChangeEvent);

    xhci.post_event(evt);
    xhci.service_event_ring(&mut mem);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    // Exercise sub-dword MMIO writes: a byte write with EHB set should still acknowledge the
    // interrupt pending latch.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 1, ERDP_EHB as u32);
    assert!(!xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());
}

/// A tiny `MemoryBus` that never panics on out-of-range accesses.
///
/// This is important for xHCI robustness: guests can program ERSTBA/ERDP to arbitrary physical
/// addresses.
struct SafeMemory {
    bytes: Vec<u8>,
}

impl SafeMemory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }
}

impl MemoryBus for SafeMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let Ok(start) = usize::try_from(paddr) else {
            buf.fill(0);
            return;
        };
        let Some(end) = start.checked_add(buf.len()) else {
            buf.fill(0);
            return;
        };
        if end > self.bytes.len() {
            buf.fill(0);
            return;
        }
        buf.copy_from_slice(&self.bytes[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        let Some(end) = start.checked_add(buf.len()) else {
            return;
        };
        if end > self.bytes.len() {
            return;
        }
        self.bytes[start..end].copy_from_slice(buf);
    }
}

#[test]
fn event_ring_invalid_config_sets_host_controller_error() {
    let mut mem = SafeMemory::new(0x1000);
    let mut xhci = XhciController::new();

    // Program ERSTSZ/ERSTBA/ERDP with an out-of-range ERSTBA (unmapped guest memory).
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, 0xdead_beef);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, 0x100);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, 0);

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);

    // Must not panic, and should set USBSTS.HCE as a sticky error bit.
    xhci.service_event_ring(&mut mem);

    let sts = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HCE, 0, "controller should latch HCE");

    // USBSTS is RW1C, but HCE is sticky: writing 1 must not clear it.
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_HCE);
    let sts2 = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_ne!(sts2 & regs::USBSTS_HCE, 0, "HCE should be sticky");
}

#[test]
fn event_ring_invalid_erdp_sets_host_controller_error() {
    let mut mem = TestMemory::new(0x20_000);
    let mut xhci = XhciController::new();

    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);

    // ERDP points outside the configured segment (segment length = 4 * 16 bytes = 0x40).
    let invalid_erdp = ring_base + 0x1000;
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, invalid_erdp as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (invalid_erdp >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);

    xhci.service_event_ring(&mut mem);
    assert_eq!(
        xhci.pending_event_count(),
        1,
        "invalid ERDP must not consume the queued event"
    );

    let sts = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_ne!(
        sts & regs::USBSTS_HCE,
        0,
        "controller should latch HCE on invalid ERDP"
    );
}

#[test]
fn event_ring_wrap_and_budget_are_bounded() {
    let mut mem = TestMemory::new(0x40_000);

    let erstba = 0x1000;
    let ring_base = 0x8000;
    // One segment with enough space for `EVENT_ENQUEUE_BUDGET_PER_TICK + 1` TRBs.
    write_erst_entry(
        &mut mem,
        erstba,
        ring_base,
        (EVENT_ENQUEUE_BUDGET_PER_TICK as u32) + 1,
    );

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    for i in 0..(EVENT_ENQUEUE_BUDGET_PER_TICK + 1) {
        let mut evt = Trb::default();
        evt.parameter = i as u64;
        evt.set_trb_type(TrbType::PortStatusChangeEvent);
        xhci.post_event(evt);
    }

    xhci.service_event_ring(&mut mem);

    // Only the budgeted number of events should have been written.
    assert_eq!(xhci.pending_event_count(), 1);

    let last_written_addr = ring_base + (EVENT_ENQUEUE_BUDGET_PER_TICK as u64) * (TRB_LEN as u64);
    let last = Trb::read_from(&mut mem, last_written_addr);
    assert!(
        !last.cycle(),
        "TRB just beyond the enqueue budget should still be empty/zeroed"
    );
}

#[test]
fn event_ring_wrap_toggles_cycle_and_respects_consumer_erdp() {
    let mut mem = TestMemory::new(0x20_000);

    let erstba = 0x1000;
    let ring_base = 0x2000;
    // Single segment, 2 TRBs. This makes it easy to force a wrap + cycle toggle.
    write_erst_entry(&mut mem, erstba, ring_base, 2);

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut ev0 = Trb::default();
    ev0.parameter = 0xaaaa;
    ev0.set_trb_type(TrbType::PortStatusChangeEvent);
    let mut ev1 = Trb::default();
    ev1.parameter = 0xbbbb;
    ev1.set_trb_type(TrbType::PortStatusChangeEvent);
    let mut ev2 = Trb::default();
    ev2.parameter = 0xcccc;
    ev2.set_trb_type(TrbType::PortStatusChangeEvent);

    // Post 3 events into a ring that can only hold 2; the 3rd should remain pending.
    xhci.post_event(ev0);
    xhci.post_event(ev1);
    xhci.post_event(ev2);
    xhci.service_event_ring(&mut mem);

    assert_eq!(
        xhci.pending_event_count(),
        1,
        "event ring should stop before overwriting the consumer"
    );

    // Both TRBs should be written with the initial producer cycle state (C=1).
    let got0 = Trb::read_from(&mut mem, ring_base);
    let got1 = Trb::read_from(&mut mem, ring_base + 1 * TRB_LEN as u64);
    assert!(got0.cycle());
    assert!(got1.cycle());
    assert_eq!(got0.parameter, 0xaaaa);
    assert_eq!(got1.parameter, 0xbbbb);

    // Simulate the guest consuming the first TRB by advancing ERDP to entry 1.
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_LO,
        4,
        (ring_base + 1 * TRB_LEN as u64) as u32,
    );
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        ((ring_base + 1 * TRB_LEN as u64) >> 32) as u32,
    );

    // Now the producer should be able to wrap and enqueue the final event with a toggled cycle bit.
    xhci.service_event_ring(&mut mem);
    assert_eq!(xhci.pending_event_count(), 0);

    let wrapped = Trb::read_from(&mut mem, ring_base);
    assert!(
        !wrapped.cycle(),
        "producer should toggle cycle bit after wrapping the segment"
    );
    assert_eq!(wrapped.parameter, 0xcccc);
}

#[test]
fn port_status_change_event_is_delivered_into_guest_event_ring() {
    let mut mem = TestMemory::new(0x20_000);

    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Root hub port 0 connect should enqueue a Port Status Change Event TRB.
    xhci.attach_device(0, Box::new(DummyDevice::default()));

    xhci.service_event_ring(&mut mem);

    let got = Trb::read_from(&mut mem, ring_base);
    assert_eq!(got.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = ((got.dword0() >> 24) & 0xff) as u8;
    assert_eq!(port_id, 1);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());
}

#[test]
fn event_ring_cycle_toggles_on_wrap() {
    let mut mem = TestMemory::new(0x20_000);

    let erstba = 0x1000;
    let ring_base = 0x4000;
    // Small ring: 2 TRBs so we can exercise producer wrap + cycle toggle.
    write_erst_entry(&mut mem, erstba, ring_base, 2);

    let mut xhci = XhciController::new();
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let mut evt1 = Trb::default();
    evt1.parameter = 0x1111_1111;
    evt1.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt1);

    let mut evt2 = Trb::default();
    evt2.parameter = 0x2222_2222;
    evt2.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt2);

    xhci.service_event_ring(&mut mem);

    let got1 = Trb::read_from(&mut mem, ring_base);
    let got2 = Trb::read_from(&mut mem, ring_base + TRB_LEN as u64);
    assert!(got1.cycle());
    assert!(got2.cycle());

    // Advance ERDP to the second entry so the ring is no longer full.
    let erdp = ring_base + TRB_LEN as u64;
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, erdp as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (erdp >> 32) as u32);

    let mut evt3 = Trb::default();
    evt3.parameter = 0x3333_3333;
    evt3.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt3);
    xhci.service_event_ring(&mut mem);

    // Third event wraps to slot 0 with cycle bit toggled (0).
    let got3 = Trb::read_from(&mut mem, ring_base);
    assert!(!got3.cycle(), "cycle bit should toggle after producer wrap");
    assert_eq!(got3.parameter, 0x3333_3333);
}

#[test]
fn event_ring_flushes_pending_events_after_erst_programmed() {
    let mut mem = TestMemory::new(0x20_000);

    let mut xhci = XhciController::new();
    let mut evt = Trb::default();
    evt.parameter = 0xdead_beef;
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);

    // Without ERST programming, servicing the ring should not consume the event.
    xhci.service_event_ring(&mut mem);
    assert_eq!(xhci.pending_event_count(), 1);
    assert!(!xhci.interrupter0().interrupt_pending());

    let erstba = 0x1000;
    let ring_base = 0x6000;
    write_erst_entry(&mut mem, erstba, ring_base, 4);

    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    xhci.service_event_ring(&mut mem);

    assert_eq!(xhci.pending_event_count(), 0);
    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    let got = Trb::read_from(&mut mem, ring_base);
    assert_eq!(got.parameter, 0xdead_beef);
    assert!(got.cycle());
}
