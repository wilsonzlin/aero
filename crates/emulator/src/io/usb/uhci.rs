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

    pub fn irq_level(&self) -> bool {
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
        let Some(offset) = port.checked_sub(self.io_base) else {
            return u32::MAX;
        };
        self.controller.io_read(offset, size)
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        let Some(offset) = port.checked_sub(self.io_base) else {
            return;
        };
        self.controller.io_write(offset, size, val);
    }
}
