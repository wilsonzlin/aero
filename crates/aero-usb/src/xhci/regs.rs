//! xHCI register offsets and bit definitions.
//!
//! This file provides a small set of well-known offsets/constants to support an eventual MMIO
//! implementation. It is not a complete model of the xHCI register set.

/// Minimal register offsets used by the controller model in [`super::XhciController`].
///
/// These are absolute offsets into the controller's MMIO BAR. They are also used by the emulator's
/// thin PCI/MMIO wrapper (`emulator::io::usb::xhci`).
pub const REG_CAPLENGTH_HCIVERSION: u64 = 0x00;
pub const REG_HCSPARAMS1: u64 = 0x04;
pub const REG_HCSPARAMS2: u64 = 0x08;
pub const REG_HCSPARAMS3: u64 = 0x0c;
pub const REG_HCCPARAMS1: u64 = 0x10;
pub const REG_DBOFF: u64 = 0x14;
pub const REG_RTSOFF: u64 = 0x18;
pub const REG_HCCPARAMS2: u64 = 0x1c;
pub const REG_USBCMD: u64 = (CAPLENGTH_BYTES as u64) + (op::USBCMD as u64);
pub const REG_USBSTS: u64 = (CAPLENGTH_BYTES as u64) + (op::USBSTS as u64);
pub const REG_PAGESIZE: u64 = (CAPLENGTH_BYTES as u64) + (op::PAGESIZE as u64);
pub const REG_DNCTRL: u64 = (CAPLENGTH_BYTES as u64) + (op::DNCTRL as u64);
/// Command Ring Control Register (CRCR), 64-bit (low/high dwords).
pub const REG_CRCR_LO: u64 = (CAPLENGTH_BYTES as u64) + (op::CRCR as u64);
pub const REG_CRCR_HI: u64 = REG_CRCR_LO + 4;
pub const REG_DCBAAP_LO: u64 = (CAPLENGTH_BYTES as u64) + (op::DCBAAP as u64);
pub const REG_DCBAAP_HI: u64 = REG_DCBAAP_LO + 4;
/// Configure register (CONFIG), 32-bit.
pub const REG_CONFIG: u64 = (CAPLENGTH_BYTES as u64) + (op::CONFIG as u64);

/// Runtime register absolute offsets (subset).
pub const REG_MFINDEX: u64 = RTSOFF_VALUE as u64 + runtime::MFINDEX as u64;
pub const REG_INTR0_BASE: u64 = RTSOFF_VALUE as u64 + runtime::INTERRUPTER_STRIDE as u64;
pub const REG_INTR0_IMAN: u64 = REG_INTR0_BASE;
pub const REG_INTR0_IMOD: u64 = REG_INTR0_BASE + 0x04;
pub const REG_INTR0_ERSTSZ: u64 = REG_INTR0_BASE + 0x08;
pub const REG_INTR0_ERSTBA_LO: u64 = REG_INTR0_BASE + 0x10;
pub const REG_INTR0_ERSTBA_HI: u64 = REG_INTR0_BASE + 0x14;
pub const REG_INTR0_ERDP_LO: u64 = REG_INTR0_BASE + 0x18;
pub const REG_INTR0_ERDP_HI: u64 = REG_INTR0_BASE + 0x1c;

/// USBCMD bit 0 (Run/Stop).
pub const USBCMD_RUN: u32 = 1 << 0;
/// USBCMD bit 1 (Host Controller Reset).
pub const USBCMD_HCRST: u32 = 1 << 1;
/// USBCMD bit 2 (Interrupter Enable).
pub const USBCMD_INTE: u32 = 1 << 2;
/// USBCMD bit 3 (Host System Error Enable).
pub const USBCMD_HSEE: u32 = 1 << 3;
/// USBCMD bit 7 (Light Host Controller Reset).
pub const USBCMD_LHCRST: u32 = 1 << 7;
/// USBCMD bit 8 (Controller Save State).
pub const USBCMD_CSS: u32 = 1 << 8;
/// USBCMD bit 9 (Controller Restore State).
pub const USBCMD_CRS: u32 = 1 << 9;
/// USBCMD bit 10 (Enable Wrap Event).
pub const USBCMD_EWE: u32 = 1 << 10;
/// USBCMD bit 11 (Enable U3 MFINDEX Stop).
pub const USBCMD_EU3S: u32 = 1 << 11;

/// Bits we preserve for controller snapshots.
///
/// Reset bits (`HCRST`, `LHCRST`) are excluded since they are self-clearing in hardware and are
/// modelled as side effects, not latched state.
pub const USBCMD_SNAPSHOT_MASK: u32 =
    USBCMD_RUN | USBCMD_INTE | USBCMD_HSEE | USBCMD_CSS | USBCMD_CRS | USBCMD_EWE | USBCMD_EU3S;

/// PAGESIZE register value: 4KiB page size supported.
pub const PAGESIZE_4K: u32 = 1 << 0;

/// USBSTS bit 0 (Host Controller Halted).
///
/// The controller model derives this bit from `USBCMD.RUN` (RUN=0 => halted).
pub const USBSTS_HCHALTED: u32 = 1 << 0;
/// Alias for [`USBSTS_HCHALTED`].
pub const USBSTS_HCH: u32 = USBSTS_HCHALTED;
/// USBSTS bit 2 (Host System Error).
pub const USBSTS_HSE: u32 = 1 << 2;
/// USBSTS bit 3 (Event Interrupt).
///
/// The controller models this bit as "interrupt pending", derived from interrupter 0's `IMAN.IP`.
pub const USBSTS_EINT: u32 = 1 << 3;
/// USBSTS bit 4 (Port Change Detect).
pub const USBSTS_PCD: u32 = 1 << 4;
/// USBSTS bit 8 (Save State Status).
pub const USBSTS_SSS: u32 = 1 << 8;
/// USBSTS bit 9 (Restore State Status).
pub const USBSTS_RSS: u32 = 1 << 9;
/// USBSTS bit 10 (Save/Restore Error).
pub const USBSTS_SRE: u32 = 1 << 10;
/// USBSTS bit 11 (Controller Not Ready).
pub const USBSTS_CNR: u32 = 1 << 11;
/// USBSTS bit 12 (Host Controller Error).
///
/// This bit is sticky and is set when the controller detects an unrecoverable internal error.
///
/// The Aero xHCI model uses this bit to report malformed guest Event Ring configuration
/// (e.g. ERST/ERDP values that cannot be mapped safely).
pub const USBSTS_HCE: u32 = 1 << 12;

/// Bits we persist for controller snapshots.
///
/// Includes derived bits that are part of the software-visible USBSTS register (e.g. `HCHalted`,
/// `EINT`, `HCE`) so older snapshots that relied on them can be restored.
///
/// We persist the full 32-bit register image (including currently-unused/reserved bits) so
/// snapshot/restore remains forward-compatible as the controller model grows and so unit tests can
/// assert exact register preservation.
///
/// Derived bits (`USBSTS.EINT`, `USBSTS.HCH`, `USBSTS.HCE`) are still treated as derived by the
/// controller implementation and are re-materialized on restore.
pub const USBSTS_SNAPSHOT_MASK: u32 = u32::MAX;

/// CRCR fields (subset): preserve cycle state / control bits and the ring pointer.
///
/// The command ring is composed of 16-byte TRBs, so the pointer is at least 16-byte aligned and
/// the low bits may carry flags. The minimal controller model therefore preserves all bits while
/// still providing a pointer mask for consumers that want the aligned address.
pub const CRCR_RCS: u64 = 1 << 0;
pub const CRCR_CS: u64 = 1 << 1;
pub const CRCR_CA: u64 = 1 << 2;
pub const CRCR_CRR: u64 = 1 << 3;
pub const CRCR_PTR_MASK: u64 = !0x3f;
pub const CRCR_SNAPSHOT_MASK: u64 = CRCR_RCS | CRCR_CS | CRCR_CA | CRCR_CRR | CRCR_PTR_MASK;

/// DCBAAP pointer mask (64B aligned).
pub const DCBAAP_SNAPSHOT_MASK: u64 = !0x3f;

/// CONFIG register snapshot mask.
pub const CONFIG_SNAPSHOT_MASK: u32 = 0x3ff;

/// xHCI Interrupter Management (IMAN) bits.
pub const IMAN_IP: u32 = 1 << 0;
pub const IMAN_IE: u32 = 1 << 1;
pub const IMAN_MASK: u32 = IMAN_IP | IMAN_IE;

/// Event Ring Segment Table Size (ERSTSZ) mask (16-bit size).
pub const ERSTSZ_MASK: u32 = 0xffff;

/// Event Ring Segment Table Base Address (ERSTBA) pointer mask (64-byte alignment).
pub const ERSTBA_MASK: u64 = !0x3f;

/// Event Ring Dequeue Pointer (ERDP) mask: preserve EHB (bit 3) and pointer (16-byte alignment).
pub const ERDP_EHB: u64 = 1 << 3;
pub const ERDP_PTR_MASK: u64 = !0x0f;
pub const ERDP_MASK: u64 = ERDP_EHB | ERDP_PTR_MASK;

/// HCCPARAMS1 Context Size (CSZ) bit.
///
/// When set (`1`), contexts are 64 bytes. When clear (`0`), contexts are 32 bytes.
///
/// MVP assumption for Aero: **CSZ=0** (32-byte contexts).
pub const HCCPARAMS1_CSZ_64B: u32 = 1 << 2;

/// Capability registers (base of MMIO region).
pub mod cap {
    /// CAPLENGTH (u8): Capability register length (offset to operational registers).
    pub const CAPLENGTH: u32 = 0x00;
    /// HCIVERSION (u16): Interface version number.
    pub const HCIVERSION: u32 = 0x02;
    /// HCSPARAMS1 (u32).
    pub const HCSPARAMS1: u32 = 0x04;
    /// HCSPARAMS2 (u32).
    pub const HCSPARAMS2: u32 = 0x08;
    /// HCSPARAMS3 (u32).
    pub const HCSPARAMS3: u32 = 0x0c;
    /// HCCPARAMS1 (u32).
    pub const HCCPARAMS1: u32 = 0x10;
    /// DBOFF (u32): Doorbell array offset.
    pub const DBOFF: u32 = 0x14;
    /// RTSOFF (u32): Runtime registers offset.
    pub const RTSOFF: u32 = 0x18;
    /// HCCPARAMS2 (u32).
    pub const HCCPARAMS2: u32 = 0x1c;
}

/// Operational registers (base at `CAPLENGTH`).
pub mod op {
    pub const USBCMD: u32 = 0x00;
    pub const USBSTS: u32 = 0x04;
    pub const PAGESIZE: u32 = 0x08;
    pub const DNCTRL: u32 = 0x14;
    pub const CRCR: u32 = 0x18;
    pub const DCBAAP: u32 = 0x30;
    pub const CONFIG: u32 = 0x38;

    // USBCMD bits (subset).
    pub const USBCMD_RUN_STOP: u32 = 1 << 0;
    pub const USBCMD_HCRST: u32 = 1 << 1;

    // USBSTS bits (subset).
    pub const USBSTS_HCH: u32 = 1 << 0;
    pub const USBSTS_EINT: u32 = 1 << 3;
    pub const USBSTS_HCE: u32 = 1 << 12;
}

/// Runtime registers (base at `RTSOFF`).
pub mod runtime {
    /// Microframe Index register.
    pub const MFINDEX: u32 = 0x00;

    /// Interrupter register block stride in bytes.
    pub const INTERRUPTER_STRIDE: u32 = 0x20;

    /// MFINDEX is a 14-bit microframe counter.
    pub const MFINDEX_MASK: u32 = 0x3fff;
}

/// Doorbell register array (base at `DBOFF`).
pub mod doorbell {
    /// Doorbell register stride in bytes.
    pub const DOORBELL_STRIDE: u32 = 0x04;
}

// ---- Capability register values ----

/// xHCI CAPLENGTH (bytes): length of the capability register block / offset to operational regs.
///
/// Real xHCI controllers typically expose a 0x40-byte capability register block (spec 5.3.1). Keep
/// the canonical size and place Extended Capabilities elsewhere in the MMIO window (via xECP).
pub const CAPLENGTH_BYTES: u8 = 0x40;

/// Alias retained for tests/docs that refer to CAPLENGTH's field name in the specification.
pub const CAPLENGTH_VALUE: u8 = CAPLENGTH_BYTES;

/// Maximum number of device slots exposed by this controller model.
///
/// This value is reported through `HCSPARAMS1.MaxSlots` and is used to clamp `CONFIG.MaxSlotsEn`.
pub const MAX_SLOTS: u8 = 32;

/// xHCI interface version (HCIVERSION).
///
/// We advertise xHCI 1.0.
pub const HCIVERSION_VALUE: u16 = 0x0100;

/// DWORD0 value at offset 0x00 (CAPLENGTH + HCIVERSION).
pub const CAPLENGTH_HCIVERSION: u32 = (HCIVERSION_VALUE as u32) << 16 | (CAPLENGTH_BYTES as u32);

/// DBOFF register value (doorbell array base offset).
///
/// Real xHCI controllers place the doorbell array well after the operational registers; guests use
/// this value to locate the doorbell MMIO region. The skeleton controller does not implement the
/// doorbell array yet, but still exposes a realistic offset so guest drivers do not alias doorbell
/// accesses onto capability registers.
pub const DBOFF_VALUE: u32 = 0x2000;

/// RTSOFF register value (runtime register base offset).
///
/// Guests use this value to locate the runtime registers (MFINDEX + interrupter blocks). The
/// skeleton currently models only a subset of the architecture, but providing a stable runtime
/// base avoids overlapping reads with capability registers.
pub const RTSOFF_VALUE: u32 = 0x3000;

// ---- Extended capabilities (xECP) ----

/// Byte offset of the first xHCI extended capability from the base of the MMIO region.
///
/// This must be 4-byte aligned and is referenced by `HCCPARAMS1.xECP` (which stores the offset in
/// DWORDs).
///
/// Keep this outside the capability register block so CAPLENGTH remains stable.
pub const EXT_CAPS_OFFSET_BYTES: u32 = 0x100;

/// xHCI Extended Capability ID: USB Legacy Support.
pub const EXT_CAP_ID_USB_LEGACY_SUPPORT: u8 = 1;

/// xHCI Extended Capability ID: Supported Protocol.
pub const EXT_CAP_ID_SUPPORTED_PROTOCOL: u8 = 2;

/// USB Legacy Support: BIOS Owned Semaphore bit.
pub const USBLEGSUP_BIOS_OWNED: u32 = 1 << 16;
/// USB Legacy Support: OS Owned Semaphore bit.
pub const USBLEGSUP_OS_OWNED: u32 = 1 << 24;

/// Supported Protocol: Protocol name string "USB ".
pub const PROTOCOL_NAME_USB2: u32 = u32::from_le_bytes(*b"USB ");

/// USB revision number encoded as BCD (e.g. USB 2.0 == 0x0200).
pub const USB_REVISION_2_0: u16 = 0x0200;

/// Protocol Slot Type used for USB 2.0 ports.
///
/// This value is consumed by some guests when mapping ports to roothub protocol types.
pub const USB2_PROTOCOL_SLOT_TYPE: u8 = 0x01;

// ---- Supported Protocol: Protocol Speed ID Descriptor ----

/// Protocol Speed ID values used by this model.
///
/// For compatibility with common xHCI drivers, use the canonical ordering:
/// - 1 = full speed
/// - 2 = low speed
/// - 3 = high speed
pub const PSIV_FULL_SPEED: u8 = 1;
pub const PSIV_LOW_SPEED: u8 = 2;
pub const PSIV_HIGH_SPEED: u8 = 3;

/// Protocol Speed ID Types (PSIT).
///
/// These values match the xHCI specification's encoding for the USB 2.0 protocol.
pub const PSI_TYPE_LOW: u8 = 1;
pub const PSI_TYPE_FULL: u8 = 2;
pub const PSI_TYPE_HIGH: u8 = 3;

/// Encodes a Protocol Speed ID Descriptor (PSID).
///
/// Field layout (xHCI spec):
/// - Bits 3:0  PSIV (Protocol Speed ID Value)
/// - Bits 7:4  PSIT (Protocol Speed ID Type)
/// - Bits 15:8 PSIM (Protocol Speed ID Mantissa)
/// - Bits 17:16 PSIE (Protocol Speed ID Exponent)
pub const fn encode_psi(psiv: u8, psit: u8, mantissa: u8, exponent: u8) -> u32 {
    (psiv as u32 & 0xf)
        | ((psit as u32 & 0xf) << 4)
        | ((mantissa as u32) << 8)
        | ((exponent as u32 & 0x3) << 16)
}

/// Per-port registers (part of the operational register space).
///
/// xHCI exposes a block of port register sets beginning at offset `0x400` from the operational
/// register base (i.e. `REG_USBCMD + 0x400`). Each port has a 0x10-byte register set:
/// - PORTSC (Port Status and Control) @ +0x0
/// - PORTPMSC (Port Power Management Status and Control) @ +0x4
/// - PORTLI (Port Link Info) @ +0x8
/// - PORTHLPMC (Port Hardware LPM Control) @ +0xc
///
/// The current controller model only implements `PORTSC`; the remaining registers read as 0 and
/// ignore writes.
pub mod port {
    /// Base offset (from the operational register base) for the port register block.
    pub const PORTREGS_BASE: u64 = 0x400;
    /// Stride between port register sets in bytes.
    pub const PORTREGS_STRIDE: u64 = 0x10;

    /// Offset of PORTSC within a port register set.
    pub const PORTSC: u64 = 0x00;

    #[inline]
    pub const fn portsc_offset(port: usize) -> u64 {
        super::REG_USBCMD + PORTREGS_BASE + (port as u64) * PORTREGS_STRIDE + PORTSC
    }
}

// ---- PORTSC bit definitions (subset) ----

/// Current Connect Status (CCS), bit 0.
pub const PORTSC_CCS: u32 = 1 << 0;
/// Port Enabled/Disabled (PED), bit 1.
pub const PORTSC_PED: u32 = 1 << 1;
/// Port Reset (PR), bit 4.
pub const PORTSC_PR: u32 = 1 << 4;

/// Port Link State (PLS), bits 5..=8.
pub const PORTSC_PLS_SHIFT: u32 = 5;
pub const PORTSC_PLS_MASK: u32 = 0x0f << PORTSC_PLS_SHIFT;

/// Port Power (PP), bit 9.
pub const PORTSC_PP: u32 = 1 << 9;

/// Port Speed ID (PS), bits 10..=13.
pub const PORTSC_PS_SHIFT: u32 = 10;
pub const PORTSC_PS_MASK: u32 = 0x0f << PORTSC_PS_SHIFT;

/// Port Link State Write Strobe (LWS), bit 16.
///
/// xHCI uses this as a write-only "strobe" bit: when set alongside a PORTSC.PLS value, the host
/// controller should attempt the requested link-state transition.
pub const PORTSC_LWS: u32 = 1 << 16;

/// Connect Status Change (CSC), bit 17 (RW1C).
pub const PORTSC_CSC: u32 = 1 << 17;
/// Port Enabled/Disabled Change (PEC), bit 18 (RW1C).
pub const PORTSC_PEC: u32 = 1 << 18;
/// Port Reset Change (PRC), bit 21 (RW1C).
pub const PORTSC_PRC: u32 = 1 << 21;
/// Port Link State Change (PLC), bit 22 (RW1C).
pub const PORTSC_PLC: u32 = 1 << 22;

// ---- Port Status Change Event TRB encoding helpers ----

/// In a Port Status Change Event TRB, the Port ID lives in bits 31:24 of the parameter dword.
pub const PSC_EVENT_PORT_ID_SHIFT: u32 = 24;
