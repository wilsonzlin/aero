use memory::MemoryBus;

use super::codec::{CodecCmd, HdaVerbResponse};
use super::mask_for_size;
use super::regs::*;

#[derive(Debug)]
pub struct Corb {
    lbase: u32,
    ubase: u32,
    wp: u16,
    rp: u16,
    ctl: u8,
    sts: u8,
    size: u8,
}

impl Corb {
    pub fn new() -> Self {
        Self {
            lbase: 0,
            ubase: 0,
            wp: 0,
            rp: 0,
            ctl: 0,
            sts: 0,
            size: RING_SIZE_CAP_2 | RING_SIZE_CAP_16 | RING_SIZE_CAP_256,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn is_running(&self) -> bool {
        self.ctl & CORBCTL_RUN != 0
    }

    pub fn entries(&self) -> u16 {
        corb_entries(self.size)
    }

    pub fn sanitize_pointers(&mut self) {
        let entries = self.entries();
        let mask = entries.saturating_sub(1);
        self.wp &= mask;
        self.rp &= mask;
    }

    fn base(&self) -> u64 {
        // CORB base is 128-byte aligned in the Intel HDA spec; bits 6:0 are reserved.
        ((self.ubase as u64) << 32) | (self.lbase as u64 & !0x7f)
    }

    pub fn mmio_read(&self, reg: CorbReg, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        match reg {
            CorbReg::Lbase => self.lbase as u64 & mask_for_size(size),
            CorbReg::Ubase => self.ubase as u64 & mask_for_size(size),
            CorbReg::Wp => self.wp as u64 & mask_for_size(size),
            CorbReg::Rp => self.rp as u64 & mask_for_size(size),
            CorbReg::Ctl => self.ctl as u64 & mask_for_size(size),
            CorbReg::Sts => self.sts as u64 & mask_for_size(size),
            CorbReg::Size => self.size as u64 & mask_for_size(size),
        }
    }

    pub fn mmio_write(&mut self, reg: CorbReg, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let value = value & mask_for_size(size);
        match reg {
            CorbReg::Lbase => self.lbase = value as u32,
            CorbReg::Ubase => self.ubase = value as u32,
            CorbReg::Wp => {
                let entries = corb_entries(self.size);
                let mask = entries.saturating_sub(1);
                self.wp = (value as u16) & mask;
            }
            CorbReg::Rp => {
                let val = value as u16;
                if val & 0x8000 != 0 {
                    self.rp = 0;
                } else {
                    let entries = corb_entries(self.size);
                    let mask = entries.saturating_sub(1);
                    self.rp = val & mask;
                }
            }
            CorbReg::Ctl => self.ctl = value as u8,
            CorbReg::Sts => {
                // W1C.
                self.sts &= !(value as u8);
            }
            CorbReg::Size => {
                // Only the size selection bits (1:0) are writable; capability bits are RO.
                let old_sel = self.size & 0x3;
                self.size = (self.size & !0x3) | (value as u8 & 0x3);
                if (self.size & 0x3) != old_sel {
                    // Keep the read/write pointers in-range for the newly-selected ring size.
                    //
                    // If the CORB size is reduced after CORBWP was programmed for a larger ring,
                    // the (now out-of-range) WP would never be reached by RP modulo the new size,
                    // causing `pop_command` to spin forever.
                    self.sanitize_pointers();
                }
            }
        }
    }

    pub fn pop_command(&mut self, mem: &mut dyn MemoryBus) -> Option<CodecCmd> {
        let entries = corb_entries(self.size);
        if self.rp == self.wp {
            return None;
        }
        let next_rp = (self.rp + 1) % entries;

        let addr = (next_rp as u64)
            .checked_mul(4)
            .and_then(|offset| self.base().checked_add(offset));

        // Always advance RP, even when the address overflows: otherwise we'd reattempt forever.
        self.rp = next_rp;

        let Some(addr) = addr else {
            // If the guest programmed a CORB base address close to `u64::MAX`, adding the entry
            // offset can overflow in debug builds (panic) and wrap in release builds (DMA into low
            // memory). Skip the DMA read.
            return None;
        };

        let cmd = mem.read_u32(addr);
        Some(CodecCmd::decode(cmd))
    }
}

#[derive(Debug)]
pub struct Rirb {
    lbase: u32,
    ubase: u32,
    wp: u16,
    rintcnt: u16,
    ctl: u8,
    sts: u8,
    size: u8,
    responses_since_irq: u16,
}

impl Rirb {
    pub fn new() -> Self {
        Self {
            lbase: 0,
            ubase: 0,
            wp: 0,
            rintcnt: 0,
            ctl: 0,
            sts: 0,
            size: RING_SIZE_CAP_2 | RING_SIZE_CAP_16 | RING_SIZE_CAP_256,
            responses_since_irq: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn is_running(&self) -> bool {
        self.ctl & RIRBCTL_RUN != 0
    }

    fn base(&self) -> u64 {
        // RIRB base is 128-byte aligned in the Intel HDA spec; bits 6:0 are reserved.
        ((self.ubase as u64) << 32) | (self.lbase as u64 & !0x7f)
    }

    pub fn mmio_read(&self, reg: RirbReg, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        match reg {
            RirbReg::Lbase => self.lbase as u64 & mask_for_size(size),
            RirbReg::Ubase => self.ubase as u64 & mask_for_size(size),
            RirbReg::Wp => self.wp as u64 & mask_for_size(size),
            RirbReg::RintCnt => self.rintcnt as u64 & mask_for_size(size),
            RirbReg::Ctl => self.ctl as u64 & mask_for_size(size),
            RirbReg::Sts => self.sts as u64 & mask_for_size(size),
            RirbReg::Size => self.size as u64 & mask_for_size(size),
        }
    }

    pub fn mmio_write(&mut self, reg: RirbReg, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let value = value & mask_for_size(size);
        match reg {
            RirbReg::Lbase => self.lbase = value as u32,
            RirbReg::Ubase => self.ubase = value as u32,
            RirbReg::Wp => {
                let val = value as u16;
                if val & 0x8000 != 0 {
                    self.wp = 0;
                    self.responses_since_irq = 0;
                }
            }
            RirbReg::RintCnt => self.rintcnt = value as u16,
            RirbReg::Ctl => self.ctl = value as u8,
            RirbReg::Sts => {
                // W1C.
                self.sts &= !(value as u8);
            }
            RirbReg::Size => {
                self.size = (self.size & !0x3) | (value as u8 & 0x3);
            }
        }
    }

    pub fn push_response(
        &mut self,
        mem: &mut dyn MemoryBus,
        resp: HdaVerbResponse,
        intsts: &mut u32,
    ) {
        let entries = rirb_entries(self.size);
        let next_wp = (self.wp + 1) % entries;

        if let Some(addr) = (next_wp as u64)
            .checked_mul(8)
            .and_then(|offset| self.base().checked_add(offset))
        {
            let encoded = resp.encode();
            write_u64(mem, addr, encoded);
        } else {
            // Same overflow hazard as CORB: skip DMA writes on overflow so we never wrap around
            // into low guest physical addresses in release builds.
        }

        self.wp = next_wp;
        self.responses_since_irq = self.responses_since_irq.wrapping_add(1);
        let threshold = self.rintcnt.max(1);
        if (self.ctl & RIRBCTL_INTCTL != 0) && self.responses_since_irq >= threshold {
            self.responses_since_irq = 0;
            self.sts |= 0x01; // RINTFL
            *intsts |= INTSTS_CIS;
        }
    }
}

fn write_u64(mem: &mut dyn MemoryBus, addr: u64, value: u64) {
    mem.write_physical(addr, &value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory::Bus;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    const CAPS_MASK: u8 = RING_SIZE_CAP_2 | RING_SIZE_CAP_16 | RING_SIZE_CAP_256;

    #[derive(Debug)]
    struct PanicOnReadMem;

    impl MemoryBus for PanicOnReadMem {
        fn read_physical(&mut self, _addr: u64, _dst: &mut [u8]) {
            panic!("unexpected guest memory read");
        }

        fn write_physical(&mut self, _addr: u64, _src: &[u8]) {}
    }

    #[derive(Debug)]
    struct PanicOnWriteMem;

    impl MemoryBus for PanicOnWriteMem {
        fn read_physical(&mut self, _addr: u64, dst: &mut [u8]) {
            dst.fill(0);
        }

        fn write_physical(&mut self, _addr: u64, _src: &[u8]) {
            panic!("unexpected guest memory write");
        }
    }

    #[test]
    fn corb_size_preserves_capability_bits_and_allows_selection_bits() {
        let mut corb = Corb::new();

        // All ring sizes are supported by the legacy model; the capability bits should always
        // remain set, regardless of what the guest writes.
        assert_eq!(
            corb.mmio_read(CorbReg::Size, 1) as u8 & CAPS_MASK,
            CAPS_MASK
        );

        // Attempt to clear everything (including capability bits) by writing 0.
        corb.mmio_write(CorbReg::Size, 1, 0);
        assert_eq!(
            corb.mmio_read(CorbReg::Size, 1) as u8 & CAPS_MASK,
            CAPS_MASK
        );

        // Selection bits are writable.
        corb.mmio_write(CorbReg::Size, 1, 0x1);
        assert_eq!(corb.mmio_read(CorbReg::Size, 1) as u8 & 0x3, 0x1);
        assert_eq!(
            corb.mmio_read(CorbReg::Size, 1) as u8 & CAPS_MASK,
            CAPS_MASK
        );

        corb.mmio_write(CorbReg::Size, 1, 0x2);
        assert_eq!(corb.mmio_read(CorbReg::Size, 1) as u8 & 0x3, 0x2);
        assert_eq!(
            corb.mmio_read(CorbReg::Size, 1) as u8 & CAPS_MASK,
            CAPS_MASK
        );

        // High bits (including reserved bits) are RO; writing 0xff must not affect them.
        corb.mmio_write(CorbReg::Size, 1, 0xff);
        assert_eq!(corb.mmio_read(CorbReg::Size, 1) as u8 & 0xFC, CAPS_MASK);
        assert_eq!(corb.mmio_read(CorbReg::Size, 1) as u8 & 0x3, 0x3);
    }

    #[test]
    fn corb_wp_and_rp_are_masked_to_selected_ring_size() {
        let mut corb = Corb::new();

        // 2 entries (mask 0x1)
        corb.mmio_write(CorbReg::Size, 1, 0x0);
        corb.mmio_write(CorbReg::Wp, 2, 0xFFFF);
        assert_eq!(corb.wp, 1);
        corb.mmio_write(CorbReg::Rp, 2, 0x7FFF);
        assert_eq!(corb.rp, 1);

        // 16 entries (mask 0xF)
        corb.mmio_write(CorbReg::Size, 1, 0x1);
        corb.mmio_write(CorbReg::Wp, 2, 0xFFFF);
        assert_eq!(corb.wp, 0xF);
        corb.mmio_write(CorbReg::Rp, 2, 0x7FFF);
        assert_eq!(corb.rp, 0xF);

        // 256 entries (mask 0xFF)
        corb.mmio_write(CorbReg::Size, 1, 0x2);
        corb.mmio_write(CorbReg::Wp, 2, 0xFFFF);
        assert_eq!(corb.wp, 0xFF);
        corb.mmio_write(CorbReg::Rp, 2, 0x7FFF);
        assert_eq!(corb.rp, 0xFF);
    }

    #[test]
    fn corb_rp_reset_bit_clears_read_pointer() {
        let mut corb = Corb::new();
        corb.mmio_write(CorbReg::Size, 1, 0x1); // 16 entries

        corb.mmio_write(CorbReg::Rp, 2, 0x5);
        assert_eq!(corb.rp, 0x5);

        // Setting bit 15 is a write-only reset request.
        corb.mmio_write(CorbReg::Rp, 2, 0x8000 | 0x5);
        assert_eq!(corb.rp, 0);
    }

    #[test]
    fn corb_pop_command_does_not_overflow_address_or_dma_read() {
        // Regression test: base + offset used to overflow/panic in debug builds and wrap in
        // release builds, causing DMA reads from low addresses.
        let mut corb = Corb::new();
        // Select 256-entry ring to allow rp=31->32 (offset=128).
        corb.mmio_write(CorbReg::Size, 1, 0x2);
        // Program base close to u64::MAX. Low bits are masked to 128-byte alignment.
        corb.mmio_write(CorbReg::Lbase, 4, 0xFFFF_FFFF);
        corb.mmio_write(CorbReg::Ubase, 4, 0xFFFF_FFFF);

        // Force a command fetch at rp=32, which would make base+128 overflow.
        corb.rp = 31;
        corb.wp = 32;

        let mut mem = PanicOnReadMem;
        let result = catch_unwind(AssertUnwindSafe(|| corb.pop_command(&mut mem)));
        assert!(result.is_ok(), "pop_command panicked on address overflow");
        // RP must still advance so we don't retry forever.
        assert_eq!(corb.rp, 32);
    }

    #[test]
    fn rirb_size_preserves_capability_bits_and_allows_selection_bits() {
        let mut rirb = Rirb::new();

        assert_eq!(
            rirb.mmio_read(RirbReg::Size, 1) as u8 & CAPS_MASK,
            CAPS_MASK
        );

        rirb.mmio_write(RirbReg::Size, 1, 0);
        assert_eq!(
            rirb.mmio_read(RirbReg::Size, 1) as u8 & CAPS_MASK,
            CAPS_MASK
        );

        rirb.mmio_write(RirbReg::Size, 1, 0x1);
        assert_eq!(rirb.mmio_read(RirbReg::Size, 1) as u8 & 0x3, 0x1);
        assert_eq!(
            rirb.mmio_read(RirbReg::Size, 1) as u8 & CAPS_MASK,
            CAPS_MASK
        );

        rirb.mmio_write(RirbReg::Size, 1, 0xff);
        assert_eq!(rirb.mmio_read(RirbReg::Size, 1) as u8 & 0xFC, CAPS_MASK);
        assert_eq!(rirb.mmio_read(RirbReg::Size, 1) as u8 & 0x3, 0x3);
    }

    #[test]
    fn rirb_wp_reset_clears_wp_and_responses_since_irq() {
        let mut mem = Bus::new(0x10_000);
        let mut rirb = Rirb::new();

        rirb.mmio_write(RirbReg::Lbase, 4, 0x1000);
        rirb.mmio_write(RirbReg::Ubase, 4, 0);
        rirb.mmio_write(RirbReg::Size, 1, 0x0); // 2 entries
        let mut intsts = 0u32;
        rirb.push_response(&mut mem, HdaVerbResponse { data: 0, ext: 0 }, &mut intsts);
        assert_eq!(rirb.wp, 1);
        assert_eq!(rirb.responses_since_irq, 1);

        // Writes without bit 15 set are ignored.
        rirb.mmio_write(RirbReg::Wp, 2, 0x0000);
        assert_eq!(rirb.wp, 1);
        assert_eq!(rirb.responses_since_irq, 1);

        // Writes with bit 15 set reset the WP and associated response counter.
        rirb.mmio_write(RirbReg::Wp, 2, 0x8000);
        assert_eq!(rirb.wp, 0);
        assert_eq!(rirb.responses_since_irq, 0);
    }

    #[test]
    fn rirb_interrupt_threshold_sets_rintfl_and_controller_intsts() {
        let mut mem = Bus::new(0x10_000);
        let mut rirb = Rirb::new();

        rirb.mmio_write(RirbReg::Lbase, 4, 0x2000);
        rirb.mmio_write(RirbReg::Ubase, 4, 0);
        rirb.mmio_write(RirbReg::Size, 1, 0x1); // 16 entries

        let threshold = 3u16;
        rirb.mmio_write(RirbReg::RintCnt, 2, threshold as u64);
        rirb.mmio_write(RirbReg::Ctl, 1, RIRBCTL_INTCTL as u64);

        let mut intsts = 0u32;
        for i in 0..(threshold - 1) {
            rirb.push_response(
                &mut mem,
                HdaVerbResponse {
                    data: i as u32,
                    ext: 0,
                },
                &mut intsts,
            );
            assert_eq!(intsts & INTSTS_CIS, 0);
            assert_eq!(rirb.sts & 0x01, 0);
            assert_eq!(rirb.responses_since_irq, i + 1);
        }

        // The Nth response triggers an interrupt.
        rirb.push_response(
            &mut mem,
            HdaVerbResponse {
                data: 0xDEAD_BEEF,
                ext: 0,
            },
            &mut intsts,
        );
        assert_eq!(intsts & INTSTS_CIS, INTSTS_CIS);
        assert_eq!(rirb.sts & 0x01, 0x01);
        assert_eq!(rirb.responses_since_irq, 0);
    }
    #[test]
    fn rirb_push_response_does_not_overflow_address_or_dma_write() {
        // Regression test: base + offset used to overflow/panic in debug builds and wrap in
        // release builds, causing DMA writes into low addresses.
        let mut rirb = Rirb::new();
        // Select 256-entry ring to allow wp=15->16 (offset=128).
        rirb.mmio_write(RirbReg::Size, 1, 0x2);
        // Program base close to u64::MAX (aligned down to 128 bytes).
        rirb.mmio_write(RirbReg::Lbase, 4, 0xFFFF_FFFF);
        rirb.mmio_write(RirbReg::Ubase, 4, 0xFFFF_FFFF);

        // Force the next response to target wp=16.
        rirb.wp = 15;

        let mut mem = PanicOnWriteMem;
        let mut intsts = 0u32;
        let result = catch_unwind(AssertUnwindSafe(|| {
            rirb.push_response(
                &mut mem,
                HdaVerbResponse {
                    data: 0x1234_5678,
                    ext: 0,
                },
                &mut intsts,
            );
        }));
        assert!(result.is_ok(), "push_response panicked on address overflow");
        // WP and internal bookkeeping must still advance.
        assert_eq!(rirb.wp, 16);
        assert_eq!(rirb.responses_since_irq, 1);
    }
}
