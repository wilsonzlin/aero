use std::io::{Read, Write};

use crate::error::{Result, SnapshotError};
use crate::format::DeviceId;
use crate::io::{ReadLeExt, WriteLeExt};
use crate::limits;

const FXSAVE_AREA_SIZE: usize = 512;

fn decode_string_u32_bounded<R: Read>(
    r: &mut R,
    max_len: u32,
    too_long_error: &'static str,
    truncated_error: &'static str,
    invalid_utf8_error: &'static str,
) -> Result<String> {
    let len = r.read_u32_le()?;
    if len > max_len {
        return Err(SnapshotError::Corrupt(too_long_error));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    let mut limited = r.take(len as u64);
    limited.read_to_end(&mut bytes)?;
    if limited.limit() != 0 {
        return Err(SnapshotError::Corrupt(truncated_error));
    }
    String::from_utf8(bytes).map_err(|_| SnapshotError::Corrupt(invalid_utf8_error))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotMeta {
    pub snapshot_id: u64,
    pub parent_snapshot_id: Option<u64>,
    pub created_unix_ms: u64,
    pub label: Option<String>,
}

impl SnapshotMeta {
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u64_le(self.snapshot_id)?;
        match self.parent_snapshot_id {
            Some(id) => {
                w.write_u8(1)?;
                w.write_u64_le(id)?;
            }
            None => w.write_u8(0)?,
        }
        w.write_u64_le(self.created_unix_ms)?;
        match &self.label {
            Some(label) => {
                w.write_u8(1)?;
                if label.len() > limits::MAX_LABEL_LEN as usize {
                    return Err(SnapshotError::Corrupt("label too long"));
                }
                w.write_string_u32(label)?;
            }
            None => w.write_u8(0)?,
        }
        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        let snapshot_id = r.read_u64_le()?;
        let parent_present = r.read_u8()?;
        let parent_snapshot_id = match parent_present {
            0 => None,
            1 => Some(r.read_u64_le()?),
            _ => return Err(SnapshotError::Corrupt("invalid parent presence tag")),
        };
        let created_unix_ms = r.read_u64_le()?;
        let label_present = r.read_u8()?;
        let label = match label_present {
            0 => None,
            1 => Some(decode_string_u32_bounded(
                r,
                limits::MAX_LABEL_LEN,
                "label too long",
                "label: truncated string bytes",
                "label: invalid utf-8",
            )?),
            _ => return Err(SnapshotError::Corrupt("invalid label presence tag")),
        };
        Ok(Self {
            snapshot_id,
            parent_snapshot_id,
            created_unix_ms,
            label,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    /// Materialized architectural RFLAGS value.
    pub rflags: u64,
    pub mode: CpuMode,
    pub halted: bool,
    /// Interrupt vector recorded when real/v8086-mode vector delivery enters a BIOS ROM stub
    /// (`HLT; IRET`).
    ///
    /// Tier-0 treats `HLT` as a BIOS hypercall boundary only when this marker is set, surfacing
    /// the event as `BiosInterrupt(vector)` instead of permanently halting.
    pub pending_bios_int: u8,
    pub pending_bios_int_valid: bool,
    /// A20 gate (real mode address wrap) state.
    pub a20_enabled: bool,
    /// x87 external interrupt indicator for `CR0.NE = 0` mode (IRQ13).
    pub irq13_pending: bool,
    pub es: SegmentState,
    pub cs: SegmentState,
    pub ss: SegmentState,
    pub ds: SegmentState,
    pub fs: SegmentState,
    pub gs: SegmentState,
    pub fpu: FpuState,
    pub mxcsr: u32,
    pub xmm: [u128; 16],
    /// Raw FXSAVE area bytes (512 bytes).
    pub fxsave: [u8; FXSAVE_AREA_SIZE],
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: 0,
            rflags: 0,
            mode: CpuMode::default(),
            halted: false,
            pending_bios_int: 0,
            pending_bios_int_valid: false,
            a20_enabled: true,
            irq13_pending: false,
            es: SegmentState::default(),
            cs: SegmentState::default(),
            ss: SegmentState::default(),
            ds: SegmentState::default(),
            fs: SegmentState::default(),
            gs: SegmentState::default(),
            fpu: FpuState::default(),
            mxcsr: 0,
            xmm: [0u128; 16],
            fxsave: [0u8; FXSAVE_AREA_SIZE],
        }
    }
}

impl CpuState {
    pub fn encode_v1<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u64_le(self.rax)?;
        w.write_u64_le(self.rbx)?;
        w.write_u64_le(self.rcx)?;
        w.write_u64_le(self.rdx)?;
        w.write_u64_le(self.rsi)?;
        w.write_u64_le(self.rdi)?;
        w.write_u64_le(self.rbp)?;
        w.write_u64_le(self.rsp)?;
        w.write_u64_le(self.r8)?;
        w.write_u64_le(self.r9)?;
        w.write_u64_le(self.r10)?;
        w.write_u64_le(self.r11)?;
        w.write_u64_le(self.r12)?;
        w.write_u64_le(self.r13)?;
        w.write_u64_le(self.r14)?;
        w.write_u64_le(self.r15)?;
        w.write_u64_le(self.rip)?;
        w.write_u64_le(self.rflags)?;
        w.write_u16_le(self.cs.selector)?;
        w.write_u16_le(self.ds.selector)?;
        w.write_u16_le(self.es.selector)?;
        w.write_u16_le(self.fs.selector)?;
        w.write_u16_le(self.gs.selector)?;
        w.write_u16_le(self.ss.selector)?;
        for xmm in self.xmm {
            w.write_u128_le(xmm)?;
        }
        Ok(())
    }

    pub fn decode_v1<R: Read>(r: &mut R) -> Result<Self> {
        let rax = r.read_u64_le()?;
        let rbx = r.read_u64_le()?;
        let rcx = r.read_u64_le()?;
        let rdx = r.read_u64_le()?;
        let rsi = r.read_u64_le()?;
        let rdi = r.read_u64_le()?;
        let rbp = r.read_u64_le()?;
        let rsp = r.read_u64_le()?;
        let r8 = r.read_u64_le()?;
        let r9 = r.read_u64_le()?;
        let r10 = r.read_u64_le()?;
        let r11 = r.read_u64_le()?;
        let r12 = r.read_u64_le()?;
        let r13 = r.read_u64_le()?;
        let r14 = r.read_u64_le()?;
        let r15 = r.read_u64_le()?;
        let rip = r.read_u64_le()?;
        let rflags = r.read_u64_le()?;
        // v1 stores selectors only. Populate the hidden caches with real-mode defaults to make
        // best-effort v1 restores usable for simple guests.
        let cs = SegmentState::real_mode(r.read_u16_le()?);
        let ds = SegmentState::real_mode(r.read_u16_le()?);
        let es = SegmentState::real_mode(r.read_u16_le()?);
        let fs = SegmentState::real_mode(r.read_u16_le()?);
        let gs = SegmentState::real_mode(r.read_u16_le()?);
        let ss = SegmentState::real_mode(r.read_u16_le()?);
        let mut xmm = [0u128; 16];
        for slot in &mut xmm {
            *slot = r.read_u128_le()?;
        }
        Ok(Self {
            rax,
            rbx,
            rcx,
            rdx,
            rsi,
            rdi,
            rbp,
            rsp,
            r8,
            r9,
            r10,
            r11,
            r12,
            r13,
            r14,
            r15,
            rip,
            rflags,
            cs,
            ds,
            es,
            fs,
            gs,
            ss,
            xmm,
            ..Self::default()
        })
    }

    pub fn encode_v2<W: Write>(&self, w: &mut W) -> Result<()> {
        // GPRs in architectural order (RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8..R15).
        w.write_u64_le(self.rax)?;
        w.write_u64_le(self.rcx)?;
        w.write_u64_le(self.rdx)?;
        w.write_u64_le(self.rbx)?;
        w.write_u64_le(self.rsp)?;
        w.write_u64_le(self.rbp)?;
        w.write_u64_le(self.rsi)?;
        w.write_u64_le(self.rdi)?;
        w.write_u64_le(self.r8)?;
        w.write_u64_le(self.r9)?;
        w.write_u64_le(self.r10)?;
        w.write_u64_le(self.r11)?;
        w.write_u64_le(self.r12)?;
        w.write_u64_le(self.r13)?;
        w.write_u64_le(self.r14)?;
        w.write_u64_le(self.r15)?;
        w.write_u64_le(self.rip)?;
        w.write_u64_le(self.rflags)?;
        w.write_u8(self.mode as u8)?;
        w.write_u8(self.halted as u8)?;
        self.es.encode(w)?;
        self.cs.encode(w)?;
        self.ss.encode(w)?;
        self.ds.encode(w)?;
        self.fs.encode(w)?;
        self.gs.encode(w)?;
        self.fpu.encode(w)?;
        w.write_u32_le(self.mxcsr)?;
        for xmm in self.xmm {
            w.write_u128_le(xmm)?;
        }
        w.write_bytes(&self.fxsave)?;
        // Extension fields appended to CPU v2 after the initial v2 release.
        //
        // The length prefix keeps the encoding forward-compatible when new fields are added.
        const CPU_V2_EXT_LEN: u32 = 4;
        w.write_u32_le(CPU_V2_EXT_LEN)?;
        w.write_u8(self.a20_enabled as u8)?;
        w.write_u8(self.irq13_pending as u8)?;
        w.write_u8(self.pending_bios_int_valid as u8)?;
        w.write_u8(self.pending_bios_int)?;
        Ok(())
    }

    pub fn decode_v2<R: Read>(r: &mut R) -> Result<Self> {
        let rax = r.read_u64_le()?;
        let rcx = r.read_u64_le()?;
        let rdx = r.read_u64_le()?;
        let rbx = r.read_u64_le()?;
        let rsp = r.read_u64_le()?;
        let rbp = r.read_u64_le()?;
        let rsi = r.read_u64_le()?;
        let rdi = r.read_u64_le()?;
        let r8 = r.read_u64_le()?;
        let r9 = r.read_u64_le()?;
        let r10 = r.read_u64_le()?;
        let r11 = r.read_u64_le()?;
        let r12 = r.read_u64_le()?;
        let r13 = r.read_u64_le()?;
        let r14 = r.read_u64_le()?;
        let r15 = r.read_u64_le()?;
        let rip = r.read_u64_le()?;
        let rflags = r.read_u64_le()?;
        let mode = CpuMode::decode(r.read_u8()?)?;
        let halted = r.read_u8()? != 0;
        let es = SegmentState::decode(r)?;
        let cs = SegmentState::decode(r)?;
        let ss = SegmentState::decode(r)?;
        let ds = SegmentState::decode(r)?;
        let fs = SegmentState::decode(r)?;
        let gs = SegmentState::decode(r)?;
        let fpu = FpuState::decode(r)?;
        let mxcsr = r.read_u32_le()?;
        let mut xmm = [0u128; 16];
        for slot in &mut xmm {
            *slot = r.read_u128_le()?;
        }
        let mut fxsave = [0u8; FXSAVE_AREA_SIZE];
        r.read_exact(&mut fxsave)?;
        let mut state = Self {
            rax,
            rbx,
            rcx,
            rdx,
            rsi,
            rdi,
            rbp,
            rsp,
            r8,
            r9,
            r10,
            r11,
            r12,
            r13,
            r14,
            r15,
            rip,
            rflags,
            mode,
            halted,
            es,
            cs,
            ss,
            ds,
            fs,
            gs,
            fpu,
            mxcsr,
            xmm,
            fxsave,
            ..Self::default()
        };
        // Optional CPU v2 extension. Older v2 snapshots may end at the FXSAVE bytes.
        let mut ext_len_first = [0u8; 1];
        match r.read_exact(&mut ext_len_first) {
            Ok(()) => {
                let mut ext_len_bytes = [0u8; 4];
                ext_len_bytes[0] = ext_len_first[0];
                // If we saw at least one byte of the extension length prefix, the remaining
                // bytes must be present too; otherwise the snapshot is truncated/corrupt.
                r.read_exact(&mut ext_len_bytes[1..])?;
                let ext_len = u32::from_le_bytes(ext_len_bytes);
                if ext_len > limits::MAX_CPU_V2_EXT_LEN {
                    return Err(SnapshotError::Corrupt("cpu v2 extension too large"));
                }
                let ext = r.read_exact_vec(ext_len as usize)?;
                if ext_len >= 1 {
                    state.a20_enabled = ext[0] != 0;
                }
                if ext_len >= 2 {
                    state.irq13_pending = ext[1] != 0;
                }
                if ext_len >= 3 {
                    state.pending_bios_int_valid = ext[2] != 0;
                }
                if ext_len >= 4 {
                    state.pending_bios_int = ext[3];
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // No extension present (legacy v2 snapshots).
                return Ok(state);
            }
            Err(e) => return Err(e.into()),
        };
        Ok(state)
    }

    /// Encode using the latest supported CPU section version.
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        self.encode_v2(w)
    }

    /// Decode using the latest supported CPU section version.
    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        Self::decode_v2(r)
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CpuMode {
    #[default]
    Real = 0,
    Protected = 1,
    Long = 2,
    Vm86 = 3,
}

impl CpuMode {
    fn decode(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Real),
            1 => Ok(Self::Protected),
            2 => Ok(Self::Long),
            3 => Ok(Self::Vm86),
            _ => Err(SnapshotError::Corrupt("invalid CPU mode")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SegmentState {
    pub selector: u16,
    pub base: u64,
    pub limit: u32,
    /// VMX-style access rights (AR bytes) plus "unusable" bit.
    pub access: u32,
}

impl SegmentState {
    fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u16_le(self.selector)?;
        w.write_u64_le(self.base)?;
        w.write_u32_le(self.limit)?;
        w.write_u32_le(self.access)?;
        Ok(())
    }

    fn decode<R: Read>(r: &mut R) -> Result<Self> {
        Ok(Self {
            selector: r.read_u16_le()?,
            base: r.read_u64_le()?,
            limit: r.read_u32_le()?,
            access: r.read_u32_le()?,
        })
    }

    pub fn real_mode(selector: u16) -> Self {
        Self {
            selector,
            base: (selector as u64) << 4,
            limit: 0xFFFF,
            access: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FpuState {
    pub fcw: u16,
    pub fsw: u16,
    pub ftw: u16,
    pub top: u8,
    pub fop: u16,
    pub fip: u64,
    pub fdp: u64,
    pub fcs: u16,
    pub fds: u16,
    pub st: [u128; 8],
}

impl FpuState {
    fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u16_le(self.fcw)?;
        w.write_u16_le(self.fsw)?;
        w.write_u16_le(self.ftw)?;
        w.write_u8(self.top)?;
        w.write_u16_le(self.fop)?;
        w.write_u64_le(self.fip)?;
        w.write_u64_le(self.fdp)?;
        w.write_u16_le(self.fcs)?;
        w.write_u16_le(self.fds)?;
        for st in self.st {
            w.write_u128_le(st)?;
        }
        Ok(())
    }

    fn decode<R: Read>(r: &mut R) -> Result<Self> {
        let fcw = r.read_u16_le()?;
        let fsw = r.read_u16_le()?;
        let ftw = r.read_u16_le()?;
        let top = r.read_u8()?;
        let fop = r.read_u16_le()?;
        let fip = r.read_u64_le()?;
        let fdp = r.read_u64_le()?;
        let fcs = r.read_u16_le()?;
        let fds = r.read_u16_le()?;
        let mut st = [0u128; 8];
        for slot in &mut st {
            *slot = r.read_u128_le()?;
        }
        Ok(Self {
            fcw,
            fsw,
            ftw,
            top,
            fop,
            fip,
            fdp,
            fcs,
            fds,
            st,
        })
    }
}

/// Snapshot representation of one virtual CPU.
///
/// `CpuState` intentionally only captures architectural register state. Any
/// higher-level runtime bookkeeping (e.g. run state, pending interrupts, local
/// APIC queue) should be stored in `internal_state` by the machine snapshot
/// adapter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VcpuSnapshot {
    /// vCPU identifier used to map snapshot entries back to a runtime CPU.
    ///
    /// For x86 this is typically the local APIC ID.
    pub apic_id: u32,
    pub cpu: CpuState,
    /// Machine-defined per-vCPU state that isn't covered by `CpuState`.
    pub internal_state: Vec<u8>,
}

impl VcpuSnapshot {
    pub fn encode_v1<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u32_le(self.apic_id)?;
        self.cpu.encode_v1(w)?;
        let internal_len: u64 = self
            .internal_state
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("vCPU internal state too large"))?;
        if internal_len > limits::MAX_VCPU_INTERNAL_LEN {
            return Err(SnapshotError::Corrupt("vCPU internal state too large"));
        }
        w.write_u64_le(internal_len)?;
        w.write_bytes(&self.internal_state)?;
        Ok(())
    }

    pub fn decode_v1<R: Read>(r: &mut R, max_internal_len: u64) -> Result<Self> {
        let apic_id = r.read_u32_le()?;
        let cpu = CpuState::decode_v1(r)?;
        let internal_len = r.read_u64_le()?;
        if internal_len > max_internal_len {
            return Err(SnapshotError::Corrupt("vCPU internal state too large"));
        }
        let internal_state = r.read_exact_vec(internal_len as usize)?;
        Ok(Self {
            apic_id,
            cpu,
            internal_state,
        })
    }

    pub fn encode_v2<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u32_le(self.apic_id)?;
        self.cpu.encode_v2(w)?;
        let internal_len: u64 = self
            .internal_state
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("vCPU internal state too large"))?;
        if internal_len > limits::MAX_VCPU_INTERNAL_LEN {
            return Err(SnapshotError::Corrupt("vCPU internal state too large"));
        }
        w.write_u64_le(internal_len)?;
        w.write_bytes(&self.internal_state)?;
        Ok(())
    }

    pub fn decode_v2<R: Read>(r: &mut R, max_internal_len: u64) -> Result<Self> {
        let apic_id = r.read_u32_le()?;
        let cpu = CpuState::decode_v2(r)?;
        let internal_len = r.read_u64_le()?;
        if internal_len > max_internal_len {
            return Err(SnapshotError::Corrupt("vCPU internal state too large"));
        }
        let internal_state = r.read_exact_vec(internal_len as usize)?;
        Ok(Self {
            apic_id,
            cpu,
            internal_state,
        })
    }

    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        self.encode_v2(w)
    }

    pub fn decode<R: Read>(r: &mut R, max_internal_len: u64) -> Result<Self> {
        Self::decode_v2(r, max_internal_len)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MmuState {
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub dr0: u64,
    pub dr1: u64,
    pub dr2: u64,
    pub dr3: u64,
    pub dr4: u64,
    pub dr5: u64,
    pub dr6: u64,
    pub dr7: u64,
    pub efer: u64,
    pub star: u64,
    pub lstar: u64,
    pub cstar: u64,
    pub sfmask: u64,
    pub sysenter_cs: u64,
    pub sysenter_eip: u64,
    pub sysenter_esp: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub kernel_gs_base: u64,
    pub apic_base: u64,
    pub tsc: u64,
    pub gdtr_base: u64,
    pub gdtr_limit: u16,
    pub idtr_base: u64,
    pub idtr_limit: u16,
    pub ldtr: SegmentState,
    pub tr: SegmentState,
}

impl MmuState {
    pub fn encode_v1<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u64_le(self.cr0)?;
        w.write_u64_le(self.cr2)?;
        w.write_u64_le(self.cr3)?;
        w.write_u64_le(self.cr4)?;
        w.write_u64_le(self.cr8)?;
        w.write_u64_le(self.efer)?;
        w.write_u64_le(self.gdtr_base)?;
        w.write_u16_le(self.gdtr_limit)?;
        w.write_u64_le(self.idtr_base)?;
        w.write_u16_le(self.idtr_limit)?;
        Ok(())
    }

    pub fn decode_v1<R: Read>(r: &mut R) -> Result<Self> {
        Ok(Self {
            cr0: r.read_u64_le()?,
            cr2: r.read_u64_le()?,
            cr3: r.read_u64_le()?,
            cr4: r.read_u64_le()?,
            cr8: r.read_u64_le()?,
            efer: r.read_u64_le()?,
            gdtr_base: r.read_u64_le()?,
            gdtr_limit: r.read_u16_le()?,
            idtr_base: r.read_u64_le()?,
            idtr_limit: r.read_u16_le()?,
            ..Self::default()
        })
    }

    pub fn encode_v2<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u64_le(self.cr0)?;
        w.write_u64_le(self.cr2)?;
        w.write_u64_le(self.cr3)?;
        w.write_u64_le(self.cr4)?;
        w.write_u64_le(self.cr8)?;

        w.write_u64_le(self.dr0)?;
        w.write_u64_le(self.dr1)?;
        w.write_u64_le(self.dr2)?;
        w.write_u64_le(self.dr3)?;
        w.write_u64_le(self.dr4)?;
        w.write_u64_le(self.dr5)?;
        w.write_u64_le(self.dr6)?;
        w.write_u64_le(self.dr7)?;

        w.write_u64_le(self.efer)?;
        w.write_u64_le(self.star)?;
        w.write_u64_le(self.lstar)?;
        w.write_u64_le(self.cstar)?;
        w.write_u64_le(self.sfmask)?;
        w.write_u64_le(self.sysenter_cs)?;
        w.write_u64_le(self.sysenter_eip)?;
        w.write_u64_le(self.sysenter_esp)?;
        w.write_u64_le(self.fs_base)?;
        w.write_u64_le(self.gs_base)?;
        w.write_u64_le(self.kernel_gs_base)?;
        w.write_u64_le(self.apic_base)?;
        w.write_u64_le(self.tsc)?;

        w.write_u64_le(self.gdtr_base)?;
        w.write_u16_le(self.gdtr_limit)?;
        w.write_u64_le(self.idtr_base)?;
        w.write_u16_le(self.idtr_limit)?;
        self.ldtr.encode(w)?;
        self.tr.encode(w)?;
        Ok(())
    }

    pub fn decode_v2<R: Read>(r: &mut R) -> Result<Self> {
        Ok(Self {
            cr0: r.read_u64_le()?,
            cr2: r.read_u64_le()?,
            cr3: r.read_u64_le()?,
            cr4: r.read_u64_le()?,
            cr8: r.read_u64_le()?,

            dr0: r.read_u64_le()?,
            dr1: r.read_u64_le()?,
            dr2: r.read_u64_le()?,
            dr3: r.read_u64_le()?,
            dr4: r.read_u64_le()?,
            dr5: r.read_u64_le()?,
            dr6: r.read_u64_le()?,
            dr7: r.read_u64_le()?,

            efer: r.read_u64_le()?,
            star: r.read_u64_le()?,
            lstar: r.read_u64_le()?,
            cstar: r.read_u64_le()?,
            sfmask: r.read_u64_le()?,
            sysenter_cs: r.read_u64_le()?,
            sysenter_eip: r.read_u64_le()?,
            sysenter_esp: r.read_u64_le()?,
            fs_base: r.read_u64_le()?,
            gs_base: r.read_u64_le()?,
            kernel_gs_base: r.read_u64_le()?,
            apic_base: r.read_u64_le()?,
            tsc: r.read_u64_le()?,

            gdtr_base: r.read_u64_le()?,
            gdtr_limit: r.read_u16_le()?,
            idtr_base: r.read_u64_le()?,
            idtr_limit: r.read_u16_le()?,
            ldtr: SegmentState::decode(r)?,
            tr: SegmentState::decode(r)?,
        })
    }

    /// Encode using the latest supported MMU section version.
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        self.encode_v2(w)
    }

    /// Decode using the latest supported MMU section version.
    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        Self::decode_v2(r)
    }
}

/// Non-architectural CPU bookkeeping that must be restored to resume deterministically.
///
/// This is intended to be stored as a `DeviceState` entry with `DeviceId::CPU_INTERNAL` and
/// `version = 2`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuInternalState {
    /// Interrupt inhibition/shadow counter (e.g. STI shadow).
    pub interrupt_inhibit: u8,
    /// FIFO of pending externally injected interrupt vectors (PIC/APIC).
    pub pending_external_interrupts: Vec<u8>,
}

impl CpuInternalState {
    pub const VERSION: u16 = 2;

    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u8(self.interrupt_inhibit)?;
        let len: u32 = self
            .pending_external_interrupts
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("too many pending interrupts"))?;
        if len > limits::MAX_PENDING_INTERRUPTS {
            return Err(SnapshotError::Corrupt("too many pending interrupts"));
        }
        w.write_u32_le(len)?;
        w.write_bytes(&self.pending_external_interrupts)?;
        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        let interrupt_inhibit = r.read_u8()?;
        let len = r.read_u32_le()?;
        if len > limits::MAX_PENDING_INTERRUPTS {
            return Err(SnapshotError::Corrupt("too many pending interrupts"));
        }
        let pending_external_interrupts = r.read_exact_vec(len as usize)?;
        Ok(Self {
            interrupt_inhibit,
            pending_external_interrupts,
        })
    }

    pub fn to_device_state(&self) -> Result<DeviceState> {
        let mut data = Vec::with_capacity(1 + 4 + self.pending_external_interrupts.len());
        self.encode(&mut data)?;
        Ok(DeviceState {
            id: DeviceId::CPU_INTERNAL,
            version: Self::VERSION,
            flags: 0,
            data,
        })
    }

    pub fn from_device_state(state: &DeviceState) -> Result<Self> {
        if state.id != DeviceId::CPU_INTERNAL {
            return Err(SnapshotError::Corrupt("expected CPU_INTERNAL device state"));
        }
        if state.version != Self::VERSION {
            return Err(SnapshotError::Corrupt(
                "unsupported CPU_INTERNAL device version",
            ));
        }
        Self::decode(&mut std::io::Cursor::new(&state.data))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceState {
    pub id: DeviceId,
    pub version: u16,
    pub flags: u16,
    pub data: Vec<u8>,
}

impl DeviceState {
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u32_le(self.id.0)?;
        w.write_u16_le(self.version)?;
        w.write_u16_le(self.flags)?;
        let len: u64 = self
            .data
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("device data too large"))?;
        if len > limits::MAX_DEVICE_ENTRY_LEN {
            return Err(SnapshotError::Corrupt("device entry too large"));
        }
        w.write_u64_le(len)?;
        w.write_bytes(&self.data)?;
        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R, max_len: u64) -> Result<Self> {
        let id = DeviceId(r.read_u32_le()?);
        let version = r.read_u16_le()?;
        let flags = r.read_u16_le()?;
        let len = r.read_u64_le()?;
        if len > max_len {
            return Err(SnapshotError::Corrupt("device entry too large"));
        }
        let data = r.read_exact_vec(len as usize)?;
        Ok(Self {
            id,
            version,
            flags,
            data,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiskOverlayRefs {
    pub disks: Vec<DiskOverlayRef>,
}

impl DiskOverlayRefs {
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        if self.disks.len() > limits::MAX_DISK_REFS as usize {
            return Err(SnapshotError::Corrupt("too many disks"));
        }
        let count: u32 = self
            .disks
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("too many disks"))?;
        w.write_u32_le(count)?;
        for disk in &self.disks {
            disk.encode(w)?;
        }
        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        let count = r.read_u32_le()? as usize;
        if count > limits::MAX_DISK_REFS as usize {
            return Err(SnapshotError::Corrupt("too many disks"));
        }
        let mut disks = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            disks.push(DiskOverlayRef::decode(r)?);
        }
        Ok(Self { disks })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskOverlayRef {
    pub disk_id: u32,
    pub base_image: String,
    pub overlay_image: String,
}

impl DiskOverlayRef {
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
        w.write_u32_le(self.disk_id)?;
        if self.base_image.len() > limits::MAX_DISK_PATH_LEN as usize {
            return Err(SnapshotError::Corrupt("disk base_image too long"));
        }
        w.write_string_u32(&self.base_image)?;
        if self.overlay_image.len() > limits::MAX_DISK_PATH_LEN as usize {
            return Err(SnapshotError::Corrupt("disk overlay_image too long"));
        }
        w.write_string_u32(&self.overlay_image)?;
        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        Ok(Self {
            disk_id: r.read_u32_le()?,
            base_image: decode_string_u32_bounded(
                r,
                limits::MAX_DISK_PATH_LEN,
                "disk base_image too long",
                "disk base_image: truncated string bytes",
                "disk base_image: invalid utf-8",
            )?,
            overlay_image: decode_string_u32_bounded(
                r,
                limits::MAX_DISK_PATH_LEN,
                "disk overlay_image too long",
                "disk overlay_image: truncated string bytes",
                "disk overlay_image: invalid utf-8",
            )?,
        })
    }
}
