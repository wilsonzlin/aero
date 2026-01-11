use crate::format::{
    AerogpuMemoryRangeRef, BlobKind, FrameTocEntry, RecordType, TraceFooter, TraceHeader, TraceToc,
    AEROGPU_SUBMISSION_MEMORY_RANGE_ENTRY_SIZE, AEROGPU_SUBMISSION_RECORD_HEADER_SIZE,
    CONTAINER_VERSION, CONTAINER_VERSION_V1, FOOTER_MAGIC, TOC_ENTRY_SIZE, TOC_HEADER_SIZE,
    TOC_MAGIC, TOC_VERSION, TRACE_BLOB_HEADER_SIZE, TRACE_FOOTER_SIZE, TRACE_HEADER_SIZE,
    TRACE_MAGIC, TRACE_RECORD_HEADER_SIZE,
};
use std::io;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug)]
pub enum TraceReadError {
    Io(io::Error),
    InvalidMagic,
    UnsupportedHeaderSize(u32),
    UnsupportedFooterSize(u32),
    UnsupportedContainerVersion(u32),
    UnsupportedTocVersion(u32),
    TocOutOfBounds,
    RecordOutOfBounds,
    UnknownRecordType(u8),
    UnknownBlobKind(u32),
    MalformedBlob,
}

impl From<io::Error> for TraceReadError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceRecord {
    BeginFrame {
        frame_index: u32,
    },
    Present {
        frame_index: u32,
    },
    Packet {
        bytes: Vec<u8>,
    },
    Blob {
        blob_id: u64,
        kind: BlobKind,
        bytes: Vec<u8>,
    },
    AerogpuSubmission {
        record_version: u32,
        submit_flags: u32,
        context_id: u32,
        engine_id: u32,
        signal_fence: u64,
        cmd_stream_blob_id: u64,
        alloc_table_blob_id: u64,
        memory_ranges: Vec<AerogpuMemoryRangeRef>,
    },
}

pub struct TraceReader<R> {
    reader: R,
    pub header: TraceHeader,
    pub meta_json: Vec<u8>,
    pub footer: TraceFooter,
    pub toc: TraceToc,
}

impl<R: Read + Seek> TraceReader<R> {
    pub fn open(mut reader: R) -> Result<Self, TraceReadError> {
        let header = read_header(&mut reader)?;
        let mut meta_json = vec![0u8; header.meta_len as usize];
        reader.read_exact(&mut meta_json)?;

        let file_len = reader.seek(SeekFrom::End(0))?;
        if file_len < TRACE_FOOTER_SIZE as u64 {
            return Err(TraceReadError::TocOutOfBounds);
        }
        reader.seek(SeekFrom::End(-(TRACE_FOOTER_SIZE as i64)))?;
        let footer = read_footer(&mut reader)?;

        if footer.container_version != header.container_version {
            return Err(TraceReadError::UnsupportedContainerVersion(
                footer.container_version,
            ));
        }

        if footer.toc_offset + footer.toc_len > file_len {
            return Err(TraceReadError::TocOutOfBounds);
        }

        reader.seek(SeekFrom::Start(footer.toc_offset))?;
        let toc = read_toc(&mut reader, footer.toc_len)?;

        Ok(Self {
            reader,
            header,
            meta_json,
            footer,
            toc,
        })
    }

    pub fn frame_entries(&self) -> &[FrameTocEntry] {
        &self.toc.entries
    }

    pub fn read_records_in_range(
        &mut self,
        start: u64,
        end: u64,
    ) -> Result<Vec<TraceRecord>, TraceReadError> {
        if start > end {
            return Err(TraceReadError::RecordOutOfBounds);
        }
        self.reader.seek(SeekFrom::Start(start))?;
        let mut out = Vec::new();
        while self.reader.stream_position()? < end {
            let record = read_record(&mut self.reader, end)?;
            out.push(record);
        }
        Ok(out)
    }
}

fn read_header<R: Read>(reader: &mut R) -> Result<TraceHeader, TraceReadError> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if magic != TRACE_MAGIC {
        return Err(TraceReadError::InvalidMagic);
    }

    let header_size = read_u32(reader)?;
    if header_size != TRACE_HEADER_SIZE {
        return Err(TraceReadError::UnsupportedHeaderSize(header_size));
    }
    let container_version = read_u32(reader)?;
    if !is_supported_container_version(container_version) {
        return Err(TraceReadError::UnsupportedContainerVersion(
            container_version,
        ));
    }
    let command_abi_version = read_u32(reader)?;
    let flags = read_u32(reader)?;
    let meta_len = read_u32(reader)?;
    let _reserved = read_u32(reader)?;

    Ok(TraceHeader {
        container_version,
        command_abi_version,
        flags,
        meta_len,
    })
}

fn read_footer<R: Read>(reader: &mut R) -> Result<TraceFooter, TraceReadError> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if magic != FOOTER_MAGIC {
        return Err(TraceReadError::InvalidMagic);
    }
    let footer_size = read_u32(reader)?;
    if footer_size != TRACE_FOOTER_SIZE {
        return Err(TraceReadError::UnsupportedFooterSize(footer_size));
    }
    let container_version = read_u32(reader)?;
    if !is_supported_container_version(container_version) {
        return Err(TraceReadError::UnsupportedContainerVersion(
            container_version,
        ));
    }

    let toc_offset = read_u64(reader)?;
    let toc_len = read_u64(reader)?;
    Ok(TraceFooter {
        container_version,
        toc_offset,
        toc_len,
    })
}

fn read_toc<R: Read>(reader: &mut R, toc_len: u64) -> Result<TraceToc, TraceReadError> {
    if toc_len < TOC_HEADER_SIZE as u64 {
        return Err(TraceReadError::TocOutOfBounds);
    }
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if magic != TOC_MAGIC {
        return Err(TraceReadError::InvalidMagic);
    }

    let toc_version = read_u32(reader)?;
    if toc_version != TOC_VERSION {
        return Err(TraceReadError::UnsupportedTocVersion(toc_version));
    }

    let frame_count = read_u32(reader)? as usize;
    let expected_len = TOC_HEADER_SIZE as u64 + (frame_count as u64) * (TOC_ENTRY_SIZE as u64);
    if toc_len != expected_len {
        return Err(TraceReadError::TocOutOfBounds);
    }

    let mut entries = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        let frame_index = read_u32(reader)?;
        let flags = read_u32(reader)?;
        let start_offset = read_u64(reader)?;
        let present_offset = read_u64(reader)?;
        let end_offset = read_u64(reader)?;
        entries.push(FrameTocEntry {
            frame_index,
            flags,
            start_offset,
            present_offset,
            end_offset,
        });
    }

    Ok(TraceToc { entries })
}

fn read_record<R: Read + Seek>(reader: &mut R, end: u64) -> Result<TraceRecord, TraceReadError> {
    let record_type_raw = read_u8(reader)?;
    let _flags = read_u8(reader)?;
    let _reserved = read_u16(reader)?;
    let payload_len = read_u32(reader)? as u64;
    debug_assert_eq!(TRACE_RECORD_HEADER_SIZE, 8);

    let payload_start = reader.stream_position()?;
    let payload_end = payload_start
        .checked_add(payload_len)
        .ok_or(TraceReadError::RecordOutOfBounds)?;
    if payload_end > end {
        return Err(TraceReadError::RecordOutOfBounds);
    }

    let mut payload = vec![0u8; payload_len as usize];
    reader.read_exact(&mut payload)?;

    let record_type = RecordType::from_u8(record_type_raw)
        .ok_or(TraceReadError::UnknownRecordType(record_type_raw))?;

    match record_type {
        RecordType::BeginFrame => {
            if payload.len() != 4 {
                return Err(TraceReadError::RecordOutOfBounds);
            }
            let frame_index = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            Ok(TraceRecord::BeginFrame { frame_index })
        }
        RecordType::Present => {
            if payload.len() != 4 {
                return Err(TraceReadError::RecordOutOfBounds);
            }
            let frame_index = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            Ok(TraceRecord::Present { frame_index })
        }
        RecordType::Packet => Ok(TraceRecord::Packet { bytes: payload }),
        RecordType::Blob => {
            if payload.len() < TRACE_BLOB_HEADER_SIZE as usize {
                return Err(TraceReadError::MalformedBlob);
            }
            let blob_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
            let kind_raw = u32::from_le_bytes(payload[8..12].try_into().unwrap());
            let kind =
                BlobKind::from_u32(kind_raw).ok_or(TraceReadError::UnknownBlobKind(kind_raw))?;
            let _reserved = u32::from_le_bytes(payload[12..16].try_into().unwrap());
            let bytes = payload[16..].to_vec();
            Ok(TraceRecord::Blob {
                blob_id,
                kind,
                bytes,
            })
        }
        RecordType::AerogpuSubmission => {
            if payload.len() < AEROGPU_SUBMISSION_RECORD_HEADER_SIZE as usize {
                return Err(TraceReadError::RecordOutOfBounds);
            }

            let record_version = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            let header_size = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
            if header_size < AEROGPU_SUBMISSION_RECORD_HEADER_SIZE as usize
                || header_size > payload.len()
            {
                return Err(TraceReadError::RecordOutOfBounds);
            }

            let submit_flags = u32::from_le_bytes(payload[8..12].try_into().unwrap());
            let context_id = u32::from_le_bytes(payload[12..16].try_into().unwrap());
            let engine_id = u32::from_le_bytes(payload[16..20].try_into().unwrap());
            let _reserved0 = u32::from_le_bytes(payload[20..24].try_into().unwrap());
            let signal_fence = u64::from_le_bytes(payload[24..32].try_into().unwrap());
            let cmd_stream_blob_id = u64::from_le_bytes(payload[32..40].try_into().unwrap());
            let alloc_table_blob_id = u64::from_le_bytes(payload[40..48].try_into().unwrap());
            let memory_range_count = u32::from_le_bytes(payload[48..52].try_into().unwrap()) as usize;
            let _reserved1 = u32::from_le_bytes(payload[52..56].try_into().unwrap());

            let required_len = header_size
                .checked_add(
                    memory_range_count * (AEROGPU_SUBMISSION_MEMORY_RANGE_ENTRY_SIZE as usize),
                )
                .ok_or(TraceReadError::RecordOutOfBounds)?;
            if required_len > payload.len() {
                return Err(TraceReadError::RecordOutOfBounds);
            }

            let mut memory_ranges = Vec::with_capacity(memory_range_count);
            let mut off = header_size;
            for _ in 0..memory_range_count {
                let alloc_id = u32::from_le_bytes(payload[off..off + 4].try_into().unwrap());
                let flags = u32::from_le_bytes(payload[off + 4..off + 8].try_into().unwrap());
                let gpa = u64::from_le_bytes(payload[off + 8..off + 16].try_into().unwrap());
                let size_bytes = u64::from_le_bytes(payload[off + 16..off + 24].try_into().unwrap());
                let blob_id = u64::from_le_bytes(payload[off + 24..off + 32].try_into().unwrap());
                memory_ranges.push(AerogpuMemoryRangeRef {
                    alloc_id,
                    flags,
                    gpa,
                    size_bytes,
                    blob_id,
                });
                off += AEROGPU_SUBMISSION_MEMORY_RANGE_ENTRY_SIZE as usize;
            }

            Ok(TraceRecord::AerogpuSubmission {
                record_version,
                submit_flags,
                context_id,
                engine_id,
                signal_fence,
                cmd_stream_blob_id,
                alloc_table_blob_id,
                memory_ranges,
            })
        }
    }
}

fn is_supported_container_version(v: u32) -> bool {
    (CONTAINER_VERSION_V1..=CONTAINER_VERSION).contains(&v)
}

fn read_u8<R: Read>(reader: &mut R) -> Result<u8, TraceReadError> {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u16<R: Read>(reader: &mut R) -> Result<u16, TraceReadError> {
    let mut buf = [0u8; 2];
    reader.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32, TraceReadError> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64, TraceReadError> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}
