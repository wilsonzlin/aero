use std::cell::Cell;
use std::rc::Rc;

use aero_usb::xhci::interrupter::{IMAN_IE, IMAN_IP};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{
    regs, XhciController, PORTSC_CCS, PORTSC_CSC, PORTSC_PEC, PORTSC_PED, PORTSC_PR, PORTSC_PRC,
};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

mod util;

use util::TestMemory;

#[derive(Clone)]
struct DummyDevice {
    reset_count: Rc<Cell<u32>>,
}

impl UsbDeviceModel for DummyDevice {
    fn reset(&mut self) {
        self.reset_count.set(self.reset_count.get() + 1);
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

fn assert_portsc_has(portsc: u32, bit: u32) {
    assert!(
        portsc & bit != 0,
        "expected PORTSC to have bit {bit:#x}, got {portsc:#x}"
    );
}

fn assert_portsc_not(portsc: u32, bit: u32) {
    assert!(
        portsc & bit == 0,
        "expected PORTSC to not have bit {bit:#x}, got {portsc:#x}"
    );
}

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

fn drain_event(xhci: &mut XhciController, mem: &mut TestMemory, ring_base: u64, index: u64) -> Trb {
    let ev = Trb::read_from(mem, ring_base + index * (TRB_LEN as u64));
    assert!(ev.cycle());
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    // Acknowledge the interrupt by clearing IMAN.IP.
    xhci.mmio_write(mem, regs::REG_INTR0_IMAN, 4, IMAN_IP | IMAN_IE);
    assert!(!xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());

    ev
}

#[test]
fn xhci_ports_attach_reset_and_events() {
    let mut xhci = XhciController::new();
    let mut mem = TestMemory::new(0x20_000);
    let portsc_off = regs::port::portsc_offset(0);

    // Configure interrupter 0 with a single-segment event ring so PSC events can be delivered
    // via guest memory.
    let erstba = 0x1000;
    let ring_base = 0x2000;
    write_erst_entry(&mut mem, erstba, ring_base, 8);

    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERSTBA_HI,
        4,
        (erstba >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (ring_base >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    let reset_count = Rc::new(Cell::new(0));
    xhci.attach_device(
        0,
        Box::new(DummyDevice {
            reset_count: reset_count.clone(),
        }),
    );

    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_has(portsc, PORTSC_CCS);
    assert_portsc_has(portsc, PORTSC_CSC);

    // Flush PSC event into the guest event ring.
    xhci.tick_1ms(&mut mem);
    let ev = drain_event(&mut xhci, &mut mem, ring_base, 0);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Clear CSC via write-1-to-clear.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_CSC);
    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_not(portsc, PORTSC_CSC);

    // Trigger port reset.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_PR);
    assert_eq!(reset_count.get(), 1, "device reset should be called once");

    // Wait ~50ms for reset to complete.
    for _ in 0..50 {
        xhci.tick_1ms(&mut mem);
    }

    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_not(portsc, PORTSC_PR);
    assert_portsc_has(portsc, PORTSC_PED);
    assert_portsc_has(portsc, PORTSC_PEC);
    assert_portsc_has(portsc, PORTSC_PRC);

    let ev = drain_event(&mut xhci, &mut mem, ring_base, 1);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Clear PEC/PRC so we can observe a second reset completion while the port is already enabled.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_PEC | PORTSC_PRC);

    // Trigger a second reset while the port is enabled. This sets PEC and queues a PSC event.
    xhci.mmio_write(&mut mem, portsc_off, 4, PORTSC_PR);
    assert_eq!(reset_count.get(), 2, "device reset should be called twice");

    // Flush PSC event for the port enable change (port disabled during reset).
    xhci.tick_1ms(&mut mem);
    let ev = drain_event(&mut xhci, &mut mem, ring_base, 2);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Wait for reset completion. PEC is already set, so PRC must drive the PSC event.
    for _ in 0..50 {
        xhci.tick_1ms(&mut mem);
    }

    let portsc = xhci.mmio_read(&mut mem, portsc_off, 4);
    assert_portsc_not(portsc, PORTSC_PR);
    assert_portsc_has(portsc, PORTSC_PED);
    assert_portsc_has(portsc, PORTSC_PRC);

    let ev = drain_event(&mut xhci, &mut mem, ring_base, 3);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);
    assert_eq!(xhci.pending_event_count(), 0);

    // Sanity: TRB is stable POD.
    let _trb_layout: Trb = ev;
}

#[test]
fn xhci_tick_advances_mfindex_and_flushes_events_after_erst_configured() {
    let mut xhci = XhciController::with_port_count(1);
    let mut mem = TestMemory::new(0x20_000);

    let mf0 = xhci.mmio_read(&mut mem, regs::REG_MFINDEX, 4);

    let reset_count = Rc::new(Cell::new(0));
    xhci.attach_device(
        0,
        Box::new(DummyDevice {
            reset_count: reset_count.clone(),
        }),
    );
    assert_eq!(xhci.pending_event_count(), 1);

    // Pre-fill a would-be event ring slot so we can detect accidental writes before ERST is configured.
    let ring_base = 0x2000usize;
    mem.data[ring_base..ring_base + TRB_LEN].fill(0xA5);

    // Tick without an event ring: MFINDEX should advance, but the PSC event should remain queued.
    xhci.tick_1ms(&mut mem);
    let mf1 = xhci.mmio_read(&mut mem, regs::REG_MFINDEX, 4);
    assert_ne!(mf1, mf0, "expected MFINDEX to advance on tick_1ms");
    assert_eq!(xhci.pending_event_count(), 1);
    assert_eq!(&mem.data[ring_base..ring_base + TRB_LEN], &[0xA5; TRB_LEN]);

    // Now configure interrupter 0 with a single-segment event ring and tick again. The queued PSC event
    // should flush into guest memory.
    let erstba = 0x1000u64;
    write_erst_entry(&mut mem, erstba, ring_base as u64, 8);

    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERSTBA_HI,
        4,
        (erstba >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (ring_base >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    xhci.tick_1ms(&mut mem);
    assert_eq!(xhci.pending_event_count(), 0);

    let ev = Trb::read_from(&mut mem, ring_base as u64);
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    assert!(ev.cycle());
}
