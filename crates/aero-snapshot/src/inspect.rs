use std::io::{Read, Seek, SeekFrom};

use crate::error::{Result, SnapshotError};
use crate::format::{SectionId, SNAPSHOT_ENDIANNESS_LITTLE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION_V1};
use crate::io::ReadLeExt;
use crate::ram::{Compression, RamMode};
use crate::types::SnapshotMeta;

const SECTION_HEADER_LEN: u64 = 4 + 2 + 2 + 8;

const MAX_PAGE_SIZE: u32 = 2 * 1024 * 1024;
const MAX_CHUNK_SIZE: u32 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotSectionInfo {
    pub id: SectionId,
    pub version: u16,
    pub flags: u16,
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamHeaderSummary {
    pub total_len: u64,
    pub page_size: u32,
    pub mode: RamMode,
    pub compression: Compression,
    pub chunk_size: Option<u32>,
    pub dirty_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotIndex {
    pub version: u16,
    pub endianness: u8,
    pub meta: Option<SnapshotMeta>,
    pub sections: Vec<SnapshotSectionInfo>,
    pub ram: Option<RamHeaderSummary>,
}

pub fn inspect_snapshot<R: Read + Seek>(r: &mut R) -> Result<SnapshotIndex> {
    let start_pos = r.stream_position()?;
    let end_pos = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(start_pos))?;

    let (version, endianness) = read_file_header(r)?;

    let mut meta = None;
    let mut ram = None;
    let mut sections = Vec::new();

    while r.stream_position()? < end_pos {
        let header_pos = r.stream_position()?;
        let remaining = end_pos
            .checked_sub(header_pos)
            .ok_or(SnapshotError::Corrupt("stream position underflow"))?;
        if remaining < SECTION_HEADER_LEN {
            return Err(SnapshotError::Corrupt("truncated section header"));
        }

        let id = SectionId(r.read_u32_le()?);
        let section_version = r.read_u16_le()?;
        let flags = r.read_u16_le()?;
        let len = r.read_u64_le()?;
        let payload_offset = r.stream_position()?;
        let payload_end = payload_offset
            .checked_add(len)
            .ok_or(SnapshotError::Corrupt("section offset overflow"))?;
        if payload_end > end_pos {
            return Err(SnapshotError::Corrupt("section extends past end of file"));
        }

        sections.push(SnapshotSectionInfo {
            id,
            version: section_version,
            flags,
            offset: payload_offset,
            len,
        });

        if id == SectionId::META && section_version == 1 && meta.is_none() {
            let decoded = {
                let mut limited = r.take(len);
                SnapshotMeta::decode(&mut limited)?
            };
            meta = Some(decoded);
        } else if id == SectionId::RAM && section_version == 1 && ram.is_none() {
            let summary = {
                let mut limited = r.take(len);
                inspect_ram_section(&mut limited, len)?
            };
            ram = Some(summary);
        }

        r.seek(SeekFrom::Start(payload_end))?;
    }

    Ok(SnapshotIndex {
        version,
        endianness,
        meta,
        sections,
        ram,
    })
}

pub fn read_snapshot_meta<R: Read + Seek>(r: &mut R) -> Result<Option<SnapshotMeta>> {
    let start_pos = r.stream_position()?;
    let end_pos = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(start_pos))?;

    let _ = read_file_header(r)?;

    let mut meta = None;

    while r.stream_position()? < end_pos {
        let header_pos = r.stream_position()?;
        let remaining = end_pos
            .checked_sub(header_pos)
            .ok_or(SnapshotError::Corrupt("stream position underflow"))?;
        if remaining < SECTION_HEADER_LEN {
            return Err(SnapshotError::Corrupt("truncated section header"));
        }

        let id = SectionId(r.read_u32_le()?);
        let section_version = r.read_u16_le()?;
        let _flags = r.read_u16_le()?;
        let len = r.read_u64_le()?;
        let payload_offset = r.stream_position()?;
        let payload_end = payload_offset
            .checked_add(len)
            .ok_or(SnapshotError::Corrupt("section offset overflow"))?;
        if payload_end > end_pos {
            return Err(SnapshotError::Corrupt("section extends past end of file"));
        }

        if id == SectionId::META && section_version == 1 && meta.is_none() {
            let decoded = {
                let mut limited = r.take(len);
                SnapshotMeta::decode(&mut limited)?
            };
            meta = Some(decoded);
        }

        r.seek(SeekFrom::Start(payload_end))?;
    }

    Ok(meta)
}

fn read_file_header<R: Read>(r: &mut R) -> Result<(u16, u8)> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != SNAPSHOT_MAGIC {
        return Err(SnapshotError::InvalidMagic);
    }
    let version = r.read_u16_le()?;
    if version != SNAPSHOT_VERSION_V1 {
        return Err(SnapshotError::UnsupportedVersion(version));
    }
    let endianness = r.read_u8()?;
    if endianness != SNAPSHOT_ENDIANNESS_LITTLE {
        return Err(SnapshotError::InvalidEndianness(endianness));
    }
    let _reserved = r.read_u8()?;
    let _flags = r.read_u32_le()?;
    Ok((version, endianness))
}

fn inspect_ram_section<R: Read>(r: &mut R, section_len: u64) -> Result<RamHeaderSummary> {
    if section_len < 16 {
        return Err(SnapshotError::Corrupt("truncated ram section"));
    }

    let total_len = r.read_u64_le()?;
    let page_size = r.read_u32_le()?;
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return Err(SnapshotError::Corrupt("invalid page size"));
    }

    let mode = RamMode::from_u8(r.read_u8()?)?;
    let compression = Compression::from_u8(r.read_u8()?)?;
    let _reserved = r.read_u16_le()?;

    let mut summary = RamHeaderSummary {
        total_len,
        page_size,
        mode,
        compression,
        chunk_size: None,
        dirty_count: None,
    };

    match mode {
        RamMode::Full => {
            if section_len < 20 {
                return Err(SnapshotError::Corrupt("truncated ram section"));
            }
            let chunk_size = r.read_u32_le()?;
            if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
                return Err(SnapshotError::Corrupt("invalid chunk size"));
            }

            let chunk_count = total_len
                .checked_add(chunk_size as u64 - 1)
                .ok_or(SnapshotError::Corrupt("chunk count overflow"))?
                / chunk_size as u64;
            let min_payload_len = 20u64
                .checked_add(
                    chunk_count
                        .checked_mul(8)
                        .ok_or(SnapshotError::Corrupt("chunk count overflow"))?,
                )
                .ok_or(SnapshotError::Corrupt("chunk count overflow"))?;
            if section_len < min_payload_len {
                return Err(SnapshotError::Corrupt("truncated ram section"));
            }

            summary.chunk_size = Some(chunk_size);
        }
        RamMode::Dirty => {
            if section_len < 24 {
                return Err(SnapshotError::Corrupt("truncated ram section"));
            }
            let dirty_count = r.read_u64_le()?;
            let min_payload_len = 24u64
                .checked_add(
                    dirty_count
                        .checked_mul(16)
                        .ok_or(SnapshotError::Corrupt("dirty count overflow"))?,
                )
                .ok_or(SnapshotError::Corrupt("dirty count overflow"))?;
            if section_len < min_payload_len {
                return Err(SnapshotError::Corrupt("truncated ram section"));
            }

            summary.dirty_count = Some(dirty_count);
        }
    }

    Ok(summary)
}
