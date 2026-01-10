use crate::chipset::A20GateHandle;

/// Applies chipset-level physical address transformations.
///
/// A20 masking is applied here (in the physical memory layer), not in the CPU, so that all
/// physical memory accesses (RAM and MMIO) observe the same behaviour.
#[derive(Clone)]
pub struct AddressFilter {
    a20: A20GateHandle,
}

impl AddressFilter {
    pub fn new(a20: A20GateHandle) -> Self {
        Self { a20 }
    }

    #[inline]
    pub fn filter_paddr(&self, paddr: u64) -> u64 {
        if self.a20.enabled() {
            paddr
        } else {
            // When A20 is disabled, the A20 address line is forced low, aliasing addresses
            // that differ only by bit 20.
            paddr & !(1 << 20)
        }
    }
}
