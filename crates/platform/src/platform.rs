use crate::address_filter::AddressFilter;
use crate::chipset::ChipsetState;
use crate::io::IoPortBus;
use crate::memory::MemoryBus;

pub struct Platform {
    pub chipset: ChipsetState,
    pub io: IoPortBus,
    pub memory: MemoryBus,
}

impl Platform {
    pub fn new(ram_size: usize) -> Self {
        let chipset = ChipsetState::new(false);
        let filter = AddressFilter::new(chipset.a20());
        Self {
            chipset,
            io: IoPortBus::new(),
            memory: MemoryBus::new(filter, ram_size),
        }
    }
}
