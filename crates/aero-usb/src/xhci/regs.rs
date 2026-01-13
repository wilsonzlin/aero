//! xHCI register offsets and bit definitions.
//!
//! This file provides a small set of well-known offsets/constants to support an eventual MMIO
//! implementation. It is not a complete model of the xHCI register set.

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

