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
    match size {
        0 => 0,
        1 => 0xff,
        2 => 0xffff,
        3 => 0x00ff_ffff,
        4 => 0xffff_ffff,
        5 => 0x0000_ffff_ffff,
        6 => 0x00ff_ffff_ffff,
        7 => 0x00ff_ffff_ffff_ffff,
        _ => u64::MAX,
    }
}

