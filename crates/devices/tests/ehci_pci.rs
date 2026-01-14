use aero_devices::pci::{
    PciBarDefinition, PciDevice, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_devices::usb::ehci::{regs, EhciPciDevice};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::memory::MemoryBus;
use memory::{GuestMemory, GuestMemoryError, GuestMemoryResult};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn ehci_pci_config_and_bar_mmio() {
    let mut dev = EhciPciDevice::default();

    // Validate config-space identity and BAR definition.
    {
        let cfg = dev.config_mut();

        let id = cfg.vendor_device_id();
        assert_eq!(id.vendor_id, 0x8086);
        assert_eq!(id.device_id, 0x293a);

        let class = cfg.class_code();
        assert_eq!(class.class, 0x0c);
        assert_eq!(class.subclass, 0x03);
        assert_eq!(class.prog_if, 0x20);

        assert_eq!(
            cfg.bar_definition(EhciPciDevice::MMIO_BAR_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: EhciPciDevice::MMIO_BAR_SIZE,
                prefetchable: false
            })
        );

        // Interrupt pin/line should reflect a typical INTx routing for the canonical EHCI profile.
        let router = PciIntxRouter::new(PciIntxRouterConfig::default());
        let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
        let expected_gsi = router.gsi_for_intx(bdf, PciInterruptPin::IntA);
        assert_eq!(
            cfg.read(0x3d, 1) as u8,
            PciInterruptPin::IntA.to_config_u8()
        );
        assert_eq!(cfg.read(0x3c, 1) as u8, u8::try_from(expected_gsi).unwrap());
    }

    // With COMMAND.MEM disabled, MMIO reads float high.
    assert_eq!(
        memory::MmioHandler::read(&mut dev, regs::REG_USBCMD, 4) as u32,
        0xffff_ffff
    );

    // Enable MMIO decoding and confirm reads now reach the controller.
    dev.config_mut().set_command(0x0002);
    assert_ne!(
        memory::MmioHandler::read(&mut dev, regs::REG_CAPLENGTH_HCIVERSION, 4) as u32,
        0xffff_ffff
    );
}

#[test]
fn ehci_irq_level_is_gated_by_pci_command_intx_disable() {
    let mut dev = EhciPciDevice::default();

    dev.controller_mut()
        .mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    dev.controller_mut().set_usbsts_bits(regs::USBSTS_USBINT);
    dev.config_mut().set_command(0x0002);

    assert!(dev.irq_level(), "IRQ should assert when USBINTR is enabled");

    // PCI command bit 10 disables legacy INTx assertion.
    dev.config_mut().set_command(0x0002 | (1 << 10));
    assert!(
        !dev.irq_level(),
        "IRQ must be suppressed when PCI COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx without touching EHCI register state: the pending controller interrupt should
    // become visible again.
    dev.config_mut().set_command(0x0002);
    assert!(dev.irq_level());
}

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

#[test]
fn ehci_tick_dma_is_gated_by_pci_bus_master_enable() {
    let chipset = ChipsetState::new(false);
    let filter = AddressFilter::new(chipset.a20());
    let ram = CountingRam::new(0x8000);
    let ram_handle = ram.clone();
    let mut mem = MemoryBus::with_ram(filter, Box::new(ram));

    let mut dev = EhciPciDevice::default();

    // Configure a trivial periodic schedule with the first frame-list entry terminated.
    dev.controller_mut()
        .mmio_write(regs::REG_PERIODICLISTBASE, 4, 0x2000);
    mem.write_physical(0x2000, &1u32.to_le_bytes());
    dev.controller_mut()
        .mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    // With bus mastering disabled, tick must not touch guest memory.
    dev.config_mut().set_command(0x0002);
    ram_handle.clear_counts();
    dev.tick_1ms(&mut mem);
    assert_eq!(
        ram_handle.counts(),
        (0, 0),
        "EHCI should not DMA when PCI COMMAND.BUS_MASTER is clear"
    );

    // With bus mastering enabled, tick should read at least the frame list entry.
    dev.config_mut().set_command(0x0002 | (1 << 2));
    ram_handle.clear_counts();
    dev.tick_1ms(&mut mem);
    let (reads, writes) = ram_handle.counts();
    assert!(
        reads != 0 || writes != 0,
        "EHCI should access guest memory when PCI COMMAND.BUS_MASTER is set"
    );
}

#[test]
fn ehci_pci_snapshot_roundtrip_restores_pci_and_controller_state() {
    let mut dev = EhciPciDevice::default();

    // Configure some PCI state (BAR + command bits) and drive BAR probing so we exercise the
    // internal BAR-probe bookkeeping.
    let bar_offset = 0x10u16;
    dev.config_mut()
        .set_bar_base(EhciPciDevice::MMIO_BAR_INDEX, 0x1000);
    dev.config_mut().set_command(0x0002 | (1 << 2));
    dev.config_mut().write(bar_offset, 4, 0xffff_ffff);

    // Configure some controller registers.
    dev.controller_mut()
        .mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    dev.controller_mut()
        .mmio_write(regs::REG_FRINDEX, 4, 0x123u32);
    dev.controller_mut()
        .mmio_write(regs::REG_CONFIGFLAG, 4, regs::CONFIGFLAG_CF);
    dev.controller_mut()
        .mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    let snapshot = dev.save_state();
    assert_eq!(
        dev.save_state(),
        snapshot,
        "save_state output must be deterministic"
    );

    let mut restored = EhciPciDevice::default();
    restored
        .load_state(&snapshot)
        .expect("snapshot load should succeed");

    // Config-space bytes and BAR probe state should restore exactly.
    assert_eq!(
        dev.config().snapshot_state(),
        restored.config().snapshot_state()
    );

    // Reading the BAR should still return the size mask because BAR probing was active.
    assert_eq!(restored.config_mut().read(bar_offset, 4), 0xffff_f000);

    // Controller state should restore (compare controller snapshots for an easy equality check).
    assert_eq!(
        dev.controller().save_state(),
        restored.controller().save_state()
    );
}
