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
/// Periodic Schedule Status (PSS).
pub const USBSTS_PSS: u32 = 1 << 14;
/// Asynchronous Schedule Status (ASS).
pub const USBSTS_ASS: u32 = 1 << 15;

pub const USBSTS_W1C_MASK: u32 =
    USBSTS_USBINT | USBSTS_USBERRINT | USBSTS_PCD | USBSTS_FLR | USBSTS_HSE | USBSTS_IAA;
pub const USBSTS_READ_MASK: u32 = USBSTS_W1C_MASK | USBSTS_HCHALTED | USBSTS_PSS | USBSTS_ASS;

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
/// High-speed port indicator: 1 = device is attached at high-speed.
pub const PORTSC_HSP: u32 = 1 << 9;
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

// --- EHCI extended capabilities ---
//
// Many EHCI drivers consult HCCPARAMS.EECP to locate the extended capabilities list and will
// perform the "BIOS handoff" sequence using the USB Legacy Support capability (CAPID=1).
//
// Real PCI EHCI controllers expose these registers in PCI config space; this project places the
// extended capability list inside the controller's MMIO window (relative to the capability
// registers base) to keep the model self-contained.

/// `HCCPARAMS` EECP field shift (bits 15:8).
pub const HCCPARAMS_EECP_SHIFT: u32 = 8;

/// Byte offset of the first EHCI extended capability.
pub const EECP_OFFSET: u8 = 0x40;

/// EHCI extended capability ID for USB Legacy Support.
pub const USBLEGSUP_CAPID: u32 = 0x01;

/// USB Legacy Support register (`USBLEGSUP`) offset.
pub const REG_USBLEGSUP: u64 = EECP_OFFSET as u64;
/// USB Legacy Support control/status register (`USBLEGCTLSTS`) offset.
pub const REG_USBLEGCTLSTS: u64 = REG_USBLEGSUP + 0x04;

/// `USBLEGSUP` BIOS-owned semaphore bit.
pub const USBLEGSUP_BIOS_SEM: u32 = 1 << 16;
/// `USBLEGSUP` OS-owned semaphore bit.
pub const USBLEGSUP_OS_SEM: u32 = 1 << 24;

/// Bits in `USBLEGSUP` that are writable in this model.
pub const USBLEGSUP_RW_MASK: u32 = USBLEGSUP_BIOS_SEM | USBLEGSUP_OS_SEM;

/// Fixed `USBLEGSUP` header bits: CAPID=1, NEXT=0.
pub const USBLEGSUP_HEADER: u32 = USBLEGSUP_CAPID;
