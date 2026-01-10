use crate::descriptors::{Descriptor, DescriptorAttributes, SegmentDescriptor};
use crate::mode::{CpuMode, CR0_PE};
use crate::{CpuBus, CpuState, Exception};

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
            CpuMode::Protected => self.segments.reg(seg).cache.base,
            CpuMode::Real => (self.segments.reg(seg).selector as u64) << 4,
        }
    }

    pub fn seg_limit(&self, seg: Seg) -> u32 {
        match self.cpu_mode() {
            CpuMode::Long => 0xFFFF_FFFF,
            CpuMode::Protected => self.segments.reg(seg).cache.limit,
            CpuMode::Real => 0xFFFF,
        }
    }

    /// Computes a linear address from a segment:offset pair and performs the
    /// segmentation checks required for the active CPU mode.
    pub fn linearize(&self, seg: Seg, offset: u64, access: AccessType) -> Result<u64, Exception> {
        match self.cpu_mode() {
            CpuMode::Real => {
                let base = (self.segments.reg(seg).selector as u64) << 4;
                let linear = base.wrapping_add(offset);
                let linear = if self.a20_enabled {
                    linear
                } else {
                    linear & 0xFFFFF
                };
                Ok(linear)
            }
            CpuMode::Protected => self.linearize_protected(seg, offset, access),
            CpuMode::Long => self.linearize_long(seg, offset),
        }
    }

    fn linearize_long(&self, seg: Seg, offset: u64) -> Result<u64, Exception> {
        let base = match seg {
            Seg::FS => self.msr_fs_base,
            Seg::GS => self.msr_gs_base,
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
        let reg = self.segments.reg(seg);
        if reg.cache.attrs.unusable {
            return Err(match seg {
                Seg::SS => Exception::ss(0),
                _ => Exception::gp0(),
            });
        }

        // Rights checks.
        match access.kind {
            AccessKind::Execute => {
                if seg != Seg::CS || !reg.cache.attrs.is_code() {
                    return Err(Exception::gp0());
                }
            }
            AccessKind::Read => {
                if reg.cache.attrs.is_code() && !reg.cache.attrs.code_readable() {
                    return Err(Exception::gp0());
                }
            }
            AccessKind::Write => {
                if reg.cache.attrs.is_code() {
                    return Err(Exception::gp0());
                }
                if reg.cache.attrs.is_data() && !reg.cache.attrs.data_writable() {
                    return Err(Exception::gp0());
                }
            }
        }

        // Limit checks (ignored in long mode).
        let size = access.size.max(1) as u64;
        let end = offset.checked_add(size - 1).ok_or_else(|| match seg {
            Seg::SS => Exception::ss(0),
            _ => Exception::gp0(),
        })?;

        let max = if reg.cache.attrs.default_big {
            0xFFFF_FFFFu64
        } else {
            0xFFFFu64
        };

        let limit = reg.cache.limit as u64;
        let ok = if reg.cache.attrs.data_expand_down() {
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

        Ok(reg.cache.base.wrapping_add(offset))
    }

    pub fn load_seg(
        &mut self,
        bus: &mut impl CpuBus,
        seg: Seg,
        selector: u16,
        reason: LoadReason,
    ) -> Result<(), Exception> {
        match self.cpu_mode() {
            CpuMode::Real => self.load_seg_real(seg, selector),
            CpuMode::Protected => self.load_seg_protected(bus, seg, selector, reason),
            CpuMode::Long => self.load_seg_protected(bus, seg, selector, reason),
        }
    }

    fn load_seg_real(&mut self, seg: Seg, selector: u16) -> Result<(), Exception> {
        let reg = self.segments.reg_mut(seg);
        reg.selector = selector;
        reg.cache.base = (selector as u64) << 4;
        reg.cache.limit = 0xFFFF;
        reg.cache.attrs = DescriptorAttributes {
            typ: if seg == Seg::CS { 0xB } else { 0x3 },
            s: true,
            dpl: 0,
            present: true,
            avl: false,
            long: false,
            default_big: false,
            granularity: false,
            unusable: false,
        };
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
                Seg::CS => return Err(Exception::gp(selector)),
                Seg::SS => return Err(Exception::gp(selector)),
                Seg::DS | Seg::ES | Seg::FS | Seg::GS => {
                    let reg = self.segments.reg_mut(seg);
                    reg.selector = selector;
                    reg.cache = SegmentCache::unusable();
                    return Ok(());
                }
            }
        }

        // Fetch descriptor.
        let desc = self.read_descriptor_8(bus, selector)?;
        let (seg_desc, attrs) = match desc {
            Descriptor::Segment(SegmentDescriptor { base, limit, attrs }) => {
                (SegmentDescriptor { base, limit, attrs }, attrs)
            }
            Descriptor::System(_) => return Err(Exception::gp(selector)),
        };

        // Present check.
        if !attrs.present {
            return Err(match seg {
                Seg::SS => Exception::ss(selector),
                _ => Exception::np(selector),
            });
        }

        // Type + privilege checks depend on which segment register is being loaded.
        match seg {
            Seg::CS => self.validate_load_cs(selector, seg_desc, reason)?,
            Seg::SS => self.validate_load_ss(selector, seg_desc, reason)?,
            Seg::DS | Seg::ES | Seg::FS | Seg::GS => {
                self.validate_load_data_seg(selector, seg_desc, reason)?
            }
        }

        // Commit: visible selector + hidden cache.
        let reg = self.segments.reg_mut(seg);
        reg.selector = selector;
        reg.cache = SegmentCache {
            base: seg_desc.base,
            limit: seg_desc.limit,
            attrs: seg_desc.attrs,
        };

        // On far transfers loading CS, long mode may become active.
        if seg == Seg::CS && reason == LoadReason::FarControlTransfer {
            self.after_cs_load(seg_desc.attrs);
        }

        // Loading SS updates CPL to match the selector in a strict model; here CPL
        // is derived from CS, so SS load does not change CPL.
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
        let epl = self.cpl.max(rpl);
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

        let rpl = selector_rpl(selector);
        if rpl != self.cpl {
            return Err(Exception::gp(selector));
        }
        if desc.attrs.dpl != self.cpl {
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

        let rpl = selector_rpl(selector);
        if rpl > self.cpl {
            return Err(Exception::gp(selector));
        }

        if desc.attrs.code_conforming() {
            // Conforming code: DPL must be <= CPL.
            if desc.attrs.dpl > self.cpl {
                return Err(Exception::gp(selector));
            }
        } else {
            // Non-conforming code: DPL must equal CPL.
            if desc.attrs.dpl != self.cpl {
                return Err(Exception::gp(selector));
            }
        }

        // Long-mode entry requires a 64-bit code segment to be loaded via far transfer.
        if self.long_mode_conditions_met() && desc.attrs.long {
            // OK; after_cs_load will flip LMA.
        } else if self.lma {
            // In our simplified model, if we're already in long mode we don't allow
            // switching back to non-64-bit CS.
            if !desc.attrs.long {
                return Err(Exception::gp(selector));
            }
        }

        Ok(())
    }

    fn after_cs_load(&mut self, attrs: DescriptorAttributes) {
        // Conforming code segments do not change CPL; non-conforming transfers set
        // CPL to the descriptor's DPL (direct far jumps/calls require equality).
        let new_cpl = if attrs.code_conforming() {
            self.cpl
        } else {
            attrs.dpl
        };
        self.cpl = new_cpl;
        // Hardware forces CS.RPL == CPL.
        self.segments.cs.selector = (self.segments.cs.selector & !0x3) | (new_cpl as u16);

        // Long-mode activation (project model): only when far transfer loads CS.L=1.
        if !self.lma && self.long_mode_conditions_met() && self.segments.cs.cache.attrs.long {
            self.lma = true;
            // Long mode treats CS/DS/ES/SS base as 0; keep cached base for potential
            // debugging but `seg_base` will return 0.
            // Mark default_big based on 64-bit CS: CS.D must be 0, but software can
            // still set it; we don't enforce that strictly here.
        }
    }
}

impl CpuState {
    /// Convenience: updates CR0.PE while keeping the rest of CR0 unchanged.
    pub fn set_protected_enable(&mut self, enabled: bool) {
        if enabled {
            self.cr0 |= CR0_PE;
        } else {
            self.cr0 &= !CR0_PE;
        }
        // Leaving protected mode implicitly leaves long mode.
        if !enabled {
            self.lma = false;
            self.cpl = 0;
        }
    }
}
