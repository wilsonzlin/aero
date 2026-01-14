//! Canonical argument buffer layouts for WebGPU indirect draw commands.
//!
//! WebGPU (and the underlying D3D/Vulkan/Metal backends) define fixed binary layouts for the
//! argument buffers consumed by `draw_indirect` / `draw_indexed_indirect`. Aero uses compute-based
//! emulation for some pipeline stages (GS/HS/DS) that writes these argument buffers on the GPU,
//! so we need a single source of truth for:
//! - Field order
//! - Signedness (`base_vertex` is `i32`)
//! - Size and alignment
//!
//! These structs are intentionally minimal: they only encode the byte layout expected by wgpu.

/// Arguments for [`wgpu::RenderPass::draw_indirect`].
///
/// This is re-exported from `aero-gpu` so all GPU/compute pipelines share a single canonical
/// layout definition.
pub use aero_gpu::indirect::DrawIndirectArgs;

/// Arguments for [`wgpu::RenderPass::draw_indexed_indirect`].
///
/// This is re-exported from `aero-gpu` so all GPU/compute pipelines share a single canonical
/// layout definition.
pub use aero_gpu::indirect::DrawIndexedIndirectArgs;

// Compile-time layout validation.
const _: [(); 16] = [(); core::mem::size_of::<DrawIndirectArgs>()];
const _: [(); 20] = [(); core::mem::size_of::<DrawIndexedIndirectArgs>()];
const _: [(); 4] = [(); core::mem::align_of::<DrawIndirectArgs>()];
const _: [(); 4] = [(); core::mem::align_of::<DrawIndexedIndirectArgs>()];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indirect_args_offsets_match_webgpu_spec() {
        assert_eq!(core::mem::size_of::<DrawIndirectArgs>(), 16);
        assert_eq!(core::mem::align_of::<DrawIndirectArgs>(), 4);
        assert_eq!(core::mem::offset_of!(DrawIndirectArgs, vertex_count), 0);
        assert_eq!(core::mem::offset_of!(DrawIndirectArgs, instance_count), 4);
        assert_eq!(core::mem::offset_of!(DrawIndirectArgs, first_vertex), 8);
        assert_eq!(core::mem::offset_of!(DrawIndirectArgs, first_instance), 12);

        assert_eq!(core::mem::size_of::<DrawIndexedIndirectArgs>(), 20);
        assert_eq!(core::mem::align_of::<DrawIndexedIndirectArgs>(), 4);
        assert_eq!(
            core::mem::offset_of!(DrawIndexedIndirectArgs, index_count),
            0
        );
        assert_eq!(
            core::mem::offset_of!(DrawIndexedIndirectArgs, instance_count),
            4
        );
        assert_eq!(
            core::mem::offset_of!(DrawIndexedIndirectArgs, first_index),
            8
        );
        assert_eq!(
            core::mem::offset_of!(DrawIndexedIndirectArgs, base_vertex),
            12
        );
        assert_eq!(
            core::mem::offset_of!(DrawIndexedIndirectArgs, first_instance),
            16
        );
    }
}
