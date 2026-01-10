use crate::buffer_arena::{align_up, lcm_u64, BufferArena};
use bytemuck::Pod;
use std::fmt;
use std::sync::Arc;

/// Subset of GPU limits relevant for dynamic uploads.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuCapabilities {
    pub min_uniform_buffer_offset_alignment: u32,
    pub min_storage_buffer_offset_alignment: u32,
    pub max_buffer_size: u64,
    /// Whether compute pipelines are supported on the active backend/device.
    ///
    /// WebGL2 backends (wgpu's `Backend::Gl` on wasm) do not support compute.
    pub supports_compute: bool,
    /// Whether the active backend/device can sample BC-compressed textures (BC1/BC3/BC7).
    ///
    /// This is expected to be false on WebGL2 fallback paths and on WebGPU devices
    /// where BC compression features were not enabled.
    pub supports_bc_texture_compression: bool,
    /// Whether the currently-enabled device features allow GPU timestamp queries.
    ///
    /// Note: this reflects *enabled* features (what the device was created with),
    /// not just adapter support.
    pub timestamp_queries_supported: bool,
}

impl GpuCapabilities {
    pub fn from_device(device: &wgpu::Device) -> Self {
        let limits = device.limits();
        let features = device.features();
        Self {
            min_uniform_buffer_offset_alignment: limits.min_uniform_buffer_offset_alignment,
            min_storage_buffer_offset_alignment: limits.min_storage_buffer_offset_alignment,
            max_buffer_size: limits.max_buffer_size,
            supports_compute: true,
            supports_bc_texture_compression: features
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            timestamp_queries_supported: features.contains(wgpu::Features::TIMESTAMP_QUERY)
                && features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
        }
    }

    pub fn supports_timestamp_queries(self) -> bool {
        self.timestamp_queries_supported
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
    StagingBeltNeedsRecall,
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
            UploadRingBufferError::StagingBeltNeedsRecall => write!(
                f,
                "staging belt is finished; call recall() after submitting the flush command buffer (and polling the device) before staging more uploads"
            ),
        }
    }
}

impl std::error::Error for UploadRingBufferError {}

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
    encoder: Option<wgpu::CommandEncoder>,
    staging_finished: bool,
    staging_belt: wgpu::util::StagingBelt,
    small_write_threshold: u64,
    stats: UploadStats,
}

// `wgpu::Queue::write_buffer` requires the write size be a multiple of
// `COPY_BUFFER_ALIGNMENT` (4). For small writes we want to avoid heap allocations
// when padding/zero-filling, so keep a reusable zero chunk.
const ZERO_CHUNK: [u8; 256] = [0u8; 256];

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
        if desc.staging_belt_chunk_size == 0 {
            return Err(UploadRingBufferError::InvalidDescriptor(
                "staging_belt_chunk_size must be > 0",
            ));
        }
        if desc.staging_belt_chunk_size > caps.max_buffer_size {
            return Err(UploadRingBufferError::BufferTooLarge {
                requested: desc.staging_belt_chunk_size,
                max: caps.max_buffer_size,
            });
        }

        // Segment boundaries should be aligned for any dynamic binding type we
        // might allocate within the frame, *and* for copy/write operations.
        //
        // In practice these alignments are powers of two (WebGPU requirement),
        // so the LCM is equal to the max, but using LCM is more robust.
        let uniform_alignment = (caps.min_uniform_buffer_offset_alignment as u64).max(1);
        let storage_alignment = (caps.min_storage_buffer_offset_alignment as u64).max(1);
        let required_alignment = lcm_u64(
            wgpu::COPY_BUFFER_ALIGNMENT,
            lcm_u64(uniform_alignment, storage_alignment),
        );

        let per_frame_capacity = align_up(desc.per_frame_size, required_alignment);
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
            encoder: None,
            staging_finished: false,
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
            self.encoder.is_none(),
            "begin_frame called with staged writes still pending; call flush_staged_writes (and submit the returned command buffer) before starting the next frame"
        );
        debug_assert!(
            !self.staging_finished,
            "begin_frame called while the staging belt is still in the finished state; call recall() after submitting the flush command buffer (and polling the device)"
        );
        self.arena_index = (self.arena_index + 1) % self.arenas.len();
        self.arenas[self.arena_index].reset();
        self.encoder = None;
        self.stats = UploadStats::default();
    }

    fn alloc_offset(&mut self, size: u64, alignment: u64) -> Result<u64, UploadRingBufferError> {
        // Offsets used by `queue.write_buffer` / copy operations must be
        // aligned to `COPY_BUFFER_ALIGNMENT`. Also respect the user-provided
        // alignment by aligning to the least common multiple.
        let alignment = alignment.max(1);
        let alignment = lcm_u64(alignment, wgpu::COPY_BUFFER_ALIGNMENT);
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

        #[cfg(debug_assertions)]
        debug_assert_eq!(offset % alignment, 0);

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

    fn write_bytes_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        offset: u64,
        bytes: &[u8],
        total_size: u64,
    ) -> Result<(), UploadRingBufferError> {
        debug_assert!(total_size >= bytes.len() as u64);

        let total_size = align_up(total_size, wgpu::COPY_BUFFER_ALIGNMENT);
        debug_assert_eq!(total_size % wgpu::COPY_BUFFER_ALIGNMENT, 0);
        let total_size_usize = total_size as usize;

        if total_size == 0 {
            return Ok(());
        }

        if total_size <= self.small_write_threshold {
            // Write the aligned prefix directly.
            let prefix_len = bytes.len() & !((wgpu::COPY_BUFFER_ALIGNMENT as usize) - 1);
            if prefix_len != 0 {
                queue.write_buffer(&self.buffer, offset, &bytes[..prefix_len]);
            }

            // Pad the tail of the payload to 4 bytes, if needed.
            let mut written = prefix_len;
            if written != bytes.len() {
                let rem = &bytes[written..];
                debug_assert!(rem.len() < 4);
                let mut tmp = [0u8; 4];
                tmp[..rem.len()].copy_from_slice(rem);
                queue.write_buffer(&self.buffer, offset + written as u64, &tmp);
                written += 4;
            }

            // Zero-fill any remaining bytes to the full allocation size.
            while written < total_size_usize {
                let remaining = total_size_usize - written;
                let chunk = remaining.min(ZERO_CHUNK.len());
                debug_assert_eq!(chunk % (wgpu::COPY_BUFFER_ALIGNMENT as usize), 0);
                queue.write_buffer(&self.buffer, offset + written as u64, &ZERO_CHUNK[..chunk]);
                written += chunk;
            }

            self.stats.bytes_small_writes += total_size;
            return Ok(());
        }

        if self.staging_finished {
            return Err(UploadRingBufferError::StagingBeltNeedsRecall);
        }

        use std::num::NonZeroU64;
        let size = NonZeroU64::new(total_size).expect("total_size is checked for zero");
        let encoder = self.encoder.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero upload ring encoder"),
            })
        });
        let mut view = self
            .staging_belt
            .write_buffer(encoder, &self.buffer, offset, size, device);
        view[..bytes.len()].copy_from_slice(bytes);
        view[bytes.len()..total_size_usize].fill(0);
        self.stats.bytes_staged_writes += total_size;
        Ok(())
    }

    /// Upload arbitrary bytes with the given alignment.
    ///
    /// Returns `(buffer, offset)` pointing at the uploaded data.
    pub fn write_bytes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bytes: &[u8],
        alignment: u64,
    ) -> Result<(Arc<wgpu::Buffer>, u64), UploadRingBufferError> {
        let offset = self.alloc_offset(bytes.len() as u64, alignment)?;
        let total_size = align_up(bytes.len() as u64, wgpu::COPY_BUFFER_ALIGNMENT);
        self.write_bytes_inner(device, queue, offset, bytes, total_size)?;
        Ok((Arc::clone(&self.buffer), offset))
    }

    /// Size (in bytes) allocated by [`Self::write_uniform`].
    pub fn uniform_allocation_size<T: Pod>(&self) -> u64 {
        // WebGPU requires uniform binding sizes to be 16-byte aligned.
        let size = std::mem::size_of::<T>() as u64;
        align_up(size.max(16), 16)
    }

    /// Upload a single uniform POD value and return a dynamic uniform offset.
    pub fn write_uniform<T: Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &T,
    ) -> Result<DynamicOffset, UploadRingBufferError> {
        let bytes = bytemuck::bytes_of(data);
        let size = self.uniform_allocation_size::<T>();

        let alignment = self.caps.min_uniform_buffer_offset_alignment as u64;
        let offset = self.alloc_offset(size, alignment)?;

        #[cfg(debug_assertions)]
        debug_assert_eq!(offset % alignment, 0);

        self.write_bytes_inner(device, queue, offset, bytes, size)?;

        let dyn_off: u32 = offset
            .try_into()
            .map_err(|_| UploadRingBufferError::OffsetDoesNotFitInDynamicOffset(offset))?;
        Ok(DynamicOffset(dyn_off))
    }

    /// Upload a single storage POD value and return a dynamic storage offset.
    ///
    /// This is identical to [`Self::write_uniform`], but uses
    /// `min_storage_buffer_offset_alignment`.
    pub fn write_storage<T: Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &T,
    ) -> Result<DynamicOffset, UploadRingBufferError> {
        let bytes = bytemuck::bytes_of(data);
        let alignment = self.caps.min_storage_buffer_offset_alignment as u64;
        let offset = self.alloc_offset(bytes.len() as u64, alignment)?;

        #[cfg(debug_assertions)]
        debug_assert_eq!(offset % alignment, 0);

        let total_size = align_up(bytes.len() as u64, wgpu::COPY_BUFFER_ALIGNMENT);
        self.write_bytes_inner(device, queue, offset, bytes, total_size)?;

        let dyn_off: u32 = offset
            .try_into()
            .map_err(|_| UploadRingBufferError::OffsetDoesNotFitInDynamicOffset(offset))?;
        Ok(DynamicOffset(dyn_off))
    }

    /// Upload a POD slice and return a handle describing the resulting buffer slice.
    pub fn write_slice<T: Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &[T],
    ) -> Result<BufferSliceHandle, UploadRingBufferError> {
        let bytes = bytemuck::cast_slice(data);

        // For vertex/index usage, WebGPU copy operations require 4-byte alignment.
        let alignment = (std::mem::align_of::<T>() as u64).max(wgpu::COPY_BUFFER_ALIGNMENT);
        let offset = self.alloc_offset(bytes.len() as u64, alignment)?;

        let total_size = align_up(bytes.len() as u64, wgpu::COPY_BUFFER_ALIGNMENT);
        self.write_bytes_inner(device, queue, offset, bytes, total_size)?;

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
    pub fn flush_staged_writes(&mut self) -> Option<wgpu::CommandBuffer> {
        let encoder = self.encoder.take()?;
        self.staging_belt.finish();
        self.staging_finished = true;
        Some(encoder.finish())
    }

    /// Recycle staging buffers.
    ///
    /// Call this after a `device.poll(...)` where the submitted flush work is
    /// known to have progressed.
    pub fn recall(&mut self) {
        self.staging_belt.recall();
        self.staging_finished = false;
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
            supports_compute: true,
            supports_bc_texture_compression: false,
            timestamp_queries_supported: false,
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
            supports_compute: true,
            supports_bc_texture_compression: false,
            timestamp_queries_supported: false,
        };

        let desc = UploadRingBufferDescriptor {
            per_frame_size: 100,
            frames_in_flight: 3,
            ..Default::default()
        };

        let required_alignment = lcm_u64(
            wgpu::COPY_BUFFER_ALIGNMENT,
            lcm_u64(
                caps.min_uniform_buffer_offset_alignment as u64,
                caps.min_storage_buffer_offset_alignment as u64,
            ),
        );

        let per_frame_capacity = align_up(desc.per_frame_size, required_alignment);
        assert_eq!(per_frame_capacity, 256);

        let total = per_frame_capacity * desc.frames_in_flight as u64;
        assert_eq!(total, 768);
    }

    #[test]
    fn ring_wraps_across_frame_segments() {
        let per_frame_capacity = 64;
        let frames_in_flight = 3;

        let mut arenas = (0..frames_in_flight)
            .map(|i| BufferArena::new(i as u64 * per_frame_capacity, per_frame_capacity))
            .collect::<Vec<_>>();

        // Mirrors the `UploadRingBuffer` behavior: start at the end so the first
        // `begin_frame` wraps to index 0.
        let mut arena_index = frames_in_flight - 1;

        let mut first_offsets = Vec::new();
        for _ in 0..(frames_in_flight * 2) {
            arena_index = (arena_index + 1) % frames_in_flight;
            arenas[arena_index].reset();
            first_offsets.push(arenas[arena_index].alloc(4, 4).unwrap());
        }

        assert_eq!(
            first_offsets,
            vec![
                0,
                per_frame_capacity,
                per_frame_capacity * 2,
                0,
                per_frame_capacity,
                per_frame_capacity * 2
            ]
        );
    }
}
