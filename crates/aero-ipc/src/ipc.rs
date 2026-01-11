//! Parser and builder for the top-level Aero IPC `SharedArrayBuffer` layout.
//!
//! TypeScript owns the canonical implementation (`web/src/ipc/ipc.ts`). This
//! module mirrors it so Rust/WASM can locate ring queues by `kind` without
//! hard-coded offsets.

use crate::layout::{
    align_up, ipc_header, queue_desc, ring_ctrl, IPC_MAGIC, IPC_VERSION, RECORD_ALIGN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcQueueInfo {
    pub kind: u32,
    pub offset_bytes: usize,
    pub capacity_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcLayout {
    pub total_bytes: usize,
    pub queues: Vec<IpcQueueInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcLayoutError {
    BufferTooSmallForHeader {
        actual_bytes: usize,
    },
    BadMagic {
        expected: u32,
        got: u32,
    },
    UnsupportedVersion {
        expected: u32,
        got: u32,
    },
    LengthMismatch {
        header_total_bytes: u32,
        actual_bytes: usize,
    },
    DescriptorRegionOverflow {
        queue_count: u32,
    },
    BufferTooSmallForDescriptors {
        required_bytes: usize,
        actual_bytes: usize,
    },
    QueueReservedNotZero {
        index: usize,
        reserved: u32,
    },
    QueueOffsetMisaligned {
        index: usize,
        offset_bytes: u32,
        align: usize,
    },
    QueueCapacityMisaligned {
        index: usize,
        capacity_bytes: u32,
        align: usize,
    },
    QueueOutOfBounds {
        index: usize,
        offset_bytes: u32,
        capacity_bytes: u32,
        buffer_len: usize,
    },
    RingHeaderCapacityMismatch {
        index: usize,
        descriptor_capacity_bytes: u32,
        ring_header_capacity_bytes: u32,
    },
    UnexpectedEof {
        offset: usize,
        needed: usize,
        actual_bytes: usize,
    },
}

impl core::fmt::Display for IpcLayoutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IpcLayoutError::BufferTooSmallForHeader { .. } => {
                write!(f, "buffer too small for IPC header")
            }
            IpcLayoutError::BadMagic { expected, got } => write!(
                f,
                "bad IPC magic (expected 0x{expected:08x}, got 0x{got:08x})"
            ),
            IpcLayoutError::UnsupportedVersion { expected, got } => write!(
                f,
                "unsupported IPC version {got} (expected {expected})"
            ),
            IpcLayoutError::LengthMismatch {
                header_total_bytes,
                actual_bytes,
            } => write!(
                f,
                "buffer length mismatch (header={header_total_bytes} actual={actual_bytes})"
            ),
            IpcLayoutError::DescriptorRegionOverflow { queue_count } => {
                write!(f, "queue descriptor region overflows (queue_count={queue_count})")
            }
            IpcLayoutError::BufferTooSmallForDescriptors {
                required_bytes,
                actual_bytes,
            } => write!(
                f,
                "buffer too small for queue descriptors (required={required_bytes} actual={actual_bytes})"
            ),
            IpcLayoutError::QueueReservedNotZero { index, .. } => {
                write!(f, "queue descriptor {index} reserved field must be 0")
            }
            IpcLayoutError::QueueOffsetMisaligned {
                index,
                offset_bytes,
                align,
            } => write!(
                f,
                "queue[{index}].offset_bytes must be aligned to {align} bytes (got {offset_bytes})"
            ),
            IpcLayoutError::QueueCapacityMisaligned {
                index,
                capacity_bytes,
                align,
            } => write!(
                f,
                "queue[{index}].capacity_bytes must be aligned to {align} bytes (got {capacity_bytes})"
            ),
            IpcLayoutError::QueueOutOfBounds { index, .. } => {
                write!(f, "queue descriptor {index} out of bounds")
            }
            IpcLayoutError::RingHeaderCapacityMismatch {
                index,
                descriptor_capacity_bytes,
                ring_header_capacity_bytes,
            } => write!(
                f,
                "queue descriptor {index} capacity mismatch (desc={descriptor_capacity_bytes} ringHeader={ring_header_capacity_bytes})"
            ),
            IpcLayoutError::UnexpectedEof {
                offset,
                needed,
                actual_bytes,
            } => write!(
                f,
                "unexpected end of buffer (offset={offset} needed={needed} actual={actual_bytes})"
            ),
        }
    }
}

impl std::error::Error for IpcLayoutError {}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, IpcLayoutError> {
    let end = offset.checked_add(4).ok_or(IpcLayoutError::UnexpectedEof {
        offset,
        needed: 4,
        actual_bytes: bytes.len(),
    })?;
    let slice = bytes
        .get(offset..end)
        .ok_or(IpcLayoutError::UnexpectedEof {
            offset,
            needed: 4,
            actual_bytes: bytes.len(),
        })?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn write_u32_le(bytes: &mut [u8], offset: usize, v: u32) {
    bytes[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn parse_ipc_buffer(bytes: &[u8]) -> Result<IpcLayout, IpcLayoutError> {
    if bytes.len() < ipc_header::BYTES {
        return Err(IpcLayoutError::BufferTooSmallForHeader {
            actual_bytes: bytes.len(),
        });
    }

    let magic = read_u32_le(bytes, ipc_header::MAGIC * 4)?;
    if magic != IPC_MAGIC {
        return Err(IpcLayoutError::BadMagic {
            expected: IPC_MAGIC,
            got: magic,
        });
    }

    let version = read_u32_le(bytes, ipc_header::VERSION * 4)?;
    if version != IPC_VERSION {
        return Err(IpcLayoutError::UnsupportedVersion {
            expected: IPC_VERSION,
            got: version,
        });
    }

    let total_bytes = read_u32_le(bytes, ipc_header::TOTAL_BYTES * 4)?;
    if total_bytes as usize != bytes.len() {
        return Err(IpcLayoutError::LengthMismatch {
            header_total_bytes: total_bytes,
            actual_bytes: bytes.len(),
        });
    }

    let queue_count = read_u32_le(bytes, ipc_header::QUEUE_COUNT * 4)?;
    let queue_count_usize = queue_count as usize;

    let desc_bytes = ipc_header::BYTES
        .checked_add(
            queue_count_usize
                .checked_mul(queue_desc::BYTES)
                .ok_or(IpcLayoutError::DescriptorRegionOverflow { queue_count })?,
        )
        .ok_or(IpcLayoutError::DescriptorRegionOverflow { queue_count })?;

    if bytes.len() < desc_bytes {
        return Err(IpcLayoutError::BufferTooSmallForDescriptors {
            required_bytes: desc_bytes,
            actual_bytes: bytes.len(),
        });
    }

    let mut queues = Vec::with_capacity(queue_count_usize);
    for i in 0..queue_count_usize {
        let base = ipc_header::BYTES + i * queue_desc::BYTES;
        let kind = read_u32_le(bytes, base + queue_desc::KIND * 4)?;
        let offset_bytes = read_u32_le(bytes, base + queue_desc::OFFSET_BYTES * 4)?;
        let capacity_bytes = read_u32_le(bytes, base + queue_desc::CAPACITY_BYTES * 4)?;
        let reserved = read_u32_le(bytes, base + queue_desc::RESERVED * 4)?;

        if reserved != 0 {
            return Err(IpcLayoutError::QueueReservedNotZero { index: i, reserved });
        }

        if (offset_bytes as usize) % RECORD_ALIGN != 0 {
            return Err(IpcLayoutError::QueueOffsetMisaligned {
                index: i,
                offset_bytes,
                align: RECORD_ALIGN,
            });
        }
        if (capacity_bytes as usize) % RECORD_ALIGN != 0 {
            return Err(IpcLayoutError::QueueCapacityMisaligned {
                index: i,
                capacity_bytes,
                align: RECORD_ALIGN,
            });
        }

        let region_end = (offset_bytes as usize)
            .checked_add(ring_ctrl::BYTES)
            .and_then(|v| v.checked_add(capacity_bytes as usize))
            .ok_or(IpcLayoutError::QueueOutOfBounds {
                index: i,
                offset_bytes,
                capacity_bytes,
                buffer_len: bytes.len(),
            })?;
        if region_end > bytes.len() {
            return Err(IpcLayoutError::QueueOutOfBounds {
                index: i,
                offset_bytes,
                capacity_bytes,
                buffer_len: bytes.len(),
            });
        }

        let ring_header_cap = read_u32_le(bytes, offset_bytes as usize + ring_ctrl::CAPACITY * 4)?;
        if ring_header_cap != capacity_bytes {
            return Err(IpcLayoutError::RingHeaderCapacityMismatch {
                index: i,
                descriptor_capacity_bytes: capacity_bytes,
                ring_header_capacity_bytes: ring_header_cap,
            });
        }

        queues.push(IpcQueueInfo {
            kind,
            offset_bytes: offset_bytes as usize,
            capacity_bytes: capacity_bytes as usize,
        });
    }

    Ok(IpcLayout {
        total_bytes: bytes.len(),
        queues,
    })
}

pub fn find_queue_by_kind(layout: &IpcLayout, kind: u32, nth: usize) -> Option<IpcQueueInfo> {
    let mut seen = 0usize;
    for q in &layout.queues {
        if q.kind != kind {
            continue;
        }
        if seen == nth {
            return Some(*q);
        }
        seen += 1;
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcQueueSpec {
    pub kind: u32,
    pub capacity_bytes: usize,
}

pub fn create_ipc_buffer(specs: &[IpcQueueSpec]) -> Vec<u8> {
    assert!(
        specs.len() <= u32::MAX as usize,
        "queue_count exceeds u32::MAX"
    );
    let queue_count = specs.len() as u32;

    let mut offset = ipc_header::BYTES + specs.len() * queue_desc::BYTES;
    let mut queues: Vec<IpcQueueInfo> = Vec::with_capacity(specs.len());

    for spec in specs {
        assert_eq!(
            spec.capacity_bytes % RECORD_ALIGN,
            0,
            "queue.capacity_bytes must be aligned to {RECORD_ALIGN}"
        );
        assert!(
            spec.capacity_bytes <= u32::MAX as usize,
            "queue.capacity_bytes exceeds u32::MAX"
        );

        offset = align_up(offset, RECORD_ALIGN);
        let ring_offset = offset;
        offset += ring_ctrl::BYTES + spec.capacity_bytes;

        queues.push(IpcQueueInfo {
            kind: spec.kind,
            offset_bytes: ring_offset,
            capacity_bytes: spec.capacity_bytes,
        });
    }

    assert!(offset <= u32::MAX as usize, "total_bytes exceeds u32::MAX");

    let mut bytes = vec![0u8; offset];
    write_u32_le(&mut bytes, ipc_header::MAGIC * 4, IPC_MAGIC);
    write_u32_le(&mut bytes, ipc_header::VERSION * 4, IPC_VERSION);
    write_u32_le(&mut bytes, ipc_header::TOTAL_BYTES * 4, offset as u32);
    write_u32_le(&mut bytes, ipc_header::QUEUE_COUNT * 4, queue_count);

    for (i, q) in queues.iter().enumerate() {
        let base = ipc_header::BYTES + i * queue_desc::BYTES;
        write_u32_le(&mut bytes, base + queue_desc::KIND * 4, q.kind);
        write_u32_le(
            &mut bytes,
            base + queue_desc::OFFSET_BYTES * 4,
            q.offset_bytes as u32,
        );
        write_u32_le(
            &mut bytes,
            base + queue_desc::CAPACITY_BYTES * 4,
            q.capacity_bytes as u32,
        );
        write_u32_le(&mut bytes, base + queue_desc::RESERVED * 4, 0);

        // Initialize ring header to [0, 0, 0, capacity].
        let ring_base = q.offset_bytes;
        write_u32_le(&mut bytes, ring_base + ring_ctrl::HEAD * 4, 0);
        write_u32_le(&mut bytes, ring_base + ring_ctrl::TAIL_RESERVE * 4, 0);
        write_u32_le(&mut bytes, ring_base + ring_ctrl::TAIL_COMMIT * 4, 0);
        write_u32_le(
            &mut bytes,
            ring_base + ring_ctrl::CAPACITY * 4,
            q.capacity_bytes as u32,
        );
    }

    bytes
}
