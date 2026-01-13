use aero_gpu::bindings::bind_group_cache::BufferId;
use aero_gpu::BufferArena;
use anyhow::{anyhow, bail, Result};

/// An allocation carved out of a [`GpuScratchAllocator`].
///
/// This is intentionally a plain data struct so it can be stored without
/// borrowing the allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchAllocation {
    pub buffer_id: BufferId,
    pub offset: u64,
    pub size: u64,
    pub(crate) buffer_index: usize,
}

impl ScratchAllocation {
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScratchBumpAlloc {
    buffer_index: usize,
    offset: u64,
    size: u64,
}

#[derive(Debug, Clone)]
struct ScratchBumpAllocator {
    initial_capacity: u64,
    min_alignment: u64,
    max_buffer_size: u64,
    buffers: Vec<BufferArena>,
    current: usize,
}

impl ScratchBumpAllocator {
    fn new(initial_capacity: u64, min_alignment: u64, max_buffer_size: u64) -> Self {
        Self {
            initial_capacity: initial_capacity.max(1),
            min_alignment: min_alignment.max(1),
            max_buffer_size,
            buffers: Vec::new(),
            current: 0,
        }
    }

    fn reset(&mut self) {
        for arena in &mut self.buffers {
            arena.reset();
        }
        self.current = 0;
    }

    fn buffer_capacity(&self, index: usize) -> Option<u64> {
        self.buffers.get(index).map(|a| a.capacity())
    }

    fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    fn alloc(&mut self, size: u64, alignment: u64) -> Result<ScratchBumpAlloc> {
        let alignment = alignment.max(1);
        let effective_align = lcm_u64(self.min_alignment, alignment)
            .ok_or_else(|| anyhow!("scratch allocation alignment overflows u64"))?;

        if self.buffers.is_empty() {
            let capacity = align_up(self.initial_capacity.max(size), 4).max(4);
            let capacity = capacity.min(self.max_buffer_size);
            if capacity < size {
                bail!(
                    "scratch allocation size {size} exceeds max_buffer_size {}",
                    self.max_buffer_size
                );
            }
            self.buffers.push(BufferArena::new(0, capacity));
            self.current = 0;
        }

        loop {
            if let Some(arena) = self.buffers.get_mut(self.current) {
                if let Some(offset) = arena.alloc(size, effective_align) {
                    return Ok(ScratchBumpAlloc {
                        buffer_index: self.current,
                        offset,
                        size,
                    });
                }
            }

            let next = self.current + 1;
            if next < self.buffers.len() {
                self.current = next;
                continue;
            }

            // Need a new backing arena.
            let prev_capacity = self
                .buffers
                .last()
                .map(|a| a.capacity())
                .unwrap_or(self.initial_capacity);
            let mut new_capacity = prev_capacity.saturating_mul(2).max(size);
            // Ensure the buffer size is at least 4-byte aligned (wgpu copy/clear requirements).
            new_capacity = align_up(new_capacity, 4).max(4);

            if new_capacity > self.max_buffer_size {
                if size > self.max_buffer_size {
                    bail!(
                        "scratch allocation size {size} exceeds max_buffer_size {}",
                        self.max_buffer_size
                    );
                }
                new_capacity = self.max_buffer_size;
            }

            self.buffers.push(BufferArena::new(0, new_capacity));
            self.current = self.buffers.len() - 1;
        }
    }
}

#[derive(Debug)]
struct ScratchBuffer {
    id: BufferId,
    buffer: wgpu::Buffer,
}

#[derive(Debug, Clone)]
pub struct GpuScratchAllocatorConfig {
    pub label: &'static str,
    pub initial_capacity: u64,
    pub usage: wgpu::BufferUsages,
    /// Base value for assigning unique [`BufferId`]s to scratch buffers.
    ///
    /// This should not collide with IDs derived from guest handles (typically `0..2^32`), or other
    /// internal scratch allocations (e.g. constant-buffer scratch in `aerogpu_cmd_executor`).
    pub buffer_id_base: u64,
}

impl Default for GpuScratchAllocatorConfig {
    fn default() -> Self {
        Self {
            label: "aero-d3d11 scratch",
            // A small default: the allocator will grow as needed based on actual workloads.
            initial_capacity: 1024 * 1024,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::INDIRECT,
            // Use a high range to avoid colliding with guest handles and other scratch buffers.
            buffer_id_base: 1u64 << 48,
        }
    }
}

/// A per-command-stream (or per-frame) scratch allocator backed by one or more `wgpu::Buffer`s.
///
/// This is intended for GPU compute expansion paths (geometry/tessellation emulation) which need
/// temporary storage for:
/// - Expanded vertex/index buffers
/// - Indirect draw argument buffers
/// - Counters/metadata buffers
#[derive(Debug)]
pub struct GpuScratchAllocator {
    cfg: GpuScratchAllocatorConfig,
    bump: ScratchBumpAllocator,
    buffers: Vec<ScratchBuffer>,
    next_buffer_id: u64,
}

impl GpuScratchAllocator {
    pub fn new(device: &wgpu::Device) -> Self {
        Self::new_with_config(device, GpuScratchAllocatorConfig::default())
    }

    pub fn new_with_config(device: &wgpu::Device, cfg: GpuScratchAllocatorConfig) -> Self {
        let limits = device.limits();
        let min_alignment = limits.min_storage_buffer_offset_alignment as u64;
        let max_buffer_size = limits.max_buffer_size;
        let bump = ScratchBumpAllocator::new(cfg.initial_capacity, min_alignment, max_buffer_size);
        Self {
            next_buffer_id: cfg.buffer_id_base,
            cfg,
            bump,
            buffers: Vec::new(),
        }
    }

    /// Reset allocator state so new allocations reuse existing buffers.
    ///
    /// This should be called once per frame / per command-stream execution, after previous
    /// submissions that reference these scratch buffers have been queued.
    pub fn reset(&mut self) {
        self.bump.reset();
    }

    /// Drop all backing buffers and reset the allocation cursor.
    pub fn clear(&mut self) {
        self.bump.buffers.clear();
        self.buffers.clear();
        self.bump.current = 0;
        // Intentionally do not reset `next_buffer_id`. Buffer IDs are part of bind-group cache keys;
        // reusing an ID while the cache still contains entries can cause old bind groups to be
        // returned for new buffers.
    }

    pub fn min_storage_alignment(&self) -> u64 {
        self.bump.min_alignment
    }

    pub fn buffer(&self, allocation: ScratchAllocation) -> &wgpu::Buffer {
        &self.buffers[allocation.buffer_index].buffer
    }

    pub fn alloc(&mut self, device: &wgpu::Device, size: u64, align: u64) -> Result<ScratchAllocation> {
        let bump_alloc = self.bump.alloc(size, align)?;

        // Create any missing GPU buffers for newly-added bump arenas.
        while self.buffers.len() < self.bump.buffer_count() {
            let index = self.buffers.len();
            let capacity = self
                .bump
                .buffer_capacity(index)
                .ok_or_else(|| anyhow!("scratch allocator internal error: missing arena capacity"))?;

            let label = format!("{} buffer[{index}] ({} bytes)", self.cfg.label, capacity);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&label),
                size: capacity,
                usage: self.cfg.usage,
                mapped_at_creation: false,
            });

            let id = BufferId(self.next_buffer_id);
            self.next_buffer_id = self.next_buffer_id.wrapping_add(1);
            self.buffers.push(ScratchBuffer { id, buffer });
        }

        let buf = self
            .buffers
            .get(bump_alloc.buffer_index)
            .ok_or_else(|| anyhow!("scratch allocator internal error: allocation refers to missing buffer"))?;

        Ok(ScratchAllocation {
            buffer_id: buf.id,
            offset: bump_alloc.offset,
            size: bump_alloc.size,
            buffer_index: bump_alloc.buffer_index,
        })
    }

    /// Allocate an expanded vertex output region (byte-sized).
    pub fn alloc_expanded_vertex_output(
        &mut self,
        device: &wgpu::Device,
        vertex_count: u32,
        stride_bytes: u32,
    ) -> Result<ScratchAllocation> {
        let bytes = (vertex_count as u64)
            .checked_mul(stride_bytes as u64)
            .ok_or_else(|| anyhow!("expanded vertex output size overflows u64"))?;
        // Storage bindings and queue writes require 4-byte multiples.
        let bytes = align_up(bytes, 4);
        self.alloc(device, bytes, 4)
    }

    /// Allocate an expanded index output region (byte-sized).
    pub fn alloc_expanded_index_output(
        &mut self,
        device: &wgpu::Device,
        index_count: u32,
        index_format: wgpu::IndexFormat,
    ) -> Result<ScratchAllocation> {
        let index_size = match index_format {
            wgpu::IndexFormat::Uint16 => 2u64,
            wgpu::IndexFormat::Uint32 => 4u64,
        };
        let bytes = (index_count as u64)
            .checked_mul(index_size)
            .ok_or_else(|| anyhow!("expanded index output size overflows u64"))?;
        // Storage bindings and queue writes require 4-byte multiples.
        let bytes = align_up(bytes, 4);
        self.alloc(device, bytes, 4)
    }

    /// Allocate a region for `wgpu::RenderPass::draw_indirect`.
    pub fn alloc_indirect_draw_args(
        &mut self,
        device: &wgpu::Device,
        draw_count: u32,
    ) -> Result<ScratchAllocation> {
        let per = DRAW_INDIRECT_ARGS_SIZE;
        let bytes = (draw_count as u64)
            .checked_mul(per)
            .ok_or_else(|| anyhow!("indirect draw args size overflows u64"))?;
        // Indirect argument offsets must be 4-byte aligned in WebGPU.
        self.alloc(device, bytes, 4)
    }

    /// Allocate a region for `wgpu::RenderPass::draw_indexed_indirect`.
    pub fn alloc_indirect_draw_indexed_args(
        &mut self,
        device: &wgpu::Device,
        draw_count: u32,
    ) -> Result<ScratchAllocation> {
        let per = DRAW_INDEXED_INDIRECT_ARGS_SIZE;
        let bytes = (draw_count as u64)
            .checked_mul(per)
            .ok_or_else(|| anyhow!("indirect indexed draw args size overflows u64"))?;
        // Indirect argument offsets must be 4-byte aligned in WebGPU.
        self.alloc(device, bytes, 4)
    }

    /// Allocate a counters/metadata region as a `u32` array.
    pub fn alloc_u32_counters(
        &mut self,
        device: &wgpu::Device,
        count_u32: u32,
    ) -> Result<ScratchAllocation> {
        let bytes = (count_u32 as u64)
            .checked_mul(4)
            .ok_or_else(|| anyhow!("u32 counters size overflows u64"))?;
        self.alloc(device, bytes, 4)
    }
}

/// Size, in bytes, of a WebGPU `DrawIndirectArgs` struct.
pub const DRAW_INDIRECT_ARGS_SIZE: u64 = 4 * 4;
/// Size, in bytes, of a WebGPU `DrawIndexedIndirectArgs` struct.
pub const DRAW_INDEXED_INDIRECT_ARGS_SIZE: u64 = 5 * 4;

/// Round `value` up to the nearest multiple of `alignment`.
///
/// `alignment` must be > 0.
fn align_up(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment > 0);

    // `value + alignment - 1` can overflow if the user passes pathological inputs, so use a
    // checked path and fall back to saturating behaviour.
    let add = alignment - 1;
    match value.checked_add(add) {
        Some(v) => v / alignment * alignment,
        None => u64::MAX / alignment * alignment,
    }
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

fn lcm_u64(a: u64, b: u64) -> Option<u64> {
    if a == 0 || b == 0 {
        return None;
    }
    let g = gcd_u64(a, b);
    Some((a / g).checked_mul(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_alloc_respects_min_storage_alignment() {
        let mut bump = ScratchBumpAllocator::new(1024, 256, 16 * 1024);

        let a = bump.alloc(4, 4).unwrap();
        assert_eq!(a.buffer_index, 0);
        assert_eq!(a.offset, 0);
        assert!(a.offset.is_multiple_of(256));

        let b = bump.alloc(4, 4).unwrap();
        assert_eq!(b.buffer_index, 0);
        assert_eq!(b.offset, 256);
        assert!(b.offset.is_multiple_of(256));
    }

    #[test]
    fn bump_alloc_uses_lcm_for_alignment() {
        // 256 and 48 -> lcm 768.
        let mut bump = ScratchBumpAllocator::new(4096, 256, 64 * 1024);
        let a = bump.alloc(4, 48).unwrap();
        let b = bump.alloc(4, 48).unwrap();
        assert_eq!(a.offset, 0);
        assert_eq!(b.offset, 768);
        assert!(b.offset.is_multiple_of(256));
        assert!(b.offset.is_multiple_of(48));
    }

    #[test]
    fn bump_growth_allocates_new_buffer() {
        let mut bump = ScratchBumpAllocator::new(64, 16, 1024);

        let a = bump.alloc(48, 1).unwrap();
        assert_eq!(a.buffer_index, 0);
        assert_eq!(a.offset, 0);

        let b = bump.alloc(32, 1).unwrap();
        assert_eq!(b.buffer_index, 1);
        assert_eq!(b.offset, 0);
        assert_eq!(bump.buffer_count(), 2);
        assert_eq!(bump.buffer_capacity(1), Some(128));
    }

    #[test]
    fn bump_reset_reuses_existing_buffers() {
        let mut bump = ScratchBumpAllocator::new(64, 16, 1024);
        let _ = bump.alloc(48, 1).unwrap();
        let _ = bump.alloc(32, 1).unwrap();
        assert_eq!(bump.buffer_count(), 2);

        bump.reset();
        let a = bump.alloc(32, 1).unwrap();
        assert_eq!(a.buffer_index, 0);
        assert_eq!(a.offset, 0);

        let b = bump.alloc(96, 1).unwrap();
        // Doesn't fit in 64-byte buffer[0], but should reuse the already-allocated 128-byte buffer[1].
        assert_eq!(b.buffer_index, 1);
        assert_eq!(b.offset, 0);
        assert_eq!(bump.buffer_count(), 2);
    }
}
