//! UHCI (USB 1.1) controller wired into the emulator's PCI + PortIO framework.
//!
//! The controller implementation itself lives in the shared `aero-usb` crate; this module is just
//! thin integration glue.

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hid::mouse::UsbHidMouseHandle;
pub use aero_usb::uhci::{regs, UhciController};

use crate::io::pci::{PciConfigSpace, PciDevice};
use crate::io::PortIO;
use memory::MemoryBus;

struct AeroUsbMemoryBus<'a> {
    inner: &'a mut dyn MemoryBus,
}

impl aero_usb::MemoryBus for AeroUsbMemoryBus<'_> {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.inner.read_physical(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.inner.write_physical(paddr, buf);
    }
}

/// A PCI wrapper that exposes a UHCI controller as an Intel PIIX3-style device.
pub struct UhciPciDevice {
    config: PciConfigSpace,
    pub io_base: u16,
    io_base_probe: bool,
    pub controller: UhciController,
}

impl UhciPciDevice {
    const IO_BAR_SIZE: u32 = 0x20;

    pub fn new(controller: UhciController, io_base: u16) -> Self {
        let mut config = PciConfigSpace::new();

        // Vendor/device: Intel PIIX3 UHCI.
        config.set_u16(0x00, 0x8086);
        config.set_u16(0x02, 0x7020);

        // Class code: serial bus / USB / UHCI.
        config.write(0x09, 1, 0x00); // prog IF
        config.write(0x0a, 1, 0x03); // subclass
        config.write(0x0b, 1, 0x0c); // class

        // BAR4 (I/O) at 0x20.
        config.set_u32(0x20, (io_base as u32) | 0x1);

        // Interrupt line: canonical profile routes 00:01.2 INTA# to IRQ 11.
        config.write(0x3c, 1, 0x0b);

        // Interrupt pin INTA#.
        config.write(0x3d, 1, 1);

        Self {
            config,
            io_base,
            io_base_probe: false,
            controller,
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn io_space_enabled(&self) -> bool {
        (self.command() & (1 << 0)) != 0
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

    pub fn new_with_hid(io_base: u16) -> (Self, UsbHidKeyboardHandle, UsbHidMouseHandle) {
        let mut controller = UhciController::new();
        let keyboard = UsbHidKeyboardHandle::new();
        let mouse = UsbHidMouseHandle::new();
        controller.hub_mut().attach(0, Box::new(keyboard.clone()));
        controller.hub_mut().attach(1, Box::new(mouse.clone()));
        (Self::new(controller, io_base), keyboard, mouse)
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        // Gate schedule DMA on PCI command Bus Master Enable (bit 2).
        //
        // UHCI schedule processing reads/writes guest memory (frame list + TD/QH chain). When the
        // guest clears COMMAND.BME, the controller must not perform bus-master DMA.
        if !self.bus_master_enabled() {
            return;
        }
        let mut adapter = AeroUsbMemoryBus { inner: mem };
        self.controller.tick_1ms(&mut adapter);
    }
}

impl PciDevice for UhciPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x20 && size == 4 {
            return if self.io_base_probe {
                // BAR4: 32-byte I/O window.
                (!(Self::IO_BAR_SIZE - 1) & 0xffff_fffc) | 0x1
            } else {
                u32::from(self.io_base) | 0x1
            };
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if offset == 0x20 && size == 4 {
            if value == 0xffff_ffff {
                self.io_base_probe = true;
                self.io_base = 0;
                self.config.write(offset, size, 0);
                return;
            }

            self.io_base_probe = false;
            let value = value as u16;
            let io_base = value & !0x3 & !((Self::IO_BAR_SIZE as u16) - 1);
            self.io_base = io_base;
            let encoded = u32::from(self.io_base) | 0x1;
            self.config.write(offset, size, encoded);
            return;
        }
        self.config.write(offset, size, value);
    }
}

impl PortIO for UhciPciDevice {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        }
        let Some(offset) = port.checked_sub(self.io_base) else {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        };
        self.controller.io_read(offset, size)
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        // Gate I/O decoding on PCI command I/O Space Enable (bit 0).
        if !self.io_space_enabled() {
            return;
        }
        let Some(offset) = port.checked_sub(self.io_base) else {
            return;
        };
        self.controller.io_write(offset, size, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::PortIO;

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
    fn pci_command_io_bit_gates_port_io_access() {
        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // COMMAND.IO is clear by default: reads float high, writes ignored.
        dev.port_write(0x1000 + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));
        assert_eq!(dev.port_read(0x1000 + regs::REG_USBCMD, 2), 0xffff);

        // Enable I/O space decoding and verify the earlier write did not take effect.
        dev.config_write(0x04, 2, 1 << 0);
        assert_eq!(
            dev.port_read(0x1000 + regs::REG_USBCMD, 2) as u16,
            regs::USBCMD_MAXP,
        );

        // Writes should apply once IO decoding is enabled.
        dev.port_write(0x1000 + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));
        assert_eq!(
            dev.port_read(0x1000 + regs::REG_USBCMD, 2) as u16,
            regs::USBCMD_RS
        );
    }

    #[test]
    fn pci_command_bme_bit_gates_tick_1ms_dma() {
        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // Enable I/O decoding so we can program the controller, but leave BME disabled.
        dev.config_write(0x04, 2, 1 << 0);

        dev.port_write(0x1000 + regs::REG_FLBASEADD, 4, 0x1000);
        dev.port_write(0x1000 + regs::REG_USBCMD, 2, u32::from(regs::USBCMD_RS));

        // With BME clear, no DMA should occur.
        let mut mem = PanicMem;
        dev.tick_1ms(&mut mem);

        // Enable BME and verify the schedule engine attempts to DMA.
        dev.config_write(0x04, 2, (1 << 0) | (1 << 2));
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dev.tick_1ms(&mut mem);
        }));
        assert!(err.is_err());
    }

    #[test]
    fn pci_command_intx_disable_bit_masks_irq_level() {
        let mut dev = UhciPciDevice::new(UhciController::new(), 0x1000);

        // Enable IO decoding so we can program USBINTR.
        dev.config_write(0x04, 2, 1 << 0);
        dev.port_write(0x1000 + regs::REG_USBINTR, 2, u32::from(regs::USBINTR_IOC));
        dev.controller.set_usbsts_bits(regs::USBSTS_USBINT);

        assert!(dev.controller.irq_level());
        assert!(dev.irq_level());

        // Disable legacy INTx delivery via PCI command bit 10.
        dev.config_write(0x04, 2, (1 << 0) | (1 << 10));
        assert!(dev.controller.irq_level());
        assert!(!dev.irq_level());
    }
}
