use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};
use memory::MemoryBus;

use super::HdaController;

/// PCI wrapper for the Intel HD Audio controller.
///
/// This provides the PCI config-space identity and BAR plumbing required for
/// OS drivers (Windows 7 `hdaudio.sys`, Linux `snd-hda-intel`) to bind the
/// controller.
#[derive(Debug)]
pub struct HdaPciDevice {
    config: PciConfigSpace,
    bar0: u32,
    bar0_probe: bool,
    pub controller: HdaController,
}

impl HdaPciDevice {
    /// Size of the HDA MMIO BAR used by common Intel ICH controllers.
    pub const MMIO_BAR_SIZE: u32 = 0x4000;

    pub fn new(controller: HdaController, bar0: u32) -> Self {
        let mut config = PciConfigSpace::new();

        // Vendor/device: Intel ICH6 HD Audio.
        config.set_u16(0x00, 0x8086);
        config.set_u16(0x02, 0x2668);

        // Revision ID.
        config.write(0x08, 1, 0x01);

        // Class code: Multimedia controller / HD Audio.
        config.write(0x09, 1, 0x00); // prog IF
        config.write(0x0a, 1, 0x03); // subclass (HD Audio)
        config.write(0x0b, 1, 0x04); // class (multimedia)

        // Subsystem vendor/device.
        config.set_u16(0x2c, 0x8086);
        config.set_u16(0x2e, 0x2668);

        // BAR0: Non-prefetchable 32-bit MMIO.
        let bar0 = bar0 & 0xffff_fff0;
        config.set_u32(0x10, bar0);

        // Interrupt pin INTA#.
        config.write(0x3d, 1, 1);

        Self {
            config,
            bar0,
            bar0_probe: false,
            controller,
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn mem_space_enabled(&self) -> bool {
        (self.command() & (1 << 1)) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        (self.command() & (1 << 2)) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.controller.irq_line()
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        // Gate DMA on PCI command Bus Master Enable (bit 2).
        //
        // HDA uses bus mastering for CORB/RIRB and stream DMA. When the guest clears COMMAND.BME,
        // the controller must not touch guest memory.
        if !self.bus_master_enabled() {
            return;
        }
        self.controller.poll(mem);
    }
}

impl PciDevice for HdaPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x10 && size == 4 {
            return if self.bar0_probe {
                !(Self::MMIO_BAR_SIZE - 1) & 0xffff_fff0
            } else {
                self.bar0
            };
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if offset == 0x10 && size == 4 {
            if value == 0xffff_ffff {
                self.bar0_probe = true;
                self.bar0 = 0;
            } else {
                self.bar0_probe = false;
                self.bar0 = value & 0xffff_fff0;
            }
            self.config.write(offset, size, self.bar0);
            return;
        }
        self.config.write(offset, size, value);
    }
}

impl MmioDevice for HdaPciDevice {
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
        self.controller.mmio_read(offset as u32, size) as u32
    }

    fn mmio_write(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }
        self.controller
            .mmio_write(offset as u32, size, value as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::audio::hda::regs::*;

    #[test]
    fn pci_config_space_reports_intel_hda_and_supports_bar_probe() {
        let mut dev = HdaPciDevice::new(HdaController::new(), 0xfebf_0000);

        assert_eq!(dev.config_read(0x00, 2), 0x8086);
        assert_eq!(dev.config_read(0x02, 2), 0x2668);
        assert_eq!(dev.config_read(0x0b, 1), 0x04);
        assert_eq!(dev.config_read(0x0a, 1), 0x03);
        assert_eq!(dev.config_read(0x09, 1), 0x00);

        dev.config_write(0x10, 4, 0xffff_ffff);
        assert_eq!(
            dev.config_read(0x10, 4),
            !(HdaPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0
        );

        dev.config_write(0x10, 4, 0xdead_beef);
        assert_eq!(dev.config_read(0x10, 4), 0xdead_bee0);
    }

    #[test]
    fn mmio_access_via_wrapper_updates_controller_state() {
        #[derive(Clone, Debug)]
        struct Mem(Vec<u8>);

        impl Mem {
            fn new(size: usize) -> Self {
                Self(vec![0; size])
            }
        }

        impl MemoryBus for Mem {
            fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
                let start = paddr as usize;
                buf.copy_from_slice(&self.0[start..start + buf.len()]);
            }

            fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
                let start = paddr as usize;
                self.0[start..start + buf.len()].copy_from_slice(buf);
            }
        }

        let mut mem = Mem::new(0x10000);
        let mut dev = HdaPciDevice::new(HdaController::new(), 0xfebf_0000);

        // Enable PCI MMIO decoding.
        dev.config_write(0x04, 2, 1 << 1);

        // Bring controller out of reset via MMIO.
        dev.mmio_write(&mut mem, HDA_GCTL as u64, 4, GCTL_CRST);
        assert_eq!(
            dev.mmio_read(&mut mem, HDA_GCTL as u64, 4) & GCTL_CRST,
            GCTL_CRST
        );

        // Codec 0 should be present in STATESTS once out of reset.
        assert_eq!(dev.mmio_read(&mut mem, HDA_STATESTS as u64, 2) & 0x1, 0x1);
    }

    #[test]
    fn pci_wrapper_gates_hda_mmio_on_pci_command_mem_bit() {
        #[derive(Clone, Debug)]
        struct Mem(Vec<u8>);

        impl Mem {
            fn new(size: usize) -> Self {
                Self(vec![0; size])
            }
        }

        impl MemoryBus for Mem {
            fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
                let start = paddr as usize;
                buf.copy_from_slice(&self.0[start..start + buf.len()]);
            }

            fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
                let start = paddr as usize;
                self.0[start..start + buf.len()].copy_from_slice(buf);
            }
        }

        let mut mem = Mem::new(0x10000);
        let mut dev = HdaPciDevice::new(HdaController::new(), 0xfebf_0000);

        // With COMMAND.MEM clear, reads float high and writes are ignored.
        assert_eq!(dev.mmio_read(&mut mem, HDA_GCTL as u64, 4), u32::MAX);
        dev.mmio_write(&mut mem, HDA_GCTL as u64, 4, GCTL_CRST);

        // Enable MMIO decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 1);
        assert_ne!(dev.mmio_read(&mut mem, HDA_GCTL as u64, 4), u32::MAX);
        assert_eq!(dev.mmio_read(&mut mem, HDA_GCTL as u64, 4) & GCTL_CRST, 0);
    }

    #[test]
    fn pci_wrapper_gates_hda_dma_on_pci_command_bme_bit() {
        struct PanicMem;

        impl MemoryBus for PanicMem {
            fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
                panic!("unexpected DMA read");
            }

            fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
                panic!("unexpected DMA write");
            }
        }

        let mut dev = HdaPciDevice::new(HdaController::new(), 0xfebf_0000);
        let mut mem = PanicMem;

        // Enable MMIO decoding so we can program the controller, but leave BME disabled.
        dev.config_write(0x04, 2, 1 << 1);

        // Bring controller out of reset and enable the position buffer so poll() will write to guest
        // memory when bus mastering is enabled.
        dev.mmio_write(&mut mem, HDA_GCTL as u64, 4, GCTL_CRST);
        dev.mmio_write(&mut mem, HDA_DPUBASE as u64, 4, 0);
        dev.mmio_write(&mut mem, HDA_DPLBASE as u64, 4, 0x7000 | DPLBASE_ENABLE);

        // With BME clear, wrapper.poll must not touch guest memory.
        dev.poll(&mut mem);

        // Enable bus mastering and verify polling attempts a memory access (position buffer write).
        dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dev.poll(&mut mem);
        }));
        assert!(err.is_err());
    }
}
