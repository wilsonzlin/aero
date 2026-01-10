//! Register definitions for the Intel ICH AC'97 controller.
//!
//! This is intentionally a minimal subset focused on playback (PCM out) for
//! compatibility with legacy guests (e.g. Linux `snd-intel8x0`).

// Native Audio Mixer (NAM) register offsets.
pub const NAM_RESET: u64 = 0x00;
pub const NAM_MASTER_VOL: u64 = 0x02;
pub const NAM_PCM_OUT_VOL: u64 = 0x18;
pub const NAM_EXT_AUDIO_ID: u64 = 0x28;
pub const NAM_EXT_AUDIO_CTRL: u64 = 0x2A;
pub const NAM_PCM_FRONT_DAC_RATE: u64 = 0x2C;
pub const NAM_VENDOR_ID1: u64 = 0x7C;
pub const NAM_VENDOR_ID2: u64 = 0x7E;

// Native Audio Bus Master (NABM) register offsets.
pub const NABM_PO_BDBAR: u64 = 0x00;
pub const NABM_PO_CIV: u64 = 0x04;
pub const NABM_PO_LVI: u64 = 0x05;
pub const NABM_PO_SR: u64 = 0x06;
pub const NABM_PO_PICB: u64 = 0x08;
pub const NABM_PO_PIV: u64 = 0x0A;
pub const NABM_PO_CR: u64 = 0x0B;

pub const NABM_GLOB_CNT: u64 = 0x2C;
pub const NABM_GLOB_STA: u64 = 0x30;
pub const NABM_ACC_SEMA: u64 = 0x34;

// Global control bits.
pub const GLOB_CNT_GIE: u32 = 1 << 0; // Global interrupt enable.
pub const GLOB_CNT_COLD_RESET: u32 = 1 << 1;
pub const GLOB_CNT_WARM_RESET: u32 = 1 << 2;

// Bus master control bits (per stream).
pub const CR_RPBM: u8 = 1 << 0; // Run/Pause Bus Master.
pub const CR_RR: u8 = 1 << 1; // Reset Registers.
pub const CR_LVBIE: u8 = 1 << 2; // Last Valid Buffer Interrupt Enable.
pub const CR_FEIE: u8 = 1 << 3; // FIFO Error Interrupt Enable.
pub const CR_IOCE: u8 = 1 << 4; // Interrupt On Completion Enable.

// Bus master status bits (per stream).
pub const SR_DCH: u16 = 1 << 0; // DMA Controller Halted.
pub const SR_CELV: u16 = 1 << 1; // Current Equals Last Valid.
pub const SR_LVBCI: u16 = 1 << 2; // Last Valid Buffer Completion Interrupt.
pub const SR_BCIS: u16 = 1 << 3; // Buffer Completion Interrupt Status.
pub const SR_FIFOE: u16 = 1 << 4; // FIFO Error.

// Buffer descriptor format.
pub const BDL_ENTRY_BYTES: u64 = 8;
pub const BDL_IOC: u32 = 1 << 31;
