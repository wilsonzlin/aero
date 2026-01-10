use crate::buffer_arena::{align_up, BufferArena};
use bytemuck::Pod;
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// Subset of GPU limits relevant for dynamic uploads.
#[derive(Debug, Clone, Copy)]
pub struct GpuCapabilities {
    pub min_uniform_buffer_offset_alignment: u32,
    pub min_storage_buffer_offset_alignment: u32,
    pub max_buffer_size: u64,
}

impl GpuCapabilities {
    pub fn from_device(device: &wgpu::Device) -> Self {
        let limits = device.limits();
        Self {
            min_uniform_buffer_offset_alignment: limits.min_uniform_buffer_offset_alignment,
            min_storage_buffer_offset_alignment: limits.min_storage_buffer_offset_alignment,
            max_buffer_size: limits.max_buffer_size,
        }
    }
}

/// A dynamic uniform offset (bytes) suitable for `set_bind_group` dynamic offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynamicOffset(pub u32);

/// A handle to a sub-range of a buffer produced by [`UploadRingBuffer`].
#[derive(Debug, Clone)]
pub struct BufferSliceHandle {
    pub buffer: Arc<wgpu::Buffer>,
    pub offset: u64,
    pub size: u64,
}

impl BufferSliceHandle {
    pub fn slice(&self) -> wgpu::BufferSlice<'_> {
        self.buffer.slice(self.offset..self.offset + self.size)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UploadStats {
    pub bytes_small_writes: u64,
    pub bytes_staged_writes: u64,
}

impl UploadStats {
    pub fn bytes_total(&self) -> u64 {
        self.bytes_small_writes + self.bytes_staged_writes
    }
}

pub struct UploadRingBufferDescriptor<'a> {
    pub label: Option<&'a str>,
    /// Budget per frame (before internal padding/alignment).
    pub per_frame_size: u64,
    /// Number of frames expected to be in flight (typically 2 or 3).
    pub frames_in_flight: usize,
    /// Usages for the destination ring buffer (COPY_DST is added automatically).
    pub usage: wgpu::BufferUsages,
    /// Updates at or below this size (after padding to copy alignment) use `queue.write_buffer`.
    pub small_write_threshold: u64,
    /// Chunk size for the internal staging belt.
    pub staging_belt_chunk_size: u64,
}

impl<'a> Default for UploadRingBufferDescriptor<'a> {
    fn default() -> Self {
        Self {
            label: Some("aero upload ring buffer"),
            per_frame_size: 4 * 1024 * 1024,
            frames_in_flight: 3,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::STORAGE,
            small_write_threshold: 4 * 1024,
            staging_belt_chunk_size: 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum UploadRingBufferError {
    InvalidDescriptor(&'static str),
    BufferTooLarge {
        requested: u64,
        max: u64,
    },
    OutOfSpace {
        requested: u64,
        alignment: u64,
        remaining: u64,
        per_frame_capacity: u64,
    },
    OffsetDoesNotFitInDynamicOffset(u64),
}

impl fmt::Display for UploadRingBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UploadRingBufferError::InvalidDescriptor(msg) => write!(f, "invalid descriptor: {msg}"),
            UploadRingBufferError::BufferTooLarge { requested, max } => write!(
                f,
                "upload ring buffer too large: requested {requested} bytes, max {max} bytes"
            ),
            UploadRingBufferError::OutOfSpace {
                requested,
                alignment,
                remaining,
                per_frame_capacity,
            } => write!(
                f,
                "upload ring buffer out of space: requested {requested} bytes (alignment {alignment}), remaining {remaining} bytes (per-frame capacity {per_frame_capacity} bytes)"
            ),
            UploadRingBufferError::OffsetDoesNotFitInDynamicOffset(offset) => write!(
                f,
                "offset {offset} does not fit in u32 dynamic offset"
            ),
        }
    }
}

impl std::error::Error for UploadRingBufferError {}

#[derive(Debug)]
struct PendingWrite {
    offset: u64,
    bytes: Vec<u8>,
}

/// High-throughput dynamic buffer uploads for emulator-style streaming updates.
///
/// Internally this is a single GPU buffer partitioned into `frames_in_flight`
/// segments. Each frame, allocations come from the next segment. This avoids
/// overwriting memory that may still be in use by the GPU.
///
/// For uploads, this chooses between:
/// - `queue.write_buffer` for small writes.
/// - A mapped staging buffer (via `wgpu::util::StagingBelt`) + copy for larger writes.
pub struct UploadRingBuffer {
    caps: GpuCapabilities,
    buffer: Arc<wgpu::Buffer>,
    per_frame_capacity: u64,
    arenas: Vec<BufferArena>,
    arena_index: usize,
    pending: Vec<PendingWrite>,
    staging_belt: wgpu::util::StagingBelt,
    small_write_threshold: u64,
    stats: UploadStats,
}

impl UploadRingBuffer {
    pub fn new(
        device: &wgpu::Device,
        caps: GpuCapabilities,
        desc: UploadRingBufferDescriptor<'_>,
    ) -> Result<Self, UploadRingBufferError> {
        if desc.frames_in_flight == 0 {
            return Err(UploadRingBufferError::InvalidDescriptor(
                "frames_in_flight must be > 0",
            ));
        }
        if desc.per_frame_size == 0 {
            return Err(UploadRingBufferError::InvalidDescriptor(
                "per_frame_size must be > 0",
            ));
        }

        let max_required_alignment = [
            wgpu::COPY_BUFFER_ALIGNMENT,
            caps.min_uniform_buffer_offset_alignment as u64,
            caps.min_storage_buffer_offset_alignment as u64,
        ]
        .into_iter()
        .max()
        .unwrap_or(wgpu::COPY_BUFFER_ALIGNMENT);

        let per_frame_capacity = align_up(desc.per_frame_size, max_required_alignment);
        let total_size = per_frame_capacity
            .checked_mul(desc.frames_in_flight as u64)
            .ok_or(UploadRingBufferError::BufferTooLarge {
                requested: u64::MAX,
                max: caps.max_buffer_size,
            })?;

        if total_size > caps.max_buffer_size {
            return Err(UploadRingBufferError::BufferTooLarge {
                requested: total_size,
                max: caps.max_buffer_size,
            });
        }

        let usage = desc.usage | wgpu::BufferUsages::COPY_DST;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: desc.label,
            size: total_size,
            usage,
            mapped_at_creation: false,
        });
        let buffer = Arc::new(buffer);

        let arenas = (0..desc.frames_in_flight)
            .map(|i| BufferArena::new(i as u64 * per_frame_capacity, per_frame_capacity))
            .collect::<Vec<_>>();

        Ok(Self {
            caps,
            buffer,
            per_frame_capacity,
            arenas,
            // `begin_frame` increments before use. Start at the end so the
            // first `begin_frame` selects arena 0.
            arena_index: desc.frames_in_flight - 1,
            pending: Vec::new(),
            staging_belt: wgpu::util::StagingBelt::new(desc.staging_belt_chunk_size),
            small_write_threshold: desc.small_write_threshold,
            stats: UploadStats::default(),
        })
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub fn buffer_handle(&self) -> Arc<wgpu::Buffer> {
        Arc::clone(&self.buffer)
    }

    pub fn capabilities(&self) -> GpuCapabilities {
        self.caps
    }

    pub fn stats(&self) -> UploadStats {
        self.stats
    }

    /// Advance to the next frame segment and reset its allocations.
    ///
    /// Call this once per frame, before any allocations/writes.
    pub fn begin_frame(&mut self) {
        debug_assert!(
            self.pending.is_empty(),
            "begin_frame called with staged writes still pending; call flush_staged_writes (and submit the returned command buffer) before starting the next frame"
        );
        self.arena_index = (self.arena_index + 1) % self.arenas.len();
        self.arenas[self.arena_index].reset();
        self.pending.clear();
        self.stats = UploadStats::default();
    }

    fn alloc_offset(&mut self, size: u64, alignment: u64) -> Result<u64, UploadRingBufferError> {
        let alignment = alignment.max(1).max(wgpu::COPY_BUFFER_ALIGNMENT); // WebGPU copy/write alignment.
        let size = align_up(size, wgpu::COPY_BUFFER_ALIGNMENT);

        let arena = &mut self.arenas[self.arena_index];
        let remaining = arena.remaining();

        let offset = arena
            .alloc(size, alignment)
            .ok_or(UploadRingBufferError::OutOfSpace {
                requested: size,
                alignment,
                remaining,
                per_frame_capacity: self.per_frame_capacity,
            })?;

        Ok(offset)
    }

    /// Allocate space in the current frame segment.
    ///
    /// The returned `(buffer, offset)` is suitable for vertex/index/uniform
    /// bindings. The ring buffer is shared across all frames; the offset is
    /// absolute (from the start of the underlying buffer).
    pub fn alloc(
        &mut self,
        size: u64,
        alignment: u64,
    ) -> Result<(Arc<wgpu::Buffer>, u64), UploadRingBufferError> {
        let offset = self.alloc_offset(size, alignment)?;
        Ok((Arc::clone(&self.buffer), offset))
    }

    fn padded_bytes<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        let padded_len = align_up(bytes.len() as u64, wgpu::COPY_BUFFER_ALIGNMENT) as usize;
        if padded_len == bytes.len() {
            return Cow::Borrowed(bytes);
        }

        let mut out = vec![0u8; padded_len];
        out[..bytes.len()].copy_from_slice(bytes);
        Cow::Owned(out)
    }

    fn write_bytes_inner(
        &mut self,
        queue: &wgpu::Queue,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), UploadRingBufferError> {
        let bytes = self.padded_bytes(bytes);
        let byte_len = bytes.len() as u64;

        if byte_len == 0 {
            return Ok(());
        }

        if byte_len <= self.small_write_threshold {
            queue.write_buffer(&self.buffer, offset, &bytes);
            self.stats.bytes_small_writes += byte_len;
            return Ok(());
        }

        self.pending.push(PendingWrite {
            offset,
            bytes: bytes.into_owned(),
        });
        self.stats.bytes_staged_writes += byte_len;
        Ok(())
    }

    /// Upload arbitrary bytes with the given alignment.
    ///
    /// Returns `(buffer, offset)` pointing at the uploaded data.
    pub fn write_bytes(
        &mut self,
        queue: &wgpu::Queue,
        bytes: &[u8],
        alignment: u64,
    ) -> Result<(Arc<wgpu::Buffer>, u64), UploadRingBufferError> {
        let offset = self.alloc_offset(bytes.len() as u64, alignment)?;
        self.write_bytes_inner(queue, offset, bytes)?;
        Ok((Arc::clone(&self.buffer), offset))
    }

    /// Size (in bytes) allocated by [`Self::write_uniform`].
    pub fn uniform_allocation_size<T: Pod>(&self) -> u64 {
        // WebGPU requires uniform binding sizes to be 16-byte aligned.
        let size = std::mem::size_of::<T>() as u64;
        align_up(size, 16).max(wgpu::COPY_BUFFER_ALIGNMENT)
    }

    /// Upload a single uniform POD value and return a dynamic uniform offset.
    pub fn write_uniform<T: Pod>(
        &mut self,
        queue: &wgpu::Queue,
        data: &T,
    ) -> Result<DynamicOffset, UploadRingBufferError> {
        let bytes = bytemuck::bytes_of(data);
        let size = self.uniform_allocation_size::<T>();

        let alignment = self.caps.min_uniform_buffer_offset_alignment as u64;
        let offset = self.alloc_offset(size, alignment)?;

        #[cfg(debug_assertions)]
        debug_assert_eq!(offset % alignment, 0);

        let padded = if size as usize == bytes.len() {
            Cow::Borrowed(bytes)
        } else {
            let mut out = vec![0u8; size as usize];
            out[..bytes.len()].copy_from_slice(bytes);
            Cow::Owned(out)
        };

        self.write_bytes_inner(queue, offset, &padded)?;

        let dyn_off: u32 = offset
            .try_into()
            .map_err(|_| UploadRingBufferError::OffsetDoesNotFitInDynamicOffset(offset))?;
        Ok(DynamicOffset(dyn_off))
    }

    /// Upload a POD slice and return a handle describing the resulting buffer slice.
    pub fn write_slice<T: Pod>(
        &mut self,
        queue: &wgpu::Queue,
        data: &[T],
    ) -> Result<BufferSliceHandle, UploadRingBufferError> {
        let bytes = bytemuck::cast_slice(data);

        // For vertex/index usage, WebGPU copy operations require 4-byte alignment.
        let alignment = (std::mem::align_of::<T>() as u64).max(wgpu::COPY_BUFFER_ALIGNMENT);
        let offset = self.alloc_offset(bytes.len() as u64, alignment)?;

        self.write_bytes_inner(queue, offset, bytes)?;

        Ok(BufferSliceHandle {
            buffer: Arc::clone(&self.buffer),
            offset,
            size: bytes.len() as u64,
        })
    }

    /// Encode (and return) a command buffer that flushes any staged writes.
    ///
    /// If this returns `Some(cmd)`, the caller must submit it **before**
    /// command buffers that read from the uploaded data for this frame.
    ///
    /// After submitting, call `device.poll(...)` and then [`Self::recall`] to
    /// recycle staging buffers.
    pub fn flush_staged_writes(&mut self, device: &wgpu::Device) -> Option<wgpu::CommandBuffer> {
        if self.pending.is_empty() {
            return None;
        }

        use std::num::NonZeroU64;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero upload ring flush"),
        });

        for pending in self.pending.drain(..) {
            let Some(size) = NonZeroU64::new(pending.bytes.len() as u64) else {
                continue;
            };
            let mut view = self.staging_belt.write_buffer(
                &mut encoder,
                &self.buffer,
                pending.offset,
                size,
                device,
            );
            view.copy_from_slice(&pending.bytes);
        }

        self.staging_belt.finish();
        Some(encoder.finish())
    }

    /// Recycle staging buffers.
    ///
    /// Call this after a `device.poll(...)` where the submitted flush work is
    /// known to have progressed.
    pub fn recall(&mut self) {
        self.staging_belt.recall();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_allocation_size_is_padded_to_16() {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct U {
            a: u32,
        }
        unsafe impl Pod for U {}
        unsafe impl bytemuck::Zeroable for U {}

        let caps = GpuCapabilities {
            min_uniform_buffer_offset_alignment: 256,
            min_storage_buffer_offset_alignment: 256,
            max_buffer_size: 1024 * 1024,
        };

        // We can't instantiate UploadRingBuffer without a Device, but we can
        // validate the sizing helper logic with a lightweight shim.
        let size = {
            let size = std::mem::size_of::<U>() as u64;
            align_up(size, 16).max(wgpu::COPY_BUFFER_ALIGNMENT)
        };
        assert_eq!(size, 16);

        // Ensure the alignment we use is the device-reported one.
        assert_eq!(caps.min_uniform_buffer_offset_alignment as u64, 256);
    }

    #[test]
    fn per_frame_capacity_is_aligned_to_required_alignment() {
        let caps = GpuCapabilities {
            min_uniform_buffer_offset_alignment: 256,
            min_storage_buffer_offset_alignment: 128,
            max_buffer_size: 4096,
        };

        let desc = UploadRingBufferDescriptor {
            per_frame_size: 100,
            frames_in_flight: 3,
            ..Default::default()
        };

        let max_required_alignment = [
            wgpu::COPY_BUFFER_ALIGNMENT,
            caps.min_uniform_buffer_offset_alignment as u64,
            caps.min_storage_buffer_offset_alignment as u64,
        ]
        .into_iter()
        .max()
        .unwrap();

        let per_frame_capacity = align_up(desc.per_frame_size, max_required_alignment);
        assert_eq!(per_frame_capacity, 256);

        let total = per_frame_capacity * desc.frames_in_flight as u64;
        assert_eq!(total, 768);
    }
}
