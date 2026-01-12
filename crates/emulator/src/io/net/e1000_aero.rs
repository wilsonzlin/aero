//! E1000 (82540EM-ish) NIC integration for the emulator crate.
//!
//! The actual device model lives in `aero-net-e1000`. This module wires it into
//! the emulator's minimal PCI/MMIO traits and preserves a small compatibility
//! shim for higher-level networking helpers (e.g. tracing).

pub use aero_net_e1000::E1000Device;

use crate::io::pci::{MmioDevice, PciDevice};

use memory::MemoryBus;

/// PCI wrapper exposing an [`E1000Device`] through the emulator's device traits.
#[derive(Debug)]
pub struct E1000PciDevice {
    pub nic: E1000Device,
}

impl E1000PciDevice {
    pub fn new(nic: E1000Device) -> Self {
        Self { nic }
    }

    pub fn irq_level(&self) -> bool {
        self.nic.irq_level()
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        self.nic.poll(mem);
    }

    pub fn receive_frame(&mut self, mem: &mut dyn MemoryBus, frame: &[u8]) {
        self.nic.receive_frame(mem, frame);
    }

    pub fn pop_tx_frame(&mut self) -> Option<Vec<u8>> {
        self.nic.pop_tx_frame()
    }
}

impl PciDevice for E1000PciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        self.nic.pci_config_read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        self.nic.pci_config_write(offset, size, value);
    }
}

impl MmioDevice for E1000PciDevice {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        self.nic.mmio_read(offset, size)
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        // `MmioDevice` provides guest RAM access, but the device's register interface must be
        // usable behind `memory::MmioHandler`-based buses that do not. Perform a register-only
        // write here and explicitly poll to keep the old "writes kick DMA soon" behavior.
        self.nic.mmio_write_reg(offset, size, value);
        self.nic.poll(mem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct VecMemory {
        data: Vec<u8>,
    }

    impl VecMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
            let start = usize::try_from(paddr).expect("paddr too large for VecMemory");
            let end = start.checked_add(len).expect("address wrap");
            assert!(end <= self.data.len(), "out-of-bounds physical access");
            start..end
        }
    }

    impl MemoryBus for VecMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let range = self.range(paddr, buf.len());
            buf.copy_from_slice(&self.data[range]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let range = self.range(paddr, buf.len());
            self.data[range].copy_from_slice(buf);
        }
    }

    fn build_test_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(aero_net_e1000::MIN_L2_FRAME_LEN + payload.len());
        frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        frame.extend_from_slice(&0x0800u16.to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn wrapper_config_space_bar0_probe_roundtrip() {
        let mut dev = E1000PciDevice::new(E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]));
        dev.config_write(0x10, 4, 0xffff_ffff);
        let mask = dev.config_read(0x10, 4);
        assert_eq!(mask, (!(aero_net_e1000::E1000_MMIO_SIZE - 1)) & 0xffff_fff0);

        dev.config_write(0x14, 4, 0xffff_ffff);
        let mask = dev.config_read(0x14, 4);
        assert_eq!(
            mask,
            ((!(aero_net_e1000::E1000_IO_SIZE - 1)) & 0xffff_fffc) | 0x1
        );
    }

    #[test]
    fn wrapper_mmio_and_dma_paths_work() {
        let mut mem = VecMemory::new(0x20_000);
        let mut dev = E1000PciDevice::new(E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]));

        // Set up a tiny RX ring (2 descriptors => 1 usable due to head/tail semantics).
        let ring_base = 0x1000u64;
        let buf_addr = 0x2000u64;

        // Descriptor 0: buffer at 0x2000.
        mem.write_physical(ring_base, &buf_addr.to_le_bytes());

        // Program ring.
        dev.mmio_write(&mut mem, 0x2800, 4, ring_base as u32); // RDBAL
        dev.mmio_write(&mut mem, 0x2808, 4, 2 * 16); // RDLEN
        dev.mmio_write(&mut mem, 0x2810, 4, 0); // RDH
        dev.mmio_write(&mut mem, 0x2818, 4, 1); // RDT
        dev.mmio_write(&mut mem, 0x0100, 4, 1 << 1); // RCTL.EN

        let frame = build_test_frame(b"hi");
        dev.receive_frame(&mut mem, &frame);

        let mut out = vec![0u8; frame.len()];
        mem.read_physical(buf_addr, &mut out);
        assert_eq!(out, frame);
    }
}
