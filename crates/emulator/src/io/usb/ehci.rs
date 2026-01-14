//! EHCI (USB 2.0) controller wired into the emulator's PCI + MMIO framework.
//!
//! The controller implementation itself lives in the shared `aero-usb` crate; this module is just
//! thin integration glue (PCI config space, MMIO decode gating, DMA gating, IRQ gating).
//!
//! Design notes + emulator/runtime contracts: see `docs/usb-ehci.md`.

pub use aero_usb::ehci::{regs, EhciController};

use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};

use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};
use memory::MemoryBus;

enum AeroUsbMemoryBus<'a> {
    Dma(&'a mut dyn MemoryBus),
    NoDma,
}

impl aero_usb::MemoryBus for AeroUsbMemoryBus<'_> {
    fn dma_enabled(&self) -> bool {
        matches!(self, AeroUsbMemoryBus::Dma(_))
    }

    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        match self {
            AeroUsbMemoryBus::Dma(inner) => inner.read_physical(paddr, buf),
            AeroUsbMemoryBus::NoDma => buf.fill(0xFF),
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        match self {
            AeroUsbMemoryBus::Dma(inner) => inner.write_physical(paddr, buf),
            AeroUsbMemoryBus::NoDma => {
                let _ = (paddr, buf);
            }
        }
    }
}

/// A PCI wrapper that exposes an EHCI controller as a PCI MMIO function.
pub struct EhciPciDevice {
    config: PciConfigSpace,
    pub mmio_base: u32,
    mmio_base_probe: bool,
    pub controller: EhciController,
}

impl EhciPciDevice {
    const MMIO_BAR_SIZE: u32 = regs::MMIO_SIZE;
    const MMIO_BAR_OFFSET: u16 = 0x10; // BAR0

    // Arbitrary, stable BDF for config-space INTx line/pin programming.
    const BDF: PciBdf = PciBdf::new(0, 1, 3);

    pub fn new(controller: EhciController, mmio_base: u32) -> Self {
        let mut config = PciConfigSpace::new();

        // Vendor/device: Intel ICH9-ish EHCI.
        config.set_u16(0x00, 0x8086);
        config.set_u16(0x02, 0x293a);

        // Class code: serial bus / USB / EHCI.
        config.set_u8(0x09, 0x20); // prog IF
        config.set_u8(0x0a, 0x03); // subclass
        config.set_u8(0x0b, 0x0c); // class

        // BAR0 (MMIO).
        let mmio_base = mmio_base & !(Self::MMIO_BAR_SIZE - 1) & 0xffff_fff0;
        config.set_u32(Self::MMIO_BAR_OFFSET as usize, mmio_base);

        // Interrupt pin/line match the canonical PCI INTx router configuration.
        let pin = PciInterruptPin::IntA;
        config.set_u8(0x3d, pin.to_config_u8());

        let router = PciIntxRouter::new(PciIntxRouterConfig::default());
        let gsi = router.gsi_for_intx(Self::BDF, pin);
        let line = u8::try_from(gsi).unwrap_or(0xFF);
        config.write(0x3c, 1, u32::from(line));

        Self {
            config,
            mmio_base,
            mmio_base_probe: false,
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
        self.controller.irq_level()
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        // Gate schedule DMA on PCI command Bus Master Enable (bit 2).
        //
        // EHCI schedule processing reads guest memory (periodic/async lists). When the guest clears
        // COMMAND.BME, the controller must not perform bus-master DMA, but it must still advance
        // internal timers (frame index, root hub reset/debounce).
        let mut adapter = if self.bus_master_enabled() {
            AeroUsbMemoryBus::Dma(mem)
        } else {
            AeroUsbMemoryBus::NoDma
        };
        self.controller.tick_1ms(&mut adapter);
    }
}

impl PciDevice for EhciPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }
        let Some(end) = offset.checked_add(size as u16) else {
            return 0;
        };
        if end as usize > 256 {
            return 0;
        }

        let bar_off = Self::MMIO_BAR_OFFSET;
        let bar_end = bar_off + 4;
        let overlaps_bar = offset < bar_end && end > bar_off;

        if overlaps_bar {
            let mask = !(Self::MMIO_BAR_SIZE - 1) & 0xffff_fff0;
            let bar_val = if self.mmio_base_probe {
                mask
            } else {
                self.mmio_base
            };

            let mut out = 0u32;
            for i in 0..size {
                let byte_off = offset + i as u16;
                let byte = if (bar_off..bar_end).contains(&byte_off) {
                    let shift = u32::from(byte_off - bar_off) * 8;
                    (bar_val >> shift) & 0xFF
                } else {
                    self.config.read(byte_off, 1) & 0xFF
                };
                out |= byte << (8 * i);
            }
            return out;
        }

        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let Some(end) = offset.checked_add(size as u16) else {
            return;
        };
        if end as usize > 256 {
            return;
        }

        let bar_off = Self::MMIO_BAR_OFFSET;
        let bar_end = bar_off + 4;
        let overlaps_bar = offset < bar_end && end > bar_off;

        if overlaps_bar {
            // PCI BAR probing uses an all-ones write to discover the size mask.
            if offset == bar_off && size == 4 && value == 0xffff_ffff {
                self.mmio_base_probe = true;
                self.mmio_base = 0;
                self.config.write(bar_off, 4, 0);
                return;
            }

            self.mmio_base_probe = false;
            self.config.write(offset, size, value);

            let raw = self.config.read(bar_off, 4);
            let base_mask = !(Self::MMIO_BAR_SIZE - 1) & 0xffff_fff0;
            let base = raw & base_mask;
            self.mmio_base = base;
            self.config.write(bar_off, 4, base);
            return;
        }
        self.config.write(offset, size, value);
    }
}

impl MmioDevice for EhciPciDevice {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        // Gate MMIO decoding on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        }
        self.controller.mmio_read(offset, size)
    }

    fn mmio_write(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        // Gate MMIO decoding on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }
        self.controller.mmio_write(offset, size, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::pci::MmioDevice;

    struct NoMmioMem;

    impl MemoryBus for NoMmioMem {
        fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
            panic!("unexpected guest memory read from MMIO path");
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
            panic!("unexpected guest memory write from MMIO path");
        }
    }

    #[test]
    fn pci_command_mem_bit_gates_mmio_access() {
        let mut mem = NoMmioMem;
        let mut dev = EhciPciDevice::new(EhciController::new(), 0x1000);

        // Enable MMIO decoding to establish a baseline value.
        dev.config_write(0x04, 2, 1 << 1);
        let baseline = dev.mmio_read(&mut mem, regs::REG_USBCMD, 4);

        // Disable MMIO decoding: reads float high, writes ignored.
        dev.config_write(0x04, 2, 0);
        dev.mmio_write(&mut mem, regs::REG_USBCMD, 4, baseline ^ regs::USBCMD_RS);
        assert_eq!(dev.mmio_read(&mut mem, regs::REG_USBCMD, 4), u32::MAX);

        // Re-enable MMIO decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 1);
        assert_eq!(dev.mmio_read(&mut mem, regs::REG_USBCMD, 4), baseline);

        // Writes should apply once MEM is enabled.
        let toggled = baseline ^ regs::USBCMD_RS;
        dev.mmio_write(&mut mem, regs::REG_USBCMD, 4, toggled);
        assert_eq!(dev.mmio_read(&mut mem, regs::REG_USBCMD, 4), toggled);
    }

    #[test]
    fn pci_command_intx_disable_bit_masks_irq_level() {
        let mut mem = NoMmioMem;
        let mut dev = EhciPciDevice::new(EhciController::new(), 0x1000);

        // Enable MMIO decoding so we can program USBINTR.
        dev.config_write(0x04, 2, 1 << 1);
        dev.mmio_write(&mut mem, regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
        dev.controller.set_usbsts_bits(regs::USBSTS_USBINT);

        assert!(dev.controller.irq_level());
        assert!(dev.irq_level());

        // Disable legacy INTx delivery via PCI command bit 10.
        dev.config_write(0x04, 2, (1 << 1) | (1 << 10));
        assert!(dev.controller.irq_level());
        assert!(!dev.irq_level());
    }

    #[test]
    fn pci_bar_probe_subword_reads_return_mask_bytes() {
        let mut dev = EhciPciDevice::new(EhciController::new(), 0x1000);

        dev.config_write(EhciPciDevice::MMIO_BAR_OFFSET, 4, 0xffff_ffff);
        let mask = dev.config_read(EhciPciDevice::MMIO_BAR_OFFSET, 4);
        assert_eq!(mask, !(EhciPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0);

        // Subword reads should return bytes from the probe mask (not the raw config bytes, which
        // are cleared during probing).
        assert_eq!(
            dev.config_read(EhciPciDevice::MMIO_BAR_OFFSET, 1),
            mask & 0xFF
        );
        assert_eq!(
            dev.config_read(EhciPciDevice::MMIO_BAR_OFFSET + 1, 1),
            (mask >> 8) & 0xFF
        );
        assert_eq!(
            dev.config_read(EhciPciDevice::MMIO_BAR_OFFSET + 2, 2),
            (mask >> 16) & 0xFFFF
        );
    }

    #[test]
    fn pci_bar_subword_write_updates_mmio_base() {
        let mut dev = EhciPciDevice::new(EhciController::new(), 0);

        // Program the BAR via a 16-bit write. This must update `mmio_base` and clamp to BAR
        // alignment.
        dev.config_write(EhciPciDevice::MMIO_BAR_OFFSET, 2, 0x1235);
        let expected = 0x0000_1235 & !(EhciPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0;
        assert_eq!(dev.mmio_base, expected);
        assert_eq!(dev.config_read(EhciPciDevice::MMIO_BAR_OFFSET, 4), expected);
    }
}
