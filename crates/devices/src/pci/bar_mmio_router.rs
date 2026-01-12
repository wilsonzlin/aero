use crate::pci::{PciBarKind, PciBdf, SharedPciConfigPorts};
use memory::MmioHandler;
use std::collections::BTreeMap;

/// Routes accesses within a fixed PCI MMIO aperture to registered PCI BAR MMIO handlers.
///
/// This is intended for platforms that:
/// - allocate all PCI MMIO BARs from a single big window (e.g. Q35-style `mmio_base..mmio_end`),
/// - want BAR reprogramming to take effect immediately, and
/// - cannot (or do not want to) dynamically unmap/remap regions in the guest MMIO bus.
///
/// For each MMIO access, the router consults the canonical PCI config space to find the current
/// BAR base/size and the device's command register state. This makes BAR updates visible without
/// needing explicit refresh hooks.
pub struct PciBarMmioRouter {
    /// Base physical address of the MMIO aperture this router is mapped to.
    window_base: u64,
    /// Canonical PCI configuration space (shared with port-based config and ECAM).
    pci_cfg: SharedPciConfigPorts,
    /// Registered BAR handlers keyed by (BDF, BAR index).
    bars: BTreeMap<(PciBdf, u8), Box<dyn MmioHandler>>,
}

impl PciBarMmioRouter {
    pub fn new(window_base: u64, pci_cfg: SharedPciConfigPorts) -> Self {
        Self {
            window_base,
            pci_cfg,
            bars: BTreeMap::new(),
        }
    }

    /// Registers an MMIO handler for the given PCI BAR.
    ///
    /// The handler is invoked when:
    /// - the device exists,
    /// - the device's MEM decoding is enabled (command bit 1),
    /// - the BAR is an MMIO BAR (32-bit or 64-bit),
    /// - the BAR base is non-zero, and
    /// - the accessed address range is fully contained within the BAR's `[base, base + size)`.
    pub fn register_bar(&mut self, bdf: PciBdf, bar_index: u8, handler: Box<dyn MmioHandler>) {
        self.bars.insert((bdf, bar_index), handler);
    }

    /// Removes a previously registered BAR handler.
    pub fn unregister_bar(&mut self, bdf: PciBdf, bar_index: u8) -> Option<Box<dyn MmioHandler>> {
        self.bars.remove(&(bdf, bar_index))
    }

    fn find_handler(&self, paddr: u64, size: usize) -> Option<((PciBdf, u8), u64)> {
        let size_u64 = u64::try_from(size).ok()?;
        let access_end = paddr.checked_add(size_u64)?;

        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        for &(bdf, bar) in self.bars.keys() {
            let Some(cfg) = bus.device_config(bdf) else {
                continue;
            };

            let mem_enabled = (cfg.command() & 0x2) != 0;
            if !mem_enabled {
                continue;
            }

            let Some(range) = cfg.bar_range(bar) else {
                continue;
            };
            if range.base == 0 {
                continue;
            }
            if !matches!(range.kind, PciBarKind::Mmio32 | PciBarKind::Mmio64) {
                continue;
            }

            let bar_end = range.base.saturating_add(range.size);
            if paddr < range.base || access_end > bar_end {
                continue;
            }

            return Some(((bdf, bar), paddr - range.base));
        }

        None
    }
}

impl MmioHandler for PciBarMmioRouter {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if size > 8 {
            return u64::MAX;
        }

        let Some(paddr) = self.window_base.checked_add(offset) else {
            return all_ones(size);
        };

        let Some((key, dev_offset)) = self.find_handler(paddr, size) else {
            return all_ones(size);
        };

        let handler = self
            .bars
            .get_mut(&key)
            .expect("handler disappeared during dispatch");
        handler.read(dev_offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        let Some(paddr) = self.window_base.checked_add(offset) else {
            return;
        };

        let Some((key, dev_offset)) = self.find_handler(paddr, size) else {
            return;
        };

        let handler = self
            .bars
            .get_mut(&key)
            .expect("handler disappeared during dispatch");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::{PciBarDefinition, PciBus, PciConfigPorts, PciConfigSpace, PciDevice};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct TestState {
        mem: Vec<u8>,
        reads: Vec<(u64, usize)>,
        writes: Vec<(u64, usize, u64)>,
    }

    #[derive(Clone)]
    struct TestMmio {
        state: Arc<Mutex<TestState>>,
    }

    impl TestMmio {
        fn new(size: usize) -> (Self, Arc<Mutex<TestState>>) {
            let state = Arc::new(Mutex::new(TestState {
                mem: vec![0; size],
                reads: Vec::new(),
                writes: Vec::new(),
            }));
            (
                Self {
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl MmioHandler for TestMmio {
        fn read(&mut self, offset: u64, size: usize) -> u64 {
            let mut state = self.state.lock().unwrap();
            state.reads.push((offset, size));

            let off = usize::try_from(offset).ok().unwrap();
            if off + size > state.mem.len() {
                return all_ones(size);
            }
            let mut buf = [0u8; 8];
            buf[..size].copy_from_slice(&state.mem[off..off + size]);
            u64::from_le_bytes(buf)
        }

        fn write(&mut self, offset: u64, size: usize, value: u64) {
            let mut state = self.state.lock().unwrap();
            state.writes.push((offset, size, value));

            let off = usize::try_from(offset).ok().unwrap();
            if off + size > state.mem.len() {
                return;
            }
            let bytes = value.to_le_bytes();
            state.mem[off..off + size].copy_from_slice(&bytes[..size]);
        }
    }

    struct TestDev {
        cfg: PciConfigSpace,
    }

    impl PciDevice for TestDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    #[test]
    fn routes_mmio_when_mem_decoding_enabled_and_tracks_bar_reprogramming() {
        let window_base = 0x8000_0000;
        let bdf = PciBdf::new(0, 5, 0);

        let mut bus = PciBus::new();
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        bus.add_device(bdf, Box::new(TestDev { cfg }));

        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(bus)));

        let (mmio, state) = TestMmio::new(0x1000);
        let mut router = PciBarMmioRouter::new(window_base, cfg_ports.clone());
        router.register_bar(bdf, 0, Box::new(mmio));

        // Program BAR0 address but leave MEM decoding disabled.
        let bar0_base0 = window_base + 0x2000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base0 as u32);
        }

        let dev_offset = 0x10u64;
        let off0 = (bar0_base0 - window_base) + dev_offset;

        // Reads float high when MEM decoding disabled.
        let got = router.read(off0, 4);
        assert_eq!(got, all_ones(4));
        assert!(state.lock().unwrap().reads.is_empty());

        // Enable MEM decoding via the command register and ensure accesses dispatch.
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
        }

        router.write(off0, 4, 0x1122_3344);
        let got = router.read(off0, 4);
        assert_eq!(got, 0x1122_3344);

        {
            let state = state.lock().unwrap();
            assert_eq!(&state.mem[0x10..0x14], &[0x44, 0x33, 0x22, 0x11]);
            assert_eq!(state.writes.len(), 1);
            assert_eq!(state.reads.len(), 1);
            assert_eq!(state.writes[0].0, dev_offset);
            assert_eq!(state.reads[0].0, dev_offset);
        }

        // Reprogram the BAR base while decoding enabled. The new address should route to the
        // handler and the old address should float high.
        let bar0_base1 = window_base + 0x4000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base1 as u32);
        }

        let off1 = (bar0_base1 - window_base) + dev_offset;
        let got_new = router.read(off1, 4);
        assert_eq!(got_new, 0x1122_3344);

        let got_old = router.read(off0, 4);
        assert_eq!(got_old, all_ones(4));

        // Programming BAR0 to zero should behave as unmapped.
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x10, 4, 0);
        }
        let got_zero = router.read(off1, 4);
        assert_eq!(got_zero, all_ones(4));
    }
}
