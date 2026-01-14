use std::fmt;
use std::sync::Arc;

use aero_gpu::BufferArena;

use super::indirect_args::{DrawIndexedIndirectArgs, DrawIndirectArgs};

/// Configuration for [`ExpansionScratchAllocator`].
#[derive(Debug, Clone, Copy)]
pub struct ExpansionScratchDescriptor {
    /// Debug label for the underlying wgpu buffer.
    pub label: Option<&'static str>,
    /// Number of frame segments in the ring.
    ///
    /// This should generally match (or exceed) the number of frames that can be in flight on the
    /// GPU (typically 2-3).
    pub frames_in_flight: usize,
    /// Requested size of each frame segment, in bytes.
    ///
    /// The allocator may round this up to satisfy alignment requirements.
    pub per_frame_size: u64,
    /// Buffer usage flags for the backing buffer.
    pub usage: wgpu::BufferUsages,
}

impl Default for ExpansionScratchDescriptor {
    fn default() -> Self {
        Self {
            label: Some("aero-d3d11 expansion scratch"),
            frames_in_flight: 3,
            // Keep the default small since most command streams do not (yet) use GS/HS/DS emulation.
            // The allocator will grow on demand.
            per_frame_size: 1024 * 1024, // 1 MiB
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        }
    }
}

/// A scratch allocation returned by [`ExpansionScratchAllocator`].
#[derive(Debug, Clone)]
pub struct ExpansionScratchAlloc {
    pub buffer: Arc<wgpu::Buffer>,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub enum ExpansionScratchError {
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
}

impl fmt::Display for ExpansionScratchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpansionScratchError::InvalidDescriptor(msg) => write!(f, "invalid descriptor: {msg}"),
            ExpansionScratchError::BufferTooLarge { requested, max } => {
                write!(
                    f,
                    "scratch buffer too large (requested={requested} max_buffer_size={max})"
                )
            }
            ExpansionScratchError::OutOfSpace {
                requested,
                alignment,
                remaining,
                per_frame_capacity,
            } => write!(
                f,
                "scratch arena out of space (requested={requested} alignment={alignment} remaining={remaining} per_frame_capacity={per_frame_capacity})"
            ),
        }
    }
}

impl std::error::Error for ExpansionScratchError {}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

fn lcm_u64(a: u64, b: u64) -> u64 {
    debug_assert!(a > 0);
    debug_assert!(b > 0);
    let g = gcd_u64(a, b);
    (a / g).saturating_mul(b)
}

fn align_up(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment > 0);
    // Handle overflow defensively.
    let add = alignment - 1;
    match value.checked_add(add) {
        Some(v) => v / alignment * alignment,
        None => u64::MAX / alignment * alignment,
    }
}

fn align_down(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment > 0);
    value / alignment * alignment
}

#[derive(Debug, Clone)]
struct SegmentedArena {
    per_frame_capacity: u64,
    arenas: Vec<BufferArena>,
    arena_index: usize,
}

impl SegmentedArena {
    fn new(frames_in_flight: usize, per_frame_capacity: u64, arena_index: usize) -> Self {
        let arenas = (0..frames_in_flight)
            .map(|i| BufferArena::new(i as u64 * per_frame_capacity, per_frame_capacity))
            .collect();
        Self {
            per_frame_capacity,
            arenas,
            arena_index,
        }
    }

    fn begin_frame(&mut self) {
        self.arena_index = (self.arena_index + 1) % self.arenas.len();
        self.arenas[self.arena_index].reset();
    }

    fn alloc(&mut self, size: u64, alignment: u64) -> Option<u64> {
        self.arenas[self.arena_index].alloc(size, alignment)
    }

    fn remaining(&self) -> u64 {
        self.arenas[self.arena_index].remaining()
    }

    fn arena_index(&self) -> usize {
        self.arena_index
    }
}

#[derive(Debug, Clone)]
struct ScratchState {
    buffer: Arc<wgpu::Buffer>,
    max_buffer_size: u64,
    storage_alignment: u64,
    required_alignment: u64,
    arenas: SegmentedArena,
}

/// Per-frame transient GPU scratch for compute-generated geometry/tessellation expansion.
///
/// This is a single `wgpu::Buffer` partitioned into `frames_in_flight` non-overlapping segments.
/// Call [`ExpansionScratchAllocator::begin_frame`] at a natural frame boundary (e.g. `PRESENT`,
/// `FLUSH`) to advance to the next segment.
#[derive(Debug, Clone)]
pub struct ExpansionScratchAllocator {
    desc: ExpansionScratchDescriptor,
    arena_index: usize,
    state: Option<ScratchState>,
}

impl ExpansionScratchAllocator {
    pub fn new(desc: ExpansionScratchDescriptor) -> Self {
        let frames_in_flight = desc.frames_in_flight.max(1);
        Self {
            desc: ExpansionScratchDescriptor {
                frames_in_flight,
                ..desc
            },
            arena_index: 0,
            state: None,
        }
    }

    pub fn reset(&mut self) {
        self.arena_index = 0;
        self.state = None;
    }

    pub fn begin_frame(&mut self) {
        self.arena_index = (self.arena_index + 1) % self.desc.frames_in_flight;
        if let Some(state) = self.state.as_mut() {
            state.arenas.begin_frame();
            debug_assert_eq!(
                state.arenas.arena_index(),
                self.arena_index,
                "segmented arena index must track allocator index"
            );
        }
    }

    /// Returns the backing buffer size (per frame segment) once initialized.
    pub fn per_frame_capacity(&self) -> Option<u64> {
        self.state.as_ref().map(|s| s.arenas.per_frame_capacity)
    }

    /// Ensure the scratch allocator is initialized.
    ///
    /// This is a lightweight operation after the first call, and does not consume any space from
    /// the current frame segment.
    pub fn init(&mut self, device: &wgpu::Device) -> Result<(), ExpansionScratchError> {
        self.ensure_init(device)
    }

    pub fn frames_in_flight(&self) -> usize {
        self.desc.frames_in_flight
    }

    fn ensure_init(&mut self, device: &wgpu::Device) -> Result<(), ExpansionScratchError> {
        if self.state.is_some() {
            return Ok(());
        }

        if self.desc.per_frame_size == 0 {
            return Err(ExpansionScratchError::InvalidDescriptor(
                "per_frame_size must be > 0",
            ));
        }

        let max_buffer_size = device.limits().max_buffer_size;
        let storage_alignment = (device.limits().min_storage_buffer_offset_alignment as u64).max(1);
        let uniform_alignment = (device.limits().min_uniform_buffer_offset_alignment as u64).max(1);

        // Segment boundaries must be aligned so offsets are valid for copy operations, and so
        // callers can bind subranges as storage/uniform buffers if needed.
        //
        // WebGPU requires these alignments to be powers of two, but use LCM to keep this robust.
        let required_alignment = lcm_u64(
            wgpu::COPY_BUFFER_ALIGNMENT,
            lcm_u64(lcm_u64(storage_alignment, uniform_alignment), 16),
        );

        let mut per_frame_capacity =
            align_up(self.desc.per_frame_size, required_alignment).max(required_alignment);

        let frames_u64 = self.desc.frames_in_flight as u64;
        let max_per_frame = max_buffer_size.checked_div(frames_u64).unwrap_or(0);
        let max_per_frame = align_down(max_per_frame, required_alignment);
        if max_per_frame < required_alignment {
            return Err(ExpansionScratchError::BufferTooLarge {
                requested: required_alignment.saturating_mul(frames_u64),
                max: max_buffer_size,
            });
        }
        per_frame_capacity = per_frame_capacity.min(max_per_frame);

        let total_size = per_frame_capacity.checked_mul(frames_u64).ok_or(
            ExpansionScratchError::BufferTooLarge {
                requested: u64::MAX,
                max: max_buffer_size,
            },
        )?;
        if total_size > max_buffer_size {
            return Err(ExpansionScratchError::BufferTooLarge {
                requested: total_size,
                max: max_buffer_size,
            });
        }

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: self.desc.label,
            size: total_size,
            usage: self.desc.usage,
            mapped_at_creation: false,
        });
        let buffer = Arc::new(buffer);

        let arenas = SegmentedArena::new(
            self.desc.frames_in_flight,
            per_frame_capacity,
            self.arena_index,
        );
        self.state = Some(ScratchState {
            buffer,
            max_buffer_size,
            storage_alignment,
            required_alignment,
            arenas,
        });
        Ok(())
    }

    fn realloc_buffer(
        &mut self,
        device: &wgpu::Device,
        new_per_frame_capacity: u64,
    ) -> Result<(), ExpansionScratchError> {
        let Some(state) = self.state.as_mut() else {
            return Err(ExpansionScratchError::InvalidDescriptor(
                "internal error: realloc called before init",
            ));
        };

        let frames_u64 = self.desc.frames_in_flight as u64;
        let total_size = new_per_frame_capacity.checked_mul(frames_u64).ok_or(
            ExpansionScratchError::BufferTooLarge {
                requested: u64::MAX,
                max: state.max_buffer_size,
            },
        )?;
        if total_size > state.max_buffer_size {
            return Err(ExpansionScratchError::BufferTooLarge {
                requested: total_size,
                max: state.max_buffer_size,
            });
        }

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: self.desc.label,
            size: total_size,
            usage: self.desc.usage,
            mapped_at_creation: false,
        });
        state.buffer = Arc::new(buffer);
        state.arenas = SegmentedArena::new(
            self.desc.frames_in_flight,
            new_per_frame_capacity,
            self.arena_index,
        );
        Ok(())
    }

    fn alloc_inner(
        &mut self,
        device: &wgpu::Device,
        size: u64,
        alignment: u64,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        self.ensure_init(device)?;
        let state = self.state.as_mut().expect("initialized above");

        if size == 0 {
            return Err(ExpansionScratchError::InvalidDescriptor(
                "allocation size must be > 0",
            ));
        }

        // Keep offsets usable for buffer copies (`COPY_BUFFER_ALIGNMENT`) and for dynamic storage
        // binding offsets (`min_storage_buffer_offset_alignment`).
        let alignment = alignment.max(1);
        let alignment = lcm_u64(alignment, wgpu::COPY_BUFFER_ALIGNMENT);
        let alignment = lcm_u64(alignment, state.storage_alignment);

        let size = align_up(size, wgpu::COPY_BUFFER_ALIGNMENT);

        let remaining = state.arenas.remaining();
        if let Some(offset) = state.arenas.alloc(size, alignment) {
            return Ok(ExpansionScratchAlloc {
                buffer: Arc::clone(&state.buffer),
                offset,
                size,
            });
        }

        // Segment is full: grow by allocating a new backing buffer and restart allocations in the
        // current segment. Previously-returned allocations keep their old `Arc<wgpu::Buffer>`.
        //
        // Grow conservatively (double), but always ensure we can fit the current request.
        let mut new_per_frame_capacity =
            state.arenas.per_frame_capacity.saturating_mul(2).max(size);
        new_per_frame_capacity = align_up(new_per_frame_capacity, state.required_alignment);

        let frames_u64 = self.desc.frames_in_flight as u64;
        let max_per_frame = state.max_buffer_size.checked_div(frames_u64).unwrap_or(0);
        let max_per_frame = align_down(max_per_frame, state.required_alignment);
        if new_per_frame_capacity > max_per_frame {
            // Clamp to the maximum possible size if that still fits the requested allocation.
            if size <= max_per_frame {
                new_per_frame_capacity = max_per_frame;
            } else {
                return Err(ExpansionScratchError::OutOfSpace {
                    requested: size,
                    alignment,
                    remaining,
                    per_frame_capacity: state.arenas.per_frame_capacity,
                });
            }
        }

        self.realloc_buffer(device, new_per_frame_capacity)?;

        // Retry the allocation in the fresh segment.
        let state = self.state.as_mut().expect("realloc keeps state present");
        let remaining = state.arenas.remaining();
        let per_frame_capacity = state.arenas.per_frame_capacity;
        let offset =
            state
                .arenas
                .alloc(size, alignment)
                .ok_or(ExpansionScratchError::OutOfSpace {
                    requested: size,
                    alignment,
                    remaining,
                    per_frame_capacity,
                })?;
        Ok(ExpansionScratchAlloc {
            buffer: Arc::clone(&state.buffer),
            offset,
            size,
        })
    }

    pub fn alloc_vertex_output(
        &mut self,
        device: &wgpu::Device,
        size: u64,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        self.alloc_inner(device, size, 16)
    }

    pub fn alloc_index_output(
        &mut self,
        device: &wgpu::Device,
        size: u64,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        self.alloc_inner(device, size, 4)
    }

    pub fn alloc_indirect_draw(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        if !self.desc.usage.contains(wgpu::BufferUsages::INDIRECT) {
            return Err(ExpansionScratchError::InvalidDescriptor(
                "scratch buffer usage must include INDIRECT for indirect draw arguments",
            ));
        }
        let (size, align) = DrawIndirectArgs::layout();
        self.alloc_inner(device, size, align)
    }

    pub fn alloc_indirect_draw_indexed(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        if !self.desc.usage.contains(wgpu::BufferUsages::INDIRECT) {
            return Err(ExpansionScratchError::InvalidDescriptor(
                "scratch buffer usage must include INDIRECT for indirect draw arguments",
            ));
        }
        let (size, align) = DrawIndexedIndirectArgs::layout();
        self.alloc_inner(device, size, align)
    }

    /// Allocate a combined storage buffer used by the translated GS prepass:
    /// `DrawIndexedIndirectArgs` followed by the small counter block.
    ///
    /// Packing these together keeps the generated compute shader within WebGPU's minimum
    /// `max_storage_buffers_per_shader_stage` limit (4 storage buffers).
    pub fn alloc_gs_prepass_state_draw_indexed(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        if !self.desc.usage.contains(wgpu::BufferUsages::INDIRECT) {
            return Err(ExpansionScratchError::InvalidDescriptor(
                "scratch buffer usage must include INDIRECT for indirect draw arguments",
            ));
        }
        let (args_size, align) = DrawIndexedIndirectArgs::layout();
        // Sized to match `runtime::gs_translate::GsPrepassCounters` (4 x u32 / 16 bytes).
        let size = args_size
            .checked_add(16)
            .ok_or(ExpansionScratchError::InvalidDescriptor(
                "indirect args allocation overflows when adding GS prepass counters",
            ))?;
        self.alloc_inner(device, size, align)
    }

    pub fn alloc_counter_u32(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        self.alloc_inner(device, 4, 4)
    }

    /// Allocate a small counter block for GS/HS/DS-style compute expansion passes.
    ///
    /// This is sized to hold a few `u32`/`atomic<u32>` counters (currently 16 bytes) without
    /// overlapping other scratch allocations, even when the compute shader binds the counter range
    /// with a larger `BufferBinding::size`.
    pub fn alloc_gs_prepass_counters(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        self.alloc_inner(device, 16, 4)
    }

    pub fn alloc_metadata(
        &mut self,
        device: &wgpu::Device,
        size: u64,
        alignment: u64,
    ) -> Result<ExpansionScratchAlloc, ExpansionScratchError> {
        self.alloc_inner(device, size, alignment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_descriptor_includes_indirect_usage() {
        assert!(
            ExpansionScratchDescriptor::default()
                .usage
                .contains(wgpu::BufferUsages::INDIRECT),
            "ExpansionScratchDescriptor::default must include INDIRECT so alloc_indirect_* is usable"
        );
    }

    #[test]
    fn segmented_arena_separates_frames_and_wraps() {
        let mut arena = SegmentedArena::new(3, 64, 0);

        // Frame 0.
        let a0 = arena.alloc(16, 16).unwrap();
        assert_eq!(a0, 0);

        // Frame 1.
        arena.begin_frame();
        let a1 = arena.alloc(16, 16).unwrap();
        assert_eq!(a1, 64);

        // Frame 2.
        arena.begin_frame();
        let a2 = arena.alloc(16, 16).unwrap();
        assert_eq!(a2, 128);

        // Wrap back to frame 0 and ensure allocation resets.
        arena.begin_frame();
        let a0b = arena.alloc(16, 16).unwrap();
        assert_eq!(a0b, 0);
    }

    #[test]
    fn segmented_arena_respects_alignment_within_segment() {
        let mut arena = SegmentedArena::new(1, 128, 0);
        let a = arena.alloc(1, 1).unwrap();
        assert_eq!(a, 0);

        let b = arena.alloc(1, 16).unwrap();
        assert_eq!(b, 16);

        let c = arena.alloc(1, 32).unwrap();
        assert_eq!(c, 32);
    }
}
