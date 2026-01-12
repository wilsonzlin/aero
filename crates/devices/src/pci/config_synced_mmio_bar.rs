use super::{PciBdf, PciDevice, SharedPciConfigPorts};
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

/// Generic adapter for PCI device models that maintain an internal PCI config image.
///
/// Many integrations keep a canonical PCI config space (typically [`SharedPciConfigPorts`]) that is
/// used for guest enumeration and for routing MMIO/PIO accesses to PCI BAR handlers. Some device
/// models also consult their own internal [`super::PciConfigSpace`] to gate MMIO behavior (e.g.
/// `COMMAND.MEM`) or to discover the BAR base being accessed.
///
/// When those two PCI config images diverge, device models can incorrectly treat MMIO as disabled
/// or use an incorrect BAR base. This wrapper keeps the device model's internal PCI config state
/// synchronized with the canonical config space on every MMIO access by mirroring:
/// - the PCI command register (offset `0x04`), and
/// - the base address of the accessed BAR.
///
/// This is intentionally BAR-scoped: it only syncs the BAR index passed at construction.
///
/// This replaces older one-off platform-specific wrappers (e.g. for AHCI/NVMe) with a reusable
/// generic adapter that can be applied to any `T: PciDevice + MmioHandler`.
pub struct PciConfigSyncedMmioBar<T> {
    pci_cfg: SharedPciConfigPorts,
    dev: Rc<RefCell<T>>,
    bdf: PciBdf,
    bar: u8,
}

impl<T> PciConfigSyncedMmioBar<T> {
    pub fn new(pci_cfg: SharedPciConfigPorts, dev: Rc<RefCell<T>>, bdf: PciBdf, bar: u8) -> Self {
        Self {
            pci_cfg,
            dev,
            bdf,
            bar,
        }
    }
}

impl<T: PciDevice> PciConfigSyncedMmioBar<T> {
    fn sync_pci_state(&mut self) {
        let (command, bar_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar_base = cfg
                .and_then(|cfg| cfg.bar_range(self.bar))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar_base)
        };

        let mut dev = self.dev.borrow_mut();
        dev.config_mut().set_command(command);
        if bar_base != 0 {
            dev.config_mut().set_bar_base(self.bar, bar_base);
        }
    }
}

impl<T: MmioHandler + PciDevice> MmioHandler for PciConfigSyncedMmioBar<T> {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if !(1..=8).contains(&size) {
            return all_ones(size);
        }
        self.sync_pci_state();
        MmioHandler::read(&mut *self.dev.borrow_mut(), offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if !(1..=8).contains(&size) {
            return;
        }
        self.sync_pci_state();
        MmioHandler::write(&mut *self.dev.borrow_mut(), offset, size, value);
    }
}

fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}

#[cfg(test)]
mod tests {
    use super::PciConfigSyncedMmioBar;
    use crate::pci::{PciBarDefinition, PciBdf, PciConfigPorts, PciConfigSpace, PciDevice};
    use memory::MmioHandler;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct TestPciConfigDevice {
        config: PciConfigSpace,
    }

    impl TestPciConfigDevice {
        fn new(bar: u8) -> Self {
            let mut config = PciConfigSpace::new(0x1234, 0x5678);
            config.set_bar_definition(
                bar,
                PciBarDefinition::Mmio32 {
                    size: 0x1000,
                    prefetchable: false,
                },
            );
            Self { config }
        }
    }

    impl PciDevice for TestPciConfigDevice {
        fn config(&self) -> &PciConfigSpace {
            &self.config
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.config
        }
    }

    struct TestMmioDevice {
        config: PciConfigSpace,
        bar: u8,
        last_command: u16,
        last_bar_base: u64,
    }

    impl TestMmioDevice {
        fn new(bar: u8) -> Self {
            let mut config = PciConfigSpace::new(0xabcd, 0xef01);
            config.set_bar_definition(
                bar,
                PciBarDefinition::Mmio32 {
                    size: 0x1000,
                    prefetchable: false,
                },
            );
            Self {
                config,
                bar,
                last_command: 0,
                last_bar_base: 0,
            }
        }
    }

    impl PciDevice for TestMmioDevice {
        fn config(&self) -> &PciConfigSpace {
            &self.config
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.config
        }
    }

    impl MmioHandler for TestMmioDevice {
        fn read(&mut self, _offset: u64, _size: usize) -> u64 {
            self.last_command = self.config.command();
            self.last_bar_base = self
                .config
                .bar_range(self.bar)
                .map(|range| range.base)
                .unwrap_or(0);
            0
        }

        fn write(&mut self, _offset: u64, _size: usize, _value: u64) {
            self.last_command = self.config.command();
            self.last_bar_base = self
                .config
                .bar_range(self.bar)
                .map(|range| range.base)
                .unwrap_or(0);
        }
    }

    #[test]
    fn pci_config_synced_mmio_bar_syncs_command_and_bar_base_before_each_access() {
        let bdf = PciBdf::new(0, 2, 0);
        let bar = 0;

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        pci_cfg
            .borrow_mut()
            .bus_mut()
            .add_device(bdf, Box::new(TestPciConfigDevice::new(bar)));

        let dev = Rc::new(RefCell::new(TestMmioDevice::new(bar)));
        let mut mmio = PciConfigSyncedMmioBar::new(pci_cfg.clone(), dev.clone(), bdf, bar);

        // Program canonical config space and ensure the wrapper mirrors it into the device model.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing config device");
            cfg.set_command(0x2);
            cfg.set_bar_base(bar, 0x1234_0000);
        }
        mmio.read(0, 4);
        {
            let dev = dev.borrow();
            assert_eq!(dev.config.command(), 0x2);
            assert_eq!(dev.config.bar_range(bar).unwrap().base, 0x1234_0000);
            assert_eq!(dev.last_command, 0x2);
            assert_eq!(dev.last_bar_base, 0x1234_0000);
        }

        // Update the canonical config again and ensure the wrapper re-syncs on the next access.
        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("missing config device");
            cfg.set_command(0);
            cfg.set_bar_base(bar, 0x5678_0000);
        }
        mmio.write(0x10, 4, 0);
        {
            let dev = dev.borrow();
            assert_eq!(dev.config.command(), 0);
            assert_eq!(dev.config.bar_range(bar).unwrap().base, 0x5678_0000);
            assert_eq!(dev.last_command, 0);
            assert_eq!(dev.last_bar_base, 0x5678_0000);
        }
    }
}
