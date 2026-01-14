use bytemuck::{Pod, Zeroable};

/// WebGPU-compatible arguments for [`wgpu::RenderPass::draw_indirect`].
///
/// This matches the WebGPU `drawIndirect` argument layout:
/// - `vertex_count: u32`
/// - `instance_count: u32`
/// - `first_vertex: u32`
/// - `first_instance: u32`
///
/// Total size: 16 bytes (4 * u32), 4-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct DrawIndirectArgs {
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
}

/// WebGPU-compatible arguments for [`wgpu::RenderPass::draw_indexed_indirect`].
///
/// This matches the WebGPU `drawIndexedIndirect` argument layout:
/// - `index_count: u32`
/// - `instance_count: u32`
/// - `first_index: u32`
/// - `base_vertex: i32`
/// - `first_instance: u32`
///
/// Total size: 20 bytes (5 * 4-byte scalars), 4-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct DrawIndexedIndirectArgs {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
}

impl DrawIndirectArgs {
    pub const SIZE_BYTES: u64 = core::mem::size_of::<Self>() as u64;
    pub const ALIGN_BYTES: u64 = core::mem::align_of::<Self>() as u64;

    /// Returns `(size, alignment)` in bytes for the argument struct.
    #[inline]
    pub const fn layout() -> (u64, u64) {
        (Self::SIZE_BYTES, Self::ALIGN_BYTES)
    }

    /// View this struct as raw bytes (little-endian on all supported platforms).
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    /// Clamp `vertex_count` so that `first_vertex..first_vertex+vertex_count` stays within
    /// `0..max_vertices`.
    #[inline]
    pub fn clamp_vertices(&mut self, max_vertices: u32) {
        self.vertex_count = clamp_count(self.first_vertex, self.vertex_count, max_vertices);
    }

    /// Clamp `instance_count` so that `first_instance..first_instance+instance_count` stays within
    /// `0..max_instances`.
    #[inline]
    pub fn clamp_instances(&mut self, max_instances: u32) {
        self.instance_count = clamp_count(self.first_instance, self.instance_count, max_instances);
    }
}

impl DrawIndexedIndirectArgs {
    pub const SIZE_BYTES: u64 = core::mem::size_of::<Self>() as u64;
    pub const ALIGN_BYTES: u64 = core::mem::align_of::<Self>() as u64;

    /// Returns `(size, alignment)` in bytes for the argument struct.
    #[inline]
    pub const fn layout() -> (u64, u64) {
        (Self::SIZE_BYTES, Self::ALIGN_BYTES)
    }

    /// View this struct as raw bytes (little-endian on all supported platforms).
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    /// Clamp `index_count` so that `first_index..first_index+index_count` stays within
    /// `0..max_indices`.
    #[inline]
    pub fn clamp_indices(&mut self, max_indices: u32) {
        self.index_count = clamp_count(self.first_index, self.index_count, max_indices);
    }

    /// Clamp `instance_count` so that `first_instance..first_instance+instance_count` stays within
    /// `0..max_instances`.
    #[inline]
    pub fn clamp_instances(&mut self, max_instances: u32) {
        self.instance_count = clamp_count(self.first_instance, self.instance_count, max_instances);
    }
}

/// Compute the maximum number of elements in a buffer slice of `size_bytes`, using a fixed stride.
///
/// Returns `0` if `stride_bytes == 0`.
#[inline]
pub fn max_elements_in_buffer(size_bytes: u64, stride_bytes: u64) -> u32 {
    if stride_bytes == 0 {
        return 0;
    }
    let count = size_bytes / stride_bytes;
    count.min(u64::from(u32::MAX)) as u32
}

#[inline]
fn clamp_count(first: u32, count: u32, max: u32) -> u32 {
    if first >= max {
        return 0;
    }
    let remaining = max - first;
    count.min(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indirect_args_layout_matches_webgpu() {
        assert_eq!(core::mem::size_of::<DrawIndirectArgs>(), 16);
        assert_eq!(core::mem::align_of::<DrawIndirectArgs>(), 4);

        assert_eq!(core::mem::size_of::<DrawIndexedIndirectArgs>(), 20);
        assert_eq!(core::mem::align_of::<DrawIndexedIndirectArgs>(), 4);
    }

    #[test]
    fn clamp_count_behavior() {
        let mut args = DrawIndirectArgs {
            vertex_count: 10,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        };
        args.clamp_vertices(3);
        assert_eq!(args.vertex_count, 3);

        let mut args = DrawIndirectArgs {
            vertex_count: 10,
            instance_count: 1,
            first_vertex: 2,
            first_instance: 0,
        };
        args.clamp_vertices(3);
        assert_eq!(args.vertex_count, 1);

        let mut args = DrawIndirectArgs {
            vertex_count: 10,
            instance_count: 1,
            first_vertex: 3,
            first_instance: 0,
        };
        args.clamp_vertices(3);
        assert_eq!(args.vertex_count, 0);
    }

    #[test]
    fn max_elements_in_buffer_behavior() {
        assert_eq!(max_elements_in_buffer(0, 4), 0);
        assert_eq!(max_elements_in_buffer(16, 4), 4);
        assert_eq!(max_elements_in_buffer(17, 4), 4);
        assert_eq!(max_elements_in_buffer(16, 0), 0);
    }
}
