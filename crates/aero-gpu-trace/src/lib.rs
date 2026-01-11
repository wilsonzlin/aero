//! Aero GPU trace container.
//!
//! This crate intentionally stays dependency-free and focuses on a stable on-disk format:
//! see `docs/abi/gpu-trace-format.md`.

mod format;
mod reader;
mod writer;

pub use format::{
    AerogpuMemoryRangeRef, BlobKind, FrameTocEntry, RecordType, TraceFooter, TraceHeader,
    TraceMeta, TraceToc, CONTAINER_VERSION, TRACE_FOOTER_SIZE, TRACE_HEADER_SIZE,
};
pub use reader::{TraceReadError, TraceReader, TraceRecord};
pub use writer::{AerogpuMemoryRangeCapture, Recorder, TraceWriteError, TraceWriter};
