use std::cell::Cell;
use std::rc::Rc;

use aero_usb::xhci::interrupter::{IMAN_IE, IMAN_IP};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController, PORTSC_CCS, PORTSC_CSC, PORTSC_LWS, PORTSC_PLC, PORTSC_PR, PORTSC_PED};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

mod util;

use util::TestMemory;

#[derive(Clone)]
struct RemoteWakeDevice {
    wake_requested: Rc<Cell<bool>>,
    suspended: Rc<Cell<bool>>,
}

impl UsbDeviceModel for RemoteWakeDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }

    fn set_suspended(&mut self, suspended: bool) {
        self.suspended.set(suspended);
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        if !self.suspended.get() {
            return false;
        }
        if self.wake_requested.get() {
            // Remote wakeup is an edge: once observed, clear until the test re-triggers it.
            self.wake_requested.set(false);
            return true;
        }
        false
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
    // Ensure events are flushed into guest memory (tick_1ms normally does this).
    xhci.service_event_ring(mem);
    assert_eq!(xhci.pending_event_count(), 0);
    let ev = Trb::read_from(mem, ring_base + index * (TRB_LEN as u64));
    assert!(ev.cycle());
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);

    assert!(xhci.interrupter0().interrupt_pending());
    assert!(xhci.irq_level());

    // Acknowledge the interrupt by clearing IMAN.IP.
    xhci.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IP | IMAN_IE));
    assert!(!xhci.interrupter0().interrupt_pending());
    assert!(!xhci.irq_level());

    ev
}

#[test]
fn xhci_usb2_port_remote_wakeup_resumes_from_u3_and_sets_plc() {
    let mut xhci = XhciController::with_port_count(1);
    let mut mem = TestMemory::new(0x20_000);
    let portsc_off = regs::port::portsc_offset(0);

    // Configure interrupter 0 with a single-segment event ring so PSC events can be delivered.
    let erstba = 0x1000u64;
    let ring_base = 0x2000u64;
    write_erst_entry(&mut mem, erstba, ring_base, 8);
    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erstba >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, ring_base);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, ring_base >> 32);
    xhci.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IE));

    let wake_requested = Rc::new(Cell::new(false));
    let suspended = Rc::new(Cell::new(false));
    xhci.attach_device(
        0,
        Box::new(RemoteWakeDevice {
            wake_requested: wake_requested.clone(),
            suspended: suspended.clone(),
        }),
    );

    let portsc = xhci.mmio_read(portsc_off, 4) as u32;
    assert_portsc_has(portsc, PORTSC_CCS);
    assert_portsc_has(portsc, PORTSC_CSC);

    // Drain initial connect event.
    xhci.tick_1ms(&mut mem);
    let ev = drain_event(&mut xhci, &mut mem, ring_base, 0);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    // Clear CSC so later events are easier to reason about.
    xhci.mmio_write(portsc_off, 4, u64::from(PORTSC_CSC));

    // Reset the port to enable it.
    xhci.mmio_write(portsc_off, 4, u64::from(PORTSC_PR));
    for _ in 0..50 {
        xhci.tick_1ms(&mut mem);
    }

    // Drain reset completion PSC event.
    let ev = drain_event(&mut xhci, &mut mem, ring_base, 1);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    let portsc = xhci.mmio_read(portsc_off, 4) as u32;
    assert_portsc_has(portsc, PORTSC_PED);
    assert_eq!(
        portsc & regs::PORTSC_PLS_MASK,
        0,
        "expected port to be in U0 after reset completes"
    );
    assert!(
        !suspended.get(),
        "device must not be marked suspended while port link state is U0"
    );

    // Suspend the port (U3) via link-state write strobe.
    let suspend = PORTSC_LWS | (3u32 << regs::PORTSC_PLS_SHIFT);
    xhci.mmio_write(portsc_off, 4, u64::from(suspend));
    xhci.tick_1ms(&mut mem);

    // Drain PSC event for the link-state transition.
    let ev = drain_event(&mut xhci, &mut mem, ring_base, 2);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    let portsc = xhci.mmio_read(portsc_off, 4) as u32;
    assert_eq!(
        portsc & regs::PORTSC_PLS_MASK,
        3u32 << regs::PORTSC_PLS_SHIFT,
        "expected port to be in U3 after suspend request"
    );
    assert_portsc_has(portsc, PORTSC_PLC);
    assert!(
        suspended.get(),
        "device must be notified of suspend while port link state is U3"
    );

    // Clear PLC so we can observe it being set again by remote wakeup.
    xhci.mmio_write(portsc_off, 4, u64::from(PORTSC_PLC));
    let portsc = xhci.mmio_read(portsc_off, 4) as u32;
    assert_portsc_not(portsc, PORTSC_PLC);

    // Trigger remote wakeup while suspended.
    wake_requested.set(true);
    xhci.tick_1ms(&mut mem);

    // Drain PSC event for remote wakeup.
    let ev = drain_event(&mut xhci, &mut mem, ring_base, 3);
    let port_id = ((ev.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) & 0xff) as u8;
    assert_eq!(port_id, 1);

    let portsc = xhci.mmio_read(portsc_off, 4) as u32;
    assert_eq!(
        portsc & regs::PORTSC_PLS_MASK,
        0,
        "expected port to return to U0 after remote wakeup"
    );
    assert_portsc_has(portsc, PORTSC_PLC);
    assert!(
        !suspended.get(),
        "device must exit suspended state after remote wakeup resumes the port"
    );
    assert!(
        !wake_requested.get(),
        "remote wakeup should be consumed as an edge-triggered signal"
    );
}

