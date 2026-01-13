//! xHCI (USB 3.x) host controller scaffolding.
//!
//! Aero's canonical USB stack lives in `aero-usb`. Today the project ships a UHCI host controller
//! model that is sufficient for Windows 7's in-box USB/HID drivers. xHCI support is being added
//! incrementally.
//!
//! The primary consumers of this module are:
//! - The xHCI controller MMIO/PCI integration in `crates/emulator` (`emulator::io::usb::xhci`)
//! - Unit tests for the core xHCI data structures (`trb`, `ring`, `context`)
//!
//! The controller implementation here is intentionally small; it currently provides:
//! - a minimal MMIO register file with basic size/unaligned access support
//! - a DMA read on the first transition of `USBCMD.RUN` (to validate PCI BME gating in the wrapper)
//! - a level-triggered `irq_level()` surface (to validate PCI INTx disable gating)
//!
//! In addition, `transfer` provides a small, deterministic transfer-ring executor that can process
//! Normal TRBs for non-control endpoints (sufficient for HID interrupt IN/OUT).

pub mod context;
pub mod regs;
pub mod ring;
pub mod trb;
pub mod transfer;

use crate::MemoryBus;
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

/// Minimal xHCI controller model.
///
/// This is *not* a full xHCI implementation. It is sufficient to wire into a PCI/MMIO wrapper and
/// to host unit tests that need a stable controller surface.
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
        mem.read_bytes(self.crcr, &mut buf);
        self.usbsts |= regs::USBSTS_EINT;
    }
}
impl IoSnapshot for XhciController {
    const DEVICE_ID: [u8; 4] = *b"XHCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(0, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_CRCR: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_USBCMD, self.usbcmd);
        w.field_u32(TAG_USBSTS, self.usbsts);
        w.field_u64(TAG_CRCR, self.crcr);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_CRCR: u16 = 3;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        *self = Self::new();
        self.usbcmd = r.u32(TAG_USBCMD)?.unwrap_or(0);
        self.usbsts = r.u32(TAG_USBSTS)?.unwrap_or(0);
        self.crcr = r.u64(TAG_CRCR)?.unwrap_or(0);

        Ok(())
    }
}
