use crate::memory::GuestMemory;
use crate::pci::VirtioPciDevice;

/// BAR0 MMIO adapter for [`VirtioPciDevice`].
///
/// The virtio-pci transport exposes BAR0 as a byte-addressable region. Aero's `memory` bus invokes
/// MMIO handlers with naturally-aligned access sizes in {1,2,4,8}. This wrapper converts between
/// that `u64`-based interface and the transport's `bar0_read/bar0_write` byte-slice API.
pub struct VirtioBar0Mmio<M: GuestMemory> {
    pub pci: VirtioPciDevice,
    pub mem: M,
}

impl<M: GuestMemory> VirtioBar0Mmio<M> {
    pub fn new(pci: VirtioPciDevice, mem: M) -> Self {
        Self { pci, mem }
    }

    pub fn poll(&mut self) {
        self.pci.poll(&mut self.mem);
    }
}

impl<M: GuestMemory> aero_memory::MmioHandler for VirtioBar0Mmio<M> {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let size = size.clamp(1, 8);
        let mut buf = [0u8; 8];
        self.pci.bar0_read(offset, &mut buf[..size]);
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let size = size.clamp(1, 8);
        let bytes = value.to_le_bytes();
        self.pci.bar0_write(offset, &bytes[..size]);
    }
}
