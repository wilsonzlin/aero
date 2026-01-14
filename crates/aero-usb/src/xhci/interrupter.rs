//! xHCI runtime interrupter register block.
//!
//! This models only the subset required for event ring delivery via Interrupter 0:
//! - IMAN: IP (interrupt pending) + IE (interrupt enable)
//! - IMOD: interrupt moderation (stored, not implemented)
//! - ERSTSZ/ERSTBA: Event Ring Segment Table configuration
//! - ERDP: Event Ring Dequeue Pointer (guest-owned)

use core::fmt;

/// Interrupter Management Register (IMAN) - Interrupt Pending bit.
pub const IMAN_IP: u32 = 1 << 0;
/// Interrupter Management Register (IMAN) - Interrupt Enable bit.
pub const IMAN_IE: u32 = 1 << 1;

/// Event Ring Dequeue Pointer (ERDP) - Event Handler Busy bit.
///
/// In xHCI, software commonly writes ERDP with this bit set as part of the interrupt acknowledgement
/// handshake. The Aero model treats ERDP.EHB as a transient write-1-to-ack bit:
/// - writing ERDP with EHB set clears IMAN.IP, and
/// - EHB is not latched in the stored ERDP value (reads typically return it as 0).
pub const ERDP_EHB: u64 = 1 << 3;

#[derive(Clone, Copy)]
pub struct InterrupterRegs {
    iman: u32,
    imod: u32,
    erstsz: u32,
    erstba: u64,
    erdp: u64,

    // Internal write-generation counters. These are not guest-visible but allow the controller
    // model to observe ERDP/ERST writes even when the guest writes the same value.
    pub(crate) erst_gen: u64,
    pub(crate) erdp_gen: u64,
}

impl fmt::Debug for InterrupterRegs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InterrupterRegs")
            .field("iman", &format_args!("{:#010x}", self.iman_raw()))
            .field("imod", &format_args!("{:#010x}", self.imod))
            .field("erstsz", &format_args!("{:#010x}", self.erstsz_raw()))
            .field("erstba", &format_args!("{:#018x}", self.erstba))
            .field("erdp", &format_args!("{:#018x}", self.erdp))
            .finish()
    }
}

impl Default for InterrupterRegs {
    fn default() -> Self {
        // Default interrupter 0's IE bit to 1. This keeps the controller's synthetic
        // DMA-on-RUN interrupt visible without additional guest programming (used by the emulator
        // PCI/MMIO gating tests).
        //
        // Real guests typically program IMAN.IE explicitly during controller initialisation.
        Self {
            iman: IMAN_IE,
            imod: 0,
            erstsz: 0,
            erstba: 0,
            erdp: 0,
            erst_gen: 0,
            erdp_gen: 0,
        }
    }
}

impl InterrupterRegs {
    pub fn interrupt_pending(&self) -> bool {
        (self.iman & IMAN_IP) != 0
    }

    pub fn interrupt_enable(&self) -> bool {
        (self.iman & IMAN_IE) != 0
    }

    pub fn set_interrupt_pending(&mut self, pending: bool) {
        if pending {
            self.iman |= IMAN_IP;
        } else {
            self.iman &= !IMAN_IP;
        }
    }

    pub fn iman_raw(&self) -> u32 {
        self.iman & (IMAN_IP | IMAN_IE)
    }

    pub fn imod_raw(&self) -> u32 {
        self.imod
    }

    pub fn erstsz_raw(&self) -> u32 {
        self.erstsz
    }

    pub fn erstba_raw(&self) -> u64 {
        self.erstba
    }

    pub fn erdp_raw(&self) -> u64 {
        self.erdp
    }

    pub fn erstsz(&self) -> u16 {
        (self.erstsz & 0xffff) as u16
    }

    pub fn erstba(&self) -> u64 {
        self.erstba
    }

    pub fn erdp_ptr(&self) -> u64 {
        self.erdp & !0x0f
    }

    pub fn erdp_flags(&self) -> u64 {
        self.erdp & 0x0f
    }

    pub fn write_iman(&mut self, value: u32) {
        // IMAN.IP is write-1-to-clear (RW1C). IE is normal R/W.
        if value & IMAN_IP != 0 {
            self.iman &= !IMAN_IP;
        }
        self.iman = (self.iman & IMAN_IP) | (value & IMAN_IE);
    }

    /// Masked write variant for sub-dword MMIO stores.
    ///
    /// `value` is assumed to already be shifted into place (i.e. it can be `value_shifted` from a
    /// byte/word store) and `mask` identifies which bits are being written.
    pub fn write_iman_masked(&mut self, value: u32, mask: u32) {
        // IMAN.IP is RW1C, so only treat it as written when the mask covers it.
        if (mask & IMAN_IP) != 0 && (value & IMAN_IP) != 0 {
            self.iman &= !IMAN_IP;
        }

        // IE is normal R/W.
        if (mask & IMAN_IE) != 0 {
            self.iman = (self.iman & !IMAN_IE) | (value & IMAN_IE);
        }
    }

    pub fn write_imod(&mut self, value: u32) {
        self.imod = value;
    }

    pub fn write_erstsz(&mut self, value: u32) {
        self.erstsz = value & 0xffff;
        self.erst_gen = self.erst_gen.wrapping_add(1);
    }

    pub fn write_erstba(&mut self, value: u64) {
        // ERSTBA must be 64-byte aligned; low 6 bits are reserved.
        self.erstba = value & !0x3f;
        self.erst_gen = self.erst_gen.wrapping_add(1);
    }

    pub fn write_erdp(&mut self, value: u64) {
        // Pointer is 16-byte aligned; low 4 bits are flags.
        let ptr = value & !0x0f;
        let mut flags = value & 0x0f;
        self.erdp_gen = self.erdp_gen.wrapping_add(1);

        // Minimal interrupt-ack handshake:
        //
        // Many xHCI drivers acknowledge event interrupts by writing ERDP with the Event Handler Busy
        // (EHB) bit set, rather than explicitly clearing IMAN.IP. Treat this as an interrupt
        // acknowledgement and clear IP when EHB is written as 1.
        //
        // In real hardware, EHB is used as part of a handshake and is not meant to be a sticky
        // guest-controlled flag. Model it as "write 1 to acknowledge" by not latching the bit in
        // the stored ERDP value.
        if (value & ERDP_EHB) != 0 {
            self.iman &= !IMAN_IP;
            flags &= !ERDP_EHB;
        }

        self.erdp = ptr | flags;
    }

    /// Restore helpers used by snapshot loading.
    ///
    /// These do **not** bump generation counters; they are considered part of a single restore
    /// transaction.
    pub(crate) fn restore_iman(&mut self, value: u32) {
        self.iman = value & (IMAN_IP | IMAN_IE);
    }

    pub(crate) fn restore_imod(&mut self, value: u32) {
        self.imod = value;
    }

    pub(crate) fn restore_erstsz(&mut self, value: u32) {
        self.erstsz = value & 0xffff;
    }

    pub(crate) fn restore_erstba(&mut self, value: u64) {
        self.erstba = value & !0x3f;
    }

    pub(crate) fn restore_erdp(&mut self, value: u64) {
        // EHB is a transient handshake bit; do not restore it as a sticky value.
        let ptr = value & !0x0f;
        let flags = (value & 0x0f) & !ERDP_EHB;
        self.erdp = ptr | flags;
    }
}
