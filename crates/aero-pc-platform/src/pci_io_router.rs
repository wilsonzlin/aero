use aero_devices::pci::{PciBarKind, PciBdf, SharedPciConfigPorts};

/// Handler for PCI I/O BAR accesses.
///
/// The router passes an `offset` relative to the BAR base (i.e. `port - bar_base`).
pub trait PciIoBarHandler {
    fn io_read(&mut self, offset: u64, size: usize) -> u32;
    fn io_write(&mut self, offset: u64, size: usize, value: u32);
}

struct PciIoBarRoute {
    bdf: PciBdf,
    bar_index: u8,
    handler: Box<dyn PciIoBarHandler>,
}

/// Routes x86 port I/O requests to PCI devices backed by I/O BARs.
///
/// The router consults the live PCI config space on every access, so BAR programming and command
/// register gating take effect immediately without needing to re-register the device.
pub struct PciIoBarRouter {
    pci_cfg: SharedPciConfigPorts,
    routes: Vec<PciIoBarRoute>,
}

impl PciIoBarRouter {
    pub fn new(pci_cfg: SharedPciConfigPorts) -> Self {
        Self {
            pci_cfg,
            routes: Vec::new(),
        }
    }

    pub fn register_handler<H>(&mut self, bdf: PciBdf, bar_index: u8, handler: H)
    where
        H: PciIoBarHandler + 'static,
    {
        self.routes.push(PciIoBarRoute {
            bdf,
            bar_index,
            handler: Box::new(handler),
        });
    }

    pub fn dispatch_read(&mut self, port: u16, size: usize) -> Option<u32> {
        let (idx, offset) = self.find_target(port, size)?;
        let entry = self.routes.get_mut(idx)?;
        Some(entry.handler.io_read(offset, size))
    }

    pub fn dispatch_write(&mut self, port: u16, size: usize, value: u32) -> bool {
        let Some((idx, offset)) = self.find_target(port, size) else {
            return false;
        };
        let Some(entry) = self.routes.get_mut(idx) else {
            return false;
        };
        entry.handler.io_write(offset, size, value);
        true
    }

    fn find_target(&mut self, port: u16, size: usize) -> Option<(usize, u64)> {
        if size == 0 {
            return None;
        }

        // Keep the port-space arithmetic deterministic and avoid accidental wraparound behavior.
        let port_start = u64::from(port);
        let access_end = port_start.checked_add(size as u64)?;
        if access_end > 0x1_0000 {
            // Would wrap the 16-bit I/O port space.
            return None;
        }

        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        for (idx, route) in self.routes.iter().enumerate() {
            let Some(cfg) = bus.device_config(route.bdf) else {
                continue;
            };

            // COMMAND.IO (bit 0) gates I/O BAR decoding.
            if (cfg.command() & 0x1) == 0 {
                continue;
            }

            let Some(bar) = cfg.bar_range(route.bar_index) else {
                continue;
            };

            if bar.kind != PciBarKind::Io || bar.base == 0 || bar.size == 0 {
                continue;
            }

            let Some(bar_end) = bar.base.checked_add(bar.size) else {
                continue;
            };

            if port_start < bar.base || access_end > bar_end {
                continue;
            }

            return Some((idx, port_start - bar.base));
        }

        None
    }
}

