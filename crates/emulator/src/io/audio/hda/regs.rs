//! MMIO register layout and bit definitions for an Intel HDA controller.

pub const HDA_GCAP: u32 = 0x00;
pub const HDA_VMIN: u32 = 0x02;
pub const HDA_VMAJ: u32 = 0x03;
pub const HDA_GCTL: u32 = 0x08;
pub const HDA_WAKEEN: u32 = 0x0C;
pub const HDA_STATESTS: u32 = 0x0E;
pub const HDA_GSTS: u32 = 0x10;
pub const HDA_INTCTL: u32 = 0x20;
pub const HDA_INTSTS: u32 = 0x24;

pub const HDA_CORBLBASE: u32 = 0x40;
pub const HDA_CORBUBASE: u32 = 0x44;
pub const HDA_CORBWP: u32 = 0x48;
pub const HDA_CORBRP: u32 = 0x4A;
pub const HDA_CORBCTL: u32 = 0x4C;
pub const HDA_CORBSTS: u32 = 0x4D;
pub const HDA_CORBSIZE: u32 = 0x4E;

pub const HDA_RIRBLBASE: u32 = 0x50;
pub const HDA_RIRBUBASE: u32 = 0x54;
pub const HDA_RIRBWP: u32 = 0x58;
pub const HDA_RINTCNT: u32 = 0x5A;
pub const HDA_RIRBCTL: u32 = 0x5C;
pub const HDA_RIRBSTS: u32 = 0x5D;
pub const HDA_RIRBSIZE: u32 = 0x5E;

// CORBSIZE/RIRBSIZE capability bits (RO) as defined by the Intel HDA spec.
pub const RING_SIZE_CAP_2: u8 = 1 << 4;
pub const RING_SIZE_CAP_16: u8 = 1 << 5;
pub const RING_SIZE_CAP_256: u8 = 1 << 6;

pub const HDA_DPLBASE: u32 = 0x70;
pub const HDA_DPUBASE: u32 = 0x74;

pub const HDA_SD0CTL: u32 = 0x80;
pub const HDA_SD0LPIB: u32 = 0x84;
pub const HDA_SD0CBL: u32 = 0x88;
pub const HDA_SD0LVI: u32 = 0x8C;
pub const HDA_SD0FIFOW: u32 = 0x8E;
pub const HDA_SD0FIFOS: u32 = 0x90;
pub const HDA_SD0FMT: u32 = 0x92;
pub const HDA_SD0BDPL: u32 = 0x98;
pub const HDA_SD0BDPU: u32 = 0x9C;

pub const HDA_SD1CTL: u32 = 0xA0;
pub const HDA_SD1LPIB: u32 = 0xA4;
pub const HDA_SD1CBL: u32 = 0xA8;
pub const HDA_SD1LVI: u32 = 0xAC;
pub const HDA_SD1FMT: u32 = 0xB2;
pub const HDA_SD1BDPL: u32 = 0xB8;
pub const HDA_SD1BDPU: u32 = 0xBC;

pub const GCTL_CRST: u32 = 1 << 0;

pub const INTCTL_GIE: u32 = 1 << 31;
pub const INTCTL_CIE: u32 = 1 << 30;
pub const INTCTL_SIE0: u32 = 1 << 0;
pub const INTCTL_SIE1: u32 = 1 << 1;

pub const INTSTS_GIS: u32 = 1 << 31;
pub const INTSTS_CIS: u32 = 1 << 30;
pub const INTSTS_SIS0: u32 = 1 << 0;
pub const INTSTS_SIS1: u32 = 1 << 1;

pub const CORBCTL_RUN: u8 = 1 << 1;

pub const RIRBCTL_RUN: u8 = 1 << 1;
pub const RIRBCTL_INTCTL: u8 = 1 << 0;

pub const SD_CTL_SRST: u32 = 1 << 0;
pub const SD_CTL_RUN: u32 = 1 << 1;
pub const SD_CTL_IOCE: u32 = 1 << 2;

pub const SD_STS_BCIS: u8 = 1 << 2;

pub const DPLBASE_ENABLE: u32 = 1 << 0;
const DPLBASE_BASE_MASK: u32 = !0x7f;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StreamId {
    Out0,
    In0,
}

impl StreamId {
    pub fn posbuf_index(self) -> u8 {
        match self {
            StreamId::Out0 => 0,
            StreamId::In0 => 1,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum HdaMmioReg {
    Gcap,
    Vmin,
    Vmaj,
    Gctl,
    Wakeen,
    Statests,
    Gsts,
    Intctl,
    Intsts,
    Dplbase,
    Dpubase,
    Corb(CorbReg),
    Rirb(RirbReg),
    Stream0(StreamReg),
    Stream1(StreamReg),
}

/// A decoded MMIO address pointing at a specific byte within an HDA register.
///
/// The legacy HDA controller supports sub-word and cross-register accesses by
/// decoding at byte granularity. Higher-level MMIO reads/writes (1/2/4 bytes)
/// are composed from consecutive byte accesses.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct HdaMmioRegByte {
    pub reg: HdaMmioReg,
    pub byte: u8,
}

impl HdaMmioReg {
    pub fn decode(offset: u32) -> Option<Self> {
        match offset {
            HDA_GCAP => Some(Self::Gcap),
            HDA_VMIN => Some(Self::Vmin),
            HDA_VMAJ => Some(Self::Vmaj),
            HDA_GCTL => Some(Self::Gctl),
            HDA_WAKEEN => Some(Self::Wakeen),
            HDA_STATESTS => Some(Self::Statests),
            HDA_GSTS => Some(Self::Gsts),
            HDA_INTCTL => Some(Self::Intctl),
            HDA_INTSTS => Some(Self::Intsts),
            HDA_DPLBASE => Some(Self::Dplbase),
            HDA_DPUBASE => Some(Self::Dpubase),
            HDA_CORBLBASE => Some(Self::Corb(CorbReg::Lbase)),
            HDA_CORBUBASE => Some(Self::Corb(CorbReg::Ubase)),
            HDA_CORBWP => Some(Self::Corb(CorbReg::Wp)),
            HDA_CORBRP => Some(Self::Corb(CorbReg::Rp)),
            HDA_CORBCTL => Some(Self::Corb(CorbReg::Ctl)),
            HDA_CORBSTS => Some(Self::Corb(CorbReg::Sts)),
            HDA_CORBSIZE => Some(Self::Corb(CorbReg::Size)),
            HDA_RIRBLBASE => Some(Self::Rirb(RirbReg::Lbase)),
            HDA_RIRBUBASE => Some(Self::Rirb(RirbReg::Ubase)),
            HDA_RIRBWP => Some(Self::Rirb(RirbReg::Wp)),
            HDA_RINTCNT => Some(Self::Rirb(RirbReg::RintCnt)),
            HDA_RIRBCTL => Some(Self::Rirb(RirbReg::Ctl)),
            HDA_RIRBSTS => Some(Self::Rirb(RirbReg::Sts)),
            HDA_RIRBSIZE => Some(Self::Rirb(RirbReg::Size)),
            HDA_SD0CTL => Some(Self::Stream0(StreamReg::CtlSts)),
            HDA_SD0LPIB => Some(Self::Stream0(StreamReg::Lpib)),
            HDA_SD0CBL => Some(Self::Stream0(StreamReg::Cbl)),
            HDA_SD0LVI => Some(Self::Stream0(StreamReg::Lvi)),
            HDA_SD0FIFOW => Some(Self::Stream0(StreamReg::Fifow)),
            HDA_SD0FIFOS => Some(Self::Stream0(StreamReg::Fifos)),
            HDA_SD0FMT => Some(Self::Stream0(StreamReg::Fmt)),
            HDA_SD0BDPL => Some(Self::Stream0(StreamReg::Bdpl)),
            HDA_SD0BDPU => Some(Self::Stream0(StreamReg::Bdpu)),
            HDA_SD1CTL => Some(Self::Stream1(StreamReg::CtlSts)),
            HDA_SD1LPIB => Some(Self::Stream1(StreamReg::Lpib)),
            HDA_SD1CBL => Some(Self::Stream1(StreamReg::Cbl)),
            HDA_SD1LVI => Some(Self::Stream1(StreamReg::Lvi)),
            HDA_SD1FMT => Some(Self::Stream1(StreamReg::Fmt)),
            HDA_SD1BDPL => Some(Self::Stream1(StreamReg::Bdpl)),
            HDA_SD1BDPU => Some(Self::Stream1(StreamReg::Bdpu)),
            _ => None,
        }
    }

    /// Decode an MMIO offset into a register + byte index.
    ///
    /// This is used to support real driver access patterns such as:
    /// - Reading `GCAP`/`VMIN`/`VMAJ` as a single dword at offset 0x00.
    /// - Accessing the stream status byte at `SDnCTL+3`.
    /// - Reading across adjacent registers (e.g. dword at `SDnLVI` spanning `LVI+FIFOW`).
    pub fn decode_byte(offset: u32) -> Option<HdaMmioRegByte> {
        // Global registers.
        if offset >= HDA_GCAP && offset < HDA_GCAP + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Gcap,
                byte: (offset - HDA_GCAP) as u8,
            });
        }
        if offset == HDA_VMIN {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Vmin,
                byte: 0,
            });
        }
        if offset == HDA_VMAJ {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Vmaj,
                byte: 0,
            });
        }
        if offset >= HDA_GCTL && offset < HDA_GCTL + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Gctl,
                byte: (offset - HDA_GCTL) as u8,
            });
        }
        if offset >= HDA_WAKEEN && offset < HDA_WAKEEN + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Wakeen,
                byte: (offset - HDA_WAKEEN) as u8,
            });
        }
        if offset >= HDA_STATESTS && offset < HDA_STATESTS + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Statests,
                byte: (offset - HDA_STATESTS) as u8,
            });
        }
        if offset >= HDA_GSTS && offset < HDA_GSTS + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Gsts,
                byte: (offset - HDA_GSTS) as u8,
            });
        }
        if offset >= HDA_INTCTL && offset < HDA_INTCTL + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Intctl,
                byte: (offset - HDA_INTCTL) as u8,
            });
        }
        if offset >= HDA_INTSTS && offset < HDA_INTSTS + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Intsts,
                byte: (offset - HDA_INTSTS) as u8,
            });
        }

        // CORB registers.
        if offset >= HDA_CORBLBASE && offset < HDA_CORBLBASE + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Corb(CorbReg::Lbase),
                byte: (offset - HDA_CORBLBASE) as u8,
            });
        }
        if offset >= HDA_CORBUBASE && offset < HDA_CORBUBASE + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Corb(CorbReg::Ubase),
                byte: (offset - HDA_CORBUBASE) as u8,
            });
        }
        if offset >= HDA_CORBWP && offset < HDA_CORBWP + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Corb(CorbReg::Wp),
                byte: (offset - HDA_CORBWP) as u8,
            });
        }
        if offset >= HDA_CORBRP && offset < HDA_CORBRP + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Corb(CorbReg::Rp),
                byte: (offset - HDA_CORBRP) as u8,
            });
        }
        if offset == HDA_CORBCTL {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Corb(CorbReg::Ctl),
                byte: 0,
            });
        }
        if offset == HDA_CORBSTS {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Corb(CorbReg::Sts),
                byte: 0,
            });
        }
        if offset == HDA_CORBSIZE {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Corb(CorbReg::Size),
                byte: 0,
            });
        }

        // RIRB registers.
        if offset >= HDA_RIRBLBASE && offset < HDA_RIRBLBASE + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Rirb(RirbReg::Lbase),
                byte: (offset - HDA_RIRBLBASE) as u8,
            });
        }
        if offset >= HDA_RIRBUBASE && offset < HDA_RIRBUBASE + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Rirb(RirbReg::Ubase),
                byte: (offset - HDA_RIRBUBASE) as u8,
            });
        }
        if offset >= HDA_RIRBWP && offset < HDA_RIRBWP + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Rirb(RirbReg::Wp),
                byte: (offset - HDA_RIRBWP) as u8,
            });
        }
        if offset >= HDA_RINTCNT && offset < HDA_RINTCNT + 2 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Rirb(RirbReg::RintCnt),
                byte: (offset - HDA_RINTCNT) as u8,
            });
        }
        if offset == HDA_RIRBCTL {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Rirb(RirbReg::Ctl),
                byte: 0,
            });
        }
        if offset == HDA_RIRBSTS {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Rirb(RirbReg::Sts),
                byte: 0,
            });
        }
        if offset == HDA_RIRBSIZE {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Rirb(RirbReg::Size),
                byte: 0,
            });
        }

        // DMA position buffer registers.
        if offset >= HDA_DPLBASE && offset < HDA_DPLBASE + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Dplbase,
                byte: (offset - HDA_DPLBASE) as u8,
            });
        }
        if offset >= HDA_DPUBASE && offset < HDA_DPUBASE + 4 {
            return Some(HdaMmioRegByte {
                reg: HdaMmioReg::Dpubase,
                byte: (offset - HDA_DPUBASE) as u8,
            });
        }

        // Stream descriptor blocks.
        fn decode_stream(
            offset: u32,
            base: u32,
            wrap: fn(StreamReg) -> HdaMmioReg,
        ) -> Option<HdaMmioRegByte> {
            if offset < base || offset >= base + 0x20 {
                return None;
            }
            let rel = offset - base;
            match rel {
                0x00..=0x03 => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::CtlSts),
                    byte: rel as u8,
                }),
                0x04..=0x07 => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Lpib),
                    byte: (rel - 0x04) as u8,
                }),
                0x08..=0x0b => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Cbl),
                    byte: (rel - 0x08) as u8,
                }),
                0x0c..=0x0d => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Lvi),
                    byte: (rel - 0x0c) as u8,
                }),
                0x0e..=0x0f => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Fifow),
                    byte: (rel - 0x0e) as u8,
                }),
                0x10..=0x11 => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Fifos),
                    byte: (rel - 0x10) as u8,
                }),
                0x12..=0x13 => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Fmt),
                    byte: (rel - 0x12) as u8,
                }),
                0x18..=0x1b => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Bdpl),
                    byte: (rel - 0x18) as u8,
                }),
                0x1c..=0x1f => Some(HdaMmioRegByte {
                    reg: wrap(StreamReg::Bdpu),
                    byte: (rel - 0x1c) as u8,
                }),
                _ => None,
            }
        }

        if let Some(decoded) = decode_stream(offset, HDA_SD0CTL, HdaMmioReg::Stream0) {
            return Some(decoded);
        }
        if let Some(decoded) = decode_stream(offset, HDA_SD1CTL, HdaMmioReg::Stream1) {
            return Some(decoded);
        }

        None
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum CorbReg {
    Lbase,
    Ubase,
    Wp,
    Rp,
    Ctl,
    Sts,
    Size,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RirbReg {
    Lbase,
    Ubase,
    Wp,
    RintCnt,
    Ctl,
    Sts,
    Size,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StreamReg {
    CtlSts,
    Lpib,
    Cbl,
    Lvi,
    Fifow,
    Fifos,
    Fmt,
    Bdpl,
    Bdpu,
}

pub fn gcap_with_streams(out: u8, input: u8, bidir: u8, nsdo: u8) -> u16 {
    // Bits layout (Intel HDA spec): OSS[3:0], ISS[7:4], BSS[11:8], NSDO[15:12].
    ((out as u16) & 0xF)
        | (((input as u16) & 0xF) << 4)
        | (((bidir as u16) & 0xF) << 8)
        | (((nsdo as u16) & 0xF) << 12)
}

pub fn corb_entries(size_reg: u8) -> u16 {
    match size_reg & 0x3 {
        0 => 2,
        1 => 16,
        2 => 256,
        _ => 2,
    }
}

pub fn rirb_entries(size_reg: u8) -> u16 {
    match size_reg & 0x3 {
        0 => 2,
        1 => 16,
        2 => 256,
        _ => 2,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DmaPositionBufferRegs {
    dplbase: u32,
    dpubase: u32,
}

impl DmaPositionBufferRegs {
    pub fn dplbase(&self) -> u32 {
        self.dplbase
    }

    pub fn dpubase(&self) -> u32 {
        self.dpubase
    }

    pub fn write_dplbase(&mut self, value: u32) {
        // DPLBASE is 128-byte aligned; bits 6:1 are reserved and must read as 0.
        self.dplbase = (value & DPLBASE_ENABLE) | (value & DPLBASE_BASE_MASK);
    }

    pub fn write_dpubase(&mut self, value: u32) {
        self.dpubase = value;
    }

    pub fn enabled(&self) -> bool {
        (self.dplbase & DPLBASE_ENABLE) != 0
    }

    pub fn base_addr(&self) -> u64 {
        ((self.dpubase as u64) << 32) | (self.dplbase & DPLBASE_BASE_MASK) as u64
    }

    pub fn stream_entry_addr(&self, stream_index: u8) -> Option<u64> {
        if !self.enabled() {
            return None;
        }

        let base = self.base_addr();
        base.checked_add((stream_index as u64) * 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_entry_addr_returns_none_on_overflow_instead_of_panicking() {
        let mut regs = DmaPositionBufferRegs::default();

        // Base address chosen such that adding 16 * 8 bytes would overflow u64.
        regs.write_dpubase(0xffff_ffff);
        regs.write_dplbase(DPLBASE_ENABLE | 0xffff_ff80);

        let addr = std::panic::catch_unwind(|| regs.stream_entry_addr(16))
            .expect("stream_entry_addr must not panic on overflow");
        assert_eq!(addr, None);
    }
}
