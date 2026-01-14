use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

fn write_erst_entry(mem: &mut dyn MemoryBus, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    mem.write_u64(erstba, seg_base);
    mem.write_u32(erstba + 8, seg_size_trbs);
    mem.write_u32(erstba + 12, 0);
}

fn count_completion_events(mem: &mut TestMemory, base: u64, max: usize) -> usize {
    (0..max)
        .take_while(|&i| {
            Trb::read_from(mem, base + (i as u64) * (TRB_LEN as u64)).trb_type()
                == TrbType::CommandCompletionEvent
        })
        .count()
}

#[test]
fn xhci_snapshot_preserves_cmd_kick_and_command_ring_cursor() {
    const CMD_RING_BASE: u64 = 0x1000;
    const ERST_BASE: u64 = 0x2000;
    const EVENT_RING_BASE: u64 = 0x3000;
    const CMD_COUNT: usize = 40;
    const ERST_SEG_TRBS: u32 = 128;

    let mut mem = TestMemory::new(0x40_000);

    // Command ring: N x [NoOpCmd] then a stop marker with cycle mismatch.
    for i in 0..CMD_COUNT {
        let mut noop = Trb::default();
        noop.set_cycle(true);
        noop.set_trb_type(TrbType::NoOpCommand);
        noop.write_to(&mut mem, CMD_RING_BASE + (i as u64) * (TRB_LEN as u64));
    }
    {
        let mut stop = Trb::default();
        stop.set_cycle(false);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.write_to(
            &mut mem,
            CMD_RING_BASE + (CMD_COUNT as u64) * (TRB_LEN as u64),
        );
    }

    write_erst_entry(&mut mem, ERST_BASE, EVENT_RING_BASE, ERST_SEG_TRBS);

    let mut xhci = XhciController::new();

    // Configure interrupter 0 event ring.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, ERST_BASE as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (ERST_BASE >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, EVENT_RING_BASE as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, (EVENT_RING_BASE >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Program the command ring dequeue pointer + cycle state.
    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, (CMD_RING_BASE as u32) | 1);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (CMD_RING_BASE >> 32) as u32);

    // Start controller + ring doorbell0 once to kick processing.
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);

    // Process one additional chunk of commands so we snapshot mid-flight.
    let _ = xhci.mmio_read(&mut mem, regs::REG_USBCMD, 4);
    xhci.service_event_ring(&mut mem);

    let produced_before = count_completion_events(&mut mem, EVENT_RING_BASE, CMD_COUNT);
    assert!(
        (1..CMD_COUNT).contains(&produced_before),
        "expected to snapshot mid-flight (produced_before={})",
        produced_before
    );

    let bytes = xhci.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    // Continue without ringing doorbell0 again. If `cmd_kick` didn't roundtrip, progress would
    // stall here.
    for _ in 0..128 {
        let _ = restored.mmio_read(&mut mem, regs::REG_USBSTS, 4);
        restored.service_event_ring(&mut mem);
        if count_completion_events(&mut mem, EVENT_RING_BASE, CMD_COUNT) == CMD_COUNT {
            break;
        }
    }

    assert_eq!(
        count_completion_events(&mut mem, EVENT_RING_BASE, CMD_COUNT),
        CMD_COUNT,
        "expected all command completions to be delivered after restore"
    );

    for i in 0..CMD_COUNT {
        let ev = Trb::read_from(&mut mem, EVENT_RING_BASE + (i as u64) * (TRB_LEN as u64));
        assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
        assert_eq!(ev.parameter, CMD_RING_BASE + (i as u64) * (TRB_LEN as u64));
        assert_eq!(ev.completion_code_raw(), 1);
        assert_eq!(ev.slot_id(), 0);
    }
}

