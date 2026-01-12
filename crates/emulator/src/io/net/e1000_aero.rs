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

    fn command(&self) -> u16 {
        self.nic.pci_config_read(0x04, 2) as u16
    }

    fn mem_space_enabled(&self) -> bool {
        (self.command() & (1 << 1)) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        // Gate DMA on PCI COMMAND.BME (bit 2) to avoid touching guest memory before the guest
        // explicitly enables bus mastering during enumeration.
        (self.command() & (1 << 2)) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.nic.irq_level()
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        if !self.bus_master_enabled() {
            return;
        }
        self.nic.poll(mem);
    }

    pub fn receive_frame(&mut self, mem: &mut dyn MemoryBus, frame: &[u8]) {
        // Always accept host RX frames, but only flush them into guest memory once the guest has
        // enabled PCI bus mastering.
        self.nic.enqueue_rx_frame(frame.to_vec());
        self.poll(mem);
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
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => 0,
            };
        }
        self.nic.mmio_read(offset, size)
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }
        // Preserve the legacy behavior for `MmioDevice` callers: register writes are immediately
        // followed by a poll() so doorbells kick DMA "soon", while still respecting PCI
        // COMMAND.BME gating (no DMA/polling until the guest enables bus mastering).
        self.nic.mmio_write_reg(offset, size, value);
        self.poll(mem);
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
    fn wrapper_gates_mmio_on_pci_command_mem_bit() {
        let mut mem = VecMemory::new(0x1000);
        let mut dev = E1000PciDevice::new(E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]));

        // With COMMAND.MEM clear, reads must float high and writes must be ignored.
        assert_eq!(dev.mmio_read(&mut mem, 0x0000, 4), u32::MAX);
        dev.mmio_write(&mut mem, 0x00d0, 4, 0x1234_5678);

        // Enable MMIO decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 1);
        assert_ne!(dev.mmio_read(&mut mem, 0x0000, 4), u32::MAX);
        assert_eq!(dev.mmio_read(&mut mem, 0x00d0, 4), 0);

        // Writes should apply once MEM is enabled.
        dev.mmio_write(&mut mem, 0x00d0, 4, 0x1234_5678);
        assert_eq!(dev.mmio_read(&mut mem, 0x00d0, 4), 0x1234_5678);
    }

    #[test]
    fn wrapper_gates_intx_on_pci_command_intx_disable_bit() {
        let mut mem = VecMemory::new(0x1000);
        let mut dev = E1000PciDevice::new(E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]));

        // Enable MMIO decoding (but not bus mastering).
        dev.config_write(0x04, 2, 1 << 1);

        // Enable and assert TXDW interrupt via IMS + ICS.
        dev.mmio_write(&mut mem, 0x00d0, 4, aero_net_e1000::ICR_TXDW);
        dev.mmio_write(&mut mem, 0x00c8, 4, aero_net_e1000::ICR_TXDW);

        assert!(dev.nic.irq_level(), "device model asserts IRQ line");
        assert!(dev.irq_level(), "wrapper forwards IRQ when INTX is enabled");

        // Disable legacy INTx delivery via PCI command bit 10.
        dev.config_write(0x04, 2, (1 << 1) | (1 << 10));
        assert!(
            !dev.nic.irq_level(),
            "device model must gate IRQ when INTX is disabled"
        );
        assert!(
            !dev.irq_level(),
            "wrapper must suppress IRQ when INTX is disabled"
        );
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

        // PCI command register gates both decode (MEM) and DMA (BME) on real hardware.
        dev.config_write(0x04, 2, 0x6);

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
