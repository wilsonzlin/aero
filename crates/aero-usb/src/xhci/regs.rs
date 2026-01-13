//! xHCI register offsets and bit definitions.
//!
//! This file provides a small set of well-known offsets/constants to support an eventual MMIO
//! implementation. It is not a complete model of the xHCI register set.

/// Size of the exposed xHCI MMIO region for the current skeleton controller.
pub const XHCI_MMIO_SIZE: u32 = 0x1000;

/// Minimal register offsets used by the controller skeleton in [`super::XhciController`].
///
/// These are absolute offsets into the controller's MMIO BAR.
pub const REG_CAPLENGTH_HCIVERSION: u64 = 0x00;
pub const REG_HCSPARAMS1: u64 = 0x04;
pub const REG_HCCPARAMS1: u64 = 0x10;
pub const REG_USBCMD: u64 = 0x40;
pub const REG_USBSTS: u64 = 0x44;
pub const REG_CRCR_LO: u64 = 0x58;
pub const REG_CRCR_HI: u64 = 0x5c;

/// USBCMD bit 0 (Run/Stop).
pub const USBCMD_RUN: u32 = 1 << 0;
/// USBSTS bit 3 (Event Interrupt).
///
/// The full xHCI interrupt model is not implemented yet; the skeleton uses this bit as a generic
/// level-triggered IRQ pending flag.
pub const USBSTS_EINT: u32 = 1 << 3;

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
    pub const USBSTS_EINT: u32 = 1 << 3;
}

/// Runtime registers (base at `RTSOFF`).
pub mod runtime {
    /// Microframe Index register.
    pub const MFINDEX: u32 = 0x00;

    /// Interrupter register block stride in bytes.
    pub const INTERRUPTER_STRIDE: u32 = 0x20;
}

/// Doorbell register array (base at `DBOFF`).
pub mod doorbell {
    /// Doorbell register stride in bytes.
    pub const DOORBELL_STRIDE: u32 = 0x04;
}

// ---- Capability register values ----

/// We expose a 0x40-byte capability register block, which leaves room for xECP structures before
/// the operational registers region (which begins at offset 0x40 in this model).
pub const CAPLENGTH_BYTES: u8 = 0x40;

/// xHCI interface version (HCIVERSION).
///
/// We advertise xHCI 1.0.
pub const HCIVERSION_VALUE: u16 = 0x0100;

/// DWORD0 value at offset 0x00 (CAPLENGTH + HCIVERSION).
pub const CAPLENGTH_HCIVERSION: u32 = (HCIVERSION_VALUE as u32) << 16 | (CAPLENGTH_BYTES as u32);

// ---- Extended capabilities (xECP) ----

/// Byte offset of the first xHCI extended capability from the base of the MMIO region.
///
/// This must be 4-byte aligned and is referenced by `HCCPARAMS1.xECP` (which stores the offset in
/// DWORDs).
pub const EXT_CAPS_OFFSET_BYTES: u32 = 0x20;

/// xHCI Extended Capability ID: Supported Protocol.
pub const EXT_CAP_ID_SUPPORTED_PROTOCOL: u8 = 2;

/// Supported Protocol: Protocol name string "USB ".
pub const PROTOCOL_NAME_USB2: u32 = u32::from_le_bytes(*b"USB ");

pub const USB_REVISION_2_0_MAJOR: u8 = 0x02;
pub const USB_REVISION_2_0_MINOR: u8 = 0x00;

/// Protocol Slot Type used for USB 2.0 ports.
///
/// This value is consumed by some guests when mapping ports to roothub protocol types.
pub const USB2_PROTOCOL_SLOT_TYPE: u8 = 0x01;

// ---- Supported Protocol: Protocol Speed ID Descriptor ----

/// Protocol Speed ID values used by this model.
pub const PSIV_LOW_SPEED: u8 = 1;
pub const PSIV_FULL_SPEED: u8 = 2;
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
/// - Bits 5:4  PSIT (Protocol Speed ID Type)
/// - Bits 15:8 PSIM (Protocol Speed ID Mantissa)
/// - Bits 17:16 PSIE (Protocol Speed ID Exponent)
pub const fn encode_psi(psiv: u8, psit: u8, mantissa: u8, exponent: u8) -> u32 {
    (psiv as u32 & 0xf)
        | ((psit as u32 & 0x3) << 4)
        | ((mantissa as u32) << 8)
        | ((exponent as u32 & 0x3) << 16)
}

