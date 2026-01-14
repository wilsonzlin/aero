use aero_devices::pci::{
    msi::PCI_CAP_ID_MSI, msix::PCI_CAP_ID_MSIX, MsiCapability, MsixCapability, PciBdf, PciDevice,
    PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_devices::usb::xhci::XhciPciDevice;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

struct GuestCpu {
    idt_installed: [bool; 256],
    handled_vectors: Vec<u8>,
}

impl GuestCpu {
    fn new() -> Self {
        Self {
            idt_installed: [false; 256],
            handled_vectors: Vec::new(),
        }
    }

    fn install_isr(&mut self, vector: u8) {
        self.idt_installed[vector as usize] = true;
    }

    fn service_next_interrupt(&mut self, interrupts: &mut PlatformInterrupts) {
        let Some(vector) = interrupts.get_pending() else {
            return;
        };

        interrupts.acknowledge(vector);
        assert!(self.idt_installed[vector as usize]);
        self.handled_vectors.push(vector);
        interrupts.eoi(vector);
    }
}

#[test]
fn xhci_msi_interrupt_reaches_guest_idt_vector() {
    let mut dev = XhciPciDevice::default();

    // Platform interrupt controller used as an MSI sink.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts.clone())));

    let mut cpu = GuestCpu::new();
    cpu.install_isr(0x45);

    // Program MSI config space.
    let cap_offset = dev
        .config_mut()
        .find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI)
        .unwrap() as u16;
    dev.config_mut().write(cap_offset + 0x04, 4, 0xfee0_0000);
    dev.config_mut().write(cap_offset + 0x08, 4, 0);
    dev.config_mut().write(cap_offset + 0x0c, 2, 0x0045);
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, (ctrl | 0x0001) as u32);

    dev.raise_event_interrupt();

    assert!(
        !dev.irq_level(),
        "legacy INTx must be suppressed while MSI is active"
    );

    cpu.service_next_interrupt(&mut interrupts.borrow_mut());
    assert_eq!(cpu.handled_vectors, vec![0x45]);
}

#[test]
fn xhci_msi_pending_delivers_on_unmask_even_after_interrupt_cleared() {
    let mut dev = XhciPciDevice::default();

    // Platform interrupt controller used as an MSI sink.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts.clone())));

    let mut cpu = GuestCpu::new();
    cpu.install_isr(0x45);

    // Program MSI config space.
    let cap_offset = dev.config_mut().find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
    dev.config_mut().write(cap_offset + 0x04, 4, 0xfee0_0000);
    dev.config_mut().write(cap_offset + 0x08, 4, 0);
    dev.config_mut().write(cap_offset + 0x0c, 2, 0x0045);
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, (ctrl | 0x0001) as u32);

    // Mask the vector (per-vector masking is always enabled for our MSI capability, but keep the
    // test defensive in case profiles change).
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(
        per_vector_masking,
        "test requires per-vector masking support"
    );
    let mask_off = if is_64bit {
        cap_offset + 0x10
    } else {
        cap_offset + 0x0c
    };
    dev.config_mut().write(mask_off, 4, 1);

    // Raise an interrupt while masked. The MSI capability should latch the pending bit instead of
    // delivering an interrupt.
    dev.raise_event_interrupt();
    assert_eq!(interrupts.borrow_mut().get_pending(), None);

    // Clear the interrupt condition before unmasking, so delivery relies solely on the pending bit.
    dev.clear_event_interrupt();
    assert_eq!(
        dev.config()
            .capability::<MsiCapability>()
            .unwrap()
            .pending_bits()
            & 1,
        1
    );

    // Unmask the vector and service interrupts again without reasserting the interrupt condition.
    dev.config_mut().write(mask_off, 4, 0);
    dev.clear_event_interrupt();

    cpu.service_next_interrupt(&mut interrupts.borrow_mut());
    assert_eq!(cpu.handled_vectors, vec![0x45]);
    assert_eq!(
        dev.config()
            .capability::<MsiCapability>()
            .unwrap()
            .pending_bits()
            & 1,
        0
    );
}

#[test]
fn xhci_msi_unprogrammed_address_sets_pending_and_delivers_after_programming() {
    let mut dev = XhciPciDevice::default();

    // Platform interrupt controller used as an MSI sink.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts.clone())));

    let mut cpu = GuestCpu::new();
    cpu.install_isr(0x45);

    // Enable MSI before programming the message address. Real guests should program the address
    // first, but device models should be robust against mis-ordered configuration.
    let cap_offset = dev.config_mut().find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(
        per_vector_masking,
        "test requires per-vector masking/pending support"
    );

    let data_off = if is_64bit {
        cap_offset + 0x0c
    } else {
        cap_offset + 0x08
    };
    dev.config_mut().write(data_off, 2, 0x0045);
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));

    // Triggering while the MSI message address is unprogrammed should not deliver an interrupt, but
    // should latch the MSI pending bit (when supported).
    dev.raise_event_interrupt();
    assert!(
        !dev.irq_level(),
        "legacy INTx must be suppressed while MSI is active"
    );
    assert_eq!(
        interrupts.borrow_mut().get_pending(),
        None,
        "unprogrammed MSI address must not inject an interrupt"
    );
    assert_eq!(
        dev.config()
            .capability::<MsiCapability>()
            .unwrap()
            .pending_bits()
            & 1,
        1,
        "device should latch MSI pending when message address is invalid"
    );

    // Clear the interrupt condition so delivery relies solely on the latched pending bit.
    dev.clear_event_interrupt();

    // Program a valid MSI address and service interrupts again without reasserting the interrupt
    // condition. Pending delivery should now occur.
    dev.config_mut().write(cap_offset + 0x04, 4, 0xfee0_0000);
    if is_64bit {
        dev.config_mut().write(cap_offset + 0x08, 4, 0);
    }
    dev.clear_event_interrupt();

    cpu.service_next_interrupt(&mut interrupts.borrow_mut());
    assert_eq!(cpu.handled_vectors, vec![0x45]);
    assert_eq!(
        dev.config()
            .capability::<MsiCapability>()
            .unwrap()
            .pending_bits()
            & 1,
        0,
        "pending bit should clear after delivery"
    );
}

#[test]
fn xhci_msix_interrupt_reaches_guest_idt_vector() {
    let mut dev = XhciPciDevice::default();

    // Platform interrupt controller used as an MSI sink.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts.clone())));

    let mut cpu = GuestCpu::new();
    cpu.install_isr(0x45);

    // Program BAR0 base and enable MEM decoding so the MSI-X table/PBA region is accessible.
    dev.config_mut()
        .set_bar_base(XhciPciDevice::MMIO_BAR_INDEX, 0x1000_0000);
    dev.config_mut().set_command(0x2);

    // Enable MSI-X in config space.
    let cap_offset = dev.config_mut().find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));

    // Program MSI-X table entry 0 via BAR0 MMIO.
    let table_base = u64::from(
        dev.config_mut()
            .capability::<MsixCapability>()
            .unwrap()
            .table_offset(),
    );
    MmioHandler::write(&mut dev, table_base, 4, 0xfee0_0000);
    MmioHandler::write(&mut dev, table_base + 0x04, 4, 0);
    MmioHandler::write(&mut dev, table_base + 0x08, 4, 0x0045);
    MmioHandler::write(&mut dev, table_base + 0x0c, 4, 0); // unmasked

    dev.raise_event_interrupt();

    assert!(
        !dev.irq_level(),
        "legacy INTx must be suppressed while MSI-X is active"
    );

    cpu.service_next_interrupt(&mut interrupts.borrow_mut());
    assert_eq!(cpu.handled_vectors, vec![0x45]);
}

#[test]
fn xhci_msix_unprogrammed_address_sets_pending_and_delivers_after_programming() {
    let mut dev = XhciPciDevice::default();

    // Platform interrupt controller used as an MSI sink.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts.clone())));

    let mut cpu = GuestCpu::new();
    cpu.install_isr(0x46);

    // Program BAR0 base and enable MEM decoding so the MSI-X table/PBA region is accessible.
    dev.config_mut()
        .set_bar_base(XhciPciDevice::MMIO_BAR_INDEX, 0x1000_0000);
    dev.config_mut().set_command(0x2);

    // Enable MSI-X in config space.
    let cap_offset = dev.config_mut().find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));

    let (table_base, pba_base) = {
        let msix = dev
            .config_mut()
            .capability::<MsixCapability>()
            .expect("xHCI should expose MSI-X capability");
        (u64::from(msix.table_offset()), u64::from(msix.pba_offset()))
    };

    // Program MSI-X table entry 0 with an invalid address (unprogrammed), but with a valid vector.
    MmioHandler::write(&mut dev, table_base, 4, 0);
    MmioHandler::write(&mut dev, table_base + 0x04, 4, 0);
    MmioHandler::write(&mut dev, table_base + 0x08, 4, 0x0046);
    MmioHandler::write(&mut dev, table_base + 0x0c, 4, 0); // unmasked

    dev.raise_event_interrupt();

    assert!(
        !dev.irq_level(),
        "legacy INTx must be suppressed while MSI-X is active"
    );

    assert_eq!(
        interrupts.borrow_mut().get_pending(),
        None,
        "unprogrammed MSI-X address must not inject an interrupt"
    );

    let msix = dev.config().capability::<MsixCapability>().unwrap();
    assert_eq!(
        msix.snapshot_pba().first().copied().unwrap_or(0) & 1,
        1,
        "device should latch MSI-X PBA bit 0 when the table entry address is invalid"
    );

    // Clear the interrupt condition so delivery relies solely on the pending bit.
    dev.clear_event_interrupt();

    // Program a valid MSI-X address. The xHCI wrapper services pending MSI-X vectors when the
    // guest writes the table, so delivery should occur without reasserting the interrupt
    // condition.
    MmioHandler::write(&mut dev, table_base, 4, 0xfee0_0000);

    cpu.service_next_interrupt(&mut interrupts.borrow_mut());
    assert_eq!(cpu.handled_vectors, vec![0x46]);

    let msix = dev.config().capability::<MsixCapability>().unwrap();
    assert_eq!(
        msix.snapshot_pba().first().copied().unwrap_or(0) & 1,
        0,
        "PBA bit should clear after pending MSI-X delivery"
    );

    // Also assert the guest can observe the pending bit via BAR0 PBA MMIO reads.
    assert_eq!(
        MmioHandler::read(&mut dev, pba_base, 8) & 1,
        0,
        "PBA MMIO should reflect the cleared pending bit"
    );
}

#[test]
fn xhci_intx_fallback_routes_through_pci_intx_router() {
    let mut dev = XhciPciDevice::default();
    let bdf = PciBdf::new(0, 0, 0);
    let pin = PciInterruptPin::IntA;

    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    let mut intx_router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let gsi = intx_router.gsi_for_intx(bdf, pin);

    let vector = 0x46u8;
    // PCI INTx is active-low + level-triggered.
    let low = u32::from(vector) | (1 << 13) | (1 << 15);
    program_ioapic_entry(&mut interrupts, gsi, low, 0);

    dev.raise_event_interrupt();
    assert!(
        dev.irq_level(),
        "device should assert INTx when MSI is disabled"
    );
    intx_router.assert_intx(bdf, pin, &mut interrupts);
    assert_eq!(interrupts.get_pending(), Some(vector));

    interrupts.acknowledge(vector);
    dev.clear_event_interrupt();
    intx_router.deassert_intx(bdf, pin, &mut interrupts);
    interrupts.eoi(vector);
    assert_eq!(interrupts.get_pending(), None);
}

#[test]
fn xhci_irq_level_is_gated_by_pci_command_intx_disable() {
    let mut dev = XhciPciDevice::default();

    dev.raise_event_interrupt();
    assert!(
        dev.irq_level(),
        "xHCI should assert legacy INTx when MSI is disabled"
    );

    // PCI command bit 10 disables legacy INTx assertion.
    dev.config_mut().set_command(1 << 10);
    assert!(
        !dev.irq_level(),
        "IRQ must be suppressed when PCI COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx without touching the pending interrupt state: the asserted interrupt should
    // become visible again.
    dev.config_mut().set_command(0);
    assert!(dev.irq_level());

    dev.clear_event_interrupt();
    assert!(!dev.irq_level());
}

#[test]
fn xhci_msix_snapshot_roundtrip_preserves_table_and_pba() {
    struct NoopMsiSink;
    impl MsiTrigger for NoopMsiSink {
        fn trigger_msi(&mut self, _message: MsiMessage) {}
    }

    let mut dev = XhciPciDevice::default();
    dev.set_msi_target(Some(Box::new(NoopMsiSink)));

    // Make BAR0 MMIO accessible so we can program the MSI-X table via guest-style MMIO writes.
    dev.config_mut()
        .set_bar_base(XhciPciDevice::MMIO_BAR_INDEX, 0x1000_0000);
    dev.config_mut().set_command(0x2);

    // Enable MSI-X in config space.
    let cap_offset = dev.config_mut().find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));

    let table_base = u64::from(
        dev.config_mut()
            .capability::<MsixCapability>()
            .unwrap()
            .table_offset(),
    );

    // Program entry 0 but keep it masked so the device sets PBA[0] when an interrupt fires.
    MmioHandler::write(&mut dev, table_base, 4, 0xfee0_0000);
    MmioHandler::write(&mut dev, table_base + 0x04, 4, 0);
    MmioHandler::write(&mut dev, table_base + 0x08, 4, 0x0045);
    MmioHandler::write(&mut dev, table_base + 0x0c, 4, 1); // masked

    dev.raise_event_interrupt();
    dev.clear_event_interrupt();

    let msix = dev.config().capability::<MsixCapability>().unwrap();
    let table_before = msix.snapshot_table().to_vec();
    let pba_before = msix.snapshot_pba().to_vec();
    assert_eq!(
        pba_before.first().copied().unwrap_or(0) & 1,
        1,
        "masked MSI-X delivery should set PBA bit 0"
    );

    let snapshot = dev.save_state();

    let mut restored = XhciPciDevice::default();
    restored.load_state(&snapshot).unwrap();

    let msix_restored = restored.config().capability::<MsixCapability>().unwrap();
    assert_eq!(msix_restored.snapshot_table(), table_before.as_slice());
    assert_eq!(msix_restored.snapshot_pba(), pba_before.as_slice());
}
