//! Domain shader (DS) evaluation compute pass.
//!
//! The DS evaluation pass executes a translated domain shader as a compute shader
//! over all tessellated vertices. It reads per-patch metadata produced by the
//! layout pass (tessellation level, output base, vertex count), computes
//! `SV_DomainLocation` for each vertex, and writes an expanded vertex buffer that
//! can be consumed by a render pipeline.

use crate::runtime::tessellator;

/// Workgroup size in the `y` dimension.
///
/// The DS evaluation dispatch is 2D:
/// - `dispatch.x = patch_count_total`
/// - `dispatch.y = ceil(max_vertex_count_per_patch / DOMAIN_EVAL_WORKGROUP_SIZE_Y)`
///
/// With `@workgroup_size(1, DOMAIN_EVAL_WORKGROUP_SIZE_Y, 1)`, the shader sees:
/// - `global_invocation_id.x = patch_id`
/// - `global_invocation_id.y = local_vertex_index`
pub const DOMAIN_EVAL_WORKGROUP_SIZE_Y: u32 = 64;

pub const DOMAIN_EVAL_INTERNAL_GROUP: u32 = 0;
pub const DOMAIN_EVAL_DOMAIN_GROUP: u32 = 3;

pub const DOMAIN_EVAL_BINDING_PATCH_META: u32 = 0;
pub const DOMAIN_EVAL_BINDING_HS_CONTROL_POINTS: u32 = 1;
pub const DOMAIN_EVAL_BINDING_HS_PATCH_CONSTANTS: u32 = 2;
pub const DOMAIN_EVAL_BINDING_OUT_VERTICES: u32 = 3;

/// Returns the number of `dispatch.y` workgroups required to cover `vertex_count`
/// vertices for a single patch.
pub fn chunk_count_for_vertex_count(vertex_count: u32) -> u32 {
    vertex_count.div_ceil(DOMAIN_EVAL_WORKGROUP_SIZE_Y)
}

fn wgsl_ds_out_struct(out_reg_count: u32) -> String {
    let mut s = String::new();
    s.push_str("struct AeroDsOut {\n");
    for i in 0..out_reg_count {
        s.push_str(&format!("    o{i}: vec4<f32>,\n"));
    }
    s.push_str("};\n");
    s
}

fn wgsl_store_out_regs(out_reg_count: u32) -> String {
    let mut s = String::new();
    for i in 0..out_reg_count {
        s.push_str(&format!(
            "    aero_out_vertices[out_base + {i}u] = out.o{i};\n"
        ));
    }
    s
}

/// Builds WGSL for a triangle-domain integer-partitioning DS evaluation kernel.
///
/// `user_ds_wgsl` must provide a function:
///
/// ```wgsl
/// fn ds_eval(patch_id: u32, domain: vec3<f32>, local_vertex: u32) -> AeroDsOut;
/// ```
///
/// The generated entry point is `cs_main`.
pub fn build_triangle_domain_eval_wgsl(user_ds_wgsl: &str, out_reg_count: u32) -> String {
    assert!(out_reg_count > 0, "out_reg_count must be > 0");

    let ds_out_struct = wgsl_ds_out_struct(out_reg_count);
    let store_out_regs = wgsl_store_out_regs(out_reg_count);

    // NOTE: Control points per patch is fixed to 3 for a triangle domain.
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

 @group({internal_group}) @binding({binding_patch_meta})
 // NOTE: This is bound as `read_write` even though we only read it. wgpu tracks buffer usage at
 // the whole-buffer granularity (not per binding range), and tessellation expansion suballocates
 // multiple logical buffers from a single backing scratch buffer. Binding scratch inputs as
 // `read_write` avoids mixing read-only and read-write storage views of the same underlying buffer
 // in one dispatch.
 var<storage, read_write> aero_patch_meta: array<PatchMeta>;

// HS output control points. Stored as `vec4<f32>` registers.
 @group({internal_group}) @binding({binding_hs_control_points})
 // NOTE: Bound as `read_write` even though we only read it. See note on `aero_patch_meta`.
 var<storage, read_write> aero_hs_control_points: array<vec4<f32>>;

// HS patch constants. Stored as `vec4<f32>` registers.
 @group({internal_group}) @binding({binding_hs_patch_constants})
 // NOTE: Bound as `read_write` even though we only read it. See note on `aero_patch_meta`.
 var<storage, read_write> aero_hs_patch_constants: array<vec4<f32>>;

// Expanded vertex output buffer. Stored as `vec4<f32>` registers with stride:
// `AERO_DS_OUT_REG_COUNT * 16`.
@group({internal_group}) @binding({binding_out_vertices})
var<storage, read_write> aero_out_vertices: array<vec4<f32>>;

const AERO_DS_OUT_REG_COUNT: u32 = {out_reg_count}u;
const AERO_HS_CONTROL_POINTS_PER_PATCH: u32 = 3u;

{ds_out_struct}

{user_ds_wgsl}

@compute @workgroup_size(1, {workgroup_size_y}, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let patch_id = id.x;
    let local_vertex = id.y;
    // NOTE: `meta` is a reserved keyword in WGSL (wgpu 0.20 / naga), so avoid it as an identifier.
    let patch_meta = aero_patch_meta[patch_id];
    if (local_vertex >= patch_meta.vertex_count) {{
        return;
    }}

    let domain = tri_vertex_domain_location(patch_meta.tess_level, local_vertex);
    let out = ds_eval(patch_id, domain, local_vertex);
 
    let out_base = (patch_meta.vertex_base + local_vertex) * AERO_DS_OUT_REG_COUNT;
{store_out_regs}
}}
"#,
        tess_lib = tessellator::wgsl_tri_tessellator_lib_default(),
        internal_group = DOMAIN_EVAL_INTERNAL_GROUP,
        binding_patch_meta = DOMAIN_EVAL_BINDING_PATCH_META,
        binding_hs_control_points = DOMAIN_EVAL_BINDING_HS_CONTROL_POINTS,
        binding_hs_patch_constants = DOMAIN_EVAL_BINDING_HS_PATCH_CONSTANTS,
        binding_out_vertices = DOMAIN_EVAL_BINDING_OUT_VERTICES,
        out_reg_count = out_reg_count,
        ds_out_struct = ds_out_struct,
        user_ds_wgsl = user_ds_wgsl,
        workgroup_size_y = DOMAIN_EVAL_WORKGROUP_SIZE_Y,
        store_out_regs = store_out_regs,
    )
}

/// Cached pipeline state for DS evaluation.
#[derive(Debug)]
pub struct DomainEvalPipeline {
    pipeline: wgpu::ComputePipeline,
    internal_bgl: wgpu::BindGroupLayout,
    empty_bg: wgpu::BindGroup,
}

impl DomainEvalPipeline {
    /// Creates a DS evaluation pipeline.
    ///
    /// `domain_bgl` is the bind group layout for translated DS resources and must
    /// correspond to `@group(3)`.
    pub fn new(
        device: &wgpu::Device,
        shader_module: &wgpu::ShaderModule,
        domain_bgl: &wgpu::BindGroupLayout,
    ) -> Self {
        let internal_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero tess domain eval internal bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: DOMAIN_EVAL_BINDING_PATCH_META,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: DOMAIN_EVAL_BINDING_HS_CONTROL_POINTS,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: DOMAIN_EVAL_BINDING_HS_PATCH_CONSTANTS,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: DOMAIN_EVAL_BINDING_OUT_VERTICES,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero tess domain eval empty bgl"),
            entries: &[],
        });
        let empty_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero tess domain eval empty bg"),
            layout: &empty_bgl,
            entries: &[],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero tess domain eval pipeline layout"),
            bind_group_layouts: &[&internal_bgl, &empty_bgl, &empty_bgl, domain_bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero tess domain eval pipeline"),
            layout: Some(&pipeline_layout),
            module: shader_module,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        Self {
            pipeline,
            internal_bgl,
            empty_bg,
        }
    }

    pub fn create_internal_bind_group(
        &self,
        device: &wgpu::Device,
        patch_meta: wgpu::BufferBinding<'_>,
        hs_control_points: wgpu::BufferBinding<'_>,
        hs_patch_constants: wgpu::BufferBinding<'_>,
        out_vertices: wgpu::BufferBinding<'_>,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero tess domain eval internal bg"),
            layout: &self.internal_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: DOMAIN_EVAL_BINDING_PATCH_META,
                    resource: wgpu::BindingResource::Buffer(patch_meta),
                },
                wgpu::BindGroupEntry {
                    binding: DOMAIN_EVAL_BINDING_HS_CONTROL_POINTS,
                    resource: wgpu::BindingResource::Buffer(hs_control_points),
                },
                wgpu::BindGroupEntry {
                    binding: DOMAIN_EVAL_BINDING_HS_PATCH_CONSTANTS,
                    resource: wgpu::BindingResource::Buffer(hs_patch_constants),
                },
                wgpu::BindGroupEntry {
                    binding: DOMAIN_EVAL_BINDING_OUT_VERTICES,
                    resource: wgpu::BindingResource::Buffer(out_vertices),
                },
            ],
        })
    }

    /// Dispatch the DS evaluation pass.
    ///
    /// `patch_count_total` is the number of patches.
    /// `vertex_chunks_y` is the number of workgroups in the Y dimension
    /// (chunks of `local_vertex_index`).
    pub fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        internal_bind_group: &wgpu::BindGroup,
        domain_bind_group: &wgpu::BindGroup,
        patch_count_total: u32,
        vertex_chunks_y: u32,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aero tess domain eval compute pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(DOMAIN_EVAL_INTERNAL_GROUP, internal_bind_group, &[]);
        // Groups 1/2 are reserved by the binding model (PS/CS). Bind empty groups
        // so the pipeline remains compatible even if validation becomes stricter.
        pass.set_bind_group(1, &self.empty_bg, &[]);
        pass.set_bind_group(2, &self.empty_bg, &[]);
        pass.set_bind_group(DOMAIN_EVAL_DOMAIN_GROUP, domain_bind_group, &[]);
        pass.dispatch_workgroups(patch_count_total, vertex_chunks_y, 1);
    }
}
