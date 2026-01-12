//! AeroGPU ring + submission + fence-page layouts.
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_ring.h`.

use super::aerogpu_pci::{parse_and_validate_abi_version_u32, AerogpuAbiError};
use std::borrow::Cow;

pub const AEROGPU_ALLOC_TABLE_MAGIC: u32 = 0x434F_4C41; // "ALOC" LE
pub const AEROGPU_RING_MAGIC: u32 = 0x474E_5241; // "ARNG" LE
pub const AEROGPU_FENCE_PAGE_MAGIC: u32 = 0x434E_4546; // "FENC" LE

pub const AEROGPU_SUBMIT_FLAG_NONE: u32 = 0;
pub const AEROGPU_SUBMIT_FLAG_PRESENT: u32 = 1u32 << 0;
pub const AEROGPU_SUBMIT_FLAG_NO_IRQ: u32 = 1u32 << 1;

pub const AEROGPU_ALLOC_FLAG_NONE: u32 = 0;
pub const AEROGPU_ALLOC_FLAG_READONLY: u32 = 1u32 << 0;

pub const AEROGPU_ENGINE_0: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuRingDecodeError {
    BufferTooSmall,
    BadMagic { found: u32 },
    Abi(AerogpuAbiError),
    BadSizeField { found: u32 },
    BadEntryCount { found: u32 },
    BadStrideField { found: u32 },
}

impl From<AerogpuAbiError> for AerogpuRingDecodeError {
    fn from(value: AerogpuAbiError) -> Self {
        Self::Abi(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuRingEncodeError {
    BufferTooSmall,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AerogpuAllocTableHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub entry_count: u32,
    pub entry_stride_bytes: u32,
    pub reserved0: u32,
}

impl AerogpuAllocTableHeader {
    pub const SIZE_BYTES: usize = 24;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Result<Self, AerogpuRingDecodeError> {
        if buf.len() < Self::SIZE_BYTES {
            return Err(AerogpuRingDecodeError::BufferTooSmall);
        }

        Ok(Self {
            magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            abi_version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            size_bytes: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            entry_count: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            entry_stride_bytes: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            reserved0: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
        })
    }

    pub fn validate_prefix(&self) -> Result<(), AerogpuRingDecodeError> {
        if self.magic != AEROGPU_ALLOC_TABLE_MAGIC {
            return Err(AerogpuRingDecodeError::BadMagic { found: self.magic });
        }

        let _ = parse_and_validate_abi_version_u32(self.abi_version)?;

        if self.entry_stride_bytes < AerogpuAllocEntry::SIZE_BYTES as u32 {
            return Err(AerogpuRingDecodeError::BadStrideField {
                found: self.entry_stride_bytes,
            });
        }

        let required = match (self.entry_count as u64).checked_mul(self.entry_stride_bytes as u64) {
            Some(bytes) => (Self::SIZE_BYTES as u64).saturating_add(bytes),
            None => u64::MAX,
        };
        if required > self.size_bytes as u64 {
            return Err(AerogpuRingDecodeError::BadSizeField {
                found: self.size_bytes,
            });
        }

        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AerogpuAllocEntry {
    pub alloc_id: u32,
    pub flags: u32,
    pub gpa: u64,
    pub size_bytes: u64,
    pub reserved0: u64,
}

impl AerogpuAllocEntry {
    pub const SIZE_BYTES: usize = 32;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Result<Self, AerogpuRingDecodeError> {
        if buf.len() < Self::SIZE_BYTES {
            return Err(AerogpuRingDecodeError::BufferTooSmall);
        }

        Ok(Self {
            alloc_id: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            gpa: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            size_bytes: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            reserved0: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
        })
    }
}

#[derive(Clone)]
pub struct AerogpuAllocTableView<'a> {
    pub header: AerogpuAllocTableHeader,
    pub entries: Cow<'a, [AerogpuAllocEntry]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuAllocTableDecodeError {
    BufferTooSmall,
    BadMagic { found: u32 },
    Abi(AerogpuAbiError),
    BadSize { found: u32, buf_len: usize },
    BadStride { found: u32, expected: u32 },
    CountOutOfBounds,
    Misaligned,
}

impl From<AerogpuAbiError> for AerogpuAllocTableDecodeError {
    fn from(value: AerogpuAbiError) -> Self {
        Self::Abi(value)
    }
}

pub fn decode_alloc_table_le(
    buf: &[u8],
) -> Result<AerogpuAllocTableView<'_>, AerogpuAllocTableDecodeError> {
    if buf.len() < AerogpuAllocTableHeader::SIZE_BYTES {
        return Err(AerogpuAllocTableDecodeError::BufferTooSmall);
    }

    let header = AerogpuAllocTableHeader {
        magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        abi_version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
        size_bytes: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        entry_count: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        entry_stride_bytes: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        reserved0: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
    };

    if header.magic != AEROGPU_ALLOC_TABLE_MAGIC {
        return Err(AerogpuAllocTableDecodeError::BadMagic {
            found: header.magic,
        });
    }

    let _ = parse_and_validate_abi_version_u32(header.abi_version)?;

    let header_size_bytes = AerogpuAllocTableHeader::SIZE_BYTES;

    if header.size_bytes < header_size_bytes as u32 {
        return Err(AerogpuAllocTableDecodeError::BadSize {
            found: header.size_bytes,
            buf_len: buf.len(),
        });
    }

    let size_bytes = header.size_bytes as usize;
    if size_bytes > buf.len() {
        return Err(AerogpuAllocTableDecodeError::BadSize {
            found: header.size_bytes,
            buf_len: buf.len(),
        });
    }

    let expected_stride = core::mem::size_of::<AerogpuAllocEntry>() as u32;
    if header.entry_stride_bytes < expected_stride {
        return Err(AerogpuAllocTableDecodeError::BadStride {
            found: header.entry_stride_bytes,
            expected: expected_stride,
        });
    }

    let entry_count = header.entry_count as usize;
    let entry_stride = header.entry_stride_bytes as usize;
    let entries_size_bytes = entry_count
        .checked_mul(entry_stride)
        .ok_or(AerogpuAllocTableDecodeError::CountOutOfBounds)?;

    let available_bytes = size_bytes - header_size_bytes;
    if entries_size_bytes > available_bytes {
        return Err(AerogpuAllocTableDecodeError::CountOutOfBounds);
    }

    let entries_buf = &buf[header_size_bytes..header_size_bytes + entries_size_bytes];

    let align = core::mem::align_of::<AerogpuAllocEntry>();
    if entry_stride == AerogpuAllocEntry::SIZE_BYTES
        && (entries_buf.as_ptr() as usize).is_multiple_of(align)
    {
        let entries = unsafe {
            core::slice::from_raw_parts(
                entries_buf.as_ptr() as *const AerogpuAllocEntry,
                entry_count,
            )
        };
        return Ok(AerogpuAllocTableView {
            header,
            entries: Cow::Borrowed(entries),
        });
    }

    let mut entries = Vec::new();
    if entries.try_reserve_exact(entry_count).is_err() {
        return Err(AerogpuAllocTableDecodeError::CountOutOfBounds);
    }
    for idx in 0..entry_count {
        let off = header_size_bytes
            .checked_add(
                idx.checked_mul(entry_stride)
                    .ok_or(AerogpuAllocTableDecodeError::CountOutOfBounds)?,
            )
            .ok_or(AerogpuAllocTableDecodeError::CountOutOfBounds)?;
        let end = off
            .checked_add(AerogpuAllocEntry::SIZE_BYTES)
            .ok_or(AerogpuAllocTableDecodeError::CountOutOfBounds)?;
        let Some(entry_bytes) = buf.get(off..end) else {
            return Err(AerogpuAllocTableDecodeError::CountOutOfBounds);
        };
        entries.push(
            AerogpuAllocEntry::decode_from_le_bytes(entry_bytes)
                .map_err(|_| AerogpuAllocTableDecodeError::BufferTooSmall)?,
        );
    }

    Ok(AerogpuAllocTableView {
        header,
        entries: Cow::Owned(entries),
    })
}

pub fn lookup_alloc<'a>(
    table: &'a AerogpuAllocTableView<'_>,
    alloc_id: u32,
) -> Option<&'a AerogpuAllocEntry> {
    table
        .entries
        .iter()
        .find(|entry| entry.alloc_id == alloc_id)
}

/// Fixed-size submission descriptor written into the ring by the guest KMD.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuSubmitDesc {
    pub desc_size_bytes: u32,
    pub flags: u32,
    pub context_id: u32,
    pub engine_id: u32,
    pub cmd_gpa: u64,
    pub cmd_size_bytes: u32,
    pub cmd_reserved0: u32,
    pub alloc_table_gpa: u64,
    pub alloc_table_size_bytes: u32,
    pub alloc_table_reserved0: u32,
    pub signal_fence: u64,
    pub reserved0: u64,
}

impl AerogpuSubmitDesc {
    pub const SIZE_BYTES: usize = 64;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Result<Self, AerogpuRingDecodeError> {
        if buf.len() < Self::SIZE_BYTES {
            return Err(AerogpuRingDecodeError::BufferTooSmall);
        }

        Ok(Self {
            desc_size_bytes: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            context_id: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            engine_id: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            cmd_gpa: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            cmd_size_bytes: u32::from_le_bytes(buf[24..28].try_into().unwrap()),
            cmd_reserved0: u32::from_le_bytes(buf[28..32].try_into().unwrap()),
            alloc_table_gpa: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
            alloc_table_size_bytes: u32::from_le_bytes(buf[40..44].try_into().unwrap()),
            alloc_table_reserved0: u32::from_le_bytes(buf[44..48].try_into().unwrap()),
            signal_fence: u64::from_le_bytes(buf[48..56].try_into().unwrap()),
            reserved0: u64::from_le_bytes(buf[56..64].try_into().unwrap()),
        })
    }

    pub fn validate_prefix(&self) -> Result<(), AerogpuRingDecodeError> {
        if self.desc_size_bytes < Self::SIZE_BYTES as u32 {
            return Err(AerogpuRingDecodeError::BadSizeField {
                found: self.desc_size_bytes,
            });
        }
        Ok(())
    }
}

/// Ring header at the start of the ring shared-memory region.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuRingHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub entry_count: u32,
    pub entry_stride_bytes: u32,
    pub flags: u32,
    pub head: u32,
    pub tail: u32,
    pub reserved0: u32,
    pub reserved1: u32,
    pub reserved2: [u64; 3],
}

impl AerogpuRingHeader {
    pub const SIZE_BYTES: usize = 64;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Result<Self, AerogpuRingDecodeError> {
        if buf.len() < Self::SIZE_BYTES {
            return Err(AerogpuRingDecodeError::BufferTooSmall);
        }

        Ok(Self {
            magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            abi_version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            size_bytes: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            entry_count: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            entry_stride_bytes: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
            head: u32::from_le_bytes(buf[24..28].try_into().unwrap()),
            tail: u32::from_le_bytes(buf[28..32].try_into().unwrap()),
            reserved0: u32::from_le_bytes(buf[32..36].try_into().unwrap()),
            reserved1: u32::from_le_bytes(buf[36..40].try_into().unwrap()),
            reserved2: [
                u64::from_le_bytes(buf[40..48].try_into().unwrap()),
                u64::from_le_bytes(buf[48..56].try_into().unwrap()),
                u64::from_le_bytes(buf[56..64].try_into().unwrap()),
            ],
        })
    }

    pub fn validate_prefix(&self) -> Result<(), AerogpuRingDecodeError> {
        if self.magic != AEROGPU_RING_MAGIC {
            return Err(AerogpuRingDecodeError::BadMagic { found: self.magic });
        }

        let _ = parse_and_validate_abi_version_u32(self.abi_version)?;

        if self.entry_count == 0 || !self.entry_count.is_power_of_two() {
            return Err(AerogpuRingDecodeError::BadEntryCount {
                found: self.entry_count,
            });
        }

        if self.entry_stride_bytes < AerogpuSubmitDesc::SIZE_BYTES as u32 {
            return Err(AerogpuRingDecodeError::BadStrideField {
                found: self.entry_stride_bytes,
            });
        }

        let required = match (self.entry_count as u64).checked_mul(self.entry_stride_bytes as u64) {
            Some(bytes) => (Self::SIZE_BYTES as u64).saturating_add(bytes),
            None => u64::MAX,
        };

        if required > self.size_bytes as u64 {
            return Err(AerogpuRingDecodeError::BadSizeField {
                found: self.size_bytes,
            });
        }

        Ok(())
    }
}

/// Optional shared fence page written by the host device model.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuFencePage {
    pub magic: u32,
    pub abi_version: u32,
    pub completed_fence: u64,
    pub reserved0: [u64; 5],
}

impl AerogpuFencePage {
    pub const SIZE_BYTES: usize = 56;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Result<Self, AerogpuRingDecodeError> {
        if buf.len() < Self::SIZE_BYTES {
            return Err(AerogpuRingDecodeError::BufferTooSmall);
        }

        Ok(Self {
            magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            abi_version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            completed_fence: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            reserved0: [
                u64::from_le_bytes(buf[16..24].try_into().unwrap()),
                u64::from_le_bytes(buf[24..32].try_into().unwrap()),
                u64::from_le_bytes(buf[32..40].try_into().unwrap()),
                u64::from_le_bytes(buf[40..48].try_into().unwrap()),
                u64::from_le_bytes(buf[48..56].try_into().unwrap()),
            ],
        })
    }

    pub fn validate_prefix(&self) -> Result<(), AerogpuRingDecodeError> {
        if self.magic != AEROGPU_FENCE_PAGE_MAGIC {
            return Err(AerogpuRingDecodeError::BadMagic { found: self.magic });
        }

        let _ = parse_and_validate_abi_version_u32(self.abi_version)?;
        Ok(())
    }
}

/// Write the completed fence value into an existing fence page mapping.
pub fn write_fence_page_completed_fence_le(
    buf: &mut [u8],
    fence_value: u64,
) -> Result<(), AerogpuRingEncodeError> {
    if buf.len() < AerogpuFencePage::SIZE_BYTES {
        return Err(AerogpuRingEncodeError::BufferTooSmall);
    }
    buf[8..16].copy_from_slice(&fence_value.to_le_bytes());
    Ok(())
}
