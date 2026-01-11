use crate::pci::PciBarDefinition;

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
}

impl PciResourceAllocator {
    pub fn new(cfg: PciResourceAllocatorConfig) -> Self {
        Self {
            next_mmio: cfg.mmio_base,
            next_io: cfg.io_base,
            cfg,
        }
    }

    pub fn reset(&mut self) {
        self.next_mmio = self.cfg.mmio_base;
        self.next_io = self.cfg.io_base;
    }

    pub fn allocate_bar(&mut self, bar: PciBarDefinition) -> Result<u64, PciResourceError> {
        let size = bar.size();
        if size == 0 || !size.is_power_of_two() {
            return Err(PciResourceError::InvalidBarSize);
        }

        match bar {
            PciBarDefinition::Io { .. } => {
                let base = align_up_u64(u64::from(self.next_io), size);
                let end = base.saturating_add(size);
                let window_end =
                    u64::from(self.cfg.io_base).saturating_add(u64::from(self.cfg.io_size));
                if end > window_end {
                    return Err(PciResourceError::OutOfIoSpace);
                }
                self.next_io = end as u32;
                Ok(base)
            }
            PciBarDefinition::Mmio32 { .. } => {
                let base = align_up_u64(self.next_mmio, size);
                let end = base.saturating_add(size);
                let window_end = self.cfg.mmio_base.saturating_add(self.cfg.mmio_size);
                if end > window_end {
                    return Err(PciResourceError::OutOfMmioSpace);
                }
                self.next_mmio = end;
                Ok(base)
            }
            PciBarDefinition::Mmio64 { .. } => {
                // We currently only allocate from the 32-bit MMIO window. This keeps the
                // allocator simple while still supporting devices that prefer 64-bit BARs
                // (they will just receive a 32-bit address).
                let base = align_up_u64(self.next_mmio, size);
                let end = base.saturating_add(size);
                let window_end = self.cfg.mmio_base.saturating_add(self.cfg.mmio_size);
                if end > window_end {
                    return Err(PciResourceError::OutOfMmioSpace);
                }
                self.next_mmio = end;
                Ok(base)
            }
        }
    }
}

fn align_up_u64(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}
