//! xHCI (USB 3.x) controller wired into the emulator's PCI + MMIO framework.
//!
//! The canonical xHCI controller model lives in `crates/aero-usb` (`aero_usb::xhci`). This module
//! keeps the emulator crate's `emulator::io::usb` path stable by providing thin integration glue
//! similar to the existing UHCI wrapper:
//! - PCI config-space identity + BAR probing/programming
//! - MMIO decode gating on PCI COMMAND.MEM
//! - DMA gating on PCI COMMAND.BME
//! - IRQ gating on PCI COMMAND.INTX_DISABLE

pub use aero_usb::xhci::{regs, XhciController};

use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};
use memory::MemoryBus;

use aero_devices::pci::profile::{
    PCI_DEVICE_ID_QEMU_XHCI, PCI_VENDOR_ID_REDHAT_QEMU, USB_XHCI_QEMU,
};
use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};

enum AeroUsbMemoryBus<'a> {
    Dma(&'a mut dyn MemoryBus),
    #[allow(dead_code)]
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
            AeroUsbMemoryBus::NoDma => {}
        }
    }
}

/// A PCI wrapper that exposes an xHCI controller via a single MMIO BAR.
///
/// This is intentionally minimal; it exists to integrate the shared `aero-usb` xHCI model into the
/// emulator crate's simplified PCI/MMIO traits.
pub struct XhciPciDevice {
    config: PciConfigSpace,
    pub mmio_base: u32,
    mmio_base_probe: bool,
    pub controller: XhciController,
}

impl XhciPciDevice {
    /// MMIO register block size (BAR0).
    const MMIO_BAR_SIZE: u32 = XhciController::MMIO_SIZE;

    /// PCI BDF used for interrupt line/pin programming.
    ///
    /// The emulator crate does not model a full PCI bus, but Windows/Linux still read the
    /// `Interrupt Line` register during enumeration. Use a stable (but otherwise arbitrary) BDF so
    /// the computed IRQ line matches other legacy devices.
    ///
    /// We choose the same canonical BDF as the `aero-devices` PCI profile (`USB_XHCI_QEMU`) so
    /// guest-visible enumeration (and the derived INTx line value) stays consistent across native
    /// integrations and the web runtime.
    const BDF: PciBdf = USB_XHCI_QEMU.bdf;

    pub fn new(controller: XhciController, mmio_base: u32) -> Self {
        let mut config = PciConfigSpace::new();

        // Ensure the BAR base is aligned to the window size so subsequent BAR probing/relocation
        // logic behaves consistently.
        let mmio_base = mmio_base & !(Self::MMIO_BAR_SIZE - 1) & 0xffff_fff0;

        // Vendor/device: QEMU-style xHCI (stable but not architecturally important for most guests
        // that bind based on class code).
        config.set_u16(0x00, PCI_VENDOR_ID_REDHAT_QEMU);
        config.set_u16(0x02, PCI_DEVICE_ID_QEMU_XHCI);
        config.write(0x08, 1, 0x01); // revision ID (AERO xHCI contract)
        config.set_u16(0x2c, PCI_VENDOR_ID_REDHAT_QEMU);
        config.set_u16(0x2e, PCI_DEVICE_ID_QEMU_XHCI);

        // Class code: serial bus / USB / xHCI.
        config.write(0x09, 1, 0x30); // prog IF (xHCI)
        config.write(0x0a, 1, 0x03); // subclass (USB)
        config.write(0x0b, 1, 0x0c); // class (serial bus)

        // BAR0 (MMIO) at 0x10.
        config.set_u32(0x10, mmio_base);

        // Interrupt pin/line: mirror the UHCI pattern (INTA on a conventional routed line).
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

    /// Advance the controller by 1ms.
    ///
    /// This ticks internal timers (MFINDEX + port reset/debounce) regardless of DMA, but gates
    /// transfer execution + event ring delivery on PCI `COMMAND.BME` so clearing bus mastering does
    /// not cause the controller model to interpret guest ring pointers using "open bus" reads.
    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        if self.bus_master_enabled() {
            let mut adapter = AeroUsbMemoryBus::Dma(mem);
            self.controller.tick_1ms(&mut adapter);
        } else {
            self.controller.tick_1ms_no_dma();
        }
    }
}

impl PciDevice for XhciPciDevice {
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

        let bar_off = 0x10u16;
        let bar_end = bar_off + 4;
        let overlaps_bar = offset < bar_end && end > bar_off;

        if overlaps_bar {
            let mask = !(Self::MMIO_BAR_SIZE - 1) & 0xffff_fff0;
            let bar_val = if self.mmio_base_probe {
                mask
            } else {
                self.mmio_base & 0xffff_fff0
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

        let bar_off = 0x10u16;
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

impl MmioDevice for XhciPciDevice {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        // `mem` is unused for MMIO reads today (xHCI DMA happens in `tick_1ms`), but is part of the
        // emulator's `MmioDevice` trait signature.
        let _ = mem;

        // Gate MMIO decoding on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => 0,
            };
        }

        self.controller.mmio_read(offset, size) as u32
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        let _ = mem;

        // Gate MMIO decoding on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }

        self.controller.mmio_write(offset, size, value as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PanicMem;

    impl MemoryBus for PanicMem {
        fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
            panic!("unexpected DMA read");
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
            panic!("unexpected DMA write");
        }
    }

    #[test]
    fn pci_command_bme_bit_gates_tick_1ms_dma() {
        let mut dev = XhciPciDevice::new(XhciController::new(), 0);

        // Enable MMIO decoding so we can configure interrupter registers, but leave BME disabled.
        dev.config_write(0x04, 2, 1 << 1);

        // Configure an event ring so servicing it would require DMA.
        // (We don't need valid guest memory backing; PanicMem will assert if DMA occurs.)
        dev.mmio_write(&mut PanicMem, regs::REG_INTR0_ERSTSZ, 4, 1);
        dev.mmio_write(&mut PanicMem, regs::REG_INTR0_ERSTBA_LO, 4, 0x1000);
        dev.mmio_write(&mut PanicMem, regs::REG_INTR0_ERDP_LO, 4, 0x2000);

        // With BME clear, ticking must not DMA.
        let mut mem = PanicMem;
        dev.tick_1ms(&mut mem);

        // Enable BME and verify ticking now attempts to DMA (event ring refresh).
        dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dev.tick_1ms(&mut mem);
        }));
        assert!(err.is_err());
    }

    #[test]
    fn pci_bar_probe_subword_reads_return_mask_bytes() {
        let mut dev = XhciPciDevice::new(XhciController::new(), 0x1000);

        dev.config_write(0x10, 4, 0xffff_ffff);
        let mask = dev.config_read(0x10, 4);
        assert_eq!(mask, !(XhciPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0);

        // Subword reads should return bytes from the probe mask (not the raw config bytes, which
        // are cleared during probing).
        assert_eq!(dev.config_read(0x10, 1), mask & 0xFF);
        assert_eq!(dev.config_read(0x11, 1), (mask >> 8) & 0xFF);
        assert_eq!(dev.config_read(0x12, 2), (mask >> 16) & 0xFFFF);
    }

    #[test]
    fn pci_bar_subword_write_updates_mmio_base() {
        let mut dev = XhciPciDevice::new(XhciController::new(), 0);

        // Program the BAR via a 16-bit write. This must update `mmio_base` and clamp to BAR
        // alignment.
        dev.config_write(0x10, 2, 0x1235);
        let expected = 0x0000_1235 & !(XhciPciDevice::MMIO_BAR_SIZE - 1) & 0xffff_fff0;
        assert_eq!(dev.mmio_base, expected);
        assert_eq!(dev.config_read(0x10, 4), expected);
    }

    #[test]
    fn pci_command_bme_clear_still_advances_port_reset_timers() {
        #[derive(Clone)]
        struct DummyDevice;

        impl aero_usb::UsbDeviceModel for DummyDevice {
            fn handle_control_request(
                &mut self,
                _setup: aero_usb::SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> aero_usb::ControlResponse {
                aero_usb::ControlResponse::Stall
            }
        }

        let mut dev = XhciPciDevice::new(XhciController::new(), 0);

        // Enable MMIO decoding so we can poke PORTSC, but keep BME disabled.
        dev.config_write(0x04, 2, 1 << 1);

        // Attach a dummy device so the port is connected; when reset completes the root hub model
        // enables the port automatically.
        dev.controller.attach_device(0, Box::new(DummyDevice));

        let portsc = regs::port::portsc_offset(0);
        dev.mmio_write(&mut PanicMem, portsc, 4, regs::PORTSC_PR);

        // With BME clear, ticking must not DMA but should still run port reset timers.
        let mut mem = PanicMem;
        for _ in 0..50 {
            dev.tick_1ms(&mut mem);
        }

        let portsc_val = dev.mmio_read(&mut mem, portsc, 4);
        assert_eq!(
            portsc_val & regs::PORTSC_PR,
            0,
            "port reset should self-clear"
        );
        assert_ne!(
            portsc_val & regs::PORTSC_PED,
            0,
            "port should be enabled after reset"
        );
    }
}
