use crate::address_filter::AddressFilter;

pub struct Ram {
    data: Vec<u8>,
}

impl Ram {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    #[inline]
    fn check_bounds(&self, paddr: u64) -> usize {
        let idx = paddr as usize;
        assert!(
            idx < self.data.len(),
            "physical address out of bounds: {paddr:#x}"
        );
        idx
    }

    pub fn read_u8(&self, paddr: u64) -> u8 {
        let idx = self.check_bounds(paddr);
        self.data[idx]
    }

    pub fn write_u8(&mut self, paddr: u64, value: u8) {
        let idx = self.check_bounds(paddr);
        self.data[idx] = value;
    }
}

pub struct MemoryBus {
    filter: AddressFilter,
    ram: Ram,
}

impl MemoryBus {
    pub fn new(filter: AddressFilter, ram_size: usize) -> Self {
        Self {
            filter,
            ram: Ram::new(ram_size),
        }
    }

    pub fn read_u8(&self, paddr: u64) -> u8 {
        self.ram.read_u8(self.filter.filter_paddr(paddr))
    }

    pub fn write_u8(&mut self, paddr: u64, value: u8) {
        let paddr = self.filter.filter_paddr(paddr);
        self.ram.write_u8(paddr, value);
    }

    pub fn read_physical(&self, paddr: u64, buf: &mut [u8]) {
        for (offset, dst) in buf.iter_mut().enumerate() {
            let addr = paddr
                .checked_add(offset as u64)
                .expect("physical address overflow");
            *dst = self.read_u8(addr);
        }
    }

    pub fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (offset, &src) in buf.iter().enumerate() {
            let addr = paddr
                .checked_add(offset as u64)
                .expect("physical address overflow");
            self.write_u8(addr, src);
        }
    }
}
