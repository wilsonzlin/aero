use crate::format::{
    AerogpuMemoryRangeRef, BlobKind, FrameTocEntry, RecordType, TraceFooter, TraceHeader,
    TraceMeta, TraceToc, AEROGPU_SUBMISSION_MEMORY_RANGE_ENTRY_SIZE,
    AEROGPU_SUBMISSION_RECORD_HEADER_SIZE, AEROGPU_SUBMISSION_RECORD_VERSION, CONTAINER_VERSION_V1,
    CONTAINER_VERSION_V2, FOOTER_MAGIC, TOC_ENTRY_SIZE, TOC_HEADER_SIZE, TOC_MAGIC, TOC_VERSION,
    TRACE_BLOB_HEADER_SIZE, TRACE_FOOTER_SIZE, TRACE_HEADER_SIZE, TRACE_MAGIC,
    TRACE_RECORD_HEADER_SIZE,
};
use std::io;
use std::io::Write;

#[derive(Debug)]
pub enum TraceWriteError {
    Io(io::Error),
    FrameAlreadyOpen,
    NoOpenFrame,
    PresentWithoutFrame,
    DuplicateFrameIndex { frame_index: u32 },
    FinishWithOpenFrame,
    UnsupportedContainerVersion(u32),
}

impl From<io::Error> for TraceWriteError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Sideband capture of a guest memory range referenced by an AeroGPU submission.
///
/// These are emitted as `BlobKind::AerogpuAllocMemory` blobs and referenced by
/// `RecordType::AerogpuSubmission`.
#[derive(Clone, Copy, Debug)]
pub struct AerogpuMemoryRangeCapture<'a> {
    pub alloc_id: u32,
    pub flags: u32,
    pub gpa: u64,
    pub size_bytes: u64,
    pub bytes: &'a [u8],
}

pub struct TraceWriter<W> {
    writer: W,
    pos: u64,
    container_version: u32,
    toc: Vec<FrameTocEntry>,
    open_frame: Option<usize>,
    next_blob_id: u64,
}

impl<W: Write> TraceWriter<W> {
    pub fn new(writer: W, meta: &TraceMeta) -> Result<Self, TraceWriteError> {
        Self::new_with_container_version(writer, meta, CONTAINER_VERSION_V1)
    }

    pub fn new_v2(writer: W, meta: &TraceMeta) -> Result<Self, TraceWriteError> {
        Self::new_with_container_version(writer, meta, CONTAINER_VERSION_V2)
    }

    pub fn new_with_container_version(
        writer: W,
        meta: &TraceMeta,
        container_version: u32,
    ) -> Result<Self, TraceWriteError> {
        if container_version != CONTAINER_VERSION_V1 && container_version != CONTAINER_VERSION_V2 {
            return Err(TraceWriteError::UnsupportedContainerVersion(container_version));
        }

        let meta_bytes = meta.to_json_bytes();
        let header = TraceHeader {
            container_version,
            command_abi_version: meta.command_abi_version,
            flags: 0,
            meta_len: meta_bytes
                .len()
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "meta too large"))?,
        };

        let mut this = Self {
            writer,
            pos: 0,
            container_version,
            toc: Vec::new(),
            open_frame: None,
            next_blob_id: 1,
        };

        this.write_trace_header(&header)?;
        this.write_exact(&meta_bytes)?;

        Ok(this)
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn begin_frame(&mut self, frame_index: u32) -> Result<(), TraceWriteError> {
        if self.open_frame.is_some() {
            return Err(TraceWriteError::FrameAlreadyOpen);
        }
        if self
            .toc
            .last()
            .is_some_and(|e| e.frame_index == frame_index)
        {
            return Err(TraceWriteError::DuplicateFrameIndex { frame_index });
        }

        let start_offset = self.pos;
        self.write_record_begin_frame(frame_index)?;

        self.toc.push(FrameTocEntry {
            frame_index,
            flags: 0,
            start_offset,
            present_offset: 0,
            end_offset: 0,
        });
        self.open_frame = Some(self.toc.len() - 1);
        Ok(())
    }

    pub fn write_packet(&mut self, packet_bytes: &[u8]) -> Result<(), TraceWriteError> {
        if self.open_frame.is_none() {
            return Err(TraceWriteError::NoOpenFrame);
        }
        self.write_record(RecordType::Packet, 0, packet_bytes)?;
        Ok(())
    }

    pub fn write_blob(&mut self, kind: BlobKind, data: &[u8]) -> Result<u64, TraceWriteError> {
        let blob_id = self.next_blob_id;
        self.next_blob_id = self.next_blob_id.wrapping_add(1);

        let mut payload = Vec::with_capacity(TRACE_BLOB_HEADER_SIZE as usize + data.len());
        payload.extend_from_slice(&blob_id.to_le_bytes());
        payload.extend_from_slice(&(kind as u32).to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes()); // reserved
        payload.extend_from_slice(data);

        self.write_record(RecordType::Blob, 0, &payload)?;
        Ok(blob_id)
    }

    pub fn write_aerogpu_submission(
        &mut self,
        submit_flags: u32,
        context_id: u32,
        engine_id: u32,
        signal_fence: u64,
        cmd_stream_bytes: &[u8],
        alloc_table_bytes: Option<&[u8]>,
        memory_ranges: &[AerogpuMemoryRangeCapture<'_>],
    ) -> Result<(), TraceWriteError> {
        if self.container_version < CONTAINER_VERSION_V2 {
            return Err(TraceWriteError::UnsupportedContainerVersion(
                self.container_version,
            ));
        }
        if self.open_frame.is_none() {
            return Err(TraceWriteError::NoOpenFrame);
        }

        let cmd_stream_blob_id = self.write_blob(BlobKind::AerogpuCmdStream, cmd_stream_bytes)?;
        let alloc_table_blob_id = match alloc_table_bytes {
            Some(bytes) => self.write_blob(BlobKind::AerogpuAllocTable, bytes)?,
            None => 0,
        };

        let mut range_refs = Vec::with_capacity(memory_ranges.len());
        for range in memory_ranges {
            let blob_id = self.write_blob(BlobKind::AerogpuAllocMemory, range.bytes)?;
            range_refs.push(AerogpuMemoryRangeRef {
                alloc_id: range.alloc_id,
                flags: range.flags,
                gpa: range.gpa,
                size_bytes: range.size_bytes,
                blob_id,
            });
        }

        let range_count: u32 = range_refs
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many memory ranges"))?;

        let mut payload = Vec::with_capacity(
            AEROGPU_SUBMISSION_RECORD_HEADER_SIZE as usize
                + (range_refs.len() * AEROGPU_SUBMISSION_MEMORY_RANGE_ENTRY_SIZE as usize),
        );

        payload.extend_from_slice(&AEROGPU_SUBMISSION_RECORD_VERSION.to_le_bytes());
        payload.extend_from_slice(&AEROGPU_SUBMISSION_RECORD_HEADER_SIZE.to_le_bytes());
        payload.extend_from_slice(&submit_flags.to_le_bytes());
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&engine_id.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        payload.extend_from_slice(&signal_fence.to_le_bytes());
        payload.extend_from_slice(&cmd_stream_blob_id.to_le_bytes());
        payload.extend_from_slice(&alloc_table_blob_id.to_le_bytes());
        payload.extend_from_slice(&range_count.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for range in &range_refs {
            payload.extend_from_slice(&range.alloc_id.to_le_bytes());
            payload.extend_from_slice(&range.flags.to_le_bytes());
            payload.extend_from_slice(&range.gpa.to_le_bytes());
            payload.extend_from_slice(&range.size_bytes.to_le_bytes());
            payload.extend_from_slice(&range.blob_id.to_le_bytes());
        }

        debug_assert_eq!(
            payload.len(),
            AEROGPU_SUBMISSION_RECORD_HEADER_SIZE as usize
                + (range_refs.len() * AEROGPU_SUBMISSION_MEMORY_RANGE_ENTRY_SIZE as usize)
        );

        self.write_record(RecordType::AerogpuSubmission, 0, &payload)?;
        Ok(())
    }

    pub fn present(&mut self, frame_index: u32) -> Result<(), TraceWriteError> {
        let Some(frame_slot) = self.open_frame else {
            return Err(TraceWriteError::PresentWithoutFrame);
        };
        if self.toc[frame_slot].frame_index != frame_index {
            return Err(TraceWriteError::PresentWithoutFrame);
        }

        let present_offset = self.pos;
        self.write_record_present(frame_index)?;

        self.toc[frame_slot].present_offset = present_offset;
        self.toc[frame_slot].end_offset = self.pos;
        self.open_frame = None;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W, TraceWriteError> {
        if self.open_frame.is_some() {
            return Err(TraceWriteError::FinishWithOpenFrame);
        }

        let toc_offset = self.pos;
        let toc = TraceToc {
            entries: self.toc.clone(),
        };
        self.write_toc(&toc)?;
        let toc_len = toc.len_bytes();

        let footer = TraceFooter {
            container_version: self.container_version,
            toc_offset,
            toc_len,
        };
        self.write_footer(&footer)?;
        Ok(self.writer)
    }

    fn write_trace_header(&mut self, header: &TraceHeader) -> Result<(), TraceWriteError> {
        self.write_exact(&TRACE_MAGIC)?;
        self.write_u32(TRACE_HEADER_SIZE)?;
        self.write_u32(header.container_version)?;
        self.write_u32(header.command_abi_version)?;
        self.write_u32(header.flags)?;
        self.write_u32(header.meta_len)?;
        self.write_u32(0)?; // reserved
        Ok(())
    }

    fn write_footer(&mut self, footer: &TraceFooter) -> Result<(), TraceWriteError> {
        self.write_exact(&FOOTER_MAGIC)?;
        self.write_u32(TRACE_FOOTER_SIZE)?;
        self.write_u32(footer.container_version)?;
        self.write_u64(footer.toc_offset)?;
        self.write_u64(footer.toc_len)?;
        Ok(())
    }

    fn write_toc(&mut self, toc: &TraceToc) -> Result<(), TraceWriteError> {
        self.write_exact(&TOC_MAGIC)?;
        self.write_u32(TOC_VERSION)?;
        self.write_u32(
            toc.entries
                .len()
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many frames"))?,
        )?;

        for entry in &toc.entries {
            self.write_u32(entry.frame_index)?;
            self.write_u32(entry.flags)?;
            self.write_u64(entry.start_offset)?;
            self.write_u64(entry.present_offset)?;
            self.write_u64(entry.end_offset)?;
        }

        debug_assert_eq!(
            toc.len_bytes(),
            TOC_HEADER_SIZE as u64 + (toc.entries.len() as u64) * (TOC_ENTRY_SIZE as u64)
        );
        Ok(())
    }

    fn write_record_begin_frame(&mut self, frame_index: u32) -> Result<(), TraceWriteError> {
        self.write_record(RecordType::BeginFrame, 0, &frame_index.to_le_bytes())
    }

    fn write_record_present(&mut self, frame_index: u32) -> Result<(), TraceWriteError> {
        self.write_record(RecordType::Present, 0, &frame_index.to_le_bytes())
    }

    fn write_record(
        &mut self,
        record_type: RecordType,
        flags: u8,
        payload: &[u8],
    ) -> Result<(), TraceWriteError> {
        let payload_len: u32 = payload
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "record payload too large"))?;

        self.write_u8(record_type as u8)?;
        self.write_u8(flags)?;
        self.write_u16(0)?; // reserved
        self.write_u32(payload_len)?;
        debug_assert_eq!(TRACE_RECORD_HEADER_SIZE, 8);
        self.write_exact(payload)?;
        Ok(())
    }

    fn write_u8(&mut self, value: u8) -> Result<(), TraceWriteError> {
        self.write_exact(&[value])
    }

    fn write_u16(&mut self, value: u16) -> Result<(), TraceWriteError> {
        self.write_exact(&value.to_le_bytes())
    }

    fn write_u32(&mut self, value: u32) -> Result<(), TraceWriteError> {
        self.write_exact(&value.to_le_bytes())
    }

    fn write_u64(&mut self, value: u64) -> Result<(), TraceWriteError> {
        self.write_exact(&value.to_le_bytes())
    }

    fn write_exact(&mut self, bytes: &[u8]) -> Result<(), TraceWriteError> {
        self.writer.write_all(bytes)?;
        self.pos = self
            .pos
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "trace > u64::MAX"))?;
        Ok(())
    }
}

/// Helper wrapper for integrating tracing without paying the cost when disabled.
///
/// In the emulator, the GPU command processor can hold a `Recorder` and call
/// `record_packet()` unconditionally. When disabled this is a single match.
pub enum Recorder<W> {
    Disabled,
    Enabled(TraceWriter<W>),
}

impl<W: Write> Recorder<W> {
    pub fn record_packet(&mut self, packet: &[u8]) -> Result<(), TraceWriteError> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled(writer) => writer.write_packet(packet),
        }
    }

    pub fn record_blob(
        &mut self,
        kind: BlobKind,
        data: &[u8],
    ) -> Result<Option<u64>, TraceWriteError> {
        match self {
            Self::Disabled => Ok(None),
            Self::Enabled(writer) => writer.write_blob(kind, data).map(Some),
        }
    }

    pub fn begin_frame(&mut self, frame_index: u32) -> Result<(), TraceWriteError> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled(writer) => writer.begin_frame(frame_index),
        }
    }

    pub fn present(&mut self, frame_index: u32) -> Result<(), TraceWriteError> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled(writer) => writer.present(frame_index),
        }
    }

    pub fn record_aerogpu_submission(
        &mut self,
        submit_flags: u32,
        context_id: u32,
        engine_id: u32,
        signal_fence: u64,
        cmd_stream_bytes: &[u8],
        alloc_table_bytes: Option<&[u8]>,
        memory_ranges: &[AerogpuMemoryRangeCapture<'_>],
    ) -> Result<(), TraceWriteError> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled(writer) => writer.write_aerogpu_submission(
                submit_flags,
                context_id,
                engine_id,
                signal_fence,
                cmd_stream_bytes,
                alloc_table_bytes,
                memory_ranges,
            ),
        }
    }
}
