/// Abstraction for guest physical memory access.
///
/// UHCI schedule structures (Frame List, QHs, TDs) live in guest RAM; the controller must be able
/// to read and write them. Reads are defined as `&mut self` to allow implementations with side
/// effects (e.g. MMIO-backed RAM).
pub trait MemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]);
    fn write_physical(&mut self, paddr: u64, buf: &[u8]);

    /// Whether DMA (bus-mastering) is enabled for this memory bus.
    ///
    /// Integrations should provide a `MemoryBus` adapter that returns open-bus reads (`0xFF`) and
    /// ignores writes when PCI Bus Master Enable (BME) is clear. Returning `false` here lets device
    /// models avoid interpreting open-bus data as real descriptors/TRBs and mutating internal state
    /// while DMA is disabled.
    fn dma_enabled(&self) -> bool {
        true
    }

    /// Alias for [`MemoryBus::read_physical`], provided for discoverability.
    fn read_bytes(&mut self, paddr: u64, buf: &mut [u8]) {
        self.read_physical(paddr, buf);
    }

    /// Alias for [`MemoryBus::write_physical`], provided for discoverability.
    fn write_bytes(&mut self, paddr: u64, buf: &[u8]) {
        self.write_physical(paddr, buf);
    }

    fn read_u8(&mut self, paddr: u64) -> u8 {
        let mut buf = [0u8; 1];
        self.read_physical(paddr, &mut buf);
        buf[0]
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        let mut buf = [0u8; 2];
        self.read_physical(paddr, &mut buf);
        u16::from_le_bytes(buf)
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        let mut buf = [0u8; 4];
        self.read_physical(paddr, &mut buf);
        u32::from_le_bytes(buf)
    }

    fn read_u64(&mut self, paddr: u64) -> u64 {
        let mut buf = [0u8; 8];
        self.read_physical(paddr, &mut buf);
        u64::from_le_bytes(buf)
    }

    fn write_u8(&mut self, paddr: u64, val: u8) {
        self.write_physical(paddr, &[val]);
    }

    fn write_u16(&mut self, paddr: u64, val: u16) {
        self.write_physical(paddr, &val.to_le_bytes());
    }

    fn write_u32(&mut self, paddr: u64, val: u32) {
        self.write_physical(paddr, &val.to_le_bytes());
    }

    fn write_u64(&mut self, paddr: u64, val: u64) {
        self.write_physical(paddr, &val.to_le_bytes());
    }
}
