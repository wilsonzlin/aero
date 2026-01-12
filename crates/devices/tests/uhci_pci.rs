use aero_devices::pci::{PciBarDefinition, PciDevice};
use aero_devices::usb::uhci::{register_uhci_io_ports, regs, SharedUhciPciDevice, UhciPciDevice};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::io::IoPortBus;
use aero_platform::memory::MemoryBus;
use memory::{GuestMemory, GuestMemoryError, GuestMemoryResult};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn uhci_pci_config_and_bar_io() {
    let uhci: SharedUhciPciDevice = Rc::new(RefCell::new(UhciPciDevice::default()));

    // Validate config-space identity and BAR definition.
    {
        let mut dev = uhci.borrow_mut();
        let cfg = dev.config_mut();

        let id = cfg.vendor_device_id();
        assert_eq!(id.vendor_id, 0x8086);
        assert_eq!(id.device_id, 0x7020);

        let class = cfg.class_code();
        assert_eq!(class.class, 0x0c);
        assert_eq!(class.subclass, 0x03);
        assert_eq!(class.prog_if, 0x00);

        assert_eq!(
            cfg.bar_definition(UhciPciDevice::IO_BAR_INDEX),
            Some(PciBarDefinition::Io {
                size: u32::from(UhciPciDevice::IO_BAR_SIZE)
            })
        );

        // Interrupt pin/line should reflect a typical PIIX3 UHCI wiring (INTA#/IRQ11).
        assert_eq!(cfg.read(0x3d, 1) as u8, 1);
        assert_eq!(cfg.read(0x3c, 1) as u8, 11);

        // Program BAR4 and enable I/O decoding.
        cfg.set_bar_base(UhciPciDevice::IO_BAR_INDEX, 0x1000);
        cfg.set_command(0x0001);
    }

    let mut io = IoPortBus::new();
    register_uhci_io_ports(&mut io, uhci.clone());

    let base = 0x1000;

    // Default SOFMOD is 64.
    assert_eq!(io.read(base + regs::REG_SOFMOD, 1) as u8, 64);

    // Writes to the UHCI I/O window must reach the underlying controller model.
    io.write(base + regs::REG_SOFMOD, 1, 12);
    assert_eq!(io.read(base + regs::REG_SOFMOD, 1) as u8, 12);

    io.write(base + regs::REG_USBINTR, 2, regs::USBINTR_IOC as u32);
    assert_eq!(
        io.read(base + regs::REG_USBINTR, 2) as u16,
        regs::USBINTR_IOC
    );

    // Confirm the controller's state changed (not just the I/O readback path).
    assert_eq!(uhci.borrow().controller().regs().usbintr, regs::USBINTR_IOC);
}

#[test]
fn uhci_irq_level_is_gated_by_pci_command_intx_disable() {
    let mut dev = UhciPciDevice::default();

    // Enable IOC interrupts and force a USBINT status bit so the controller asserts its IRQ line.
    dev.controller_mut()
        .io_write(regs::REG_USBINTR, 2, u32::from(regs::USBINTR_IOC));
    dev.controller_mut().set_usbsts_bits(regs::USBSTS_USBINT);
    dev.config_mut().set_command(0x0001);

    assert!(dev.irq_level(), "IRQ should assert when USBINTR is enabled");

    // PCI command bit 10 disables legacy INTx assertion.
    dev.config_mut().set_command(0x0001 | (1 << 10));
    assert!(
        !dev.irq_level(),
        "IRQ must be suppressed when PCI COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx without touching UHCI register state: the pending controller interrupt should
    // become visible again.
    dev.config_mut().set_command(0x0001);
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
fn uhci_tick_dma_is_gated_by_pci_bus_master_enable() {
    let chipset = ChipsetState::new(false);
    let filter = AddressFilter::new(chipset.a20());
    let ram = CountingRam::new(0x4000);
    let ram_handle = ram.clone();
    let mut mem = MemoryBus::with_ram(filter, Box::new(ram));

    let mut dev = UhciPciDevice::default();

    // Program a frame list base. The controller reads the current frame list entry on each tick.
    dev.controller_mut()
        .io_write(regs::REG_FLBASEADD, 4, 0x2000);
    mem.write_physical(0x2000, &1u32.to_le_bytes());

    // Start the controller.
    dev.controller_mut().io_write(
        regs::REG_USBCMD,
        2,
        u32::from(regs::USBCMD_RS | regs::USBCMD_MAXP),
    );

    // With bus mastering disabled, tick must not touch guest memory.
    dev.config_mut().set_command(0x0001);
    dev.controller_mut().io_write(regs::REG_FRNUM, 2, 0);
    ram_handle.clear_counts();
    dev.tick_1ms(&mut mem);
    assert_eq!(
        ram_handle.counts(),
        (0, 0),
        "UHCI should not DMA when PCI COMMAND.BUS_MASTER is clear"
    );

    // With bus mastering enabled, tick should read at least the frame list entry.
    dev.config_mut().set_command(0x0001 | (1 << 2));
    dev.controller_mut().io_write(regs::REG_FRNUM, 2, 0);
    ram_handle.clear_counts();
    dev.tick_1ms(&mut mem);
    let (reads, writes) = ram_handle.counts();
    assert!(
        reads != 0 || writes != 0,
        "UHCI should access guest memory when PCI COMMAND.BUS_MASTER is set"
    );
}

#[test]
fn uhci_pci_snapshot_roundtrip_restores_pci_and_controller_state() {
    let mut dev = UhciPciDevice::default();

    // Configure some PCI state (BAR + command bits) and drive BAR probing so we exercise the
    // internal BAR-probe bookkeeping.
    let bar_offset = 0x10 + u16::from(UhciPciDevice::IO_BAR_INDEX) * 4;
    dev.config_mut()
        .set_bar_base(UhciPciDevice::IO_BAR_INDEX, 0x1000);
    dev.config_mut().set_command(0x0001 | (1 << 2));
    dev.config_mut().write(bar_offset, 4, 0xFFFF_FFFF);

    // Configure some controller registers.
    dev.controller_mut().io_write(regs::REG_SOFMOD, 1, 12u32);
    dev.controller_mut()
        .io_write(regs::REG_USBINTR, 2, regs::USBINTR_IOC as u32);
    dev.controller_mut().io_write(
        regs::REG_USBCMD,
        2,
        u32::from(regs::USBCMD_RS | regs::USBCMD_MAXP),
    );
    dev.controller_mut().io_write(regs::REG_FRNUM, 2, 0x123u32);

    let snapshot = dev.save_state();
    assert_eq!(
        dev.save_state(),
        snapshot,
        "save_state output must be deterministic"
    );

    let mut restored = UhciPciDevice::default();
    restored
        .load_state(&snapshot)
        .expect("snapshot load should succeed");

    // Config-space bytes and BAR probe state should restore exactly.
    assert_eq!(
        dev.config().snapshot_state(),
        restored.config().snapshot_state()
    );

    // Reading the BAR should still return the size mask because BAR probing was active.
    let bar_read = restored.config_mut().read(bar_offset, 4);
    assert_eq!(bar_read, 0xFFFF_FFE1);

    // Controller register state should restore.
    let before = dev.controller().regs();
    let after = restored.controller().regs();
    assert_eq!(before.usbcmd, after.usbcmd);
    assert_eq!(before.usbsts, after.usbsts);
    assert_eq!(before.usbintr, after.usbintr);
    assert_eq!(before.usbint_causes, after.usbint_causes);
    assert_eq!(before.frnum, after.frnum);
    assert_eq!(before.flbaseadd, after.flbaseadd);
    assert_eq!(before.sofmod, after.sofmod);
    assert_eq!(dev.irq_level(), restored.irq_level());
}
