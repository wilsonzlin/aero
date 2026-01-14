use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::{MsixCapability, PciDevice};
use aero_devices::usb::xhci::XhciPciDevice;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

fn program_msi(dev: &mut XhciPciDevice, vector: u8) {
    let cap_offset = dev
        .config_mut()
        .find_capability(PCI_CAP_ID_MSI)
        .expect("MSI capability") as u16;

    dev.config_mut().write(cap_offset + 0x04, 4, 0xfee0_0000);
    dev.config_mut().write(cap_offset + 0x08, 4, 0);
    dev.config_mut()
        .write(cap_offset + 0x0c, 2, u32::from(vector));

    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | 0x0001));
}

fn program_msix_table_entry0(dev: &mut XhciPciDevice, address: u64, vector: u8, masked: bool) {
    let msix = dev
        .config()
        .capability::<MsixCapability>()
        .expect("MSI-X capability");
    let table_base = u64::from(msix.table_offset());

    MmioHandler::write(dev, table_base + 0x0, 4, u64::from(address as u32));
    MmioHandler::write(dev, table_base + 0x4, 4, u64::from((address >> 32) as u32));
    MmioHandler::write(dev, table_base + 0x8, 4, u64::from(u32::from(vector)));
    MmioHandler::write(dev, table_base + 0xc, 4, if masked { 1 } else { 0 });
}

fn enable_msix(dev: &mut XhciPciDevice) {
    let cap_offset = dev
        .config_mut()
        .find_capability(PCI_CAP_ID_MSIX)
        .expect("MSI-X capability") as u16;
    let ctrl = dev.config_mut().read(cap_offset + 0x02, 2) as u16;
    dev.config_mut()
        .write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));
}

#[test]
fn xhci_msix_interrupt_reaches_guest_and_suppresses_intx_and_msi() {
    let mut dev = XhciPciDevice::default();

    // Allow guest accesses to BAR0 (including the MSI-X table/PBA) via COMMAND.MEM.
    dev.config_mut().set_command(1 << 1);

    // Platform interrupt controller used as an MSI sink.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts.clone())));

    // Enable both MSI and MSI-X. The device should prefer MSI-X and suppress MSI/INTx so the guest
    // doesn't receive interrupts twice.
    program_msi(&mut dev, 0x46);

    // Program MSI-X table entry 0.
    program_msix_table_entry0(&mut dev, 0xfee0_0000, 0x45, false);
    enable_msix(&mut dev);

    dev.raise_event_interrupt();

    assert!(
        !dev.irq_level(),
        "legacy INTx must be suppressed while MSI-X is active"
    );

    let mut ints = interrupts.borrow_mut();
    assert_eq!(ints.get_pending(), Some(0x45), "MSI-X vector should win");
    ints.acknowledge(0x45);
    ints.eoi(0x45);

    // If the MSI path also fired we would observe a second pending vector.
    assert_eq!(
        ints.get_pending(),
        None,
        "interrupt should not be delivered twice"
    );

    // MSI/MSI-X delivery is edge-triggered. If the interrupt condition remains asserted (we did not
    // clear it above), calling `raise_event_interrupt` again must not re-deliver.
    drop(ints);
    dev.raise_event_interrupt();
    assert_eq!(
        interrupts.borrow_mut().get_pending(),
        None,
        "MSI-X should not retrigger without a new rising edge"
    );
}

#[test]
fn xhci_msix_masked_vector_sets_pba_and_delivers_on_unmask() {
    let mut dev = XhciPciDevice::default();
    dev.config_mut().set_command(1 << 1);

    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts.clone())));

    // Program a masked MSI-X table entry and enable MSI-X.
    program_msix_table_entry0(&mut dev, 0xfee0_0000, 0x45, true);
    enable_msix(&mut dev);

    let (table_base, pba_base) = {
        let msix = dev
            .config()
            .capability::<MsixCapability>()
            .expect("MSI-X capability");
        (u64::from(msix.table_offset()), u64::from(msix.pba_offset()))
    };

    // Trigger: masked entry should set PBA[0] instead of delivering.
    dev.raise_event_interrupt();
    assert!(
        !dev.irq_level(),
        "legacy INTx must be suppressed while MSI-X is active"
    );
    assert_eq!(
        interrupts.borrow_mut().get_pending(),
        None,
        "masked MSI-X entry must not deliver immediately"
    );
    let pba = MmioHandler::read(&mut dev, pba_base, 8);
    assert_eq!(pba & 1, 1, "masked trigger should set PBA pending bit");

    // Unmask the vector. The xHCI interrupt condition is still asserted, so this MMIO write should
    // cause `service_interrupts()` to retry delivery and clear the pending bit.
    MmioHandler::write(&mut dev, table_base + 0x0c, 4, 0);
    let mut ints = interrupts.borrow_mut();
    assert_eq!(ints.get_pending(), Some(0x45));
    ints.acknowledge(0x45);
    ints.eoi(0x45);
    assert_eq!(ints.get_pending(), None);

    let pba_after = MmioHandler::read(&mut dev, pba_base, 8);
    assert_eq!(pba_after & 1, 0, "PBA pending bit should clear after delivery");
}

#[test]
fn xhci_snapshot_roundtrip_preserves_msix_table_and_pba() {
    let mut dev = XhciPciDevice::default();
    dev.config_mut().set_command(1 << 1);

    // Provide an MSI sink so `raise_event_interrupt` exercises the MSI-X delivery path and can set
    // the PBA pending bit when the vector is masked.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    dev.set_msi_target(Some(Box::new(interrupts)));

    // Program a masked MSI-X table entry and enable MSI-X.
    program_msix_table_entry0(&mut dev, 0xfee0_0000, 0x45, true);
    enable_msix(&mut dev);

    // Trigger: masked entry should set PBA[0] instead of delivering.
    dev.raise_event_interrupt();
    let msix = dev
        .config()
        .capability::<MsixCapability>()
        .expect("MSI-X capability");
    let pba_base = u64::from(msix.pba_offset());
    let pba = MmioHandler::read(&mut dev, pba_base, 8);
    assert_eq!(
        pba & 1,
        1,
        "masked MSI-X trigger should set PBA pending bit"
    );

    let snapshot = dev.save_state();

    let mut restored = XhciPciDevice::default();
    restored.load_state(&snapshot).expect("load snapshot");

    let msix = restored
        .config()
        .capability::<MsixCapability>()
        .expect("MSI-X capability");
    assert!(msix.enabled(), "MSI-X enable bit should survive snapshot");

    let table_base = u64::from(msix.table_offset());
    assert_eq!(
        MmioHandler::read(&mut restored, table_base + 0x0, 4) as u32,
        0xfee0_0000
    );
    assert_eq!(
        MmioHandler::read(&mut restored, table_base + 0x4, 4) as u32,
        0
    );
    assert_eq!(
        MmioHandler::read(&mut restored, table_base + 0x8, 4) as u32,
        0x45
    );
    assert_eq!(
        MmioHandler::read(&mut restored, table_base + 0xc, 4) as u32,
        1,
        "vector control mask bit should survive snapshot"
    );

    let pba_after = MmioHandler::read(&mut restored, pba_base, 8);
    assert_eq!(pba_after & 1, 1, "PBA pending bit should survive snapshot");
}
