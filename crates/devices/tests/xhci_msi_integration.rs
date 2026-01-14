use aero_io_snapshot::io::state::IoSnapshot;
use aero_devices::pci::{
    msix::PCI_CAP_ID_MSIX, MsixCapability, PciBdf, PciDevice, PciInterruptPin, PciIntxRouter,
    PciIntxRouterConfig,
};
use aero_devices::usb::xhci::XhciPciDevice;
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};
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
    let cap_offset = dev
        .config_mut()
        .find_capability(PCI_CAP_ID_MSIX)
        .unwrap() as u16;
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
    MmioHandler::write(&mut dev, table_base + 0x00, 4, 0xfee0_0000);
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
    assert!(dev.irq_level(), "device should assert INTx when MSI is disabled");
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
    assert!(dev.irq_level(), "xHCI should assert legacy INTx when MSI is disabled");

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
    #[derive(Default)]
    struct NoopMsiSink;
    impl MsiTrigger for NoopMsiSink {
        fn trigger_msi(&mut self, _message: MsiMessage) {}
    }

    let mut dev = XhciPciDevice::default();
    dev.set_msi_target(Some(Box::new(NoopMsiSink::default())));

    // Make BAR0 MMIO accessible so we can program the MSI-X table via guest-style MMIO writes.
    dev.config_mut()
        .set_bar_base(XhciPciDevice::MMIO_BAR_INDEX, 0x1000_0000);
    dev.config_mut().set_command(0x2);

    // Enable MSI-X in config space.
    let cap_offset = dev
        .config_mut()
        .find_capability(PCI_CAP_ID_MSIX)
        .unwrap() as u16;
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
    MmioHandler::write(&mut dev, table_base + 0x00, 4, 0xfee0_0000);
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
