use memory::MemoryBus;

use super::regs::*;

#[derive(Debug, Clone)]
pub struct AudioRingBuffer {
    buf: Vec<u8>,
    read: usize,
    write: usize,
    len: usize,
}

impl AudioRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0; capacity.max(1)],
            read: 0,
            write: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn clear(&mut self) {
        self.read = 0;
        self.write = 0;
        self.len = 0;
    }

    pub fn push(&mut self, data: &[u8]) {
        for &b in data {
            if self.len == self.buf.len() {
                // Drop oldest byte on overflow.
                self.read = (self.read + 1) % self.buf.len();
                self.len -= 1;
            }
            self.buf[self.write] = b;
            self.write = (self.write + 1) % self.buf.len();
            self.len += 1;
        }
    }

    pub fn drain_all(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len);
        while self.len > 0 {
            out.push(self.buf[self.read]);
            self.read = (self.read + 1) % self.buf.len();
            self.len -= 1;
        }
        out
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub bits_per_sample: u8,
    pub channels: u8,
}

impl StreamFormat {
    pub fn from_hda_fmt(fmt: u16) -> Self {
        // This matches the common HDA encoding used by Linux and QEMU. It is
        // sufficient for 44.1/48kHz and 16-bit, which is what Windows
        // typically configures initially.
        let base = if fmt & (1 << 14) != 0 { 44100 } else { 48000 };

        let mult = match (fmt >> 11) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            4 => 5,
            5 => 6,
            6 => 7,
            7 => 8,
            _ => 1,
        };
        let div = match (fmt >> 8) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            4 => 5,
            5 => 6,
            6 => 7,
            7 => 8,
            _ => 1,
        };

        let bits = match (fmt >> 4) & 0x7 {
            0 => 8,
            1 => 16,
            2 => 20,
            3 => 24,
            4 => 32,
            _ => 16,
        };
        let channels = ((fmt & 0xF) + 1) as u8;

        Self {
            sample_rate: (base * mult) / div,
            bits_per_sample: bits,
            channels,
        }
    }
}

#[derive(Debug, Copy, Clone)]
struct BdlEntry {
    addr: u64,
    len: u32,
    ioc: bool,
}

#[derive(Debug)]
pub struct HdaStream {
    id: StreamId,

    ctl: u32, // low 24 bits
    sts: u8,
    lpib: u32,
    cbl: u32,
    lvi: u16,
    fmt: u16,
    bdpl: u32,
    bdpu: u32,

    bdl_index: u16,
    bdl_offset: u32,
}

impl HdaStream {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            ctl: 0,
            sts: 0,
            lpib: 0,
            cbl: 0,
            lvi: 0,
            fmt: 0,
            bdpl: 0,
            bdpu: 0,
            bdl_index: 0,
            bdl_offset: 0,
        }
    }

    pub fn reset(&mut self) {
        self.ctl = 0;
        self.sts = 0;
        self.lpib = 0;
        self.cbl = 0;
        self.lvi = 0;
        self.fmt = 0;
        self.bdpl = 0;
        self.bdpu = 0;
        self.bdl_index = 0;
        self.bdl_offset = 0;
    }

    fn bdl_base(&self) -> u64 {
        ((self.bdpu as u64) << 32) | (self.bdpl as u64 & !0x7F)
    }

    fn is_running(&self) -> bool {
        (self.ctl & SD_CTL_RUN != 0) && (self.ctl & SD_CTL_SRST != 0)
    }

    pub fn mmio_read(&self, reg: StreamReg, size: usize) -> u64 {
        match reg {
            StreamReg::CtlSts => {
                ((self.sts as u32 as u64) << 24 | self.ctl as u64) & mask_for_size(size)
            }
            StreamReg::Lpib => self.lpib as u64 & mask_for_size(size),
            StreamReg::Cbl => self.cbl as u64 & mask_for_size(size),
            StreamReg::Lvi => self.lvi as u64 & mask_for_size(size),
            StreamReg::Fmt => self.fmt as u64 & mask_for_size(size),
            StreamReg::Bdpl => self.bdpl as u64 & mask_for_size(size),
            StreamReg::Bdpu => self.bdpu as u64 & mask_for_size(size),
        }
    }

    pub fn mmio_write(&mut self, reg: StreamReg, size: usize, value: u64, intsts: &mut u32) {
        let value = value & mask_for_size(size);
        match reg {
            StreamReg::CtlSts => {
                let w = value as u32;
                let new_ctl = w & 0x00FF_FFFF;
                let sts_clear = (w >> 24) as u8;
                if sts_clear != 0 {
                    self.sts &= !sts_clear;
                    if self.sts & SD_STS_BCIS == 0 {
                        *intsts &= !INTSTS_SIS0;
                    }
                }

                let old_ctl = self.ctl;
                self.ctl = new_ctl;

                // Stream reset deasserted -> reset internal state.
                if (old_ctl & SD_CTL_SRST != 0) && (new_ctl & SD_CTL_SRST == 0) {
                    self.lpib = 0;
                    self.sts = 0;
                    self.bdl_index = 0;
                    self.bdl_offset = 0;
                    *intsts &= !INTSTS_SIS0;
                }
            }
            StreamReg::Lpib => {
                // Read-only in hardware.
                let _ = value;
            }
            StreamReg::Cbl => self.cbl = value as u32,
            StreamReg::Lvi => self.lvi = value as u16,
            StreamReg::Fmt => self.fmt = value as u16,
            StreamReg::Bdpl => self.bdpl = value as u32,
            StreamReg::Bdpu => self.bdpu = value as u32,
        }
    }

    pub fn process(
        &mut self,
        mem: &mut dyn MemoryBus,
        audio: &mut AudioRingBuffer,
        intsts: &mut u32,
    ) {
        if !self.is_running() {
            return;
        }
        if self.cbl == 0 {
            return;
        }

        // Consume at most one BDL entry per call. Real hardware is paced by the
        // link; callers are expected to invoke `poll()` periodically.
        let max_entries = self.lvi as usize + 1;
        for _ in 0..max_entries.max(1) {
            let entry = self.read_bdl_entry(mem, self.bdl_index);
            let remaining = entry.len.saturating_sub(self.bdl_offset);
            if remaining == 0 {
                // Skip empty entries.
                self.finish_bdl_entry(entry, intsts);
                continue;
            }

            let mut buf = vec![0u8; remaining as usize];
            mem.read_physical(entry.addr + self.bdl_offset as u64, &mut buf);
            audio.push(&buf);

            self.bdl_offset += remaining;
            self.lpib = self.lpib.wrapping_add(remaining) % self.cbl;
            self.finish_bdl_entry(entry, intsts);
            break;
        }
    }

    fn finish_bdl_entry(&mut self, entry: BdlEntry, intsts: &mut u32) {
        if self.bdl_offset < entry.len {
            return;
        }
        self.bdl_offset = 0;
        if entry.ioc {
            self.sts |= SD_STS_BCIS;
            match self.id {
                StreamId::Out0 => *intsts |= INTSTS_SIS0,
            }
        }

        if self.bdl_index >= self.lvi {
            self.bdl_index = 0;
        } else {
            self.bdl_index += 1;
        }
    }

    fn read_bdl_entry(&self, mem: &mut dyn MemoryBus, index: u16) -> BdlEntry {
        let addr = self.bdl_base() + (index as u64) * 16;
        let buf_addr = read_u64(mem, addr);
        let len = mem.read_u32(addr + 8);
        let flags = mem.read_u32(addr + 12);
        BdlEntry {
            addr: buf_addr,
            len,
            ioc: (flags & 1) != 0,
        }
    }
}

fn read_u64(mem: &mut dyn MemoryBus, addr: u64) -> u64 {
    let mut buf = [0u8; 8];
    mem.read_physical(addr, &mut buf);
    u64::from_le_bytes(buf)
}

fn mask_for_size(size: usize) -> u64 {
    match size {
        1 => 0xFF,
        2 => 0xFFFF,
        4 => 0xFFFF_FFFF,
        8 => 0xFFFF_FFFF_FFFF_FFFF,
        _ => 0xFFFF_FFFF_FFFF_FFFF,
    }
}
