use aero_devices::pci::{
    MsiCapability, PciBdf, PciConfigSpace, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, PlatformInterrupts,
};

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

struct TestPciDevice {
    bdf: PciBdf,
    intx_pin: PciInterruptPin,
    config: PciConfigSpace,
}

impl TestPciDevice {
    fn new(bdf: PciBdf, intx_pin: PciInterruptPin) -> Self {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));
        Self {
            bdf,
            intx_pin,
            config,
        }
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn raise_interrupt(
        &mut self,
        interrupts: &mut PlatformInterrupts,
        intx_router: &mut PciIntxRouter,
    ) -> bool {
        if let Some(msi) = self.config.capability_mut::<MsiCapability>() {
            if msi.enabled() {
                return msi.trigger(interrupts);
            }
        }

        intx_router.assert_intx(self.bdf, self.intx_pin, interrupts);
        false
    }

    fn clear_intx(&mut self, interrupts: &mut PlatformInterrupts, intx_router: &mut PciIntxRouter) {
        intx_router.deassert_intx(self.bdf, self.intx_pin, interrupts);
    }
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
fn msi_interrupt_reaches_guest_idt_vector() {
    let mut device = TestPciDevice::new(PciBdf::new(0, 0, 0), PciInterruptPin::IntA);
    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);
    let mut intx_router = PciIntxRouter::new(PciIntxRouterConfig::default());

    let mut cpu = GuestCpu::new();
    cpu.install_isr(0x45);

    let cap_offset = device
        .config_mut()
        .find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI)
        .unwrap() as u16;

    device.config_mut().write(cap_offset + 0x04, 4, 0xfee0_0000);
    device.config_mut().write(cap_offset + 0x08, 4, 0);
    device.config_mut().write(cap_offset + 0x0c, 2, 0x0045);
    let ctrl = device.config_mut().read(cap_offset + 0x02, 2) as u16;
    device
        .config_mut()
        .write(cap_offset + 0x02, 2, (ctrl | 0x0001) as u32);

    assert!(device.raise_interrupt(&mut interrupts, &mut intx_router));

    cpu.service_next_interrupt(&mut interrupts);
    assert_eq!(cpu.handled_vectors, vec![0x45]);
}

#[test]
fn intx_fallback_routes_through_pci_intx_router() {
    let mut device = TestPciDevice::new(PciBdf::new(0, 0, 0), PciInterruptPin::IntA);
    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    let mut intx_router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let gsi = intx_router.gsi_for_intx(device.bdf, device.intx_pin);

    let vector = 0x45u8;
    // PCI INTx is active-low + level-triggered.
    let low = u32::from(vector) | (1 << 13) | (1 << 15);
    program_ioapic_entry(&mut interrupts, gsi, low, 0);

    assert!(!device.raise_interrupt(&mut interrupts, &mut intx_router));
    assert_eq!(interrupts.get_pending(), Some(vector));

    interrupts.acknowledge(vector);
    device.clear_intx(&mut interrupts, &mut intx_router);
    interrupts.eoi(vector);
    assert_eq!(interrupts.get_pending(), None);
}
