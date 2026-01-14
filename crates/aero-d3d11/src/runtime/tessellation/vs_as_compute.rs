//! Execute a D3D11 vertex shader as a compute shader ("VS-as-compute") for tessellation emulation.
//!
//! This is the first stage of a compute-based tessellation expansion pipeline:
//! - Run the bound vertex shader for every control point in every patch (and every instance),
//! - Write the per-control-point output registers to a scratch buffer for consumption by HS.
//!
//! The initial implementation here is intentionally a placeholder: it does *not* execute the full
//! translated vertex shader WGSL yet. Instead it establishes:
//! - the binding model (IA vertex pulling + optional index pulling),
//! - the 2D dispatch shape (x = vertex/index in instance, y = instance),
//! - and the output register addressing scheme required by later HS emulation.
//!
//! The placeholder shader simply forwards input locations 0 and 1 into output registers 0 and 1
//! respectively (filling missing components with D3D defaults). This matches the
//! `vs_passthrough.dxbc` fixture used by tests.

use anyhow::{anyhow, bail, Result};

use crate::input_layout::DxgiFormatComponentType;

use crate::runtime::expansion_scratch::{ExpansionScratchAlloc, ExpansionScratchAllocator};
use crate::runtime::index_pulling::{
    wgsl_index_pulling_lib, INDEX_PULLING_BUFFER_BINDING, INDEX_PULLING_PARAMS_BINDING,
};
use crate::runtime::vertex_pulling::{
    VertexPullingAttribute, VertexPullingLayout, VERTEX_PULLING_GROUP,
    VERTEX_PULLING_UNIFORM_BINDING, VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE,
};

/// `@binding` number for [`crate::runtime::index_pulling::IndexPullingParams`] when VS-as-compute
/// needs indexed draw support.
///
/// This lives in [`VERTEX_PULLING_GROUP`] alongside the vertex pulling bindings. Index pulling is
/// bound in the same *internal* binding range as vertex pulling (`@binding >= BINDING_BASE_INTERNAL`)
/// to avoid collisions with D3D11 register-space bindings when sharing bind group 3.
///
/// Alias of [`crate::runtime::index_pulling::INDEX_PULLING_PARAMS_BINDING`].
pub const VS_AS_COMPUTE_INDEX_PARAMS_BINDING: u32 = INDEX_PULLING_PARAMS_BINDING;

/// `@binding` number for the index buffer word storage when VS-as-compute needs indexed draw support.
///
/// Alias of [`crate::runtime::index_pulling::INDEX_PULLING_BUFFER_BINDING`].
pub const VS_AS_COMPUTE_INDEX_BUFFER_BINDING: u32 = INDEX_PULLING_BUFFER_BINDING;

/// `@binding` number for the output register storage buffer (`vs_out_regs`).
///
/// This is placed immediately after the index pulling bindings so it remains disjoint from:
/// - vertex pulling's per-slot vertex buffers (`VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot`)
/// - index pulling's params + index buffer bindings
/// - the D3D11 register-space binding ranges used by guest shaders
pub const VS_AS_COMPUTE_VS_OUT_REGS_BINDING: u32 = VS_AS_COMPUTE_INDEX_BUFFER_BINDING + 1;

fn vs_as_compute_vertex_pulling_binding_numbers(slot_count: u32) -> Vec<u32> {
    // Keep ordering consistent with `VertexPullingLayout::bind_group_layout_entries()`:
    // - vertex buffers in pulling-slot order
    // - uniform last
    let mut out = Vec::with_capacity(slot_count as usize + 1);
    for slot in 0..slot_count {
        out.push(VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot);
    }
    out.push(VERTEX_PULLING_UNIFORM_BINDING);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsAsComputeConfig {
    /// Number of control points per patch (e.g. 3 for `D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST`).
    pub control_point_count: u32,
    /// Number of output registers to write per control point.
    pub out_reg_count: u32,
    /// Whether to use index pulling (`DrawIndexed*`) or vertex-id generation (`Draw*`).
    pub indexed: bool,
}

impl VsAsComputeConfig {
    fn validate(self) -> Result<()> {
        if self.control_point_count == 0 {
            bail!("VS-as-compute: control_point_count must be > 0");
        }
        // D3D11 patchlist topologies are defined for 1..=32 control points.
        if self.control_point_count > 32 {
            bail!("VS-as-compute: control_point_count must be <= 32");
        }
        if self.out_reg_count == 0 {
            bail!("VS-as-compute: out_reg_count must be > 0");
        }
        Ok(())
    }
}

/// Compute pipeline for the VS-as-compute placeholder.
///
/// The pipeline is parameterized by a [`VertexPullingLayout`] (IA bindings) and a small config
/// describing dispatch/output shape.
#[derive(Debug)]
pub struct VsAsComputePipeline {
    cfg: VsAsComputeConfig,
    bgl_group3: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl VsAsComputePipeline {
    pub fn new(
        device: &wgpu::Device,
        vertex_pulling: &VertexPullingLayout,
        cfg: VsAsComputeConfig,
    ) -> Result<Self> {
        cfg.validate()?;

        let wgsl = build_vs_as_compute_passthrough_wgsl(vertex_pulling, cfg);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 VS-as-compute (passthrough)"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let bgl_group3 = create_vs_as_compute_bind_group_layout(device, vertex_pulling, cfg);

        // The pipeline layout must include group layouts for `0..=VERTEX_PULLING_GROUP`.
        // Group 0..2 are reserved for the D3D binding model; for this placeholder pipeline they are
        // empty.
        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero-d3d11 VS-as-compute empty bgl"),
            entries: &[],
        });
        let layouts: [&wgpu::BindGroupLayout; 4] =
            [&empty_bgl, &empty_bgl, &empty_bgl, &bgl_group3];
        debug_assert_eq!(VERTEX_PULLING_GROUP, 3);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d11 VS-as-compute pipeline layout"),
            bind_group_layouts: &layouts,
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero-d3d11 VS-as-compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        Ok(Self {
            cfg,
            bgl_group3,
            pipeline,
        })
    }

    pub fn config(&self) -> VsAsComputeConfig {
        self.cfg
    }

    pub fn bind_group_layout_group3(&self) -> &wgpu::BindGroupLayout {
        &self.bgl_group3
    }

    /// Create the group-3 bind group used by this pipeline.
    ///
    /// - `vertex_buffers` are provided in pulling-slot order, matching
    ///   [`VertexPullingLayout::pulling_slot_to_d3d_slot`].
    /// - `vs_out_regs` is typically an [`ExpansionScratchAlloc`] from [`ExpansionScratchAllocator`].
    pub fn create_bind_group_group3(
        &self,
        device: &wgpu::Device,
        vertex_pulling: &VertexPullingLayout,
        vertex_buffers: &[&wgpu::Buffer],
        ia_uniform: &wgpu::Buffer,
        index_params: Option<&wgpu::Buffer>,
        index_buffer_words: Option<&wgpu::Buffer>,
        vs_out_regs: &ExpansionScratchAlloc,
    ) -> Result<wgpu::BindGroup> {
        if self.cfg.indexed {
            if index_params.is_none() || index_buffer_words.is_none() {
                bail!(
                    "VS-as-compute: indexed pipeline requires index_params and index_buffer_words"
                );
            }
        }

        if vertex_buffers.len() != vertex_pulling.slot_count() as usize {
            bail!(
                "VS-as-compute: vertex_buffers length mismatch (got={} expected={})",
                vertex_buffers.len(),
                vertex_pulling.slot_count()
            );
        }

        let mut entries: Vec<wgpu::BindGroupEntry<'_>> = Vec::new();

        for (slot, buf) in vertex_buffers.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE + slot as u32,
                resource: buf.as_entire_binding(),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: VERTEX_PULLING_UNIFORM_BINDING,
            resource: ia_uniform.as_entire_binding(),
        });

        if self.cfg.indexed {
            entries.push(wgpu::BindGroupEntry {
                binding: VS_AS_COMPUTE_INDEX_PARAMS_BINDING,
                resource: index_params.unwrap().as_entire_binding(),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: VS_AS_COMPUTE_INDEX_BUFFER_BINDING,
                resource: index_buffer_words.unwrap().as_entire_binding(),
            });
        }

        let size = wgpu::BufferSize::new(vs_out_regs.size)
            .ok_or_else(|| anyhow!("VS-as-compute: vs_out_regs allocation has zero size"))?;
        entries.push(wgpu::BindGroupEntry {
            binding: VS_AS_COMPUTE_VS_OUT_REGS_BINDING,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: vs_out_regs.buffer.as_ref(),
                offset: vs_out_regs.offset,
                size: Some(size),
            }),
        });

        Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 VS-as-compute bind group (group3)"),
            layout: &self.bgl_group3,
            entries: &entries,
        }))
    }

    /// Record the compute dispatch.
    ///
    /// `invocations_per_instance` is:
    /// - `vertex_count` for non-indexed draws
    /// - `index_count` for indexed draws
    pub fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        invocations_per_instance: u32,
        instance_count: u32,
        bind_group_group3: &wgpu::BindGroup,
    ) -> Result<()> {
        if invocations_per_instance == 0 || instance_count == 0 {
            bail!(
                "VS-as-compute: invalid dispatch size (invocations_per_instance={invocations_per_instance} instance_count={instance_count})"
            );
        }
        if invocations_per_instance % self.cfg.control_point_count != 0 {
            bail!(
                "VS-as-compute: invocations_per_instance must be a multiple of control_point_count (invocations_per_instance={invocations_per_instance} control_point_count={})",
                self.cfg.control_point_count
            );
        }

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aero-d3d11 VS-as-compute pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(VERTEX_PULLING_GROUP, bind_group_group3, &[]);
        pass.dispatch_workgroups(invocations_per_instance, instance_count, 1);
        Ok(())
    }
}

/// Allocate a `vs_out_regs` scratch buffer large enough for the VS-as-compute dispatch.
///
/// The output layout is:
/// `[patch_id_total][control_point_id][out_register]` where each output register is `vec4<u32>`
/// (16 bytes).
pub fn alloc_vs_out_regs(
    scratch: &mut ExpansionScratchAllocator,
    device: &wgpu::Device,
    invocations_per_instance: u32,
    instance_count: u32,
    out_reg_count: u32,
) -> Result<ExpansionScratchAlloc> {
    if invocations_per_instance == 0 || instance_count == 0 || out_reg_count == 0 {
        bail!(
            "VS-as-compute: invalid output sizing (invocations_per_instance={invocations_per_instance} instance_count={instance_count} out_reg_count={out_reg_count})"
        );
    }

    let invocations_u64 = u64::from(invocations_per_instance)
        .checked_mul(u64::from(instance_count))
        .ok_or_else(|| anyhow!("VS-as-compute: total invocations overflow"))?;
    let regs_u64 = invocations_u64
        .checked_mul(u64::from(out_reg_count))
        .ok_or_else(|| anyhow!("VS-as-compute: output register count overflow"))?;
    let size_bytes = regs_u64
        .checked_mul(16)
        .ok_or_else(|| anyhow!("VS-as-compute: output size overflow"))?;

    scratch
        .alloc_vertex_output(device, size_bytes)
        .map_err(|e| anyhow!("VS-as-compute: scratch alloc failed: {e}"))
}

fn create_vs_as_compute_bind_group_layout(
    device: &wgpu::Device,
    vertex_pulling: &VertexPullingLayout,
    cfg: VsAsComputeConfig,
) -> wgpu::BindGroupLayout {
    // Start with the IA vertex pulling bindings.
    let slot_count = vertex_pulling.slot_count();
    let pulling_bindings = vs_as_compute_vertex_pulling_binding_numbers(slot_count);
    let mut entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::new();
    for slot in 0..slot_count {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: pulling_bindings[slot as usize],
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: *pulling_bindings.last().unwrap(),
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(vertex_pulling.uniform_size_bytes()),
        },
        count: None,
    });

    if cfg.indexed {
        // Index pulling params + index buffer view.
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: VS_AS_COMPUTE_INDEX_PARAMS_BINDING,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(16),
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: VS_AS_COMPUTE_INDEX_BUFFER_BINDING,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }

    // Output registers.
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: VS_AS_COMPUTE_VS_OUT_REGS_BINDING,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    });

    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aero-d3d11 VS-as-compute bind group layout (group3)"),
        entries: &entries,
    })
}

fn build_vs_as_compute_passthrough_wgsl(
    vertex_pulling: &VertexPullingLayout,
    cfg: VsAsComputeConfig,
) -> String {
    // Note: this is a WGSL module containing only the compute entrypoint. The translated D3D vertex
    // shader WGSL is not linked in yet (placeholder pass-through).
    let mut wgsl = String::new();
    wgsl.push_str(&vertex_pulling.wgsl_prelude());

    if cfg.indexed {
        wgsl.push_str(&wgsl_index_pulling_lib(
            VERTEX_PULLING_GROUP,
            VS_AS_COMPUTE_INDEX_PARAMS_BINDING,
            VS_AS_COMPUTE_INDEX_BUFFER_BINDING,
        ));
        wgsl.push('\n');
    }

    wgsl.push_str(&format!(
        r#"
// ---- Aero VS-as-compute (placeholder) ----
@group({group}) @binding({out_binding})
var<storage, read_write> vs_out_regs: array<vec4<u32>>;

const CONTROL_POINT_COUNT: u32 = {cp}u;
const OUT_REG_COUNT: u32 = {out_regs}u;
"#,
        group = VERTEX_PULLING_GROUP,
        out_binding = VS_AS_COMPUTE_VS_OUT_REGS_BINDING,
        cp = cfg.control_point_count,
        out_regs = cfg.out_reg_count,
    ));

    // Generate per-location load helpers.
    for attr in &vertex_pulling.attributes {
        wgsl.push_str(&wgsl_load_attr_expanded_fn(attr));
        wgsl.push('\n');
    }

    // Build the entry point. Use a 2D dispatch:
    // - gid.x = vertex/index in instance
    // - gid.y = instance in draw
    // `num_workgroups` is used as the dispatched x/y sizes (since workgroup_size=1).
    wgsl.push_str(
        r#"
@compute @workgroup_size(1, 1, 1)
fn cs_main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) num_wg: vec3<u32>,
) {
    let vertex_in_instance: u32 = gid.x;
    let instance_in_draw: u32 = gid.y;

    // Flatten `[patch][control_point]` into a linear control-point index. This matches the required
    // layout `[patch_id_total][control_point_id][out_register]` when flattened.
    let patch_count_per_instance: u32 = num_wg.x / CONTROL_POINT_COUNT;
    let patch_id_in_instance: u32 = vertex_in_instance / CONTROL_POINT_COUNT;
    let control_point_id: u32 = vertex_in_instance % CONTROL_POINT_COUNT;
    let patch_id_total: u32 = patch_id_in_instance + instance_in_draw * patch_count_per_instance;
    let base_out: u32 = (patch_id_total * CONTROL_POINT_COUNT + control_point_id) * OUT_REG_COUNT;

    // Resolve vertex + instance IDs for vertex pulling.
    let instance_id: u32 = instance_in_draw + aero_vp_ia.first_instance;
"#,
    );

    if cfg.indexed {
        wgsl.push_str(
            r#"
    let vertex_id_i32: i32 = index_pulling_resolve_vertex_id(vertex_in_instance);
"#,
        );
    } else {
        wgsl.push_str(
            r#"
    let vertex_id_i32: i32 = i32(vertex_in_instance + aero_vp_ia.first_vertex);
"#,
        );
    }

    // Output registers: currently write-through input0->o0 and input1->o1, zeroing the rest.
    //
    // This is enough to validate the binding model and addressing. Later passes will link the
    // translated D3D vertex shader and write its full output register set.
    let loc0 = vertex_pulling
        .attributes
        .iter()
        .any(|a| a.shader_location == 0);
    let loc1 = vertex_pulling
        .attributes
        .iter()
        .any(|a| a.shader_location == 1);

    for reg in 0..cfg.out_reg_count {
        match reg {
            0 => {
                if loc0 {
                    wgsl.push_str(
                        r#"
    vs_out_regs[base_out + 0u] = bitcast<vec4<u32>>(aero_vp_load_loc0(vertex_id_i32, instance_id));
"#,
                    );
                } else {
                    wgsl.push_str(
                        r#"
    vs_out_regs[base_out + 0u] = vec4<u32>(0u, 0u, 0u, 0u);
"#,
                    );
                }
            }
            1 => {
                if loc1 {
                    wgsl.push_str(
                        r#"
    vs_out_regs[base_out + 1u] = bitcast<vec4<u32>>(aero_vp_load_loc1(vertex_id_i32, instance_id));
"#,
                    );
                } else {
                    wgsl.push_str(
                        r#"
    vs_out_regs[base_out + 1u] = vec4<u32>(0u, 0u, 0u, 0u);
"#,
                    );
                }
            }
            other => {
                wgsl.push_str(&format!(
                    r#"
    vs_out_regs[base_out + {other}u] = vec4<u32>(0u, 0u, 0u, 0u);
"#
                ));
            }
        }
    }

    wgsl.push_str("}\n");
    wgsl
}

fn wgsl_load_attr_expanded_fn(attr: &VertexPullingAttribute) -> String {
    // Returns a `vec4<f32>` where missing components are filled with D3D IA defaults.
    //
    // For scalar/vector float formats, D3D fills missing lanes with (0,0,0,1).
    // For UNORM8x4, we already return vec4<f32>.
    let load_expr = match attr.format.component_type {
        DxgiFormatComponentType::F32 => match attr.format.component_count {
            1 => "load_attr_f32".to_owned(),
            2 => "load_attr_f32x2".to_owned(),
            3 => "load_attr_f32x3".to_owned(),
            4 => "load_attr_f32x4".to_owned(),
            _ => "load_attr_f32x4".to_owned(),
        },
        DxgiFormatComponentType::Unorm8 => "load_attr_unorm8x4".to_owned(),
        // TODO: extend vertex pulling prelude for additional DXGI types (F16, U32, U16).
        _ => "load_attr_f32x4".to_owned(),
    };

    let elem_index_expr = match attr.step_mode {
        wgpu::VertexStepMode::Vertex => "u32(vertex_id)".to_owned(),
        wgpu::VertexStepMode::Instance => {
            let step = attr.instance_step_rate.max(1);
            format!("instance_id / {step}u")
        }
    };

    let (load_stmt, expand_stmt) = match (attr.format.component_type, attr.format.component_count) {
        (DxgiFormatComponentType::F32, 1) => (
            format!(
                "let v: f32 = {load_expr}({slot}u, addr);",
                slot = attr.pulling_slot
            ),
            "return vec4<f32>(v, 0.0, 0.0, 1.0);".to_owned(),
        ),
        (DxgiFormatComponentType::F32, 2) => (
            format!(
                "let v: vec2<f32> = {load_expr}({slot}u, addr);",
                slot = attr.pulling_slot
            ),
            "return vec4<f32>(v.x, v.y, 0.0, 1.0);".to_owned(),
        ),
        (DxgiFormatComponentType::F32, 3) => (
            format!(
                "let v: vec3<f32> = {load_expr}({slot}u, addr);",
                slot = attr.pulling_slot
            ),
            "return vec4<f32>(v.x, v.y, v.z, 1.0);".to_owned(),
        ),
        (DxgiFormatComponentType::F32, 4) => (
            format!(
                "let v: vec4<f32> = {load_expr}({slot}u, addr);",
                slot = attr.pulling_slot
            ),
            "return v;".to_owned(),
        ),
        (DxgiFormatComponentType::Unorm8, 4) => (
            format!(
                "let v: vec4<f32> = {load_expr}({slot}u, addr);",
                slot = attr.pulling_slot
            ),
            "return v;".to_owned(),
        ),
        _ => (
            "let v: vec4<f32> = vec4<f32>(0.0);".to_owned(),
            "return v;".to_owned(),
        ),
    };

    format!(
        r#"
fn aero_vp_load_loc{loc}(vertex_id: i32, instance_id: u32) -> vec4<f32> {{
    let slot: AeroVpIaSlot = aero_vp_ia.slots[{slot}u];
    let elem: u32 = {elem_index};
    let addr: u32 = slot.base_offset_bytes + elem * slot.stride_bytes + {offset}u;
    {load_stmt}
    {expand_stmt}
}}
"#,
        loc = attr.shader_location,
        slot = attr.pulling_slot,
        elem_index = elem_index_expr,
        offset = attr.offset_bytes,
        load_stmt = load_stmt,
        expand_stmt = expand_stmt
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn vs_as_compute_vertex_pulling_binding_numbers_match_vertex_pulling_layout() {
        // Construct a minimal `VertexPullingLayout` with 3 pulling slots.
        let mut d3d_slot_to_pulling_slot = BTreeMap::new();
        d3d_slot_to_pulling_slot.insert(0, 0);
        d3d_slot_to_pulling_slot.insert(1, 1);
        d3d_slot_to_pulling_slot.insert(2, 2);

        let pulling = VertexPullingLayout {
            d3d_slot_to_pulling_slot,
            pulling_slot_to_d3d_slot: vec![0, 1, 2],
            attributes: Vec::new(),
        };

        let layout_bindings: Vec<u32> = pulling
            .bind_group_layout_entries()
            .into_iter()
            .map(|e| e.binding)
            .collect();
        let wiring_bindings = vs_as_compute_vertex_pulling_binding_numbers(pulling.slot_count());

        assert_eq!(
            wiring_bindings, layout_bindings,
            "VS-as-compute vertex pulling bindings must match VertexPullingLayout"
        );
    }
}
