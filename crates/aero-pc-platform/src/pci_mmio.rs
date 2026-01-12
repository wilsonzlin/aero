use aero_devices::pci::{PciBarKind, PciBdf, SharedPciConfigPorts};
use memory::MmioHandler;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// A minimal MMIO handler interface for a single PCI BAR.
///
/// This trait intentionally mirrors [`memory::MmioHandler`], but is scoped to PCI BAR routing so
/// device models can be adapted without being tightly coupled to the platform bus.
pub trait PciBarMmioHandler {
    fn read(&mut self, offset: u64, size: usize) -> u64;
    fn write(&mut self, offset: u64, size: usize, value: u64);
}

impl<T: MmioHandler> PciBarMmioHandler for T {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        MmioHandler::read(self, offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        MmioHandler::write(self, offset, size, value)
    }
}

/// Adapter to register an `Rc<RefCell<T>>` as a BAR MMIO handler.
///
/// This avoids coherence issues that would arise from implementing `PciBarMmioHandler` directly for
/// `Rc<RefCell<T>>` alongside a blanket impl for all `T: memory::MmioHandler`.
pub struct SharedPciBarMmioHandler<T>(pub Rc<RefCell<T>>);

impl<T> SharedPciBarMmioHandler<T> {
    pub fn new(inner: Rc<RefCell<T>>) -> Self {
        Self(inner)
    }
}

impl<T: PciBarMmioHandler> PciBarMmioHandler for SharedPciBarMmioHandler<T> {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.0.borrow_mut().read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.0.borrow_mut().write(offset, size, value)
    }
}

/// Routes MMIO accesses within a PCI MMIO window to the correct device BAR handler.
///
/// The router consults each device's live PCI config space (`PciConfigSpace::command()` +
/// `PciConfigSpace::bar_range()`) for every access, so changes to the PCI command register
/// (MEMORY_ENABLE) or BAR reprogramming are reflected immediately without needing dynamic MMIO
/// unmap/remap support in the memory bus.
pub struct PciBarMmioRouter {
    base: u64,
    pci_cfg: SharedPciConfigPorts,
    handlers: BTreeMap<(PciBdf, u8), Box<dyn PciBarMmioHandler>>,
}

impl PciBarMmioRouter {
    pub fn new(base: u64, pci_cfg: SharedPciConfigPorts) -> Self {
        Self {
            base,
            pci_cfg,
            handlers: BTreeMap::new(),
        }
    }

    pub fn register_handler<H>(&mut self, bdf: PciBdf, bar: u8, handler: H)
    where
        H: PciBarMmioHandler + 'static,
    {
        self.handlers.insert((bdf, bar), Box::new(handler));
    }

    pub fn register_shared_handler<T>(&mut self, bdf: PciBdf, bar: u8, handler: Rc<RefCell<T>>)
    where
        T: PciBarMmioHandler + 'static,
    {
        self.register_handler(bdf, bar, SharedPciBarMmioHandler::new(handler));
    }

    fn find_target(&mut self, paddr: u64, size: usize) -> Option<((PciBdf, u8), u64)> {
        let size_u64 = u64::try_from(size).ok()?;
        let access_end = paddr.checked_add(size_u64)?;

        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        // Iterate only over BARs with registered handlers to avoid scanning the entire bus.
        for key in self.handlers.keys().copied() {
            let Some(cfg) = bus.device_config(key.0) else {
                continue;
            };

            // Respect PCI command register Memory Space Enable (bit 1).
            if (cfg.command() & 0x2) == 0 {
                continue;
            }

            let Some(bar) = cfg.bar_range(key.1) else {
                continue;
            };

            if bar.base == 0 {
                continue;
            }

            if !matches!(bar.kind, PciBarKind::Mmio32 | PciBarKind::Mmio64) {
                continue;
            }

            let bar_end = bar.end_exclusive();

            if paddr < bar.base || access_end > bar_end {
                continue;
            }

            return Some((key, paddr - bar.base));
        }

        None
    }
}

impl MmioHandler for PciBarMmioRouter {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let Some(paddr) = self.base.checked_add(offset) else {
            return all_ones(size);
        };

        let Some((key, dev_offset)) = self.find_target(paddr, size) else {
            return all_ones(size);
        };

        let Some(handler) = self.handlers.get_mut(&key) else {
            return all_ones(size);
        };

        handler.read(dev_offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let Some(paddr) = self.base.checked_add(offset) else {
            return;
        };

        let Some((key, dev_offset)) = self.find_target(paddr, size) else {
            return;
        };

        let Some(handler) = self.handlers.get_mut(&key) else {
            return;
        };

        handler.write(dev_offset, size, value);
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
