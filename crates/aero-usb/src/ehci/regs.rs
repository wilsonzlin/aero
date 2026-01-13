//! EHCI (USB 2.0) register definitions.
//!
//! This module models the EHCI capability + operational MMIO registers and the PORTSC bitfields
//! required for basic OS driver bring-up.

/// Size of the EHCI MMIO register window exposed through PCI BAR0.
pub const MMIO_SIZE: u32 = 0x1000;

/// Capability register length (offset to operational registers).
///
/// Most EHCI controllers use 0x20 bytes of capability registers.
pub const CAPLENGTH: u8 = 0x20;

/// EHCI interface version (BCDVERSION).
pub const HCIVERSION: u16 = 0x0100;

// Capability registers (absolute offsets from MMIO base).
pub const REG_CAPLENGTH_HCIVERSION: u64 = 0x00;
pub const REG_HCSPARAMS: u64 = 0x04;
pub const REG_HCCPARAMS: u64 = 0x08;
pub const REG_HCSP_PORTROUTE: u64 = 0x0c;

// Operational registers (absolute offsets from MMIO base).
pub const REG_USBCMD: u64 = CAPLENGTH as u64 + 0x00;
pub const REG_USBSTS: u64 = CAPLENGTH as u64 + 0x04;
pub const REG_USBINTR: u64 = CAPLENGTH as u64 + 0x08;
pub const REG_FRINDEX: u64 = CAPLENGTH as u64 + 0x0c;
pub const REG_CTRLDSSEGMENT: u64 = CAPLENGTH as u64 + 0x10;
pub const REG_PERIODICLISTBASE: u64 = CAPLENGTH as u64 + 0x14;
pub const REG_ASYNCLISTADDR: u64 = CAPLENGTH as u64 + 0x18;
pub const REG_CONFIGFLAG: u64 = CAPLENGTH as u64 + 0x40;
pub const REG_PORTSC_BASE: u64 = CAPLENGTH as u64 + 0x44;

pub const fn reg_portsc(port: usize) -> u64 {
    REG_PORTSC_BASE + (port as u64) * 4
}

// USBCMD bits.
pub const USBCMD_RS: u32 = 1 << 0;
pub const USBCMD_HCRESET: u32 = 1 << 1;
pub const USBCMD_PSE: u32 = 1 << 4;
pub const USBCMD_ASE: u32 = 1 << 5;
/// Interrupt on Async Advance Doorbell (IAAD).
///
/// EHCI names this bit as "IAAD" in the command register (USBCMD) and "IAA" in the status register
/// (USBSTS).
pub const USBCMD_IAAD: u32 = 1 << 6;
/// Backwards-compatible alias for [`USBCMD_IAAD`].
pub const USBCMD_IAA: u32 = USBCMD_IAAD;

pub const USBCMD_WRITE_MASK: u32 = USBCMD_RS | USBCMD_PSE | USBCMD_ASE | USBCMD_IAAD;

// USBSTS bits.
pub const USBSTS_USBINT: u32 = 1 << 0;
pub const USBSTS_USBERRINT: u32 = 1 << 1;
pub const USBSTS_PCD: u32 = 1 << 2;
pub const USBSTS_FLR: u32 = 1 << 3;
pub const USBSTS_HSE: u32 = 1 << 4;
pub const USBSTS_IAA: u32 = 1 << 5;
pub const USBSTS_HCHALTED: u32 = 1 << 12;

pub const USBSTS_W1C_MASK: u32 =
    USBSTS_USBINT | USBSTS_USBERRINT | USBSTS_PCD | USBSTS_FLR | USBSTS_HSE | USBSTS_IAA;
pub const USBSTS_READ_MASK: u32 = USBSTS_W1C_MASK | USBSTS_HCHALTED;

/// Subset of USBSTS bits that can raise interrupts when enabled in USBINTR.
pub const USBSTS_IRQ_MASK: u32 = USBSTS_W1C_MASK;

// USBINTR bits.
pub const USBINTR_USBINT: u32 = 1 << 0;
pub const USBINTR_USBERRINT: u32 = 1 << 1;
pub const USBINTR_PCD: u32 = 1 << 2;
pub const USBINTR_FLR: u32 = 1 << 3;
pub const USBINTR_HSE: u32 = 1 << 4;
pub const USBINTR_IAA: u32 = 1 << 5;

pub const USBINTR_MASK: u32 =
    USBINTR_USBINT | USBINTR_USBERRINT | USBINTR_PCD | USBINTR_FLR | USBINTR_HSE | USBINTR_IAA;

// FRINDEX is a 14-bit microframe counter in bits 0..=13.
pub const FRINDEX_MASK: u32 = 0x3fff;

pub const PERIODICLISTBASE_MASK: u32 = 0xffff_f000;
pub const ASYNCLISTADDR_MASK: u32 = 0xffff_ffe0;

pub const CONFIGFLAG_CF: u32 = 1 << 0;

// PORTSC bits.
pub const PORTSC_CCS: u32 = 1 << 0;
pub const PORTSC_CSC: u32 = 1 << 1;
pub const PORTSC_PED: u32 = 1 << 2;
pub const PORTSC_PEDC: u32 = 1 << 3;
pub const PORTSC_OCA: u32 = 1 << 4;
pub const PORTSC_OCC: u32 = 1 << 5;
pub const PORTSC_FPR: u32 = 1 << 6;
pub const PORTSC_SUSP: u32 = 1 << 7;
pub const PORTSC_PR: u32 = 1 << 8;
pub const PORTSC_LS_MASK: u32 = 0b11 << 10;
pub const PORTSC_PP: u32 = 1 << 12;
/// Port Owner: 1 = companion controller owns the port, 0 = EHCI owns the port.
pub const PORTSC_PO: u32 = 1 << 13;
pub const PORTSC_PIC_MASK: u32 = 0b11 << 14;
pub const PORTSC_PTC_MASK: u32 = 0b1111 << 16;
pub const PORTSC_WKC: u32 = 1 << 20;
pub const PORTSC_WKD: u32 = 1 << 21;
pub const PORTSC_WKO: u32 = 1 << 22;

pub const PORTSC_W1C_MASK: u32 = PORTSC_CSC | PORTSC_PEDC | PORTSC_OCC;
