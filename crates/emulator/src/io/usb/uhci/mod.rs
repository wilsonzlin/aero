//! Minimal UHCI (USB 1.1) host controller.
//!
//! This implementation is focused on:
//! - Frame list / QH / TD walking
//! - Endpoint 0 control transfers
//! - Interrupt IN polling for HID reports
//! - Root hub with two ports exposed via PORTSC registers

mod schedule;

pub mod regs;

use memory::MemoryBus;

use crate::io::pci::{PciConfigSpace, PciDevice};
use crate::io::usb::hid::keyboard::UsbHidKeyboardHandle;
use crate::io::usb::hid::mouse::UsbHidMouseHandle;
use crate::io::usb::hub::RootHub;
use crate::io::PortIO;

use regs::*;
use schedule::{process_frame, ScheduleContext};

pub struct UhciController {
    regs: UhciRegs,
    hub: RootHub,
    irq_level: bool,
}

impl UhciController {
    pub fn new() -> Self {
        Self {
            regs: UhciRegs::new(),
            hub: RootHub::new(),
            irq_level: false,
        }
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn hub_mut(&mut self) -> &mut RootHub {
        &mut self.hub
    }

    pub fn regs(&self) -> &UhciRegs {
        &self.regs
    }

    fn reset(&mut self) {
        self.regs = UhciRegs::new();
        self.irq_level = false;
    }

    fn update_irq(&mut self) {
        let mut pending = false;
        if self.regs.usbsts & USBSTS_USBINT != 0
            && (self.regs.usbintr & (USBINTR_IOC | USBINTR_SHORT_PACKET)) != 0
        {
            pending = true;
        }
        if self.regs.usbsts & USBSTS_USBERRINT != 0 && self.regs.usbintr & USBINTR_TIMEOUT_CRC != 0
        {
            pending = true;
        }
        if self.regs.usbsts & USBSTS_RESUMEDETECT != 0 && self.regs.usbintr & USBINTR_RESUME != 0 {
            pending = true;
        }
        self.irq_level = pending;
    }

    fn io_read(&self, offset: u16, size: usize) -> u32 {
        match (offset, size) {
            (REG_USBCMD, 2) => self.regs.usbcmd as u32,
            (REG_USBSTS, 2) => self.regs.usbsts as u32,
            (REG_USBINTR, 2) => self.regs.usbintr as u32,
            (REG_FRNUM, 2) => self.regs.frnum as u32,
            (REG_FLBASEADD, 4) => self.regs.flbaseadd,
            (REG_SOFMOD, 1) => self.regs.sofmod as u32,
            (REG_PORTSC1, 2) => self.hub.read_portsc(0) as u32,
            (REG_PORTSC2, 2) => self.hub.read_portsc(1) as u32,
            _ => u32::MAX,
        }
    }

    fn io_write(&mut self, offset: u16, size: usize, value: u32) {
        match (offset, size) {
            (REG_USBCMD, 2) => {
                let value = value as u16;
                if value & USBCMD_HCRESET != 0 {
                    self.reset();
                    return;
                }
                self.regs.usbcmd = value & (USBCMD_RS | USBCMD_CF | USBCMD_MAXP);
                self.regs.update_halted();
            }
            (REG_USBSTS, 2) => {
                // Write-1-to-clear (bits 0..2).
                let w1c = value as u16 & (USBSTS_USBINT | USBSTS_USBERRINT | USBSTS_RESUMEDETECT);
                self.regs.usbsts &= !w1c;
            }
            (REG_USBINTR, 2) => self.regs.usbintr = value as u16 & 0x0f,
            (REG_FRNUM, 2) => self.regs.frnum = value as u16 & 0x07ff,
            (REG_FLBASEADD, 4) => self.regs.flbaseadd = value & 0xffff_f000,
            (REG_SOFMOD, 1) => self.regs.sofmod = value as u8,
            (REG_PORTSC1, 2) => self.hub.write_portsc(0, value as u16),
            (REG_PORTSC2, 2) => self.hub.write_portsc(1, value as u16),
            _ => {}
        }
        self.update_irq();
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        self.hub.tick_1ms();

        if self.regs.usbcmd & USBCMD_RS == 0 {
            self.regs.update_halted();
            self.update_irq();
            return;
        }

        self.regs.update_halted();

        let frame_index = self.regs.frnum & 0x03ff;
        if self.regs.flbaseadd != 0 {
            let mut ctx = ScheduleContext {
                mem,
                hub: &mut self.hub,
                usbsts: &mut self.regs.usbsts,
                usbintr: self.regs.usbintr,
            };
            process_frame(&mut ctx, self.regs.flbaseadd, frame_index);
        }

        self.regs.frnum = (self.regs.frnum + 1) & 0x07ff;
        self.update_irq();
    }
}

impl Default for UhciController {
    fn default() -> Self {
        Self::new()
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
        self.controller.tick_1ms(mem);
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
