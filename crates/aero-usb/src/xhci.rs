//! Extremely small xHCI (USB 3.x) controller skeleton.
//!
//! Aero's canonical USB stack lives in `aero-usb`. Today the project ships a UHCI host controller
//! model that is sufficient for Windows 7's in-box USB/HID drivers. xHCI support is being added
//! incrementally; the full xHCI device model is intentionally out-of-scope for this crate module
//! (for now).
//!
//! The primary consumer of the current implementation is the `emulator` crate's compatibility shim
//! (`emulator::io::usb::xhci`), which needs:
//! - A stable `aero_usb::xhci::XhciController` type to wire into PCI/MMIO
//! - A DMA entry point so PCI Bus Master Enable gating can be validated
//! - An `irq_level()` surface so PCI COMMAND.INTX_DISABLE gating can be validated
//!
//! The register layout below is *loosely* based on the xHCI specification (capability regs at
//! offset 0, operational regs starting at CAPLENGTH=0x40). Only a handful of registers are
//! modelled.

use crate::MemoryBus;

/// xHCI register offsets used by the minimal controller skeleton.
///
/// These are not a complete set; they exist primarily for unit tests.
pub mod regs {
    /// Capability registers:
    /// - bits 0..7: CAPLENGTH (operational register offset, in bytes)
    /// - bits 16..31: HCIVERSION
    pub const REG_CAPLENGTH_HCIVERSION: u64 = 0x00;

    /// Operational registers (at CAPLENGTH=0x40):
    pub const REG_USBCMD: u64 = 0x40;
    pub const REG_USBSTS: u64 = 0x44;
    /// Command Ring Control Register (CRCR), 64-bit.
    pub const REG_CRCR_LO: u64 = 0x58;
    pub const REG_CRCR_HI: u64 = 0x5c;

    /// USBCMD bit 0 (Run/Stop).
    pub const USBCMD_RUN: u32 = 1 << 0;
    /// USBSTS bit 3 (Event Interrupt) - used as a generic "IRQ pending" bit in the skeleton.
    pub const USBSTS_EINT: u32 = 1 << 3;
}

/// Minimal xHCI controller model.
///
/// This is *not* a complete xHCI implementation. It only provides:
/// - a tiny MMIO register file with basic size/unaligned access support
/// - a DMA read on the first transition of USBCMD.RUN (to validate PCI COMMAND.BME gating)
/// - a level-triggered `irq_level()` that follows USBSTS.EINT
#[derive(Debug, Default)]
pub struct XhciController {
    usbcmd: u32,
    usbsts: u32,
    crcr: u64,
}

impl XhciController {
    /// Size of the MMIO BAR exposed by the emulator integration.
    ///
    /// The real xHCI register set is larger; the skeleton only implements a subset and keeps the
    /// BAR small for now.
    pub const MMIO_SIZE: u32 = 0x1000;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn irq_level(&self) -> bool {
        (self.usbsts & regs::USBSTS_EINT) != 0
    }

    /// Read from the controller's MMIO register space.
    pub fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;

        let value32 = match aligned {
            regs::REG_CAPLENGTH_HCIVERSION => {
                // CAPLENGTH=0x40 (operational registers start at 0x40), HCIVERSION=0x0100.
                (0x0100u32 << 16) | 0x40
            }
            regs::REG_USBCMD => self.usbcmd,
            regs::REG_USBSTS => self.usbsts,
            regs::REG_CRCR_LO => (self.crcr & 0xffff_ffff) as u32,
            regs::REG_CRCR_HI => (self.crcr >> 32) as u32,
            _ => 0,
        };

        match size {
            1 => (value32 >> shift) & 0xff,
            2 => (value32 >> shift) & 0xffff,
            4 => value32,
            _ => 0,
        }
    }

    /// Write to the controller's MMIO register space.
    pub fn mmio_write(
        &mut self,
        mem: &mut dyn MemoryBus,
        offset: u64,
        size: usize,
        value: u32,
    ) {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let (mask, value_shifted) = match size {
            1 => (0xffu32 << shift, (value & 0xff) << shift),
            2 => (0xffffu32 << shift, (value & 0xffff) << shift),
            4 => (u32::MAX, value),
            _ => return,
        };

        let merge = |cur: u32| (cur & !mask) | (value_shifted & mask);

        match aligned {
            regs::REG_USBCMD => {
                let prev = self.usbcmd;
                self.usbcmd = merge(self.usbcmd);

                // On the rising edge of RUN, perform a small DMA read from CRCR to validate PCI
                // Bus Master Enable gating in the emulator wrapper.
                let was_running = (prev & regs::USBCMD_RUN) != 0;
                let now_running = (self.usbcmd & regs::USBCMD_RUN) != 0;
                if !was_running && now_running {
                    self.dma_on_run(mem);
                }
            }
            regs::REG_USBSTS => {
                // Treat USBSTS as RW1C. Writing 1 clears the bit.
                let write_val = merge(0);
                self.usbsts &= !write_val;
            }
            regs::REG_CRCR_LO => {
                let lo = merge(self.crcr as u32) as u64;
                self.crcr = (self.crcr & 0xffff_ffff_0000_0000) | lo;
            }
            regs::REG_CRCR_HI => {
                let hi = merge((self.crcr >> 32) as u32) as u64;
                self.crcr = (self.crcr & 0x0000_0000_ffff_ffff) | (hi << 32);
            }
            _ => {}
        }
    }

    fn dma_on_run(&mut self, mem: &mut dyn MemoryBus) {
        // Read a dword from CRCR and surface an interrupt. The data itself is ignored; the goal is
        // to touch the memory bus when bus mastering is enabled so the emulator wrapper can gate
        // the access.
        let mut buf = [0u8; 4];
        mem.read_physical(self.crcr, &mut buf);
        self.usbsts |= regs::USBSTS_EINT;
    }
}

