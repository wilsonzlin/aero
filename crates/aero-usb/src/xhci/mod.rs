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
//! - DCBAAP register storage + controller-local slot allocation (Enable Slot scaffolding)
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

use alloc::vec::Vec;
use crate::MemoryBus;
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

const DEFAULT_PORT_COUNT: u8 = 2;

use self::context::{EndpointContext, SlotContext};
use self::ring::RingCursor;

/// xHCI command completion codes (subset).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCompletionCode {
    Success,
    NoSlotsAvailableError,
    ParameterError,
    ContextStateError,
    Unknown(u8),
}

impl CommandCompletionCode {
    #[inline]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Success,
            9 => Self::NoSlotsAvailableError,
            17 => Self::ParameterError,
            19 => Self::ContextStateError,
            other => Self::Unknown(other),
        }
    }

    #[inline]
    pub const fn raw(self) -> u8 {
        match self {
            Self::Success => 1,
            Self::NoSlotsAvailableError => 9,
            Self::ParameterError => 17,
            Self::ContextStateError => 19,
            Self::Unknown(raw) => raw,
        }
    }
}

/// Result of completing an xHCI command.
///
/// In hardware this data is typically delivered via a Command Completion Event TRB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandCompletion {
    pub completion_code: CommandCompletionCode,
    pub slot_id: u8,
}

impl CommandCompletion {
    #[inline]
    pub const fn success(slot_id: u8) -> Self {
        Self {
            completion_code: CommandCompletionCode::Success,
            slot_id,
        }
    }

    #[inline]
    pub const fn failure(code: CommandCompletionCode) -> Self {
        Self {
            completion_code: code,
            slot_id: 0,
        }
    }
}

/// Per-slot state tracked by the controller.
#[derive(Debug, Clone)]
pub struct SlotState {
    enabled: bool,
    port_id: Option<u8>,
    device_attached: bool,

    /// Guest physical address of the Device Context structure, as stored in DCBAAP.
    device_context_ptr: u64,

    /// Shadow context state (mirrors guest memory once Address Device/Configure Endpoint are
    /// implemented).
    slot_context: SlotContext,
    endpoint_contexts: [EndpointContext; 31],

    /// Per-endpoint transfer ring cursors indexed by Endpoint Context ID (1..=31).
    transfer_rings: [Option<RingCursor>; 31],
}

impl Default for SlotState {
    fn default() -> Self {
        Self {
            enabled: false,
            port_id: None,
            device_attached: false,
            device_context_ptr: 0,
            slot_context: SlotContext::default(),
            endpoint_contexts: [EndpointContext::default(); 31],
            transfer_rings: [None; 31],
        }
    }
}

impl SlotState {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn port_id(&self) -> Option<u8> {
        self.port_id
    }

    pub fn device_attached(&self) -> bool {
        self.device_attached
    }

    pub fn device_context_ptr(&self) -> u64 {
        self.device_context_ptr
    }

    pub fn slot_context(&self) -> &SlotContext {
        &self.slot_context
    }

    pub fn endpoint_context(&self, idx: usize) -> Option<&EndpointContext> {
        self.endpoint_contexts.get(idx)
    }

    /// Returns the transfer ring cursor for the given Endpoint Context ID (1..=31).
    pub fn transfer_ring(&self, endpoint_id: u8) -> Option<RingCursor> {
        let idx = endpoint_id.checked_sub(1)? as usize;
        self.transfer_rings.get(idx).copied().flatten()
    }
}

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
    dcbaap: u64,
    slots: Vec<SlotState>,
}

impl Default for XhciController {
    fn default() -> Self {
        Self::with_port_count(DEFAULT_PORT_COUNT)
    }
}

impl XhciController {
    /// Size of the MMIO BAR exposed by the emulator integration.
    ///
    /// Real xHCI controllers expose a 64KiB MMIO window. The current model only implements a small
    /// subset of the architectural register set, but we still reserve the full window so PCI BAR
    /// probing/alignment matches the canonical PCI profile and the Web runtime device wrapper.
    pub const MMIO_SIZE: u32 = 0x10000;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_port_count(port_count: u8) -> Self {
        assert!(port_count > 0, "xHCI controller must expose at least one port");
        const DEFAULT_MAX_SLOTS: usize = 32;
        let slots = core::iter::repeat_with(SlotState::default)
            .take(DEFAULT_MAX_SLOTS + 1)
            .collect();
        let mut ctrl = Self {
            port_count,
            ext_caps: Vec::new(),
            usbcmd: 0,
            usbsts: 0,
            crcr: 0,
            dcbaap: 0,
            slots,
        };
        ctrl.rebuild_ext_caps();
        ctrl
    }

    pub fn mmio_read_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64) -> u32 {
        self.mmio_read(mem, offset, 4)
    }

    /// Returns the currently configured DCBAAP base (64-byte aligned), if set.
    pub fn dcbaap(&self) -> Option<u64> {
        if self.dcbaap == 0 {
            None
        } else {
            Some(self.dcbaap)
        }
    }

    /// Store the guest-provided DCBAAP pointer.
    ///
    /// The base address is 64-byte aligned; low bits are masked away.
    pub fn set_dcbaap(&mut self, paddr: u64) {
        self.dcbaap = paddr & !0x3f;
    }

    fn dcbaap_entry_paddr(&self, slot_id: u8) -> Option<u64> {
        let base = self.dcbaap()?;
        base.checked_add((slot_id as u64).checked_mul(8)?)
    }

    /// Return the slot state for `slot_id` if the slot is currently enabled.
    pub fn slot_state(&self, slot_id: u8) -> Option<&SlotState> {
        let idx = usize::from(slot_id);
        let state = self.slots.get(idx)?;
        if state.enabled {
            Some(state)
        } else {
            None
        }
    }

    /// Enable a new device slot.
    ///
    /// This is the core of handling an Enable Slot Command TRB. It allocates the lowest available
    /// slot ID and initialises controller-local state.
    ///
    /// The method is defensive: missing/zero DCBAAP returns a completion code instead of panicking.
    pub fn enable_slot(&mut self, mem: &mut impl MemoryBus) -> CommandCompletion {
        if self.dcbaap().is_none() {
            return CommandCompletion::failure(CommandCompletionCode::ContextStateError);
        }

        let slot_id = match (1u8..)
            .take(self.slots.len().saturating_sub(1))
            .find(|&id| !self.slots[usize::from(id)].enabled)
        {
            Some(id) => id,
            None => return CommandCompletion::failure(CommandCompletionCode::NoSlotsAvailableError),
        };

        let Some(entry_addr) = self.dcbaap_entry_paddr(slot_id) else {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        };

        // Initialise DCBAAP entry. This is safe even if the guest already zeroed it.
        mem.write_physical(entry_addr, &0u64.to_le_bytes());

        let slot = &mut self.slots[usize::from(slot_id)];
        *slot = SlotState {
            enabled: true,
            ..SlotState::default()
        };

        CommandCompletion::success(slot_id)
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
        let open_bus = match size {
            1 => 0xff,
            2 => 0xffff,
            4 => u32::MAX,
            _ => 0,
        };
        let Some(end) = offset.checked_add(size as u64) else {
            return open_bus;
        };
        if size == 0 || end > u64::from(Self::MMIO_SIZE) {
            return open_bus;
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
                // CSZ=0 => 32-byte contexts (MVP).
                (xecp_dwords << 16) & !regs::HCCPARAMS1_CSZ_64B
            }
            regs::REG_DBOFF => regs::DBOFF_VALUE,
            regs::REG_RTSOFF => regs::RTSOFF_VALUE,
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
            regs::REG_DCBAAP_LO => (self.dcbaap & 0xffff_ffff) as u32,
            regs::REG_DCBAAP_HI => (self.dcbaap >> 32) as u32,
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
        let Some(end) = offset.checked_add(size as u64) else {
            return;
        };
        if size == 0 || end > u64::from(Self::MMIO_SIZE) {
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
            regs::REG_DCBAAP_LO => {
                let lo = merge(self.dcbaap as u32) as u64;
                self.dcbaap = (self.dcbaap & 0xffff_ffff_0000_0000) | lo;
                self.dcbaap &= !0x3f;
            }
            regs::REG_DCBAAP_HI => {
                let hi = merge((self.dcbaap >> 32) as u32) as u64;
                self.dcbaap = (self.dcbaap & 0x0000_0000_ffff_ffff) | (hi << 32);
                self.dcbaap &= !0x3f;
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
        const TAG_DCBAAP: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_USBCMD, self.usbcmd);
        w.field_u32(TAG_USBSTS, self.usbsts);
        w.field_u64(TAG_CRCR, self.crcr);
        w.field_u8(TAG_PORT_COUNT, self.port_count);
        w.field_u64(TAG_DCBAAP, self.dcbaap);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_CRCR: u16 = 3;
        const TAG_PORT_COUNT: u16 = 4;
        const TAG_DCBAAP: u16 = 5;

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
        if let Some(v) = r.u64(TAG_DCBAAP)? {
            self.dcbaap = v & !0x3f;
        }
        // Reset slot state; full context snapshotting will be added when a command/event ring model
        // is implemented.
        for slot in self.slots.iter_mut() {
            *slot = SlotState::default();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
