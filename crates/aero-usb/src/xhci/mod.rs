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
//! Full xHCI semantics (doorbells, command/event rings, device contexts, interrupters, etc) remain
//! future work.
//!
//! In addition, `transfer` provides a small, deterministic transfer-ring executor that can process
//! Normal TRBs for non-control endpoints (sufficient for HID interrupt IN/OUT).

pub mod command_ring;
pub mod context;
pub mod regs;
pub mod ring;
pub mod transfer;
pub mod trb;

use crate::MemoryBus;
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

const DEFAULT_PORT_COUNT: u8 = 2;

/// Minimal xHCI controller model.
///
/// This is *not* a full xHCI implementation. It is sufficient to wire into a PCI/MMIO wrapper and
/// to host unit tests that need a stable controller surface.
#[derive(Debug, Clone)]
pub struct XhciController {
    port_count: u8,
    ext_caps: Vec<u32>,
    usbcmd: u32,
    usbsts: u32,
    crcr: u64,
}

impl Default for XhciController {
    fn default() -> Self {
        Self::with_port_count(DEFAULT_PORT_COUNT)
    }
}

impl XhciController {
    /// Size of the MMIO BAR exposed by the emulator integration.
    ///
    /// The real xHCI register set is larger; the current model only implements a small subset.
    pub const MMIO_SIZE: u32 = 0x1000;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_port_count(port_count: u8) -> Self {
        assert!(port_count > 0, "xHCI controller must expose at least one port");
        let mut ctrl = Self {
            port_count,
            ext_caps: Vec::new(),
            usbcmd: 0,
            usbsts: 0,
            crcr: 0,
        };
        ctrl.rebuild_ext_caps();
        ctrl
    }

    pub fn mmio_read_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64) -> u32 {
        self.mmio_read(mem, offset, 4)
    }

    pub fn irq_level(&self) -> bool {
        (self.usbsts & regs::USBSTS_EINT) != 0
    }

    fn rebuild_ext_caps(&mut self) {
        self.ext_caps = self.build_ext_caps();
    }

    fn build_ext_caps(&self) -> Vec<u32> {
        // Supported Protocol Capability for USB 2.0.
        //
        // The roothub port range is 1-based, so we expose all ports as a single USB 2.0 range.
        let mut caps = Vec::new();

        let psic = 3u8; // low/full/high-speed entries.
        let header0 = (regs::EXT_CAP_ID_SUPPORTED_PROTOCOL as u32)
            | (0u32 << 8) // next pointer (0 => end of list)
            | ((regs::USB_REVISION_2_0_MAJOR as u32) << 16)
            | ((regs::USB_REVISION_2_0_MINOR as u32) << 24);
        caps.push(header0);
        caps.push(regs::PROTOCOL_NAME_USB2);
        caps.push((1u32) | ((self.port_count as u32) << 8));
        caps.push((psic as u32) | ((regs::USB2_PROTOCOL_SLOT_TYPE as u32) << 8));

        // Protocol Speed ID descriptors.
        // These values are consumed by guest xHCI drivers to interpret PORTSC.PS values.
        caps.push(regs::encode_psi(
            regs::PSIV_LOW_SPEED,
            regs::PSI_TYPE_LOW,
            0,
            0,
        ));
        caps.push(regs::encode_psi(
            regs::PSIV_FULL_SPEED,
            regs::PSI_TYPE_FULL,
            12,
            0,
        ));
        caps.push(regs::encode_psi(
            regs::PSIV_HIGH_SPEED,
            regs::PSI_TYPE_HIGH,
            48,
            1,
        ));

        caps
    }

    /// Read from the controller's MMIO register space.
    pub fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        // Treat out-of-range reads as open bus.
        if offset >= u64::from(Self::MMIO_SIZE) {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => 0,
            };
        }

        let aligned = offset & !3;
        let shift = (offset & 3) * 8;

        let value32 = match aligned {
            regs::REG_CAPLENGTH_HCIVERSION => regs::CAPLENGTH_HCIVERSION,
            regs::REG_HCSPARAMS1 => {
                // HCSPARAMS1: MaxSlots (7:0), MaxIntrs (18:8), MaxPorts (31:24).
                let max_slots = 32u32;
                let max_intrs = 1u32;
                let max_ports = self.port_count as u32;
                (max_slots & 0xff) | ((max_intrs & 0x7ff) << 8) | ((max_ports & 0xff) << 24)
            }
            regs::REG_HCCPARAMS1 => {
                // HCCPARAMS1.xECP: offset (in DWORDs) to the xHCI Extended Capabilities list.
                let xecp_dwords = (regs::EXT_CAPS_OFFSET_BYTES / 4) & 0xffff;
                xecp_dwords << 16
            }
            off if off >= regs::EXT_CAPS_OFFSET_BYTES as u64
                && off < regs::CAPLENGTH_BYTES as u64 =>
            {
                let idx = (off - regs::EXT_CAPS_OFFSET_BYTES as u64) / 4;
                self.ext_caps.get(idx as usize).copied().unwrap_or(0)
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
    pub fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        if offset >= u64::from(Self::MMIO_SIZE) {
            return;
        }

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

                // On the rising edge of RUN, perform a small DMA read from CRCR to validate PCI Bus
                // Master Enable (BME) gating in the emulator wrapper.
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
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(0, 2);

    fn save_state(&self) -> Vec<u8> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_CRCR: u16 = 3;
        const TAG_PORT_COUNT: u16 = 4;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_USBCMD, self.usbcmd);
        w.field_u32(TAG_USBSTS, self.usbsts);
        w.field_u64(TAG_CRCR, self.crcr);
        w.field_u8(TAG_PORT_COUNT, self.port_count);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_CRCR: u16 = 3;
        const TAG_PORT_COUNT: u16 = 4;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;
        *self = Self::new();
        self.usbcmd = r.u32(TAG_USBCMD)?.unwrap_or(0);
        self.usbsts = r.u32(TAG_USBSTS)?.unwrap_or(0);
        self.crcr = r.u64(TAG_CRCR)?.unwrap_or(0);
        if let Some(v) = r.u8(TAG_PORT_COUNT)? {
            // Clamp invalid snapshots to a sane value rather than panicking.
            self.port_count = v.max(1);
            self.rebuild_ext_caps();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
