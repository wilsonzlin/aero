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

pub const HDA_SD0CTL: u32 = 0x80;
pub const HDA_SD0LPIB: u32 = 0x84;
pub const HDA_SD0CBL: u32 = 0x88;
pub const HDA_SD0LVI: u32 = 0x8C;
pub const HDA_SD0FMT: u32 = 0x92;
pub const HDA_SD0BDPL: u32 = 0x98;
pub const HDA_SD0BDPU: u32 = 0x9C;

pub const GCTL_CRST: u32 = 1 << 0;

pub const INTCTL_GIE: u32 = 1 << 31;
pub const INTCTL_CIE: u32 = 1 << 30;
pub const INTCTL_SIE0: u32 = 1 << 0;

pub const INTSTS_GIS: u32 = 1 << 31;
pub const INTSTS_CIS: u32 = 1 << 30;
pub const INTSTS_SIS0: u32 = 1 << 0;

pub const CORBCTL_RUN: u8 = 1 << 1;

pub const RIRBCTL_RUN: u8 = 1 << 1;
pub const RIRBCTL_INTCTL: u8 = 1 << 0;

pub const SD_CTL_SRST: u32 = 1 << 0;
pub const SD_CTL_RUN: u32 = 1 << 1;

pub const SD_STS_BCIS: u8 = 1 << 2;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StreamId {
    Out0,
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
    Corb(CorbReg),
    Rirb(RirbReg),
    Stream0(StreamReg),
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
            HDA_SD0FMT => Some(Self::Stream0(StreamReg::Fmt)),
            HDA_SD0BDPL => Some(Self::Stream0(StreamReg::Bdpl)),
            HDA_SD0BDPU => Some(Self::Stream0(StreamReg::Bdpu)),
            _ => None,
        }
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
    Fmt,
    Bdpl,
    Bdpu,
}

pub fn gcap_with_streams(out: u8, input: u8, bidir: u8) -> u16 {
    // Bits layout (Intel HDA spec): OSS[3:0], ISS[7:4], BSS[11:8].
    ((out as u16) & 0xF) | (((input as u16) & 0xF) << 4) | (((bidir as u16) & 0xF) << 8)
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
