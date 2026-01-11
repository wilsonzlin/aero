use crate::descriptors::{Descriptor, DescriptorAttributes, SegmentDescriptor};
use crate::state::{CpuMode, CpuState, EFER_LMA, SEG_ACCESS_UNUSABLE};
use crate::{CpuBus, Exception};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    ES,
    CS,
    SS,
    DS,
    FS,
    GS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    Read,
    Write,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessType {
    pub kind: AccessKind,
    /// Number of bytes accessed (used for limit checking in real/protected mode).
    pub size: u32,
}

impl AccessType {
    pub const fn read(size: u32) -> Self {
        Self {
            kind: AccessKind::Read,
            size,
        }
    }

    pub const fn write(size: u32) -> Self {
        Self {
            kind: AccessKind::Write,
            size,
        }
    }

    pub const fn execute(size: u32) -> Self {
        Self {
            kind: AccessKind::Execute,
            size,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadReason {
    /// MOV/POP into DS/ES/FS/GS.
    Data,
    /// MOV/POP into SS.
    Stack,
    /// Far JMP/CALL/RET/IRET affecting CS.
    FarControlTransfer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentCache {
    pub base: u64,
    pub limit: u32,
    pub attrs: DescriptorAttributes,
}

impl Default for SegmentCache {
    fn default() -> Self {
        Self {
            base: 0,
            limit: 0,
            attrs: DescriptorAttributes::default(),
        }
    }
}

impl SegmentCache {
    fn unusable() -> Self {
        let mut attrs = DescriptorAttributes::default();
        attrs.unusable = true;
        Self {
            base: 0,
            limit: 0,
            attrs,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentRegister {
    pub selector: u16,
    pub cache: SegmentCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRegisters {
    pub es: SegmentRegister,
    pub cs: SegmentRegister,
    pub ss: SegmentRegister,
    pub ds: SegmentRegister,
    pub fs: SegmentRegister,
    pub gs: SegmentRegister,
}

impl SegmentRegisters {
    pub fn new_real_mode() -> Self {
        // In real mode, segment registers behave as selector<<4 base with 64K limit.
        fn real_seg(selector: u16) -> SegmentRegister {
            let base = (selector as u64) << 4;
            let limit = 0xFFFF;
            let attrs = DescriptorAttributes {
                // Treat as read/write data, present.
                typ: 0x3,
                s: true,
                dpl: 0,
                present: true,
                avl: false,
                long: false,
                default_big: false,
                granularity: false,
                unusable: false,
            };
            SegmentRegister {
                selector,
                cache: SegmentCache { base, limit, attrs },
            }
        }

        // CS differs only in "type", but for real mode linearisation we ignore it.
        let mut cs = real_seg(0);
        cs.cache.attrs.typ = 0xB; // code, readable, accessed.

        Self {
            es: real_seg(0),
            cs,
            ss: real_seg(0),
            ds: real_seg(0),
            fs: real_seg(0),
            gs: real_seg(0),
        }
    }

    fn reg_mut(&mut self, seg: Seg) -> &mut SegmentRegister {
        match seg {
            Seg::ES => &mut self.es,
            Seg::CS => &mut self.cs,
            Seg::SS => &mut self.ss,
            Seg::DS => &mut self.ds,
            Seg::FS => &mut self.fs,
            Seg::GS => &mut self.gs,
        }
    }

    fn reg(&self, seg: Seg) -> &SegmentRegister {
        match seg {
            Seg::ES => &self.es,
            Seg::CS => &self.cs,
            Seg::SS => &self.ss,
            Seg::DS => &self.ds,
            Seg::FS => &self.fs,
            Seg::GS => &self.gs,
        }
    }
}

fn selector_index(selector: u16) -> u16 {
    selector >> 3
}

fn selector_rpl(selector: u16) -> u8 {
    (selector & 0x3) as u8
}

fn is_canonical(addr: u64) -> bool {
    // 48-bit canonical addresses: bits 63:48 are sign extension of bit 47.
    let sign = (addr >> 47) & 1;
    if sign == 0 {
        (addr >> 48) == 0
    } else {
        (addr >> 48) == 0xFFFF
    }
}

impl CpuState {
    pub fn seg_base(&self, seg: Seg) -> u64 {
        match self.cpu_mode() {
            CpuMode::Long => self.msr_seg_base(seg),
            CpuMode::Protected => self.seg_reg(seg).base,
            CpuMode::Real | CpuMode::Vm86 => self.seg_reg(seg).base,
        }
    }

    pub fn seg_limit(&self, seg: Seg) -> u32 {
        match self.cpu_mode() {
            CpuMode::Long => 0xFFFF_FFFF,
            CpuMode::Protected => self.seg_reg(seg).limit,
            CpuMode::Real | CpuMode::Vm86 => self.seg_reg(seg).limit,
        }
    }

    /// Computes a linear address from a segment:offset pair and performs the
    /// segmentation checks required for the active CPU mode.
    pub fn linearize(&self, seg: Seg, offset: u64, access: AccessType) -> Result<u64, Exception> {
        match self.cpu_mode() {
            CpuMode::Real | CpuMode::Vm86 => {
                let base = self.seg_reg(seg).base;
                Ok(self.apply_a20(base.wrapping_add(offset)))
            }
            CpuMode::Protected => self.linearize_protected(seg, offset, access),
            CpuMode::Long => self.linearize_long(seg, offset),
        }
    }

    fn linearize_long(&self, seg: Seg, offset: u64) -> Result<u64, Exception> {
        let base = match seg {
            Seg::FS => self.msr.fs_base,
            Seg::GS => self.msr.gs_base,
            _ => 0,
        };
        let linear = base.wrapping_add(offset);
        if !is_canonical(linear) {
            return Err(Exception::gp0());
        }
        Ok(linear)
    }

    fn linearize_protected(
        &self,
        seg: Seg,
        offset: u64,
        access: AccessType,
    ) -> Result<u64, Exception> {
        let reg = self.seg_reg(seg);
        if reg.is_unusable() {
            return Err(match seg {
                Seg::SS => Exception::ss(0),
                _ => Exception::gp0(),
            });
        }

        // Rights checks.
        match access.kind {
            AccessKind::Execute => {
                if seg != Seg::CS || !reg.is_code() {
                    return Err(Exception::gp0());
                }
            }
            AccessKind::Read => {
                if reg.is_code() && !reg.code_readable() {
                    return Err(Exception::gp0());
                }
            }
            AccessKind::Write => {
                if reg.is_code() {
                    return Err(Exception::gp0());
                }
                if reg.is_data() && !reg.data_writable() {
                    return Err(Exception::gp0());
                }
            }
        }

        // Limit checks.
        let size = access.size.max(1) as u64;
        let end = offset.checked_add(size - 1).ok_or_else(|| match seg {
            Seg::SS => Exception::ss(0),
            _ => Exception::gp0(),
        })?;

        let max = if reg.is_default_32bit() {
            0xFFFF_FFFFu64
        } else {
            0xFFFFu64
        };

        let limit = reg.limit as u64;
        let ok = if reg.data_expand_down() {
            offset > limit && end <= max
        } else {
            end <= limit
        };

        if !ok {
            return Err(match seg {
                Seg::SS => Exception::ss(0),
                _ => Exception::gp0(),
            });
        }

        Ok(reg.base.wrapping_add(offset))
    }

    pub fn load_seg(
        &mut self,
        bus: &mut impl CpuBus,
        seg: Seg,
        selector: u16,
        reason: LoadReason,
    ) -> Result<(), Exception> {
        match self.cpu_mode() {
            CpuMode::Real | CpuMode::Vm86 => self.load_seg_real(seg, selector),
            CpuMode::Protected | CpuMode::Long => self.load_seg_protected(bus, seg, selector, reason),
        }
    }

    fn load_seg_real(&mut self, seg: Seg, selector: u16) -> Result<(), Exception> {
        let reg = self.seg_reg_mut(seg);
        reg.selector = selector;
        reg.base = (selector as u64) << 4;
        reg.limit = 0xFFFF;
        reg.access = DescriptorAttributes {
            typ: if seg == Seg::CS { 0xB } else { 0x3 },
            s: true,
            dpl: 0,
            present: true,
            avl: false,
            long: false,
            default_big: false,
            granularity: false,
            unusable: false,
        }
        .encode_access_rights();
        Ok(())
    }

    fn load_seg_protected(
        &mut self,
        bus: &mut impl CpuBus,
        seg: Seg,
        selector: u16,
        reason: LoadReason,
    ) -> Result<(), Exception> {
        // Null selector handling (index==0).
        if selector_index(selector) == 0 {
            match seg {
                Seg::CS | Seg::SS => return Err(Exception::gp(selector)),
                Seg::DS | Seg::ES | Seg::FS | Seg::GS => {
                    let reg = self.seg_reg_mut(seg);
                    reg.selector = selector;
                    reg.base = 0;
                    reg.limit = 0;
                    reg.access = SEG_ACCESS_UNUSABLE;
                    return Ok(());
                }
            }
        }

        let old_cpl = self.cpl();

        // Fetch descriptor.
        let desc = self.read_descriptor_8(bus, selector)?;
        let seg_desc = match desc {
            Descriptor::Segment(sd) => sd,
            Descriptor::System(_) => return Err(Exception::gp(selector)),
        };

        // Present check.
        if !seg_desc.attrs.present {
            return Err(match seg {
                Seg::SS => Exception::ss(selector),
                _ => Exception::np(selector),
            });
        }

        // Type + privilege checks depend on which segment register is being loaded.
        match seg {
            Seg::CS => self.validate_load_cs(selector, seg_desc, reason)?,
            Seg::SS => self.validate_load_ss(selector, seg_desc, reason)?,
            Seg::DS | Seg::ES | Seg::FS | Seg::GS => self.validate_load_data_seg(selector, seg_desc, reason)?,
        }

        // Commit: visible selector + hidden cache.
        let reg = self.seg_reg_mut(seg);
        reg.selector = selector;
        reg.base = seg_desc.base;
        reg.limit = seg_desc.limit;
        reg.access = seg_desc.attrs.encode_access_rights();

        // On far transfers loading CS, long mode may become active.
        if seg == Seg::CS && reason == LoadReason::FarControlTransfer {
            self.after_cs_load(old_cpl, seg_desc.attrs);
        }

        Ok(())
    }

    fn validate_load_data_seg(
        &self,
        selector: u16,
        desc: SegmentDescriptor,
        _reason: LoadReason,
    ) -> Result<(), Exception> {
        // DS/ES/FS/GS can be loaded with data segments or readable code segments.
        if !(desc.attrs.is_data() || (desc.attrs.is_code() && desc.attrs.code_readable())) {
            return Err(Exception::gp(selector));
        }

        let rpl = selector_rpl(selector);
        let epl = self.cpl().max(rpl);
        if desc.attrs.dpl < epl {
            return Err(Exception::gp(selector));
        }
        Ok(())
    }

    fn validate_load_ss(
        &self,
        selector: u16,
        desc: SegmentDescriptor,
        _reason: LoadReason,
    ) -> Result<(), Exception> {
        // SS must be a writable data segment.
        if !desc.attrs.is_data() || !desc.attrs.data_writable() {
            return Err(Exception::gp(selector));
        }

        let cpl = self.cpl();
        let rpl = selector_rpl(selector);
        if rpl != cpl {
            return Err(Exception::gp(selector));
        }
        if desc.attrs.dpl != cpl {
            return Err(Exception::gp(selector));
        }
        Ok(())
    }

    fn validate_load_cs(
        &self,
        selector: u16,
        desc: SegmentDescriptor,
        reason: LoadReason,
    ) -> Result<(), Exception> {
        if reason != LoadReason::FarControlTransfer {
            return Err(Exception::gp(selector));
        }

        if !desc.attrs.is_code() {
            return Err(Exception::gp(selector));
        }

        let cpl = self.cpl();
        let rpl = selector_rpl(selector);
        if rpl > cpl {
            return Err(Exception::gp(selector));
        }

        if desc.attrs.code_conforming() {
            // Conforming code: DPL must be <= CPL.
            if desc.attrs.dpl > cpl {
                return Err(Exception::gp(selector));
            }
        } else {
            // Non-conforming code: DPL must equal CPL.
            if desc.attrs.dpl != cpl {
                return Err(Exception::gp(selector));
            }
        }

        // Long-mode entry requires a 64-bit code segment to be loaded via far transfer.
        if desc.attrs.long {
            if self.cpu_mode() != CpuMode::Long && !self.long_mode_conditions_met() {
                return Err(Exception::gp(selector));
            }
        } else if self.cpu_mode() == CpuMode::Long {
            // Simplified model: once in long mode, don't allow returning to legacy CS.
            return Err(Exception::gp(selector));
        }

        Ok(())
    }

    fn after_cs_load(&mut self, old_cpl: u8, attrs: DescriptorAttributes) {
        // Conforming code segments do not change CPL; non-conforming transfers set
        // CPL to the descriptor's DPL (direct far jumps/calls require equality).
        let new_cpl = if attrs.code_conforming() { old_cpl } else { attrs.dpl };

        // Hardware forces CS.RPL == CPL.
        self.segments.cs.selector = (self.segments.cs.selector & !0x3) | (new_cpl as u16);

        // Long-mode activation: only when far transfer loads CS.L=1 and the enabling
        // conditions are met.
        if (self.msr.efer & EFER_LMA) == 0 && self.long_mode_conditions_met() && attrs.long {
            self.msr.efer |= EFER_LMA;
        }

        self.update_mode();
    }

    fn seg_reg(&self, seg: Seg) -> &crate::state::Segment {
        match seg {
            Seg::ES => &self.segments.es,
            Seg::CS => &self.segments.cs,
            Seg::SS => &self.segments.ss,
            Seg::DS => &self.segments.ds,
            Seg::FS => &self.segments.fs,
            Seg::GS => &self.segments.gs,
        }
    }

    fn seg_reg_mut(&mut self, seg: Seg) -> &mut crate::state::Segment {
        match seg {
            Seg::ES => &mut self.segments.es,
            Seg::CS => &mut self.segments.cs,
            Seg::SS => &mut self.segments.ss,
            Seg::DS => &mut self.segments.ds,
            Seg::FS => &mut self.segments.fs,
            Seg::GS => &mut self.segments.gs,
        }
    }
}
