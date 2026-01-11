//! Shared memory layout contract for Aero IPC.
//!
//! The intent is that a coordinator (main thread) allocates a single
//! `SharedArrayBuffer` and splits it into one or more ring buffers:
//!
//! - `cmd` queues: coordinator → worker (control + device/MMIO requests)
//! - `evt` queues: worker → coordinator (frame ready, IRQ, logs, panic, ...)
//!
//! This module defines constants and helpers shared by both Rust and TS.

/// `b"AIPC"` as a little-endian `u32`.
pub const IPC_MAGIC: u32 = 0x4350_4941;

/// IPC shared-memory ABI version.
pub const IPC_VERSION: u32 = 1;

/// All ring-buffer records are aligned to this many bytes.
///
/// Alignment must be the same across TS + Rust and should stay a power-of-two.
pub const RECORD_ALIGN: usize = 4;

/// Ring-buffer wrap marker stored in the record length field.
///
/// Using `0xFFFF_FFFF` keeps zero-length payloads legal and makes debugging
/// easier (it shows up as `-1` if viewed through an `Int32Array`).
pub const WRAP_MARKER: u32 = 0xFFFF_FFFF;

/// Control word indices for the ring buffer header when viewed as an `Int32Array`.
///
/// All values are *byte offsets from the start of the ring data region* encoded
/// in a wrapping `u32`.
pub mod ring_ctrl {
    pub const HEAD: usize = 0;
    pub const TAIL_RESERVE: usize = 1;
    pub const TAIL_COMMIT: usize = 2;
    pub const CAPACITY: usize = 3; // non-atomic after initialization
    pub const WORDS: usize = 4;
    pub const BYTES: usize = WORDS * 4;
}

/// Top-level header at the start of an Aero IPC `SharedArrayBuffer`.
///
/// This header is meant to be read by both TS and Rust to discover queue
/// offsets/capacities at runtime.
///
/// Layout (all little-endian `u32`):
/// - magic
/// - version
/// - total_bytes
/// - queue_count
///
/// Followed by `queue_count` queue descriptors.
pub mod ipc_header {
    pub const WORDS: usize = 4;
    pub const BYTES: usize = WORDS * 4;

    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 1;
    pub const TOTAL_BYTES: usize = 2;
    pub const QUEUE_COUNT: usize = 3;
}

/// Queue descriptor entry following the top-level header.
///
/// Layout (all little-endian `u32`):
/// - kind (application-defined; e.g. 0=cmd, 1=evt)
/// - offset_bytes (byte offset from the start of the shared buffer)
/// - capacity_bytes (ring data capacity, excluding the ring header)
/// - reserved
pub mod queue_desc {
    pub const WORDS: usize = 4;
    pub const BYTES: usize = WORDS * 4;

    pub const KIND: usize = 0;
    pub const OFFSET_BYTES: usize = 1;
    pub const CAPACITY_BYTES: usize = 2;
    pub const RESERVED: usize = 3;
}

/// Queue kinds used by the default layout.
pub mod queue_kind {
    pub const CMD: u32 = 0;
    pub const EVT: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueRegion {
    pub kind: u32,
    pub offset_bytes: usize,
    pub capacity_bytes: usize,
}

/// Compute the total byte size for a layout with `queue_count` ring buffers.
///
/// Each ring buffer is laid out as:
/// - `ring_ctrl::BYTES` bytes of control words
/// - `capacity_bytes` bytes of data region
///
/// Callers are expected to choose capacities that are multiples of
/// [`RECORD_ALIGN`].
pub fn total_bytes_for_layout(queues: &[QueueRegion]) -> usize {
    let mut total = ipc_header::BYTES + queues.len() * queue_desc::BYTES;
    for q in queues {
        total = align_up(total, RECORD_ALIGN);
        total += ring_ctrl::BYTES + q.capacity_bytes;
    }
    total
}

pub(crate) const fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}
