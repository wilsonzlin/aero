//! Tessellation pipeline state (compute-based HS/DS emulation).
//!
//! WebGPU does not expose hull/domain shader stages. The executor emulates tessellation by running
//! a sequence of compute passes that expand D3D11 patch lists into a flat vertex + index buffer and
//! an indirect draw argument buffer.
//!
//! This module owns the **compute pipeline objects** used by that expansion so they can be cached
//! and reused across draws.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use anyhow::{anyhow, bail, Result};

use crate::binding_model::{BINDING_BASE_INTERNAL, BIND_GROUP_INTERNAL_EMULATION};

use super::layout_pass;
use super::tessellator;
use super::vs_as_compute::{VsAsComputeConfig, VsAsComputePipeline};
use crate::runtime::expansion_scratch::ExpansionScratchAlloc;
use crate::runtime::vertex_pulling::VertexPullingLayout;

const GROUP_INTERNAL: u32 = BIND_GROUP_INTERNAL_EMULATION;
const BIND_INTERNAL: u32 = BINDING_BASE_INTERNAL;

// ---- HS (placeholder) bindings ----
const HS_PARAMS_BINDING: u32 = BIND_INTERNAL;
const HS_VS_OUT_BINDING: u32 = HS_PARAMS_BINDING + 1;
const HS_HS_OUT_BINDING: u32 = HS_PARAMS_BINDING + 2;
const HS_TESS_FACTORS_BINDING: u32 = HS_PARAMS_BINDING + 3;

// ---- Layout pass bindings ----
const LAYOUT_PARAMS_BINDING: u32 = BIND_INTERNAL;
const LAYOUT_HS_TESS_FACTORS_BINDING: u32 = LAYOUT_PARAMS_BINDING + 1;
const LAYOUT_PATCH_META_BINDING: u32 = LAYOUT_PARAMS_BINDING + 2;
const LAYOUT_INDIRECT_ARGS_BINDING: u32 = LAYOUT_PARAMS_BINDING + 3;
const LAYOUT_DEBUG_BINDING: u32 = LAYOUT_PARAMS_BINDING + 4;

// ---- DS (placeholder) bindings ----
const DS_HS_OUT_BINDING: u32 = BIND_INTERNAL;
const DS_PATCH_META_BINDING: u32 = DS_HS_OUT_BINDING + 1;
const DS_OUT_VERTICES_BINDING: u32 = DS_HS_OUT_BINDING + 2;
const DS_OUT_INDICES_BINDING: u32 = DS_HS_OUT_BINDING + 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct VsAsComputePipelineKey {
    vertex_pulling_hash: u64,
    cfg: VsAsComputeConfig,
}

#[derive(Debug)]
pub(crate) struct HsPassthroughPipeline {
    bgl_group3: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

#[derive(Debug)]
pub(crate) struct LayoutPassPipeline {
    bgl_group3: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

#[derive(Debug)]
pub(crate) struct DsPassthroughPipeline {
    bgl_group3: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

#[derive(Debug, Default)]
pub(crate) struct TessellationPipelines {
    vs_as_compute: HashMap<VsAsComputePipelineKey, VsAsComputePipeline>,
    hs_passthrough: Option<HsPassthroughPipeline>,
    layout_pass: Option<LayoutPassPipeline>,
    ds_passthrough: Option<DsPassthroughPipeline>,
}

impl TessellationPipelines {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn vs_as_compute(
        &mut self,
        device: &wgpu::Device,
        vertex_pulling: &VertexPullingLayout,
        cfg: VsAsComputeConfig,
    ) -> Result<&VsAsComputePipeline> {
        // Key the pipeline by the WGSL prelude (which encodes binding numbers + attribute loads)
        // and the small config struct.
        let prelude = vertex_pulling.wgsl_prelude();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        prelude.hash(&mut hasher);
        let key = VsAsComputePipelineKey {
            vertex_pulling_hash: hasher.finish(),
            cfg,
        };

        if let Entry::Vacant(e) = self.vs_as_compute.entry(key) {
            let pipeline = VsAsComputePipeline::new(device, vertex_pulling, cfg)?;
            e.insert(pipeline);
        }

        Ok(self
            .vs_as_compute
            .get(&key)
            .expect("pipeline inserted above"))
    }

    pub(crate) fn hs_passthrough(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<&HsPassthroughPipeline> {
        if self.hs_passthrough.is_none() {
            self.hs_passthrough = Some(HsPassthroughPipeline::new(device)?);
        }
        Ok(self
            .hs_passthrough
            .as_ref()
            .expect("pipeline inserted above"))
    }

    pub(crate) fn layout_pass(&mut self, device: &wgpu::Device) -> Result<&LayoutPassPipeline> {
        if self.layout_pass.is_none() {
            self.layout_pass = Some(LayoutPassPipeline::new(device)?);
        }
        Ok(self.layout_pass.as_ref().expect("pipeline inserted above"))
    }

    pub(crate) fn ds_passthrough(
        &mut self,
        device: &wgpu::Device,
    ) -> Result<&DsPassthroughPipeline> {
        if self.ds_passthrough.is_none() {
            self.ds_passthrough = Some(DsPassthroughPipeline::new(device)?);
        }
        Ok(self
            .ds_passthrough
            .as_ref()
            .expect("pipeline inserted above"))
    }
}

fn create_empty_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aero-d3d11 tessellation empty bgl"),
        entries: &[],
    })
}

fn create_pipeline_layout_group3_only(
    device: &wgpu::Device,
    bgl_group3: &wgpu::BindGroupLayout,
    label: &'static str,
) -> wgpu::PipelineLayout {
    // Group indices 0..2 are reserved for VS/PS/CS resources. Tessellation emulation uses only the
    // internal/emulation group (3) for now, so insert empty layouts for 0..2.
    let empty = create_empty_bgl(device);
    let layouts: [&wgpu::BindGroupLayout; 4] = [&empty, &empty, &empty, bgl_group3];
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &layouts,
        push_constant_ranges: &[],
    })
}

impl HsPassthroughPipeline {
    fn new(device: &wgpu::Device) -> Result<Self> {
        if GROUP_INTERNAL != 3 {
            bail!("tessellation HS passthrough expects internal bind group index 3, got {GROUP_INTERNAL}");
        }

        let wgsl = format!(
            r#"
// ---- Aero tessellation HS pass-through (placeholder) ----
struct HsParams {{
    patch_count: u32,
    control_point_count: u32,
    out_reg_count: u32,
    tess_factor: f32,
}};

@group({group}) @binding({params_binding})
var<uniform> params: HsParams;

// VS outputs, packed as `[patch][control_point][reg]`.
@group({group}) @binding({vs_out_binding})
var<storage, read> vs_out_regs: array<vec4<u32>>;

// HS outputs, packed as `[patch][control_point][reg]`.
@group({group}) @binding({hs_out_binding})
var<storage, read_write> hs_out_regs: array<vec4<u32>>;

// HS patch constants: per patch `vec4<f32>` containing `{{edge0, edge1, edge2, inside}}` tess factors.
@group({group}) @binding({tess_binding})
var<storage, read_write> hs_tess_factors: array<vec4<f32>>;

@compute @workgroup_size(1, 1, 1)
fn hs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let cp_id: u32 = gid.x;
    let patch_id: u32 = gid.y;
    if (patch_id >= params.patch_count) {{
        return;
    }}
    if (cp_id >= params.control_point_count) {{
        return;
    }}

    let base: u32 = (patch_id * params.control_point_count + cp_id) * params.out_reg_count;
    var r: u32 = 0u;
    loop {{
        if (r >= params.out_reg_count) {{
            break;
        }}
        hs_out_regs[base + r] = vs_out_regs[base + r];
        r = r + 1u;
    }}

    if (cp_id == 0u) {{
        let t = params.tess_factor;
        hs_tess_factors[patch_id] = vec4<f32>(t, t, t, t);
    }}
}}
"#,
            group = GROUP_INTERNAL,
            params_binding = HS_PARAMS_BINDING,
            vs_out_binding = HS_VS_OUT_BINDING,
            hs_out_binding = HS_HS_OUT_BINDING,
            tess_binding = HS_TESS_FACTORS_BINDING,
        );

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 tessellation HS passthrough"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let bgl_group3 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero-d3d11 tessellation HS passthrough bgl (group3)"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: HS_PARAMS_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: HS_VS_OUT_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: HS_HS_OUT_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: HS_TESS_FACTORS_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = create_pipeline_layout_group3_only(
            device,
            &bgl_group3,
            "aero-d3d11 tessellation HS passthrough pipeline layout",
        );
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero-d3d11 tessellation HS passthrough pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "hs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        Ok(Self {
            bgl_group3,
            pipeline,
        })
    }

    pub fn pipeline(&self) -> &wgpu::ComputePipeline {
        &self.pipeline
    }

    pub fn create_bind_group_group3(
        &self,
        device: &wgpu::Device,
        params: &ExpansionScratchAlloc,
        vs_out: &ExpansionScratchAlloc,
        hs_out: &ExpansionScratchAlloc,
        hs_tess_factors: &ExpansionScratchAlloc,
    ) -> Result<wgpu::BindGroup> {
        let params_size = wgpu::BufferSize::new(params.size)
            .ok_or_else(|| anyhow!("tessellation HS: params buffer has zero size"))?;
        let vs_out_size = wgpu::BufferSize::new(vs_out.size)
            .ok_or_else(|| anyhow!("tessellation HS: vs_out buffer has zero size"))?;
        let hs_out_size = wgpu::BufferSize::new(hs_out.size)
            .ok_or_else(|| anyhow!("tessellation HS: hs_out buffer has zero size"))?;
        let hs_tess_size = wgpu::BufferSize::new(hs_tess_factors.size)
            .ok_or_else(|| anyhow!("tessellation HS: hs_tess_factors buffer has zero size"))?;

        Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 tessellation HS passthrough bind group (group3)"),
            layout: &self.bgl_group3,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: HS_PARAMS_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: params.buffer.as_ref(),
                        offset: params.offset,
                        size: Some(params_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: HS_VS_OUT_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: vs_out.buffer.as_ref(),
                        offset: vs_out.offset,
                        size: Some(vs_out_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: HS_HS_OUT_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: hs_out.buffer.as_ref(),
                        offset: hs_out.offset,
                        size: Some(hs_out_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: HS_TESS_FACTORS_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: hs_tess_factors.buffer.as_ref(),
                        offset: hs_tess_factors.offset,
                        size: Some(hs_tess_size),
                    }),
                },
            ],
        }))
    }
}

impl LayoutPassPipeline {
    fn new(device: &wgpu::Device) -> Result<Self> {
        if GROUP_INTERNAL != 3 {
            bail!("tessellation layout pass expects internal bind group index 3, got {GROUP_INTERNAL}");
        }

        let wgsl = layout_pass::wgsl_tessellation_layout_pass(
            GROUP_INTERNAL,
            LAYOUT_PARAMS_BINDING,
            LAYOUT_HS_TESS_FACTORS_BINDING,
            LAYOUT_PATCH_META_BINDING,
            LAYOUT_INDIRECT_ARGS_BINDING,
            LAYOUT_DEBUG_BINDING,
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 tessellation layout pass"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let bgl_group3 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero-d3d11 tessellation layout pass bgl (group3)"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: LAYOUT_PARAMS_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: LAYOUT_HS_TESS_FACTORS_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: LAYOUT_PATCH_META_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(20),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: LAYOUT_INDIRECT_ARGS_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(20),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: LAYOUT_DEBUG_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(4),
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = create_pipeline_layout_group3_only(
            device,
            &bgl_group3,
            "aero-d3d11 tessellation layout pass pipeline layout",
        );
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero-d3d11 tessellation layout pass pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        Ok(Self {
            bgl_group3,
            pipeline,
        })
    }

    pub fn pipeline(&self) -> &wgpu::ComputePipeline {
        &self.pipeline
    }

    pub fn create_bind_group_group3(
        &self,
        device: &wgpu::Device,
        params: &ExpansionScratchAlloc,
        hs_tess_factors: &ExpansionScratchAlloc,
        out_patch_meta: &ExpansionScratchAlloc,
        out_indirect: &ExpansionScratchAlloc,
        out_debug: &ExpansionScratchAlloc,
    ) -> Result<wgpu::BindGroup> {
        let params_size = wgpu::BufferSize::new(params.size)
            .ok_or_else(|| anyhow!("tessellation layout pass: params buffer has zero size"))?;
        let tess_size = wgpu::BufferSize::new(hs_tess_factors.size).ok_or_else(|| {
            anyhow!("tessellation layout pass: hs_tess_factors buffer has zero size")
        })?;
        let meta_size = wgpu::BufferSize::new(out_patch_meta.size).ok_or_else(|| {
            anyhow!("tessellation layout pass: out_patch_meta buffer has zero size")
        })?;
        let indirect_size = wgpu::BufferSize::new(out_indirect.size).ok_or_else(|| {
            anyhow!("tessellation layout pass: out_indirect buffer has zero size")
        })?;
        let debug_size = wgpu::BufferSize::new(out_debug.size)
            .ok_or_else(|| anyhow!("tessellation layout pass: out_debug buffer has zero size"))?;

        Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 tessellation layout pass bind group (group3)"),
            layout: &self.bgl_group3,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: LAYOUT_PARAMS_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: params.buffer.as_ref(),
                        offset: params.offset,
                        size: Some(params_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: LAYOUT_HS_TESS_FACTORS_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: hs_tess_factors.buffer.as_ref(),
                        offset: hs_tess_factors.offset,
                        size: Some(tess_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: LAYOUT_PATCH_META_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: out_patch_meta.buffer.as_ref(),
                        offset: out_patch_meta.offset,
                        size: Some(meta_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: LAYOUT_INDIRECT_ARGS_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: out_indirect.buffer.as_ref(),
                        offset: out_indirect.offset,
                        size: Some(indirect_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: LAYOUT_DEBUG_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: out_debug.buffer.as_ref(),
                        offset: out_debug.offset,
                        size: Some(debug_size),
                    }),
                },
            ],
        }))
    }
}

impl DsPassthroughPipeline {
    fn new(device: &wgpu::Device) -> Result<Self> {
        if GROUP_INTERNAL != 3 {
            bail!("tessellation DS passthrough expects internal bind group index 3, got {GROUP_INTERNAL}");
        }

        // Triangle-domain integer partitioning.
        let tess_lib = tessellator::wgsl_tri_tessellator_lib_default();

        // For P0, restrict to the common "triangle patch" case:
        // - 3 control points
        // - 2 output registers (pos + o1), matching `EXPANDED_DRAW_PASSTHROUGH_VS_WGSL`.
        //
        // Future work can extend this to higher-order patches by teaching the placeholder DS how
        // to evaluate additional control points (or by linking the translated DS WGSL).
        let wgsl = format!(
            r#"
{tess_lib}

// ---- Aero tessellation DS expansion (placeholder) ----
struct PatchMeta {{
    tess_level: u32,
    vertex_base: u32,
    index_base: u32,
    vertex_count: u32,
    index_count: u32,
}};

@group({group}) @binding({hs_out_binding})
var<storage, read> hs_out_regs: array<vec4<u32>>;

@group({group}) @binding({patch_meta_binding})
var<storage, read> patch_meta: array<PatchMeta>;

@group({group}) @binding({out_vertices_binding})
var<storage, read_write> out_vertices: array<vec4<u32>>;

@group({group}) @binding({out_indices_binding})
var<storage, read_write> out_indices: array<u32>;

const CONTROL_POINT_COUNT: u32 = 3u;
const OUT_REG_COUNT: u32 = 2u;

fn load_cp_reg(patch_id: u32, cp_id: u32, reg: u32) -> vec4<f32> {{
    let idx = (patch_id * CONTROL_POINT_COUNT + cp_id) * OUT_REG_COUNT + reg;
    return bitcast<vec4<f32>>(hs_out_regs[idx]);
}}

@compute @workgroup_size(1)
fn ds_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let patch_id = gid.x;
    let meta = patch_meta[patch_id];
    if (meta.tess_level == 0u) {{
        return;
    }}

    let p0 = load_cp_reg(patch_id, 0u, 0u);
    let p1 = load_cp_reg(patch_id, 1u, 0u);
    let p2 = load_cp_reg(patch_id, 2u, 0u);

    let c0 = load_cp_reg(patch_id, 0u, 1u);
    let c1 = load_cp_reg(patch_id, 1u, 1u);
    let c2 = load_cp_reg(patch_id, 2u, 1u);

    // Emit vertices.
    var local_v: u32 = 0u;
    loop {{
        if (local_v >= meta.vertex_count) {{
            break;
        }}
        let bary = tri_vertex_domain_location(meta.tess_level, local_v);
        let pos = p0 * bary.x + p1 * bary.y + p2 * bary.z;
        let col = c0 * bary.x + c1 * bary.y + c2 * bary.z;

        let out_base = (meta.vertex_base + local_v) * OUT_REG_COUNT;
        out_vertices[out_base + 0u] = bitcast<vec4<u32>>(pos);
        out_vertices[out_base + 1u] = bitcast<vec4<u32>>(col);

        local_v = local_v + 1u;
    }}

    // Emit indices (triangle list).
    let tri_count: u32 = meta.index_count / 3u;
    var t: u32 = 0u;
    loop {{
        if (t >= tri_count) {{
            break;
        }}
        let tri = tri_index_to_vertex_indices(meta.tess_level, t);
        let base_i = meta.index_base + t * 3u;
        out_indices[base_i + 0u] = meta.vertex_base + tri.x;
        out_indices[base_i + 1u] = meta.vertex_base + tri.y;
        out_indices[base_i + 2u] = meta.vertex_base + tri.z;
        t = t + 1u;
    }}
}}
"#,
            tess_lib = tess_lib,
            group = GROUP_INTERNAL,
            hs_out_binding = DS_HS_OUT_BINDING,
            patch_meta_binding = DS_PATCH_META_BINDING,
            out_vertices_binding = DS_OUT_VERTICES_BINDING,
            out_indices_binding = DS_OUT_INDICES_BINDING,
        );

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 tessellation DS passthrough"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let bgl_group3 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero-d3d11 tessellation DS passthrough bgl (group3)"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: DS_HS_OUT_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: DS_PATCH_META_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(20),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: DS_OUT_VERTICES_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: DS_OUT_INDICES_BINDING,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(4),
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = create_pipeline_layout_group3_only(
            device,
            &bgl_group3,
            "aero-d3d11 tessellation DS passthrough pipeline layout",
        );
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero-d3d11 tessellation DS passthrough pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "ds_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        Ok(Self {
            bgl_group3,
            pipeline,
        })
    }

    pub fn pipeline(&self) -> &wgpu::ComputePipeline {
        &self.pipeline
    }

    pub fn create_bind_group_group3(
        &self,
        device: &wgpu::Device,
        hs_out: &ExpansionScratchAlloc,
        patch_meta: &ExpansionScratchAlloc,
        out_vertices: &ExpansionScratchAlloc,
        out_indices: &ExpansionScratchAlloc,
    ) -> Result<wgpu::BindGroup> {
        let hs_out_size = wgpu::BufferSize::new(hs_out.size)
            .ok_or_else(|| anyhow!("tessellation DS: hs_out buffer has zero size"))?;
        let meta_size = wgpu::BufferSize::new(patch_meta.size)
            .ok_or_else(|| anyhow!("tessellation DS: patch_meta buffer has zero size"))?;
        let out_v_size = wgpu::BufferSize::new(out_vertices.size)
            .ok_or_else(|| anyhow!("tessellation DS: out_vertices buffer has zero size"))?;
        let out_i_size = wgpu::BufferSize::new(out_indices.size)
            .ok_or_else(|| anyhow!("tessellation DS: out_indices buffer has zero size"))?;

        Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 tessellation DS bind group (group3)"),
            layout: &self.bgl_group3,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: DS_HS_OUT_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: hs_out.buffer.as_ref(),
                        offset: hs_out.offset,
                        size: Some(hs_out_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: DS_PATCH_META_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: patch_meta.buffer.as_ref(),
                        offset: patch_meta.offset,
                        size: Some(meta_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: DS_OUT_VERTICES_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: out_vertices.buffer.as_ref(),
                        offset: out_vertices.offset,
                        size: Some(out_v_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: DS_OUT_INDICES_BINDING,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: out_indices.buffer.as_ref(),
                        offset: out_indices.offset,
                        size: Some(out_i_size),
                    }),
                },
            ],
        }))
    }
}
