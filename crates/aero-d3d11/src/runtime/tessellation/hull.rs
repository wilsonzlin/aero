use std::sync::Arc;

use aero_gpu::bindings::bind_group_cache::{BindGroupCache, BufferId, TextureViewId};
use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
use aero_gpu::bindings::samplers::CachedSampler;
use aero_gpu::pipeline_cache::PipelineCache;
use aero_gpu::pipeline_key::{ComputePipelineKey, PipelineLayoutKey};
use anyhow::{anyhow, bail, Result};

use crate::runtime::pipeline_layout_cache::PipelineLayoutCache as PipelineLayoutCacheLocal;
use crate::runtime::reflection_bindings;

#[derive(Debug)]
struct InternalBufferCopy {
    binding: u32,
    id: BufferId,
    buffer: Arc<wgpu::Buffer>,
    size: u64,
}

struct InternalBufferOverrides<'a, P> {
    base: &'a P,
    copies: &'a [InternalBufferCopy],
}

impl<P: reflection_bindings::BindGroupResourceProvider>
    reflection_bindings::BindGroupResourceProvider for InternalBufferOverrides<'_, P>
{
    fn constant_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        self.base.constant_buffer(slot)
    }

    fn constant_buffer_scratch(&self, slot: u32) -> Option<(BufferId, &wgpu::Buffer)> {
        self.base.constant_buffer_scratch(slot)
    }

    fn texture2d(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
        self.base.texture2d(slot)
    }

    fn texture2d_array(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
        self.base.texture2d_array(slot)
    }

    fn sampler(&self, slot: u32) -> Option<&CachedSampler> {
        self.base.sampler(slot)
    }

    fn srv_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        self.base.srv_buffer(slot)
    }

    fn uav_buffer(&self, slot: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        self.base.uav_buffer(slot)
    }

    fn internal_buffer(&self, binding: u32) -> Option<reflection_bindings::BufferBinding<'_>> {
        if let Some(copy) = self.copies.iter().find(|c| c.binding == binding) {
            return Some(reflection_bindings::BufferBinding {
                id: copy.id,
                buffer: copy.buffer.as_ref(),
                offset: 0,
                size: Some(copy.size),
                total_size: copy.size,
            });
        }
        self.base.internal_buffer(binding)
    }

    fn uav_texture2d(&self, slot: u32) -> Option<(TextureViewId, &wgpu::TextureView)> {
        self.base.uav_texture2d(slot)
    }

    fn dummy_uniform(&self) -> &wgpu::Buffer {
        self.base.dummy_uniform()
    }

    fn dummy_storage(&self) -> &wgpu::Buffer {
        self.base.dummy_storage()
    }

    fn dummy_storage_texture_view(
        &self,
        format: crate::StorageTextureFormat,
    ) -> Option<&wgpu::TextureView> {
        self.base.dummy_storage_texture_view(format)
    }

    fn dummy_texture_view_2d(&self) -> &wgpu::TextureView {
        self.base.dummy_texture_view_2d()
    }

    fn dummy_texture_view_2d_array(&self) -> &wgpu::TextureView {
        self.base.dummy_texture_view_2d_array()
    }

    fn default_sampler(&self) -> &CachedSampler {
        self.base.default_sampler()
    }
}

/// Metadata for a compute kernel that implements one hull-shader phase.
#[derive(Debug, Clone, Copy)]
pub(in crate::runtime) struct HullKernel<'a> {
    pub label: &'a str,
    pub wgsl: &'a str,
    pub bindings: &'a [crate::Binding],
    pub entry_point: &'static str,
    /// X dimension of `@workgroup_size`.
    pub workgroup_size_x: u32,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::runtime) struct HullDispatchParams {
    /// Total number of patches to process across all instances.
    pub patch_count_total: u32,
    /// IA patch control-point count (from `PatchList { control_points }`).
    pub ia_patch_control_points: u8,
    /// HS-declared input patch size, if discoverable from reflection.
    pub hs_input_patch_size: Option<u8>,
    /// HS output control point count (dcl_output_control_point_count).
    pub hs_output_control_points: u8,
}

fn validate_workgroups_dim(device: &wgpu::Device, workgroups: u32, label: &str) -> Result<()> {
    let max = device.limits().max_compute_workgroups_per_dimension;
    if workgroups > max {
        bail!("{label}: dispatch would exceed max_compute_workgroups_per_dimension (requested={workgroups} max={max})");
    }
    Ok(())
}

/// Dispatch HS control-point + patch-constant compute passes.
///
/// The caller is responsible for supplying a [`reflection_bindings::BindGroupResourceProvider`]
/// that resolves:
/// - D3D resources from the HS stage binding bucket (`@group(3)`).
/// - Expansion-internal buffers (also `@group(3)`) via `internal_buffer()`.
#[allow(clippy::too_many_arguments)]
pub(in crate::runtime) fn dispatch_hull_phases(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline_cache: &mut PipelineCache,
    bind_group_layout_cache: &mut BindGroupLayoutCache,
    pipeline_layout_cache: &mut PipelineLayoutCacheLocal<Arc<wgpu::PipelineLayout>>,
    bind_group_cache: &mut BindGroupCache<Arc<wgpu::BindGroup>>,
    provider: &impl reflection_bindings::BindGroupResourceProvider,
    control_point: HullKernel<'_>,
    patch_constant: HullKernel<'_>,
    params: HullDispatchParams,
) -> Result<()> {
    if params.hs_output_control_points == 0 {
        bail!("HS output control point count must be > 0");
    }
    if params.hs_output_control_points > 32 {
        bail!(
            "HS output control point count {} exceeds D3D11 limit 32",
            params.hs_output_control_points
        );
    }
    if let Some(expected) = params.hs_input_patch_size {
        if expected != params.ia_patch_control_points {
            bail!(
                "IA patch control point count ({}) does not match HS input patch size ({expected})",
                params.ia_patch_control_points
            );
        }
    }

    // HS phase dispatch semantics:
    // - Control point phase: 2D dispatch
    //     global_invocation_id.x = output_control_point_id
    //     global_invocation_id.y = patch_id
    // - Patch constant phase: 1D dispatch
    //     global_invocation_id.x = patch_id
    //
    // This matches the SM5 hull shader translation path (see `translate_hs`), and keeps the kernel
    // code simple (no need to decode a linear index into `(patch_id, cp_id)`).
    if control_point.workgroup_size_x != 1 {
        bail!(
            "HS control point: workgroup_size_x must be 1 (got {})",
            control_point.workgroup_size_x
        );
    }
    if patch_constant.workgroup_size_x != 1 {
        bail!(
            "HS patch constant: workgroup_size_x must be 1 (got {})",
            patch_constant.workgroup_size_x
        );
    }

    let cp_workgroups_x = params.hs_output_control_points as u32;
    let cp_workgroups_y = params.patch_count_total;
    validate_workgroups_dim(device, cp_workgroups_x, "HS control point")?;
    validate_workgroups_dim(device, cp_workgroups_y, "HS control point")?;

    let pc_workgroups_x = params.patch_count_total;
    validate_workgroups_dim(device, pc_workgroups_x, "HS patch constant")?;

    if cp_workgroups_x == 0 && pc_workgroups_x == 0 {
        return Ok(());
    }

    // Helper to build pipeline layout + bind groups for a single kernel.
    let mut build_kernel = |kernel: HullKernel<'_>| -> Result<(
        Vec<Arc<wgpu::BindGroup>>,
        *const wgpu::ComputePipeline,
    )> {
        // wgpu/WebGPU tracks buffer usage hazards per *buffer*, not per bound range. The expansion
        // scratch allocator sub-allocates multiple logical buffers from a single backing buffer, so
        // a kernel that reads one subrange (`var<storage, read>`) while writing another subrange
        // (`var<storage, read_write>`) would trigger a `ResourceUsageConflict`.
        //
        // Avoid this by copying any read-only internal bindings that alias a read-write binding
        // into a temporary buffer, and bind that copy instead.
        let storage_align = (device.limits().min_storage_buffer_offset_alignment as u64).max(1);
        let max_storage_binding_size = device.limits().max_storage_buffer_binding_size as u64;

        let mut write_buffers: Vec<*const wgpu::Buffer> = Vec::new();
        let mut read_only_bindings: Vec<(u32, reflection_bindings::BufferBinding<'_>)> = Vec::new();

        for binding in kernel.bindings {
            let crate::BindingKind::ExpansionStorageBuffer { read_only } = &binding.kind else {
                continue;
            };
            let bound = provider.internal_buffer(binding.binding).ok_or_else(|| {
                anyhow!(
                    "{}: missing expansion-internal buffer @group({}) @binding({})",
                    kernel.label,
                    binding.group,
                    binding.binding
                )
            })?;
            if *read_only {
                read_only_bindings.push((binding.binding, bound));
            } else {
                write_buffers.push(bound.buffer as *const wgpu::Buffer);
            }
        }

        let mut copies: Vec<InternalBufferCopy> = Vec::new();
        for (binding_num, bound) in read_only_bindings {
            let buf_ptr = bound.buffer as *const wgpu::Buffer;
            if !write_buffers.contains(&buf_ptr) {
                continue;
            }

            let offset = bound.offset;
            let total_size = bound.total_size;
            if offset >= total_size || (offset != 0 && !offset.is_multiple_of(storage_align)) {
                bail!(
                    "{}: invalid expansion-internal buffer @binding({binding_num}) (offset={offset} total_size={total_size} storage_align={storage_align})",
                    kernel.label
                );
            }

            let remaining = total_size - offset;
            let requested = bound.size.unwrap_or(remaining).min(remaining);
            let mut bind_size = requested.min(max_storage_binding_size);
            bind_size -= bind_size % 4;
            if bind_size == 0 {
                bail!(
                    "{}: invalid expansion-internal buffer @binding({binding_num}) (computed bind_size=0)",
                    kernel.label
                );
            }

            if !offset.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                || !bind_size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            {
                bail!(
                    "{}: internal buffer copy requires COPY_BUFFER_ALIGNMENT={} (binding={binding_num} offset={offset} size={bind_size})",
                    kernel.label,
                    wgpu::COPY_BUFFER_ALIGNMENT
                );
            }

            let buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-d3d11 HS internal read-only alias copy"),
                size: bind_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            encoder.copy_buffer_to_buffer(bound.buffer, offset, buffer.as_ref(), 0, bind_size);

            // Use a unique bind-group cache ID for this temporary buffer. The high bit keeps these
            // IDs disjoint from executor-managed buffer IDs (which live in lower ranges).
            let ptr_id = Arc::as_ptr(&buffer) as usize as u64;
            let id = BufferId((1u64 << 63) | (ptr_id & ((1u64 << 63) - 1)));
            copies.push(InternalBufferCopy {
                binding: binding_num,
                id,
                buffer,
                size: bind_size,
            });
        }

        let provider = InternalBufferOverrides {
            base: provider,
            copies: &copies,
        };

        let mut bindings_info = reflection_bindings::build_pipeline_bindings_info(
            device,
            bind_group_layout_cache,
            [reflection_bindings::ShaderBindingSet::Guest(
                kernel.bindings,
            )],
            reflection_bindings::BindGroupIndexValidation::GuestShaders,
        )?;

        let layout_key =
            std::mem::replace(&mut bindings_info.layout_key, PipelineLayoutKey::empty());
        let layout_refs: Vec<&wgpu::BindGroupLayout> = bindings_info
            .group_layouts
            .iter()
            .map(|l| l.layout.as_ref())
            .collect();
        let pipeline_layout = pipeline_layout_cache.get_or_create(
            device,
            &layout_key,
            &layout_refs,
            Some("aero-d3d11 tessellation HS pipeline layout"),
        );

        let mut bind_groups: Vec<Arc<wgpu::BindGroup>> =
            Vec::with_capacity(bindings_info.group_layouts.len());
        for group_index in 0..bindings_info.group_layouts.len() {
            if bindings_info.group_bindings[group_index].is_empty() {
                let entries: [aero_gpu::bindings::bind_group_cache::BindGroupCacheEntry<'_>; 0] =
                    [];
                let bg = bind_group_cache.get_or_create(
                    device,
                    &bindings_info.group_layouts[group_index],
                    &entries,
                );
                bind_groups.push(bg);
            } else {
                let bg = reflection_bindings::build_bind_group(
                    device,
                    bind_group_cache,
                    &bindings_info.group_layouts[group_index],
                    &bindings_info.group_bindings[group_index],
                    &provider,
                )?;
                bind_groups.push(bg);
            }
        }

        let pipeline_ptr = {
            let (cs_hash, _module) = pipeline_cache.get_or_create_shader_module(
                device,
                aero_gpu::pipeline_key::ShaderStage::Compute,
                kernel.wgsl,
                Some(kernel.label),
            );
            let key = ComputePipelineKey {
                shader: cs_hash,
                layout: layout_key.clone(),
                entry_point: kernel.entry_point,
            };
            let pipeline = pipeline_cache
                .get_or_create_compute_pipeline(device, key, move |device, cs| {
                    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(kernel.label),
                        layout: Some(pipeline_layout.as_ref()),
                        module: cs,
                        entry_point: kernel.entry_point,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    })
                })
                .map_err(|e| anyhow!("wgpu pipeline cache: {e:?}"))?;
            pipeline as *const wgpu::ComputePipeline
        };

        Ok((bind_groups, pipeline_ptr))
    };

    let (cp_bind_groups, cp_pipeline_ptr) = build_kernel(control_point)?;
    let cp_pipeline = unsafe { &*cp_pipeline_ptr };

    let (pc_bind_groups, pc_pipeline_ptr) = build_kernel(patch_constant)?;
    let pc_pipeline = unsafe { &*pc_pipeline_ptr };

    // Dispatch phases. Use separate compute passes to keep debugging labels clear.
    if cp_workgroups_x != 0 && cp_workgroups_y != 0 {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aero-d3d11 tessellation HS control point pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(cp_pipeline);
        for (group_index, bg) in cp_bind_groups.iter().enumerate() {
            pass.set_bind_group(group_index as u32, bg.as_ref(), &[]);
        }
        pass.dispatch_workgroups(cp_workgroups_x, cp_workgroups_y, 1);
    }

    if pc_workgroups_x != 0 {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aero-d3d11 tessellation HS patch constant pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pc_pipeline);
        for (group_index, bg) in pc_bind_groups.iter().enumerate() {
            pass.set_bind_group(group_index as u32, bg.as_ref(), &[]);
        }
        pass.dispatch_workgroups(pc_workgroups_x, 1, 1);
    }

    Ok(())
}
