use aero_devices::pci::{PciBdf, PciConfigSpace, PciDevice, PciInterruptPin};
use aero_pc_platform::PcPlatform;
use std::cell::Cell;
use std::rc::Rc;

struct TestIntxPciDevice {
    config: PciConfigSpace,
}

impl TestIntxPciDevice {
    fn new() -> Self {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        // Non-zero class code just to look like a real endpoint.
        config.set_class_code(0x01, 0x80, 0x00, 0);
        Self { config }
    }
}

impl PciDevice for TestIntxPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

#[test]
fn pc_platform_polls_all_pci_intx_sources_even_when_hda_is_disabled() {
    // HDA is disabled by default; this test ensures INTx polling does not early-return when one
    // device integration is absent.
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    let bdf = PciBdf::new(0, 5, 0);
    let level = Rc::new(Cell::new(true));

    // Add the device to the PCI bus and configure INTx routing for it.
    let mut dev = TestIntxPciDevice::new();
    pc.pci_intx
        .configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
    pc.pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(dev));

    // Register an INTx source for the device.
    pc.register_pci_intx_source(bdf, PciInterruptPin::IntA, {
        let level = level.clone();
        move |_pc| level.get()
    });

    let expected_irq = u8::try_from(pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA)).unwrap();

    // Unmask the routed IRQ (and cascade) so we can observe INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        if expected_irq >= 8 {
            interrupts.pic_mut().set_masked(2, false);
        }
        interrupts.pic_mut().set_masked(expected_irq, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    pc.poll_pci_intx_lines();

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .unwrap_or_else(|| panic!("IRQ{expected_irq} should be pending after INTx routing"));
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, expected_irq);
}
