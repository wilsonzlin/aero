use aero_devices::pci::{MsiCapability, PciConfigSpace};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

struct TestPciDevice {
    config: PciConfigSpace,
    legacy_intx_asserts: u64,
}

impl TestPciDevice {
    fn new() -> Self {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.add_capability(Box::new(MsiCapability::new()));
        Self {
            config,
            legacy_intx_asserts: 0,
        }
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn raise_interrupt(&mut self, interrupts: &mut PlatformInterrupts) -> bool {
        if let Some(msi) = self.config.capability_mut::<MsiCapability>() {
            if msi.enabled() {
                return msi.trigger(interrupts);
            }
        }

        self.legacy_intx_asserts += 1;
        false
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
    let mut device = TestPciDevice::new();
    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

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

    assert!(device.raise_interrupt(&mut interrupts));
    assert_eq!(device.legacy_intx_asserts, 0);

    cpu.service_next_interrupt(&mut interrupts);
    assert_eq!(cpu.handled_vectors, vec![0x45]);
}
