use std::sync::Arc;

use aero_gpu::bindings::bind_group_cache::BindGroupCache;
use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
use aero_gpu::pipeline_cache::PipelineCache;
use aero_gpu::pipeline_key::{ComputePipelineKey, PipelineLayoutKey};
use anyhow::{anyhow, bail, Result};

use crate::runtime::pipeline_layout_cache::PipelineLayoutCache as PipelineLayoutCacheLocal;
use crate::runtime::reflection_bindings;

/// Metadata for a compute kernel that implements one hull-shader phase.
#[derive(Debug, Clone, Copy)]
pub(in crate::runtime) struct HullKernel<'a> {
    pub label: &'a str,
    pub wgsl: &'a str,
    pub bindings: &'a [crate::Binding],
    pub entry_point: &'a str,
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

fn compute_dispatch_x(
    device: &wgpu::Device,
    thread_count: u64,
    workgroup_size_x: u32,
    label: &str,
) -> Result<u32> {
    if thread_count == 0 {
        return Ok(0);
    }
    if workgroup_size_x == 0 {
        bail!("{label}: workgroup_size_x must be > 0");
    }

    let wgx = workgroup_size_x as u64;
    let workgroups = thread_count
        .checked_add(wgx - 1)
        .ok_or_else(|| anyhow!("{label}: dispatch thread count overflow"))?
        / wgx;

    let workgroups_u32: u32 = workgroups
        .try_into()
        .map_err(|_| anyhow!("{label}: workgroup count out of u32 range"))?;

    let max = device.limits().max_compute_workgroups_per_dimension;
    if workgroups_u32 > max {
        bail!("{label}: dispatch would exceed max_compute_workgroups_per_dimension (requested={workgroups_u32} max={max})");
    }

    Ok(workgroups_u32)
}

/// Dispatch HS control-point + patch-constant compute passes.
///
/// The caller is responsible for supplying a [`reflection_bindings::BindGroupResourceProvider`]
/// that resolves:
/// - D3D resources from the HS stage binding bucket (`@group(3)`).
/// - Expansion-internal buffers (also `@group(3)`) via `internal_buffer()`.
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

    // HS control point: one thread per output control point per patch.
    let cp_threads_u64 = (params.patch_count_total as u64)
        .checked_mul(params.hs_output_control_points as u64)
        .ok_or_else(|| anyhow!("HS control point dispatch thread count overflow"))?;
    let cp_workgroups_x = compute_dispatch_x(
        device,
        cp_threads_u64,
        control_point.workgroup_size_x,
        "HS control point",
    )?;

    // HS patch constant: one thread per patch.
    let pc_threads_u64 = params.patch_count_total as u64;
    let pc_workgroups_x = compute_dispatch_x(
        device,
        pc_threads_u64,
        patch_constant.workgroup_size_x,
        "HS patch constant",
    )?;

    // Helper to build pipeline layout + bind groups for a single kernel.
    let mut build_kernel = |kernel: HullKernel<'_>| -> Result<(Vec<Arc<wgpu::BindGroup>>, *const wgpu::ComputePipeline)> {
        let mut bindings_info = reflection_bindings::build_pipeline_bindings_info(
            device,
            bind_group_layout_cache,
            [reflection_bindings::ShaderBindingSet::Guest(kernel.bindings)],
            reflection_bindings::BindGroupIndexValidation::GuestShaders,
        )?;

        let layout_key = std::mem::replace(&mut bindings_info.layout_key, PipelineLayoutKey::empty());
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
                    provider,
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
    if cp_workgroups_x != 0 {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aero-d3d11 tessellation HS control point pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(cp_pipeline);
        for (group_index, bg) in cp_bind_groups.iter().enumerate() {
            pass.set_bind_group(group_index as u32, bg.as_ref(), &[]);
        }
        pass.dispatch_workgroups(cp_workgroups_x, 1, 1);
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
