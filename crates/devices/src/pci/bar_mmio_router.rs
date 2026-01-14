use crate::pci::{PciBarKind, PciBdf, SharedPciConfigPorts};
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
/// This avoids coherence issues that would arise from implementing [`PciBarMmioHandler`] directly
/// for `Rc<RefCell<T>>` alongside a blanket impl for all `T: memory::MmioHandler`.
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

struct PciBarMmioHandlerAdapter<H> {
    inner: H,
}

impl<H: PciBarMmioHandler> MmioHandler for PciBarMmioHandlerAdapter<H> {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.inner.read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.inner.write(offset, size, value)
    }
}

/// Routes accesses within a fixed PCI MMIO BAR window to registered PCI BAR MMIO handlers.
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
    /// Base physical address of the MMIO window this router is mapped to.
    window_base: u64,
    /// Canonical PCI configuration space (shared with port-based config and ECAM).
    pci_cfg: SharedPciConfigPorts,
    /// Registered BAR handlers keyed by (BDF, BAR index).
    bars: BTreeMap<(PciBdf, u8), Box<dyn MmioHandler>>,
    /// Fast-path cache of the most recently-hit BAR key.
    ///
    /// Many real workloads issue long runs of MMIO accesses to the same BAR (e.g. framebuffer
    /// blits, NIC register polling). The router still consults live PCI config space each access
    /// (so BAR reprogramming is always observed), but checking the last-hit BAR first avoids an
    /// O(n) scan of all registered handlers on every access.
    last_hit: Option<(PciBdf, u8)>,
}

impl PciBarMmioRouter {
    pub fn new(window_base: u64, pci_cfg: SharedPciConfigPorts) -> Self {
        Self {
            window_base,
            pci_cfg,
            bars: BTreeMap::new(),
            last_hit: None,
        }
    }

    /// Registers a handler for the given PCI BAR.
    ///
    /// This is a convenience wrapper around [`PciBarMmioRouter::register_bar`] that accepts any
    /// [`PciBarMmioHandler`].
    pub fn register_handler<H>(&mut self, bdf: PciBdf, bar_index: u8, handler: H)
    where
        H: PciBarMmioHandler + 'static,
    {
        self.register_bar(
            bdf,
            bar_index,
            Box::new(PciBarMmioHandlerAdapter { inner: handler }),
        );
    }

    /// Registers a handler for the given PCI BAR backed by an `Rc<RefCell<T>>`.
    pub fn register_shared_handler<T>(
        &mut self,
        bdf: PciBdf,
        bar_index: u8,
        handler: Rc<RefCell<T>>,
    ) where
        T: PciBarMmioHandler + 'static,
    {
        self.register_handler(bdf, bar_index, SharedPciBarMmioHandler::new(handler));
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

    fn find_handler(&mut self, paddr: u64, size: usize) -> Option<((PciBdf, u8), u64)> {
        let size_u64 = u64::try_from(size).ok()?;
        let access_end = paddr.checked_add(size_u64)?;

        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let check = |(bdf, bar): (PciBdf, u8)| -> Option<u64> {
            let cfg = bus.device_config(bdf)?;

            let mem_enabled = (cfg.command() & 0x2) != 0;
            if !mem_enabled {
                return None;
            }

            let range = cfg.bar_range(bar)?;
            if range.base == 0 {
                return None;
            }
            if !matches!(range.kind, PciBarKind::Mmio32 | PciBarKind::Mmio64) {
                return None;
            }

            let bar_end = range.base.checked_add(range.size)?;
            if paddr < range.base || access_end > bar_end {
                return None;
            }

            Some(paddr - range.base)
        };

        // Fast path: most MMIO streams repeatedly hit the same BAR.
        if let Some(key) = self.last_hit {
            if self.bars.contains_key(&key) {
                if let Some(dev_offset) = check(key) {
                    return Some((key, dev_offset));
                }
            } else {
                self.last_hit = None;
            }
        }

        for &key in self.bars.keys() {
            if let Some(dev_offset) = check(key) {
                self.last_hit = Some(key);
                return Some((key, dev_offset));
            }
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
        // Ensure callers never observe junk in the upper bits for sub-8-byte reads.
        handler.read(dev_offset, size) & all_ones(size)
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
        // Mask writes so device models that treat `value` as a full 64-bit quantity still observe
        // correct byte-enable semantics.
        handler.write(dev_offset, size, value & all_ones(size));
    }
}

fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    // Safe because `size < 8` => `size * 8 < 64`.
    (1u64 << (size * 8)) - 1
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

    #[derive(Clone)]
    struct TestBarHandler {
        state: Arc<Mutex<TestState>>,
    }

    impl TestBarHandler {
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

    impl PciBarMmioHandler for TestBarHandler {
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
        let got = MmioHandler::read(&mut router, off0, 4);
        assert_eq!(got, all_ones(4));
        assert!(state.lock().unwrap().reads.is_empty());

        // Enable MEM decoding via the command register and ensure accesses dispatch.
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
        }

        MmioHandler::write(&mut router, off0, 4, 0x1122_3344);
        let got = MmioHandler::read(&mut router, off0, 4);
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
        let got_new = MmioHandler::read(&mut router, off1, 4);
        assert_eq!(got_new, 0x1122_3344);

        let got_old = MmioHandler::read(&mut router, off0, 4);
        assert_eq!(got_old, all_ones(4));

        // Programming BAR0 to zero should behave as unmapped.
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x10, 4, 0);
        }
        let got_zero = MmioHandler::read(&mut router, off1, 4);
        assert_eq!(got_zero, all_ones(4));
    }

    #[test]
    fn register_handler_accepts_non_mmio_handler() {
        let window_base = 0x8000_0000;
        let bdf = PciBdf::new(0, 6, 0);

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

        let (handler, state) = TestBarHandler::new(0x1000);
        let mut router = PciBarMmioRouter::new(window_base, cfg_ports.clone());
        router.register_handler(bdf, 0, handler);

        // Program BAR0 and enable MEM decoding.
        let bar0_base = window_base + 0x2000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base as u32);
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
        }

        let dev_offset = 0x20u64;
        let off = (bar0_base - window_base) + dev_offset;

        MmioHandler::write(&mut router, off, 4, 0xDEAD_BEEF);
        let got = MmioHandler::read(&mut router, off, 4);
        assert_eq!(got, 0xDEAD_BEEF);

        let state = state.lock().unwrap();
        assert_eq!(
            &state.mem[dev_offset as usize..dev_offset as usize + 4],
            &[0xEF, 0xBE, 0xAD, 0xDE]
        );
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.reads.len(), 1);
    }

    #[test]
    fn register_shared_handler_accepts_rc_refcell() {
        let window_base = 0x8000_0000;
        let bdf = PciBdf::new(0, 7, 0);

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
        let mmio = Rc::new(RefCell::new(mmio));

        let mut router = PciBarMmioRouter::new(window_base, cfg_ports.clone());
        router.register_shared_handler(bdf, 0, mmio);

        // Program BAR0 and enable MEM decoding.
        let bar0_base = window_base + 0x3000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base as u32);
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
        }

        let dev_offset = 0x30u64;
        let off = (bar0_base - window_base) + dev_offset;

        MmioHandler::write(&mut router, off, 4, 0x1122_3344);
        let got = MmioHandler::read(&mut router, off, 4);
        assert_eq!(got, 0x1122_3344);

        let state = state.lock().unwrap();
        assert_eq!(
            &state.mem[dev_offset as usize..dev_offset as usize + 4],
            &[0x44, 0x33, 0x22, 0x11]
        );
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.reads.len(), 1);
    }

    #[test]
    fn unmapped_reads_return_all_ones_for_non_pow2_sizes() {
        let window_base = 0x8000_0000;
        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::new()));

        let mut router = PciBarMmioRouter::new(window_base, cfg_ports);

        // These sizes can occur for string/descriptor-style instructions (e.g. LGDT/LIDT) hitting
        // an unmapped MMIO window; ensure we return the correct open-bus pattern.
        let got5 = MmioHandler::read(&mut router, 0x10, 5);
        assert_eq!(got5, (1u64 << 40) - 1);

        let got6 = MmioHandler::read(&mut router, 0x10, 6);
        assert_eq!(got6, (1u64 << 48) - 1);
    }

    #[test]
    fn routes_mmio64_bar_base_above_4gib() {
        // Place the PCI MMIO window above 4GiB so BAR0 programming uses the 64-bit BAR path and
        // the router must perform 64-bit address arithmetic.
        let window_base = 0x1_0000_0000;
        let bdf = PciBdf::new(0, 8, 0);

        let mut bus = PciBus::new();
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        bus.add_device(bdf, Box::new(TestDev { cfg }));

        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(bus)));

        let (mmio, state) = TestMmio::new(0x1000);
        let mut router = PciBarMmioRouter::new(window_base, cfg_ports.clone());
        router.register_bar(bdf, 0, Box::new(mmio));

        // Program BAR0 and enable MEM decoding.
        let bar0_base = window_base + 0x2000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
            // BAR0 low dword.
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base as u32);
            // BAR0 high dword.
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x14, 4, (bar0_base >> 32) as u32);
        }

        let dev_offset = 0x10u64;
        let off = (bar0_base - window_base) + dev_offset;

        MmioHandler::write(&mut router, off, 4, 0xA1B2_C3D4);
        let got = MmioHandler::read(&mut router, off, 4);
        assert_eq!(got, 0xA1B2_C3D4);

        let state = state.lock().unwrap();
        assert_eq!(&state.mem[0x10..0x14], &[0xD4, 0xC3, 0xB2, 0xA1]);
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.reads.len(), 1);
    }

    #[test]
    fn does_not_dispatch_when_bar_end_overflows_u64() {
        // Guests can program arbitrary BAR bases. If `base + size` overflows, treat the BAR as
        // unmapped rather than saturating the range end to `u64::MAX` (which would incorrectly
        // route almost all high addresses).
        let window_base = 0xFFFF_FFFF_FFFF_0000;
        // Avoid `00:0c.0`, which is reserved by the historical Bochs/QEMU VGA stub contract.
        let bdf = PciBdf::new(0, 14, 0);

        let mut bus = PciBus::new();
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: 0x2000,
                prefetchable: false,
            },
        );
        bus.add_device(bdf, Box::new(TestDev { cfg }));

        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(bus)));

        let (mmio, state) = TestMmio::new(0x2000);
        let mut router = PciBarMmioRouter::new(window_base, cfg_ports.clone());
        router.register_bar(bdf, 0, Box::new(mmio));

        // Program BAR0 near the top of the u64 address space so `base + size` overflows.
        let bar0_base = 0xFFFF_FFFF_FFFF_E000u64;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
            // BAR0 low dword.
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base as u32);
            // BAR0 high dword.
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x14, 4, (bar0_base >> 32) as u32);
        }

        let off = bar0_base - window_base;
        let got = MmioHandler::read(&mut router, off, 4);
        assert_eq!(got, all_ones(4));
        MmioHandler::write(&mut router, off, 4, 0x1122_3344);

        let state = state.lock().unwrap();
        assert!(state.reads.is_empty());
        assert!(state.writes.is_empty());
    }

    #[test]
    fn ignores_io_bar_kind_even_if_bar_base_matches_window() {
        let window_base = 0x8000_0000;
        let bdf = PciBdf::new(0, 9, 0);

        let mut bus = PciBus::new();
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(0, PciBarDefinition::Io { size: 0x20 });
        bus.add_device(bdf, Box::new(TestDev { cfg }));

        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(bus)));

        let (mmio, state) = TestMmio::new(0x20);
        let mut router = PciBarMmioRouter::new(window_base, cfg_ports.clone());
        router.register_bar(bdf, 0, Box::new(mmio));

        let bar0_base = window_base + 0x2000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            // Enable MEM decoding so any non-routing is attributable to BAR kind mismatch (I/O vs
            // MMIO), not command gating.
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base as u32);
        }

        let off = (bar0_base - window_base) + 0x10;
        let got = MmioHandler::read(&mut router, off, 4);
        assert_eq!(got, all_ones(4));
        assert!(state.lock().unwrap().reads.is_empty());
    }

    #[test]
    fn does_not_dispatch_accesses_that_cross_bar_end() {
        let window_base = 0x8000_0000;
        let bdf = PciBdf::new(0, 10, 0);

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

        // Program BAR0 and enable MEM decoding.
        let bar0_base = window_base + 0x2000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base as u32);
        }

        // This 8-byte access starts inside the BAR but extends past the end.
        let off = (bar0_base - window_base) + 0xFF9;
        let got = MmioHandler::read(&mut router, off, 8);
        assert_eq!(got, u64::MAX);
        MmioHandler::write(&mut router, off, 8, 0x1122_3344_5566_7788);

        let state = state.lock().unwrap();
        assert!(state.reads.is_empty());
        assert!(state.writes.is_empty());
    }

    #[test]
    fn masks_read_and_write_values_to_access_size() {
        let window_base = 0x8000_0000;
        let bdf = PciBdf::new(0, 11, 0);

        #[derive(Default)]
        struct State {
            reads: Vec<(u64, usize)>,
            writes: Vec<(u64, usize, u64)>,
        }

        #[derive(Clone)]
        struct AllOnesMmio {
            state: Arc<Mutex<State>>,
        }

        impl MmioHandler for AllOnesMmio {
            fn read(&mut self, offset: u64, size: usize) -> u64 {
                self.state.lock().unwrap().reads.push((offset, size));
                u64::MAX
            }

            fn write(&mut self, offset: u64, size: usize, value: u64) {
                self.state
                    .lock()
                    .unwrap()
                    .writes
                    .push((offset, size, value));
            }
        }

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

        let state = Arc::new(Mutex::new(State::default()));
        let mmio = AllOnesMmio {
            state: state.clone(),
        };

        let mut router = PciBarMmioRouter::new(window_base, cfg_ports.clone());
        router.register_bar(bdf, 0, Box::new(mmio));

        // Program BAR0 and enable MEM decoding.
        let bar0_base = window_base + 0x2000;
        {
            let mut cfg_ports_mut = cfg_ports.borrow_mut();
            cfg_ports_mut.bus_mut().write_config(bdf, 0x04, 2, 0x0002);
            cfg_ports_mut
                .bus_mut()
                .write_config(bdf, 0x10, 4, bar0_base as u32);
        }

        let off = (bar0_base - window_base) + 0x10;

        // Underlying handler returns `u64::MAX` regardless of size, but the router should mask the
        // returned value down to the requested width.
        assert_eq!(MmioHandler::read(&mut router, off, 1), 0xFF);
        assert_eq!(MmioHandler::read(&mut router, off, 4), 0xFFFF_FFFF);

        // Writes should also be masked before reaching the handler.
        MmioHandler::write(&mut router, off, 4, u64::MAX);
        let state = state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.writes[0].2, 0xFFFF_FFFF);
    }
}
