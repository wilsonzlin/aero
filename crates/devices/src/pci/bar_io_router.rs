use crate::pci::{PciBarKind, PciBdf, SharedPciConfigPorts};

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
    /// Fast-path cache of the most recently-hit route.
    ///
    /// Port I/O workloads often issue long runs of accesses to the same I/O BAR (e.g. polling a
    /// status register). The router still consults live PCI config space each access, but checking
    /// the last-hit route first avoids scanning the full route table for common cases.
    last_hit: Option<usize>,
}

impl PciIoBarRouter {
    pub fn new(pci_cfg: SharedPciConfigPorts) -> Self {
        Self {
            pci_cfg,
            routes: Vec::new(),
            last_hit: None,
        }
    }

    pub fn register_handler<H>(&mut self, bdf: PciBdf, bar_index: u8, handler: H)
    where
        H: PciIoBarHandler + 'static,
    {
        assert!(
            !self
                .routes
                .iter()
                .any(|r| r.bdf == bdf && r.bar_index == bar_index),
            "duplicate PCI I/O BAR handler registration for {bdf:?} BAR{bar_index}"
        );
        self.routes.push(PciIoBarRoute {
            bdf,
            bar_index,
            handler: Box::new(handler),
        });
        // The route table changed; conservatively drop any cached hit.
        self.last_hit = None;
    }

    /// Dispatches a port read to a PCI I/O BAR handler, returning `None` if the port is not mapped.
    ///
    /// This is intended for integrations that want to fall back to another port I/O bus when the
    /// access does not hit a PCI I/O BAR.
    pub fn dispatch_read(&mut self, port: u16, size: usize) -> Option<u32> {
        let (idx, offset) = self.find_target(port, size)?;
        let entry = self.routes.get_mut(idx)?;
        Some(entry.handler.io_read(offset, size))
    }

    /// Dispatches a port write to a PCI I/O BAR handler.
    ///
    /// Returns `true` if the port hit a PCI I/O BAR route and the handler was invoked.
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

    /// Reads from a PCI I/O BAR-backed port, floating the bus high (all ones) when unmapped.
    pub fn io_read(&mut self, port: u16, size: usize) -> u32 {
        self.dispatch_read(port, size)
            .map(|v| v & all_ones(size))
            .unwrap_or_else(|| all_ones(size))
    }

    /// Writes to a PCI I/O BAR-backed port, dropping writes that do not hit any route.
    pub fn io_write(&mut self, port: u16, size: usize, value: u32) {
        let _ = self.dispatch_write(port, size, value);
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

        let check = |route: &PciIoBarRoute| -> Option<u64> {
            let cfg = bus.device_config(route.bdf)?;

            // COMMAND.IO (bit 0) gates I/O BAR decoding.
            if (cfg.command() & 0x1) == 0 {
                return None;
            }

            let bar = cfg.bar_range(route.bar_index)?;
            if bar.kind != PciBarKind::Io || bar.base == 0 || bar.size == 0 {
                return None;
            }

            let bar_end = bar.base.checked_add(bar.size)?;
            if port_start < bar.base || access_end > bar_end {
                return None;
            }

            Some(port_start - bar.base)
        };

        // Fast path: try the last-hit route first.
        if let Some(idx) = self.last_hit {
            if let Some(route) = self.routes.get(idx) {
                if let Some(offset) = check(route) {
                    return Some((idx, offset));
                }
            } else {
                self.last_hit = None;
            }
        }

        for (idx, route) in self.routes.iter().enumerate() {
            if let Some(offset) = check(route) {
                self.last_hit = Some(idx);
                return Some((idx, offset));
            }
        }

        None
    }
}

fn all_ones(size: usize) -> u32 {
    if size == 0 {
        return 0;
    }
    if size >= 4 {
        return u32::MAX;
    }
    // Safe because `size < 4` => `size * 8 < 32`.
    (1u32 << (size * 8)) - 1
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
        writes: Vec<(u64, usize, u32)>,
    }

    #[derive(Clone)]
    struct TestIoBar {
        state: Arc<Mutex<TestState>>,
    }

    impl TestIoBar {
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

    impl PciIoBarHandler for TestIoBar {
        fn io_read(&mut self, offset: u64, size: usize) -> u32 {
            let mut state = self.state.lock().unwrap();
            state.reads.push((offset, size));

            let off = usize::try_from(offset).ok().unwrap();
            if off + size > state.mem.len() {
                return all_ones(size);
            }
            let mut buf = [0u8; 4];
            buf[..size].copy_from_slice(&state.mem[off..off + size]);
            u32::from_le_bytes(buf)
        }

        fn io_write(&mut self, offset: u64, size: usize, value: u32) {
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
    fn respects_command_io_and_tracks_bar_reprogramming() {
        let bdf = PciBdf::new(0, 5, 0);

        let mut bus = PciBus::new();
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(0, PciBarDefinition::Io { size: 0x20 });
        bus.add_device(bdf, Box::new(TestDev { cfg }));

        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(bus)));

        let (handler, state) = TestIoBar::new(0x20);
        let mut router = PciIoBarRouter::new(cfg_ports.clone());
        router.register_handler(bdf, 0, handler);

        // Program BAR0 but leave COMMAND.IO disabled.
        let bar0_base0 = 0x2000u16;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, u32::from(bar0_base0) | 0x1);
        }

        let dev_offset = 0x10u16;
        let port0 = bar0_base0 + dev_offset;

        // Reads float high when IO decoding disabled.
        assert_eq!(router.dispatch_read(port0, 4), None);
        assert_eq!(router.io_read(port0, 4), all_ones(4));
        assert!(state.lock().unwrap().reads.is_empty());

        // Enable IO decoding via the command register and ensure accesses dispatch.
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0001);
        }

        assert!(router.dispatch_write(port0, 4, 0x1122_3344));
        assert_eq!(router.dispatch_read(port0, 4), Some(0x1122_3344));

        {
            let state = state.lock().unwrap();
            assert_eq!(&state.mem[0x10..0x14], &[0x44, 0x33, 0x22, 0x11]);
            assert_eq!(state.writes.len(), 1);
            assert_eq!(state.reads.len(), 1);
            assert_eq!(state.writes[0].0, u64::from(dev_offset));
            assert_eq!(state.reads[0].0, u64::from(dev_offset));
        }

        // Reprogram the BAR base while decoding enabled. The new address should route to the
        // handler and the old address should float high.
        let bar0_base1 = 0x3000u16;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, u32::from(bar0_base1) | 0x1);
        }

        let port1 = bar0_base1 + dev_offset;
        assert_eq!(router.io_read(port1, 4), 0x1122_3344);
        assert_eq!(router.io_read(port0, 4), all_ones(4));

        // Programming BAR0 to zero should behave as unmapped.
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x10, 4, 0);
        }
        assert_eq!(router.io_read(port1, 4), all_ones(4));
    }

    #[test]
    fn unmapped_reads_float_high() {
        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::new()));
        let mut router = PciIoBarRouter::new(cfg_ports);

        assert_eq!(router.io_read(0x1234, 1), 0xFF);
        assert_eq!(router.io_read(0x1234, 2), 0xFFFF);
        assert_eq!(router.io_read(0x1234, 4), 0xFFFF_FFFF);
    }

    #[test]
    fn duplicate_registration_panics() {
        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::new()));
        let mut router = PciIoBarRouter::new(cfg_ports);

        let bdf = PciBdf::new(0, 1, 0);
        let (handler, _state) = TestIoBar::new(0x20);
        router.register_handler(bdf, 0, handler);

        let (handler2, _state2) = TestIoBar::new(0x20);
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            router.register_handler(bdf, 0, handler2);
        }));
        assert!(err.is_err());
    }
}
