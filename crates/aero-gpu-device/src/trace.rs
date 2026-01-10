//! GPU trace recorder integration for `aero-gpu-device`.
//!
//! The trace container format lives in `crates/aero-gpu-trace` and is described by
//! `docs/abi/gpu-trace-format.md`.

use aero_gpu_trace::{BlobKind, TraceMeta, TraceWriteError, TraceWriter};

/// In-memory trace recorder for the `aero-gpu-device` command stream.
///
/// This records the *post-processed* command bytes that the replayer understands:
/// - For `WRITE_BUFFER` and `WRITE_TEXTURE2D`, the `src_paddr` field inside the
///   recorded packet is replaced with a `blob_id` (little-endian u64).
/// - The referenced bytes are recorded as `BlobKind::{BufferData,TextureData}`.
///
/// This makes the trace replayable without requiring guest memory.
pub struct GpuTraceRecorder {
    writer: TraceWriter<Vec<u8>>,
    frame_index: u32,
    frame_open: bool,
}

impl core::fmt::Debug for GpuTraceRecorder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GpuTraceRecorder")
            .field("frame_index", &self.frame_index)
            .field("frame_open", &self.frame_open)
            .finish()
    }
}

impl GpuTraceRecorder {
    /// Create a trace recorder that writes into memory (`Vec<u8>`).
    pub fn new_in_memory(
        emulator_version: impl Into<String>,
        command_abi_version: u32,
    ) -> Result<Self, TraceWriteError> {
        let mut meta = TraceMeta::new(emulator_version, command_abi_version);
        meta.notes = Some(
            "aero-gpu-device: recorded WRITE_* commands replace src_paddr with blob_id".to_string(),
        );
        let writer = TraceWriter::new(Vec::<u8>::new(), &meta)?;
        Ok(Self {
            writer,
            frame_index: 0,
            frame_open: false,
        })
    }

    pub fn frame_index(&self) -> u32 {
        self.frame_index
    }

    fn ensure_frame_open(&mut self) -> Result<(), TraceWriteError> {
        if !self.frame_open {
            self.writer.begin_frame(self.frame_index)?;
            self.frame_open = true;
        }
        Ok(())
    }

    /// Record an unmodified packet.
    pub fn record_packet(&mut self, packet_bytes: &[u8]) -> Result<(), TraceWriteError> {
        self.ensure_frame_open()?;
        self.writer.write_packet(packet_bytes)?;
        Ok(())
    }

    /// Record a packet plus an associated buffer upload.
    pub fn record_write_buffer_packet(
        &mut self,
        packet_bytes: &[u8],
        upload_bytes: &[u8],
        src_paddr_offset_in_packet: usize,
    ) -> Result<(), TraceWriteError> {
        self.ensure_frame_open()?;
        let blob_id = self.writer.write_blob(BlobKind::BufferData, upload_bytes)?;

        let mut patched = packet_bytes.to_vec();
        let off = src_paddr_offset_in_packet;
        if patched.len() < off + 8 {
            return Err(TraceWriteError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "packet too small to patch src_paddr",
            )));
        }
        patched[off..off + 8].copy_from_slice(&blob_id.to_le_bytes());
        self.writer.write_packet(&patched)?;
        Ok(())
    }

    /// Record a packet plus an associated texture upload.
    pub fn record_write_texture_packet(
        &mut self,
        packet_bytes: &[u8],
        upload_bytes: &[u8],
        src_paddr_offset_in_packet: usize,
    ) -> Result<(), TraceWriteError> {
        self.ensure_frame_open()?;
        let blob_id = self
            .writer
            .write_blob(BlobKind::TextureData, upload_bytes)?;

        let mut patched = packet_bytes.to_vec();
        let off = src_paddr_offset_in_packet;
        if patched.len() < off + 8 {
            return Err(TraceWriteError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "packet too small to patch src_paddr",
            )));
        }
        patched[off..off + 8].copy_from_slice(&blob_id.to_le_bytes());
        self.writer.write_packet(&patched)?;
        Ok(())
    }

    /// Mark the end of the current frame (after the `PRESENT` packet has been recorded).
    pub fn record_present_marker(&mut self) -> Result<(), TraceWriteError> {
        if !self.frame_open {
            return Ok(());
        }
        self.writer.present(self.frame_index)?;
        self.frame_open = false;
        self.frame_index = self.frame_index.wrapping_add(1);
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u8>, TraceWriteError> {
        Ok(self.writer.finish()?)
    }
}
