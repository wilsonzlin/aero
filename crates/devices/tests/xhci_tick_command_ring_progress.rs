use aero_devices::pci::PciDevice;
use aero_devices::usb::xhci::{regs, XhciPciDevice};
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::memory::MemoryBus as PlatformMemoryBus;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use memory::MmioHandler;
use memory::MemoryBus as _;
use std::cell::RefCell;
use std::rc::Rc;

fn write_erst_entry(mem: &mut PlatformMemoryBus, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    mem.write_u64(erstba, seg_base);
    mem.write_u32(erstba + 8, seg_size_trbs);
    mem.write_u32(erstba + 12, 0);
}

fn write_trb(mem: &mut PlatformMemoryBus, paddr: u64, trb: &Trb) {
    mem.write_physical(paddr, &trb.to_bytes());
}

fn read_trb(mem: &mut PlatformMemoryBus, paddr: u64) -> Trb {
    let mut buf = [0u8; TRB_LEN];
    mem.read_physical(paddr, &mut buf);
    Trb::from_bytes(buf)
}

#[test]
fn xhci_tick_processes_command_ring_without_additional_mmio() {
    // Regression test for the native xHCI PCI wrapper tick path: once doorbell 0 is rung, command
    // ring processing must continue across subsequent `tick_1ms` calls even if the guest does not
    // perform additional MMIO.
    let chipset = ChipsetState::new(true);
    let filter = AddressFilter::new(chipset.a20());
    let mem: Rc<RefCell<PlatformMemoryBus>> = Rc::new(RefCell::new(PlatformMemoryBus::new(
        filter, 0x100_000,
    )));

    let mut dev = XhciPciDevice::default();
    dev.set_dma_memory_bus(Some(mem.clone()));

    // Enable MMIO + DMA (Bus Master Enable).
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    // Command ring base (CRCR bits 63:6), 64-byte aligned.
    const CMD_RING_BASE: u64 = 0x1000;
    // Guest event ring (single segment).
    const ERST_BASE: u64 = 0x4000;
    const EVENT_RING_BASE: u64 = 0x5000;
    const EVENT_RING_TRBS: u32 = 256;

    const COMMAND_COUNT: usize = 64;

    {
        let mut mem = mem.borrow_mut();
        write_erst_entry(&mut mem, ERST_BASE, EVENT_RING_BASE, EVENT_RING_TRBS);

        for i in 0..COMMAND_COUNT {
            let mut trb = Trb::new(0, 0, 0);
            trb.set_trb_type(TrbType::NoOpCommand);
            trb.set_cycle(true);
            write_trb(
                &mut mem,
                CMD_RING_BASE + (i as u64) * (TRB_LEN as u64),
                &trb,
            );
        }

        // Stop marker: cycle mismatch so the ring looks empty after the command sequence.
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.set_cycle(false);
        write_trb(
            &mut mem,
            CMD_RING_BASE + (COMMAND_COUNT as u64) * (TRB_LEN as u64),
            &stop,
        );
    }

    // Configure interrupter 0 event ring so command completion events are written to guest RAM.
    MmioHandler::write(&mut dev, regs::REG_INTR0_ERSTSZ, 4, 1);
    MmioHandler::write(&mut dev, regs::REG_INTR0_ERSTBA_LO, 4, ERST_BASE);
    MmioHandler::write(&mut dev, regs::REG_INTR0_ERSTBA_HI, 4, 0);
    MmioHandler::write(&mut dev, regs::REG_INTR0_ERDP_LO, 4, EVENT_RING_BASE);
    MmioHandler::write(&mut dev, regs::REG_INTR0_ERDP_HI, 4, 0);
    MmioHandler::write(&mut dev, regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IE));

    // Program CRCR and start the controller.
    MmioHandler::write(&mut dev, regs::REG_CRCR_LO, 4, CMD_RING_BASE | 1);
    MmioHandler::write(&mut dev, regs::REG_CRCR_HI, 4, 0);
    MmioHandler::write(
        &mut dev,
        regs::REG_USBCMD,
        4,
        u64::from(regs::USBCMD_RUN),
    );

    // Ring doorbell 0 (Command Ring). The controller processes a bounded number of TRBs per MMIO.
    MmioHandler::write(&mut dev, u64::from(regs::DBOFF_VALUE), 4, 0);

    // Tick enough times to ensure the remainder of the command ring is processed.
    {
        let mut mem = mem.borrow_mut();
        for _ in 0..32 {
            dev.tick_1ms(&mut mem);
        }
    }

    // Assert all command completion events were written to the guest event ring.
    {
        let mut mem = mem.borrow_mut();
        for i in 0..COMMAND_COUNT {
            let evt = read_trb(
                &mut mem,
                EVENT_RING_BASE + (i as u64) * (TRB_LEN as u64),
            );
            assert_eq!(
                evt.trb_type(),
                TrbType::CommandCompletionEvent,
                "event {i} should be a Command Completion Event TRB"
            );
            assert_eq!(
                evt.completion_code_raw(),
                CompletionCode::Success.as_u8(),
                "event {i} completion code should be Success"
            );
            assert_eq!(
                evt.parameter & !0x0f,
                CMD_RING_BASE + (i as u64) * (TRB_LEN as u64),
                "event {i} should point at the completed command TRB"
            );
        }
    }
}
