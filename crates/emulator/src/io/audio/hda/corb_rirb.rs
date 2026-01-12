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

    fn base(&self) -> u64 {
        ((self.ubase as u64) << 32) | (self.lbase as u64 & !0x3)
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
                self.size = (self.size & !0x3) | (value as u8 & 0x3);
            }
        }
    }

    pub fn pop_command(&mut self, mem: &mut dyn MemoryBus) -> Option<CodecCmd> {
        let entries = corb_entries(self.size);
        if self.rp == self.wp {
            return None;
        }
        self.rp = (self.rp + 1) % entries;

        let addr = self.base() + (self.rp as u64) * 4;
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
        ((self.ubase as u64) << 32) | (self.lbase as u64 & !0x7)
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
        self.wp = (self.wp + 1) % entries;

        let addr = self.base() + (self.wp as u64) * 8;
        let encoded = resp.encode();
        write_u64(mem, addr, encoded);

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
