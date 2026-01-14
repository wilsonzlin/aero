use aero_devices::pci::PciDevice;
use aero_devices::usb::xhci::{regs, XhciPciDevice};
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::memory::MemoryBus;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use memory::{GuestMemory, GuestMemoryError, GuestMemoryResult, MmioHandler};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct CountingRam {
    inner: Rc<RefCell<Vec<u8>>>,
    reads: Rc<RefCell<u64>>,
    writes: Rc<RefCell<u64>>,
}

impl CountingRam {
    fn new(size: usize) -> Self {
        Self {
            inner: Rc::new(RefCell::new(vec![0u8; size])),
            reads: Rc::new(RefCell::new(0)),
            writes: Rc::new(RefCell::new(0)),
        }
    }

    fn clear_counts(&self) {
        *self.reads.borrow_mut() = 0;
        *self.writes.borrow_mut() = 0;
    }

    fn counts(&self) -> (u64, u64) {
        (*self.reads.borrow(), *self.writes.borrow())
    }

    fn range(&self, paddr: u64, len: usize) -> GuestMemoryResult<std::ops::Range<usize>> {
        let size = self.size();
        let end = paddr
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
        if end > size {
            return Err(GuestMemoryError::OutOfRange { paddr, len, size });
        }
        let start = usize::try_from(paddr).map_err(|_| GuestMemoryError::OutOfRange {
            paddr,
            len,
            size,
        })?;
        let end =
            start
                .checked_add(len)
                .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
        Ok(start..end)
    }
}

impl GuestMemory for CountingRam {
    fn size(&self) -> u64 {
        self.inner.borrow().len() as u64
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        *self.reads.borrow_mut() += dst.len() as u64;
        let range = self.range(paddr, dst.len())?;
        dst.copy_from_slice(&self.inner.borrow()[range]);
        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        *self.writes.borrow_mut() += src.len() as u64;
        let range = self.range(paddr, src.len())?;
        self.inner.borrow_mut()[range].copy_from_slice(src);
        Ok(())
    }
}

fn write_erst_entry(mem: &mut MemoryBus, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    mem.write_physical(erstba, &seg_base.to_le_bytes());
    mem.write_physical(erstba + 8, &seg_size_trbs.to_le_bytes());
    mem.write_physical(erstba + 12, &0u32.to_le_bytes());
}

fn write_trb(mem: &mut MemoryBus, addr: u64, trb: Trb) {
    mem.write_physical(addr, &trb.to_bytes());
}

fn read_trb(mem: &mut MemoryBus, addr: u64) -> Trb {
    let mut bytes = [0u8; TRB_LEN];
    mem.read_physical(addr, &mut bytes);
    Trb::from_bytes(bytes)
}

fn new_xhci_with_shared_memory(ram_size: usize) -> (XhciPciDevice, MemoryBus, CountingRam) {
    let chipset = ChipsetState::new(false);
    let filter = AddressFilter::new(chipset.a20());

    let ram = CountingRam::new(ram_size);
    let ram_handle = ram.clone();

    // Tick uses the platform MemoryBus.
    let mem = MemoryBus::with_ram(filter, Box::new(ram));

    // MMIO-triggered DMA uses an independent physical bus backed by the same RAM. The xHCI wrapper's
    // MMIO path uses this bus, while the tick path receives `mem` directly.
    let dma_ram = ram_handle.clone();
    let dma_bus: Rc<RefCell<dyn memory::MemoryBus>> =
        Rc::new(RefCell::new(memory::PhysicalMemoryBus::new(Box::new(dma_ram))));

    let mut dev = XhciPciDevice::default();
    dev.set_dma_memory_bus(Some(dma_bus));

    (dev, mem, ram_handle)
}

fn program_test_rings(mem: &mut MemoryBus, dev: &mut XhciPciDevice, cmd_ring: u64, erstba: u64, event_ring: u64) {
    // Guest event ring (single segment).
    write_erst_entry(mem, erstba, event_ring, 256);

    // Configure event ring on interrupter 0 so command completion events are written to guest RAM.
    MmioHandler::write(dev, regs::REG_INTR0_ERSTSZ, 4, 1);
    MmioHandler::write(dev, regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    MmioHandler::write(dev, regs::REG_INTR0_ERSTBA_HI, 4, 0);
    MmioHandler::write(dev, regs::REG_INTR0_ERDP_LO, 4, event_ring);
    MmioHandler::write(dev, regs::REG_INTR0_ERDP_HI, 4, 0);
    MmioHandler::write(dev, regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IE));

    // Program CRCR and start the controller.
    MmioHandler::write(dev, regs::REG_CRCR_LO, 4, cmd_ring | 1);
    MmioHandler::write(dev, regs::REG_CRCR_HI, 4, 0);
    MmioHandler::write(dev, regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
}

#[test]
fn doorbell0_command_ring_continues_processing_on_tick() {
    // Ensure command ring processing continues across device ticks even if the guest does not
    // perform additional MMIO after ringing doorbell 0.
    let (mut dev, mut mem, _ram) = new_xhci_with_shared_memory(0x100_000);

    // Enable MMIO decoding + bus mastering (DMA).
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    // Command ring base (CRCR bits 63:6), 64-byte aligned.
    let cmd_ring = 0x1000u64;
    let erstba = 0x4000u64;
    let event_ring = 0x5000u64;

    program_test_rings(&mut mem, &mut dev, cmd_ring, erstba, event_ring);

    const COMMAND_COUNT: usize = 128;

    for i in 0..COMMAND_COUNT {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.set_cycle(true);
        write_trb(
            &mut mem,
            cmd_ring + (i as u64) * (TRB_LEN as u64),
            trb,
        );
    }

    // Stop marker: cycle mismatch so the ring looks empty after the command sequence.
    let mut stop = Trb::new(0, 0, 0);
    stop.set_trb_type(TrbType::NoOpCommand);
    stop.set_cycle(false);
    write_trb(
        &mut mem,
        cmd_ring + (COMMAND_COUNT as u64) * (TRB_LEN as u64),
        stop,
    );

    // Ring doorbell 0 (Command Ring). The controller processes a bounded number of TRBs per MMIO.
    MmioHandler::write(&mut dev, u64::from(regs::DBOFF_VALUE), 4, 0);

    // Tick enough times to ensure the remainder of the command ring is processed.
    for _ in 0..32 {
        dev.tick_1ms(&mut mem);
    }

    for i in 0..COMMAND_COUNT {
        let evt = read_trb(&mut mem, event_ring + (i as u64) * (TRB_LEN as u64));
        assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
        assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
        assert_eq!(
            evt.parameter & !0x0f,
            cmd_ring + (i as u64) * (TRB_LEN as u64)
        );
    }
}

#[test]
fn tick_dma_and_command_ring_progress_are_gated_by_pci_bme() {
    let (mut dev, mut mem, ram) = new_xhci_with_shared_memory(0x100_000);

    // Enable MMIO decoding + bus mastering (DMA).
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    let cmd_ring = 0x1000u64;
    let erstba = 0x4000u64;
    let event_ring = 0x5000u64;

    program_test_rings(&mut mem, &mut dev, cmd_ring, erstba, event_ring);

    const COMMAND_COUNT: usize = 128;

    for i in 0..COMMAND_COUNT {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.set_cycle(true);
        write_trb(
            &mut mem,
            cmd_ring + (i as u64) * (TRB_LEN as u64),
            trb,
        );
    }

    let mut stop = Trb::new(0, 0, 0);
    stop.set_trb_type(TrbType::NoOpCommand);
    stop.set_cycle(false);
    write_trb(
        &mut mem,
        cmd_ring + (COMMAND_COUNT as u64) * (TRB_LEN as u64),
        stop,
    );

    // Ring doorbell 0 once, with DMA enabled, so the controller begins processing and leaves a kick
    // latched for subsequent ticks.
    MmioHandler::write(&mut dev, u64::from(regs::DBOFF_VALUE), 4, 0);

    // Disable bus mastering. While BME is clear, ticks must not DMA and must not advance command ring
    // processing. Tick enough times that a buggy implementation would run the command ring cursor
    // past the stop marker.
    dev.config_mut().set_command(1 << 1); // MEM only
    ram.clear_counts();
    for _ in 0..16 {
        dev.tick_1ms(&mut mem);
    }
    assert_eq!(
        ram.counts(),
        (0, 0),
        "xHCI tick must not DMA when PCI COMMAND.BUS_MASTER is clear"
    );

    // Re-enable DMA: the latched doorbell 0 kick should still be active and the remaining commands
    // should complete across ticks without requiring additional MMIO.
    dev.config_mut().set_command((1 << 1) | (1 << 2)); // MEM | BME
    for _ in 0..32 {
        dev.tick_1ms(&mut mem);
    }

    for i in 0..COMMAND_COUNT {
        let evt = read_trb(&mut mem, event_ring + (i as u64) * (TRB_LEN as u64));
        assert_eq!(evt.trb_type(), TrbType::CommandCompletionEvent);
        assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
        assert_eq!(
            evt.parameter & !0x0f,
            cmd_ring + (i as u64) * (TRB_LEN as u64)
        );
    }
}
