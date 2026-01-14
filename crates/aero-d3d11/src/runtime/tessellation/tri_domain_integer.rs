//! Tri-domain integer tessellation helpers.
//!
//! This file implements a compute pass that generates a packed `u32` index buffer for tessellated
//! triangle patches. The algorithm assumes a uniform integer tessellation level `N`:
//! - output triangle count: `N^2`
//! - output vertex count: `(N+1)(N+2)/2`

use crate::runtime::tessellator;

/// Number of indices processed per workgroup in the Y dimension.
///
/// The compute dispatch is 2D:
/// - `dispatch_x = patch_count`
/// - `dispatch_y = ceil(max_index_count_per_patch / WORKGROUP_SIZE_Y)`
pub const TRI_INDEX_GEN_WORKGROUP_SIZE_Y: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TriDomainPatchMeta {
    /// Integer tessellation level `N`.
    pub tess_level: u32,
    /// Base vertex index for this patch within the expanded vertex buffer.
    pub vertex_base: u32,
    /// Base index offset (in indices, not bytes) within the expanded index buffer.
    pub index_base: u32,
    /// Vertex count for this patch (typically `(tess_level+1)(tess_level+2)/2`).
    pub vertex_count: u32,
    /// Index count for this patch (typically `3 * tess_level^2`).
    pub index_count: u32,
}

impl TriDomainPatchMeta {
    pub fn to_le_bytes(self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[0..4].copy_from_slice(&self.tess_level.to_le_bytes());
        out[4..8].copy_from_slice(&self.vertex_base.to_le_bytes());
        out[8..12].copy_from_slice(&self.index_base.to_le_bytes());
        out[12..16].copy_from_slice(&self.vertex_count.to_le_bytes());
        out[16..20].copy_from_slice(&self.index_count.to_le_bytes());
        out
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TriangleWinding {
    #[default]
    Ccw,
    Cw,
}

impl TriangleWinding {
    pub fn as_u32(self) -> u32 {
        match self {
            TriangleWinding::Ccw => 0,
            TriangleWinding::Cw => 1,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TriIndexGenParams {
    /// 0 = CCW, 1 = CW.
    pub winding: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

impl TriIndexGenParams {
    pub fn new(winding: TriangleWinding) -> Self {
        Self {
            winding: winding.as_u32(),
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        }
    }

    pub fn to_le_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.winding.to_le_bytes());
        out[4..8].copy_from_slice(&self._pad0.to_le_bytes());
        out[8..12].copy_from_slice(&self._pad1.to_le_bytes());
        out[12..16].copy_from_slice(&self._pad2.to_le_bytes());
        out
    }
}

pub fn tri_domain_integer_vertex_count(tess_level: u32) -> u32 {
    tessellator::tri_vertex_count(tess_level)
}

pub fn tri_domain_integer_triangle_count(tess_level: u32) -> u32 {
    tri_domain_integer_index_count(tess_level) / 3
}

pub fn tri_domain_integer_index_count(tess_level: u32) -> u32 {
    tessellator::tri_index_count(tess_level)
}

fn wgsl_tri_index_gen(workgroup_size_y: u32) -> String {
    let tess_lib = tessellator::wgsl_tri_tessellator_lib_default();
    format!(
        r#"
{tess_lib}

struct PatchMeta {{
    tess_level: u32,
    vertex_base: u32,
    index_base: u32,
    vertex_count: u32,
    index_count: u32,
}};

struct Params {{
    winding: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}};

 @group(0) @binding(0)
 // NOTE: This is bound as `read_write` even though the kernel only reads it. wgpu tracks buffer
 // usage at the whole-buffer granularity (not per binding range), and tessellation expansion often
 // suballocates multiple logical buffers from a single backing scratch buffer. Treating scratch
 // inputs as `read_write` avoids mixing read-only and read-write storage views of the same
 // underlying buffer in one dispatch.
 var<storage, read_write> patches: array<PatchMeta>;

@group(0) @binding(1)
var<storage, read_write> out_indices: array<u32>;

@group(0) @binding(2)
var<uniform> params: Params;

@compute @workgroup_size(1, {workgroup_size_y}, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let patch_id: u32 = gid.x;
    let local_index: u32 = gid.y;
    // NOTE: `patch` is a reserved keyword in WGSL (wgpu 0.20 / naga).
    let patch_meta: PatchMeta = patches[patch_id];

    if (local_index >= patch_meta.index_count) {{
        return;
    }}

    let vert_in_tri: u32 = local_index % 3u;
    let tri_id: u32 = local_index / 3u;

    // Index generation is parameterized by winding so we can share the same per-triangle
    // tessellator math for both CCW and CW topologies.
    let local_verts: vec3<u32> = select(
        tri_index_to_vertex_indices(patch_meta.tess_level, tri_id),
        tri_index_to_vertex_indices_cw(patch_meta.tess_level, tri_id),
        params.winding != 0u,
    );

    let v0: u32 = local_verts.x + patch_meta.vertex_base;
    let v1: u32 = local_verts.y + patch_meta.vertex_base;
    let v2: u32 = local_verts.z + patch_meta.vertex_base;

    var out_val: u32 = v0;
    if (vert_in_tri == 1u) {{
        out_val = v1;
    }} else if (vert_in_tri == 2u) {{
        out_val = v2;
    }}

    out_indices[patch_meta.index_base + local_index] = out_val;
}}
"#,
        tess_lib = tess_lib,
    )
}

#[derive(Debug)]
pub struct TriDomainIntegerIndexGen {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl TriDomainIntegerIndexGen {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero tessellation tri index gen bgl"),
            entries: &[
                // Patch metadata.
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Output index buffer.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Params.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero tessellation tri index gen pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let shader_src = wgsl_tri_index_gen(TRI_INDEX_GEN_WORKGROUP_SIZE_Y);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero tessellation tri index gen shader"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero tessellation tri index gen pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        Self {
            bind_group_layout,
            pipeline,
        }
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    pub fn create_bind_group(
        &self,
        device: &wgpu::Device,
        patch_meta: wgpu::BufferBinding<'_>,
        out_indices: wgpu::BufferBinding<'_>,
        params: wgpu::BufferBinding<'_>,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero tessellation tri index gen bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(patch_meta),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(out_indices),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(params),
                },
            ],
        })
    }

    pub fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        patch_count: u32,
        max_index_count_per_patch: u32,
    ) {
        let chunks_y = max_index_count_per_patch.div_ceil(TRI_INDEX_GEN_WORKGROUP_SIZE_Y);

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aero tessellation tri index gen pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.dispatch_workgroups(patch_count, chunks_y, 1);
    }
}
