use std::io::{Read, Write};

use crate::error::{Result, SnapshotError};
use crate::format::DeviceId;
use crate::io::{ReadLeExt, WriteLeExt};

const MAX_LABEL_LEN: u32 = 4 * 1024;
const MAX_DISK_PATH_LEN: u32 = 64 * 1024;

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
            1 => Some(r.read_string_u32(MAX_LABEL_LEN)?),
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    pub rflags: u64,
    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub fs: u16,
    pub gs: u16,
    pub ss: u16,
    pub xmm: [u128; 16],
}

impl CpuState {
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
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
        w.write_u16_le(self.cs)?;
        w.write_u16_le(self.ds)?;
        w.write_u16_le(self.es)?;
        w.write_u16_le(self.fs)?;
        w.write_u16_le(self.gs)?;
        w.write_u16_le(self.ss)?;
        for xmm in self.xmm {
            w.write_u128_le(xmm)?;
        }
        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        let mut state = CpuState::default();
        state.rax = r.read_u64_le()?;
        state.rbx = r.read_u64_le()?;
        state.rcx = r.read_u64_le()?;
        state.rdx = r.read_u64_le()?;
        state.rsi = r.read_u64_le()?;
        state.rdi = r.read_u64_le()?;
        state.rbp = r.read_u64_le()?;
        state.rsp = r.read_u64_le()?;
        state.r8 = r.read_u64_le()?;
        state.r9 = r.read_u64_le()?;
        state.r10 = r.read_u64_le()?;
        state.r11 = r.read_u64_le()?;
        state.r12 = r.read_u64_le()?;
        state.r13 = r.read_u64_le()?;
        state.r14 = r.read_u64_le()?;
        state.r15 = r.read_u64_le()?;
        state.rip = r.read_u64_le()?;
        state.rflags = r.read_u64_le()?;
        state.cs = r.read_u16_le()?;
        state.ds = r.read_u16_le()?;
        state.es = r.read_u16_le()?;
        state.fs = r.read_u16_le()?;
        state.gs = r.read_u16_le()?;
        state.ss = r.read_u16_le()?;
        for xmm in &mut state.xmm {
            *xmm = r.read_u128_le()?;
        }
        Ok(state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MmuState {
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub gdtr_base: u64,
    pub gdtr_limit: u16,
    pub idtr_base: u64,
    pub idtr_limit: u16,
}

impl MmuState {
    pub fn encode<W: Write>(&self, w: &mut W) -> Result<()> {
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

    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
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
        })
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
        w.write_string_u32(&self.base_image)?;
        w.write_string_u32(&self.overlay_image)?;
        Ok(())
    }

    pub fn decode<R: Read>(r: &mut R) -> Result<Self> {
        Ok(Self {
            disk_id: r.read_u32_le()?,
            base_image: r.read_string_u32(MAX_DISK_PATH_LEN)?,
            overlay_image: r.read_string_u32(MAX_DISK_PATH_LEN)?,
        })
    }
}
