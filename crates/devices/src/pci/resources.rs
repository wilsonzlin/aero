use crate::pci::{PciBarDefinition, PciBarKind, PciBarRange};

#[derive(Debug, Clone)]
pub struct PciResourceAllocatorConfig {
    /// Base address of the 32-bit MMIO window reserved for PCI BAR allocation.
    pub mmio_base: u64,
    pub mmio_size: u64,
    /// Base port of the I/O window reserved for PCI BAR allocation.
    pub io_base: u32,
    pub io_size: u32,
}

impl Default for PciResourceAllocatorConfig {
    fn default() -> Self {
        // These defaults are intentionally "PC-like" but not tied to a specific chipset.
        //
        // - I/O: keep clear of legacy 0x0000..0x0FFF and leave room for fixed-function devices.
        // - MMIO: put PCI devices high in the 32-bit space, away from RAM in typical setups.
        Self {
            mmio_base: 0xE000_0000,
            mmio_size: 0x1000_0000,
            io_base: 0x1000,
            io_size: 0xE000,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PciResourceError {
    OutOfMmioSpace,
    OutOfIoSpace,
    InvalidBarSize,
}

#[derive(Debug, Clone)]
pub struct PciResourceAllocator {
    cfg: PciResourceAllocatorConfig,
    next_mmio: u64,
    next_io: u32,
    reserved_mmio: Vec<std::ops::Range<u64>>,
    reserved_io: Vec<std::ops::Range<u64>>,
}

impl PciResourceAllocator {
    pub fn new(cfg: PciResourceAllocatorConfig) -> Self {
        Self {
            next_mmio: cfg.mmio_base,
            next_io: cfg.io_base,
            cfg,
            reserved_mmio: Vec::new(),
            reserved_io: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.next_mmio = self.cfg.mmio_base;
        self.next_io = self.cfg.io_base;
        self.reserved_mmio.clear();
        self.reserved_io.clear();
    }

    /// Reserve an existing BAR assignment so future allocations do not overlap it.
    ///
    /// This is used by `PciBus::reset`: some devices may start with fixed BAR assignments (e.g.
    /// legacy IDE compatibility ports or a platform-chosen I/O BAR base). When a new device is
    /// added and firmware POST is re-run, we must ensure newly allocated BARs do not overlap those
    /// preserved ranges.
    pub fn reserve_range(&mut self, range: PciBarRange) {
        if range.base == 0 || range.size == 0 {
            return;
        }
        let start = range.base;
        let end = range.end_exclusive();
        if end <= start {
            return;
        }

        match range.kind {
            PciBarKind::Io => self.reserved_io.push(start..end),
            PciBarKind::Mmio32 | PciBarKind::Mmio64 => self.reserved_mmio.push(start..end),
        }
    }

    fn ranges_overlap(a_start: u64, a_end: u64, b: &std::ops::Range<u64>) -> bool {
        a_start < b.end && b.start < a_end
    }

    pub fn allocate_bar(&mut self, bar: PciBarDefinition) -> Result<u64, PciResourceError> {
        let size = bar.size();
        if size == 0 || !size.is_power_of_two() {
            return Err(PciResourceError::InvalidBarSize);
        }

        match bar {
            PciBarDefinition::Io { .. } => {
                let window_end =
                    u64::from(self.cfg.io_base).saturating_add(u64::from(self.cfg.io_size));
                let mut base = align_up_u64(u64::from(self.next_io), size);

                loop {
                    let Some(end) = base.checked_add(size) else {
                        return Err(PciResourceError::OutOfIoSpace);
                    };
                    if end > window_end {
                        return Err(PciResourceError::OutOfIoSpace);
                    }
                    if let Some(overlap) = self
                        .reserved_io
                        .iter()
                        .find(|r| Self::ranges_overlap(base, end, r))
                    {
                        base = align_up_u64(overlap.end, size);
                        continue;
                    }

                    self.next_io = end as u32;
                    return Ok(base);
                }
            }
            PciBarDefinition::Mmio32 { .. } => {
                self.allocate_mmio(size)
            }
            PciBarDefinition::Mmio64 { .. } => {
                // We currently only allocate from the 32-bit MMIO window. This keeps the
                // allocator simple while still supporting devices that prefer 64-bit BARs
                // (they will just receive a 32-bit address).
                self.allocate_mmio(size)
            }
        }
    }

    fn allocate_mmio(&mut self, size: u64) -> Result<u64, PciResourceError> {
        let window_end = self.cfg.mmio_base.saturating_add(self.cfg.mmio_size);
        let mut base = align_up_u64(self.next_mmio, size);

        loop {
            let Some(end) = base.checked_add(size) else {
                return Err(PciResourceError::OutOfMmioSpace);
            };
            if end > window_end {
                return Err(PciResourceError::OutOfMmioSpace);
            }
            if let Some(overlap) = self
                .reserved_mmio
                .iter()
                .find(|r| Self::ranges_overlap(base, end, r))
            {
                base = align_up_u64(overlap.end, size);
                continue;
            }

            self.next_mmio = end;
            return Ok(base);
        }
    }
}

fn align_up_u64(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}
