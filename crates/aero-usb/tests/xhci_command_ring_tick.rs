mod util;

use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

use util::{Alloc, TestMemory};

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn doorbell0_command_ring_continues_processing_on_tick() {
    // Ensure command ring processing continues across controller ticks even if the guest does not
    // perform additional MMIO after ringing doorbell 0.
    let mut mem = TestMemory::new(0x100_000);
    let mut alloc = Alloc::new(0x1000);

    // Command ring base (CRCR bits 63:6), 64-byte aligned.
    let cmd_ring = alloc.alloc(0x2000, 0x40);

    // Guest event ring (single segment).
    let erstba = alloc.alloc(0x20, 0x40);
    let event_ring = alloc.alloc(256 * (TRB_LEN as u32), 0x10);
    write_erst_entry(&mut mem, erstba as u64, event_ring as u64, 256);

    let mut xhci = XhciController::new();

    // Configure event ring on interrupter 0 so command completion events are written to guest RAM.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, event_ring);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Program CRCR and start the controller.
    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, cmd_ring | 1);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    const COMMAND_COUNT: usize = 128;

    for i in 0..COMMAND_COUNT {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring as u64 + (i as u64) * (TRB_LEN as u64));
    }

    // Stop marker: cycle mismatch so the ring looks empty after the command sequence.
    let mut stop = Trb::new(0, 0, 0);
    stop.set_trb_type(TrbType::NoOpCommand);
    stop.set_cycle(false);
    stop.write_to(
        &mut mem,
        cmd_ring as u64 + (COMMAND_COUNT as u64) * (TRB_LEN as u64),
    );

    // Ring doorbell 0 (Command Ring). The controller processes a bounded number of TRBs per MMIO.
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);

    // Tick enough times to ensure the remainder of the command ring is processed.
    for _ in 0..32 {
        xhci.tick_1ms_and_service_event_ring(&mut mem);
    }
    xhci.service_event_ring(&mut mem);

    for i in 0..COMMAND_COUNT {
        let evt = Trb::read_from(&mut mem, event_ring as u64 + (i as u64) * (TRB_LEN as u64));
        assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
        assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
        assert_eq!(
            evt.parameter & !0x0f,
            cmd_ring as u64 + (i as u64) * (TRB_LEN as u64)
        );
    }
}

