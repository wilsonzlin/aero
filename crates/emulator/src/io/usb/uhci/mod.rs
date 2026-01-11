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
use crate::io::usb::hid::composite::UsbCompositeHidInputHandle;
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
        if self.regs.usbsts & USBSTS_USBINT != 0 {
            if (self.regs.usbint_causes & USBINT_CAUSE_IOC != 0 && self.regs.usbintr & USBINTR_IOC != 0)
                || (self.regs.usbint_causes & USBINT_CAUSE_SHORT_PACKET != 0
                    && self.regs.usbintr & USBINTR_SHORT_PACKET != 0)
            {
                pending = true;
            }
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

    fn write_usbcmd(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.1 "USB Command (USBCMD)".
        if value & USBCMD_HCRESET != 0 {
            self.reset();
            return;
        }

        let prev = self.regs.usbcmd;
        let mut cmd = value & USBCMD_WRITE_MASK;

        // Global reset is latched in USBCMD (software clears it), but the act of *setting*
        // the bit resets controller state.
        if cmd & USBCMD_GRESET != 0 && prev & USBCMD_GRESET == 0 {
            self.reset();
            self.hub.bus_reset();
        }

        // Force Global Resume latches in USBCMD; raising it latches RESUMEDETECT in USBSTS.
        if cmd & USBCMD_FGR != 0 && prev & USBCMD_FGR == 0 {
            self.regs.usbsts |= USBSTS_RESUMEDETECT;
        }

        // While global reset is asserted the controller shouldn't be running.
        if cmd & USBCMD_GRESET != 0 {
            cmd &= !USBCMD_RS;
        }

        self.regs.usbcmd = cmd;
        self.regs.update_halted();
    }

    fn write_usbsts(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.2 "USB Status (USBSTS)".
        // Write-1-to-clear status bits.
        let w1c = value & USBSTS_W1C_MASK;
        self.regs.usbsts &= !w1c;
        if w1c & USBSTS_USBINT != 0 {
            self.regs.usbint_causes = 0;
        }
    }

    fn write_usbintr(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.3 "USB Interrupt Enable (USBINTR)".
        self.regs.usbintr = value & 0x0f;
    }

    fn write_frnum(&mut self, value: u16) {
        // UHCI 1.1 spec, section 2.1.4 "Frame Number (FRNUM)".
        self.regs.frnum = value & 0x07ff;
    }

    fn write_flbaseadd(&mut self, value: u32) {
        // UHCI 1.1 spec, section 2.1.5 "Frame List Base Address (FLBASEADD)".
        self.regs.flbaseadd = value & 0xffff_f000;
    }

    fn io_read_u8(&self, offset: u16) -> u8 {
        const REG_USBCMD_HI: u16 = REG_USBCMD + 1;
        const REG_USBSTS_HI: u16 = REG_USBSTS + 1;
        const REG_USBINTR_HI: u16 = REG_USBINTR + 1;
        const REG_FRNUM_HI: u16 = REG_FRNUM + 1;
        const REG_FLBASEADD_END: u16 = REG_FLBASEADD + 3;
        const REG_PORTSC1_HI: u16 = REG_PORTSC1 + 1;
        const REG_PORTSC2_HI: u16 = REG_PORTSC2 + 1;

        match offset {
            REG_USBCMD => (self.regs.usbcmd & 0x00ff) as u8,
            REG_USBCMD_HI => (self.regs.usbcmd >> 8) as u8,
            REG_USBSTS => (self.regs.usbsts & 0x00ff) as u8,
            REG_USBSTS_HI => (self.regs.usbsts >> 8) as u8,
            REG_USBINTR => (self.regs.usbintr & 0x00ff) as u8,
            REG_USBINTR_HI => (self.regs.usbintr >> 8) as u8,
            REG_FRNUM => (self.regs.frnum & 0x00ff) as u8,
            REG_FRNUM_HI => (self.regs.frnum >> 8) as u8,
            REG_FLBASEADD..=REG_FLBASEADD_END => {
                let shift = (offset - REG_FLBASEADD) * 8;
                (self.regs.flbaseadd >> shift) as u8
            }
            REG_SOFMOD => self.regs.sofmod,
            REG_PORTSC1 => (self.hub.read_portsc(0) & 0x00ff) as u8,
            REG_PORTSC1_HI => (self.hub.read_portsc(0) >> 8) as u8,
            REG_PORTSC2 => (self.hub.read_portsc(1) & 0x00ff) as u8,
            REG_PORTSC2_HI => (self.hub.read_portsc(1) >> 8) as u8,
            _ => 0xff,
        }
    }

    fn io_write_u8(&mut self, offset: u16, value: u8) {
        const REG_USBCMD_HI: u16 = REG_USBCMD + 1;
        const REG_USBSTS_HI: u16 = REG_USBSTS + 1;
        const REG_USBINTR_HI: u16 = REG_USBINTR + 1;
        const REG_FRNUM_HI: u16 = REG_FRNUM + 1;
        const REG_FLBASEADD_END: u16 = REG_FLBASEADD + 3;
        const REG_PORTSC1_HI: u16 = REG_PORTSC1 + 1;
        const REG_PORTSC2_HI: u16 = REG_PORTSC2 + 1;

        match offset {
            REG_USBCMD => {
                let v = (self.regs.usbcmd & 0xff00) | (value as u16);
                self.write_usbcmd(v);
            }
            REG_USBCMD_HI => {
                let v = (self.regs.usbcmd & 0x00ff) | ((value as u16) << 8);
                self.write_usbcmd(v);
            }
            REG_USBSTS => {
                self.write_usbsts(value as u16);
            }
            REG_USBSTS_HI => {
                self.write_usbsts((value as u16) << 8);
            }
            REG_USBINTR => {
                let v = (self.regs.usbintr & 0xff00) | (value as u16);
                self.write_usbintr(v);
            }
            REG_USBINTR_HI => {
                let v = (self.regs.usbintr & 0x00ff) | ((value as u16) << 8);
                self.write_usbintr(v);
            }
            REG_FRNUM => {
                let v = (self.regs.frnum & 0xff00) | (value as u16);
                self.write_frnum(v);
            }
            REG_FRNUM_HI => {
                let v = (self.regs.frnum & 0x00ff) | ((value as u16) << 8);
                self.write_frnum(v);
            }
            REG_FLBASEADD..=REG_FLBASEADD_END => {
                let shift = (offset - REG_FLBASEADD) * 8;
                let mask = !(0xffu32 << shift);
                let v = (self.regs.flbaseadd & mask) | ((value as u32) << shift);
                self.write_flbaseadd(v);
            }
            REG_SOFMOD => self.regs.sofmod = value,
            REG_PORTSC1 => {
                let cur = self.hub.read_portsc(0);
                let v = (cur & 0xff00) | (value as u16);
                self.hub.write_portsc(0, v);
            }
            REG_PORTSC1_HI => {
                let cur = self.hub.read_portsc(0);
                let v = (cur & 0x00ff) | ((value as u16) << 8);
                self.hub.write_portsc(0, v);
            }
            REG_PORTSC2 => {
                let cur = self.hub.read_portsc(1);
                let v = (cur & 0xff00) | (value as u16);
                self.hub.write_portsc(1, v);
            }
            REG_PORTSC2_HI => {
                let cur = self.hub.read_portsc(1);
                let v = (cur & 0x00ff) | ((value as u16) << 8);
                self.hub.write_portsc(1, v);
            }
            _ => {}
        }
    }

    fn io_read(&self, offset: u16, size: usize) -> u32 {
        let mut out = 0u32;
        for i in 0..size.min(4) {
            out |= (self.io_read_u8(offset.wrapping_add(i as u16)) as u32) << (i * 8);
        }
        out
    }

    fn io_write(&mut self, offset: u16, size: usize, value: u32) {
        match (offset, size) {
            (REG_USBCMD, 2) => self.write_usbcmd(value as u16),
            (REG_USBSTS, 2) => self.write_usbsts(value as u16),
            (REG_USBINTR, 2) => self.write_usbintr(value as u16),
            (REG_FRNUM, 2) => self.write_frnum(value as u16),
            (REG_FLBASEADD, 4) => self.write_flbaseadd(value),
            (REG_SOFMOD, 1) => self.regs.sofmod = value as u8,
            (REG_PORTSC1, 2) => self.hub.write_portsc(0, value as u16),
            (REG_PORTSC2, 2) => self.hub.write_portsc(1, value as u16),
            _ => {
                for i in 0..size.min(4) {
                    let byte = ((value >> (i * 8)) & 0xff) as u8;
                    self.io_write_u8(offset.wrapping_add(i as u16), byte);
                }
            }
        }
        self.update_irq();
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        self.hub.tick_1ms();

        if self.regs.usbcmd & (USBCMD_RS | USBCMD_EGSM) != USBCMD_RS {
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
                usbint_causes: &mut self.regs.usbint_causes,
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

    pub fn new_with_composite_hid(io_base: u16) -> (Self, UsbCompositeHidInputHandle) {
        let mut controller = UhciController::new();
        let composite = UsbCompositeHidInputHandle::new();
        controller.hub_mut().attach(0, Box::new(composite.clone()));
        (Self::new(controller, io_base), composite)
    }

    pub fn new_with_composite_input(io_base: u16) -> (Self, UsbCompositeHidInputHandle) {
        Self::new_with_composite_hid(io_base)
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
