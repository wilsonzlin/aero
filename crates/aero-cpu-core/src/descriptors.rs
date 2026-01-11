use crate::state::{CpuState, EFER_LMA, EFER_LME, SEG_ACCESS_PRESENT, SEG_ACCESS_UNUSABLE};
use crate::{CpuBus, Exception};

/// GDTR/IDTR storage (base + limit).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TableRegister {
    pub base: u64,
    pub limit: u16,
}

/// Access rights/flags common to code/data and system descriptors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DescriptorAttributes {
    /// Low 4 bits of the access byte.
    pub typ: u8,
    /// Descriptor type bit (S). When set, this is a code/data descriptor.
    pub s: bool,
    pub dpl: u8,
    pub present: bool,

    pub avl: bool,
    pub long: bool,
    pub default_big: bool,
    pub granularity: bool,

    /// Whether this cached descriptor is usable (null selectors mark the cache unusable).
    pub unusable: bool,
}

impl DescriptorAttributes {
    pub fn is_code(&self) -> bool {
        self.s && (self.typ & 0b1000 != 0)
    }

    pub fn is_data(&self) -> bool {
        self.s && (self.typ & 0b1000 == 0)
    }

    pub fn code_conforming(&self) -> bool {
        self.is_code() && (self.typ & 0b0100 != 0)
    }

    pub fn code_readable(&self) -> bool {
        self.is_code() && (self.typ & 0b0010 != 0)
    }

    pub fn data_expand_down(&self) -> bool {
        self.is_data() && (self.typ & 0b0100 != 0)
    }

    pub fn data_writable(&self) -> bool {
        self.is_data() && (self.typ & 0b0010 != 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentDescriptor {
    pub base: u64,
    pub limit: u32,
    pub attrs: DescriptorAttributes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemDescriptor {
    pub base: u64,
    pub limit: u32,
    pub attrs: DescriptorAttributes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Descriptor {
    Segment(SegmentDescriptor),
    System(SystemDescriptor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemSegmentType {
    Ldt,
    TssAvailable,
    TssBusy,
    Other(u8),
}

impl SystemSegmentType {
    pub fn from_typ(typ: u8) -> Self {
        match typ {
            0x2 => Self::Ldt,
            0x9 => Self::TssAvailable,
            0xB => Self::TssBusy,
            other => Self::Other(other),
        }
    }
}

impl Default for SystemSegmentType {
    fn default() -> Self {
        Self::Other(0)
    }
}

impl Descriptor {
    pub fn attrs(&self) -> DescriptorAttributes {
        match self {
            Descriptor::Segment(s) => s.attrs,
            Descriptor::System(s) => s.attrs,
        }
    }
}

fn parse_common_fields(raw_low: u64) -> (u32, u32, DescriptorAttributes) {
    let limit_15_0 = (raw_low & 0xFFFF) as u32;
    let base_15_0 = ((raw_low >> 16) & 0xFFFF) as u32;
    let base_23_16 = ((raw_low >> 32) & 0xFF) as u32;
    let access = ((raw_low >> 40) & 0xFF) as u8;
    let limit_19_16 = ((raw_low >> 48) & 0xF) as u32;
    let flags = ((raw_low >> 52) & 0xF) as u8;
    let base_31_24 = ((raw_low >> 56) & 0xFF) as u32;

    let base_low = base_15_0 | (base_23_16 << 16) | (base_31_24 << 24);
    let limit_raw = limit_15_0 | (limit_19_16 << 16);

    let attrs = DescriptorAttributes {
        typ: access & 0xF,
        s: (access & 0x10) != 0,
        dpl: (access >> 5) & 0x3,
        present: (access & 0x80) != 0,
        avl: (flags & 0b0001) != 0,
        long: (flags & 0b0010) != 0,
        default_big: (flags & 0b0100) != 0,
        granularity: (flags & 0b1000) != 0,
        unusable: false,
    };

    (base_low, limit_raw, attrs)
}

fn effective_limit(limit_raw: u32, granularity: bool) -> u32 {
    if granularity {
        (limit_raw << 12) | 0xFFF
    } else {
        limit_raw
    }
}

pub fn parse_descriptor_8(raw_low: u64) -> Descriptor {
    let (base_low, limit_raw, attrs) = parse_common_fields(raw_low);
    let limit = effective_limit(limit_raw, attrs.granularity);
    if attrs.s {
        Descriptor::Segment(SegmentDescriptor {
            base: base_low as u64,
            limit,
            attrs,
        })
    } else {
        Descriptor::System(SystemDescriptor {
            base: base_low as u64,
            limit,
            attrs,
        })
    }
}

pub fn parse_system_descriptor_16(raw_low: u64, raw_high: u64) -> SystemDescriptor {
    let (base_low, limit_raw, mut attrs) = parse_common_fields(raw_low);
    let base_high = (raw_high & 0xFFFF_FFFF) as u64;
    let limit = effective_limit(limit_raw, attrs.granularity);
    attrs.s = false;
    SystemDescriptor {
        base: (base_low as u64) | (base_high << 32),
        limit,
        attrs,
    }
}

fn selector_index(selector: u16) -> u16 {
    selector >> 3
}

fn selector_ti(selector: u16) -> bool {
    (selector & 0b100) != 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DescriptorTableView {
    base: u64,
    limit: u32,
}

/// System segment registers (LDTR/TR) store selector + hidden cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemSegmentRegister {
    pub selector: u16,
    pub base: u64,
    pub limit: u32,
    pub typ: SystemSegmentType,
    pub dpl: u8,
    pub present: bool,
    pub unusable: bool,
}

impl SystemSegmentRegister {
    pub fn invalidate(&mut self) {
        *self = Self {
            selector: self.selector,
            ..Self::default()
        };
        self.unusable = true;
    }
}

impl CpuState {
    pub fn set_gdtr(&mut self, base: u64, limit: u16) {
        self.tables.gdtr.base = base;
        self.tables.gdtr.limit = limit;
    }

    pub fn set_idtr(&mut self, base: u64, limit: u16) {
        self.tables.idtr.base = base;
        self.tables.idtr.limit = limit;
    }

    pub fn sgdt(&self) -> TableRegister {
        TableRegister {
            base: self.tables.gdtr.base,
            limit: self.tables.gdtr.limit,
        }
    }

    pub fn sidt(&self) -> TableRegister {
        TableRegister {
            base: self.tables.idtr.base,
            limit: self.tables.idtr.limit,
        }
    }

    pub fn sldt(&self) -> u16 {
        self.tables.ldtr.selector
    }

    pub fn str(&self) -> u16 {
        self.tables.tr.selector
    }

    fn descriptor_table_for_selector(
        &self,
        selector: u16,
    ) -> Result<DescriptorTableView, Exception> {
        if selector_ti(selector) {
            if self.tables.ldtr.is_unusable() {
                return Err(Exception::gp(selector));
            }
            Ok(DescriptorTableView {
                base: self.tables.ldtr.base,
                limit: self.tables.ldtr.limit,
            })
        } else {
            Ok(DescriptorTableView {
                base: self.tables.gdtr.base,
                limit: self.tables.gdtr.limit as u32,
            })
        }
    }

    pub fn read_descriptor_8(
        &self,
        bus: &mut impl CpuBus,
        selector: u16,
    ) -> Result<Descriptor, Exception> {
        let index = selector_index(selector);
        let table = self.descriptor_table_for_selector(selector)?;
        let byte_off = (index as u64) * 8;
        let end = byte_off + 7;
        if end > table.limit as u64 {
            return Err(Exception::gp(selector));
        }

        let raw_low = bus.read_u64(table.base + byte_off)?;
        Ok(parse_descriptor_8(raw_low))
    }

    pub fn read_system_descriptor(
        &self,
        bus: &mut impl CpuBus,
        selector: u16,
        long_mode: bool,
    ) -> Result<SystemDescriptor, Exception> {
        let index = selector_index(selector);
        let table = self.descriptor_table_for_selector(selector)?;
        let entry_size = if long_mode { 16 } else { 8 };
        let byte_off = (index as u64) * 8;
        let end = byte_off + (entry_size as u64) - 1;
        if end > table.limit as u64 {
            return Err(Exception::gp(selector));
        }

        let raw_low = bus.read_u64(table.base + byte_off)?;
        let desc = parse_descriptor_8(raw_low);
        match desc {
            Descriptor::System(sys) => {
                if long_mode {
                    let raw_high = bus.read_u64(table.base + byte_off + 8)?;
                    Ok(parse_system_descriptor_16(raw_low, raw_high))
                } else {
                    Ok(sys)
                }
            }
            Descriptor::Segment(_) => Err(Exception::gp(selector)),
        }
    }

    pub fn load_ldtr(&mut self, bus: &mut impl CpuBus, selector: u16) -> Result<(), Exception> {
        let index = selector_index(selector);
        if index == 0 {
            self.tables.ldtr.selector = selector;
            self.tables.ldtr.base = 0;
            self.tables.ldtr.limit = 0;
            self.tables.ldtr.access = SEG_ACCESS_UNUSABLE;
            return Ok(());
        }

        let desc = self.read_system_descriptor(bus, selector, self.ia32e_active())?;
        if !desc.attrs.present {
            return Err(Exception::np(selector));
        }
        let typ = SystemSegmentType::from_typ(desc.attrs.typ);
        if typ != SystemSegmentType::Ldt {
            return Err(Exception::gp(selector));
        }

        self.tables.ldtr.selector = selector;
        self.tables.ldtr.base = desc.base;
        self.tables.ldtr.limit = desc.limit;
        self.tables.ldtr.access = desc.attrs.encode_access_rights();
        Ok(())
    }

    pub fn load_tr(&mut self, bus: &mut impl CpuBus, selector: u16) -> Result<(), Exception> {
        let index = selector_index(selector);
        if index == 0 {
            return Err(Exception::gp(selector));
        }

        let desc = self.read_system_descriptor(bus, selector, self.ia32e_active())?;
        if !desc.attrs.present {
            return Err(Exception::np(selector));
        }
        let typ = SystemSegmentType::from_typ(desc.attrs.typ);
        if typ != SystemSegmentType::TssAvailable && typ != SystemSegmentType::TssBusy {
            return Err(Exception::gp(selector));
        }

        self.tables.tr.selector = selector;
        self.tables.tr.base = desc.base;
        self.tables.tr.limit = desc.limit;
        self.tables.tr.access = desc.attrs.encode_access_rights();
        Ok(())
    }

    /// Reads the ring-0 stack for 32-bit privilege switching (SS0:ESP0).
    pub fn tss32_ring0_stack(&mut self, bus: &mut impl CpuBus) -> Result<(u16, u32), Exception> {
        if self.tables.tr.is_unusable()
            || !self.tables.tr.is_present()
            || selector_index(self.tables.tr.selector) == 0
            || self.tables.tr.s()
            || !matches!(self.tables.tr.typ(), 0x9 | 0xB)
        {
            return Err(Exception::ts(0));
        }
        let base = self.tables.tr.base;
        let limit = self.tables.tr.limit as u64;
        if 4u64.checked_add(3).map_or(true, |end| end > limit)
            || 8u64.checked_add(1).map_or(true, |end| end > limit)
        {
            return Err(Exception::ts(0));
        }
        // 32-bit TSS: ESP0 at +4, SS0 at +8.
        let esp0 = bus.read_u32(base + 4)?;
        let ss0 = bus.read_u16(base + 8)?;
        if (ss0 >> 3) == 0 {
            return Err(Exception::ts(0));
        }
        Ok((ss0, esp0))
    }

    /// Reads the ring-0 stack pointer for 64-bit privilege switching (RSP0).
    pub fn tss64_rsp0(&mut self, bus: &mut impl CpuBus) -> Result<u64, Exception> {
        if self.tables.tr.is_unusable()
            || !self.tables.tr.is_present()
            || selector_index(self.tables.tr.selector) == 0
            || self.tables.tr.s()
            || !matches!(self.tables.tr.typ(), 0x9 | 0xB)
        {
            return Err(Exception::ts(0));
        }
        let base = self.tables.tr.base;
        let limit = self.tables.tr.limit as u64;
        if 4u64.checked_add(7).map_or(true, |end| end > limit) {
            return Err(Exception::ts(0));
        }
        // 64-bit TSS: RSP0 at +4.
        bus.read_u64(base + 4)
    }

    /// Reads the IST entry (1..=7) from a 64-bit TSS.
    pub fn tss64_ist(&mut self, bus: &mut impl CpuBus, index: u8) -> Result<u64, Exception> {
        if !(1..=7).contains(&index) {
            return Err(Exception::ts(0));
        }
        if self.tables.tr.is_unusable()
            || !self.tables.tr.is_present()
            || selector_index(self.tables.tr.selector) == 0
            || self.tables.tr.s()
            || !matches!(self.tables.tr.typ(), 0x9 | 0xB)
        {
            return Err(Exception::ts(0));
        }
        let base = self.tables.tr.base;
        let off = 0x24u64 + (index as u64 - 1) * 8;
        let limit = self.tables.tr.limit as u64;
        if off.checked_add(7).map_or(true, |end| end > limit) {
            return Err(Exception::ts(0));
        }
        bus.read_u64(base + off)
    }

    /// Placeholder hook for the TSS I/O bitmap base.
    pub fn tss64_iomap_base(&mut self, bus: &mut impl CpuBus) -> Result<u16, Exception> {
        if self.tables.tr.is_unusable()
            || !self.tables.tr.is_present()
            || selector_index(self.tables.tr.selector) == 0
            || self.tables.tr.s()
            || !matches!(self.tables.tr.typ(), 0x9 | 0xB)
        {
            return Err(Exception::ts(0));
        }
        let limit = self.tables.tr.limit as u64;
        if 0x66u64.checked_add(1).map_or(true, |end| end > limit) {
            return Err(Exception::ts(0));
        }
        bus.read_u16(self.tables.tr.base + 0x66)
    }
}

impl DescriptorAttributes {
    /// Encodes the descriptor attribute bits into a VMX-style "access rights" value.
    pub fn encode_access_rights(self) -> u32 {
        let mut ar = (self.typ as u32) & 0xF;
        if self.s {
            ar |= 1 << 4;
        }
        ar |= ((self.dpl as u32) & 0x3) << 5;
        if self.present {
            ar |= SEG_ACCESS_PRESENT;
        }
        if self.avl {
            ar |= 1 << 8;
        }
        if self.long {
            ar |= 1 << 9;
        }
        if self.default_big {
            ar |= 1 << 10;
        }
        if self.granularity {
            ar |= 1 << 11;
        }
        if self.unusable {
            ar |= SEG_ACCESS_UNUSABLE;
        }
        ar
    }
}

impl CpuState {
    fn ia32e_active(&self) -> bool {
        (self.msr.efer & EFER_LMA) != 0
            || ((self.msr.efer & EFER_LME) != 0
                && (self.control.cr0 & crate::state::CR0_PG) != 0
                && (self.control.cr4 & crate::state::CR4_PAE) != 0)
    }
}
