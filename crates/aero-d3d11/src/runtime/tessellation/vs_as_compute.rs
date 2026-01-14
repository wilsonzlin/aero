//! Execute a D3D11 vertex shader as a compute shader ("VS-as-compute") for tessellation emulation.
//!
//! This is the first stage of a compute-based tessellation expansion pipeline:
//! - Run the bound vertex shader for every control point in every patch (and every instance),
//! - Write the per-control-point output registers to a scratch buffer for consumption by HS.
//!
//! The compute shader is constructed by *wrapping* the translated vertex shader WGSL:
//! - The original `@vertex fn vs_main(..) -> VsOut` is rewritten into a plain callable function
//!   (`fn aero_vs_impl(..) -> VsOut`), with a thin `@vertex` wrapper retained for validation.
//! - A `@compute fn cs_main(..)` entry point performs IA vertex pulling (and optional index pulling),
//!   invokes `aero_vs_impl`, and writes the resulting output registers to `vs_out_regs`.

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

/// `@binding` number for the index pulling params uniform when VS-as-compute needs indexed draw
/// support.
///
/// VS-as-compute bindings live in [`VERTEX_PULLING_GROUP`] alongside the vertex pulling bindings.
/// This group is shared with D3D11 extended-stage bindings (GS/HS/DS), so internal emulation
/// bindings must live in the internal binding range (`@binding >= BINDING_BASE_INTERNAL`) to avoid
/// collisions with D3D11 register-space bindings.
///
/// Alias of [`crate::runtime::index_pulling::INDEX_PULLING_PARAMS_BINDING`].
pub const VS_AS_COMPUTE_INDEX_PARAMS_BINDING: u32 = INDEX_PULLING_PARAMS_BINDING;

/// `@binding` number for the index buffer word storage when VS-as-compute needs indexed draw
/// support.
///
/// Alias of [`crate::runtime::index_pulling::INDEX_PULLING_BUFFER_BINDING`].
pub const VS_AS_COMPUTE_INDEX_BUFFER_BINDING: u32 = INDEX_PULLING_BUFFER_BINDING;

/// `@binding` number for the output register storage buffer (`vs_out_regs`).
///
/// This is placed immediately after the index pulling bindings so VS-as-compute can optionally
/// include index pulling in the same bind group.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VsAsComputeConfig {
    /// Number of control points per patch (e.g. 3 for `D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST`).
    pub control_point_count: u32,
    /// Number of output registers to write per control point.
    pub out_reg_count: u32,
    /// Output register index that receives `VsOut.pos` (`SV_Position`).
    pub pos_reg: u32,
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
        if self.pos_reg >= self.out_reg_count {
            bail!(
                "VS-as-compute: pos_reg must be < out_reg_count (pos_reg={} out_reg_count={})",
                self.pos_reg,
                self.out_reg_count
            );
        }
        Ok(())
    }
}

/// Compute pipeline for VS-as-compute.
///
/// The pipeline is parameterized by:
/// - A [`VertexPullingLayout`] (IA bindings)
/// - A small [`VsAsComputeConfig`] describing dispatch/output shape
/// - The translated vertex shader WGSL that will be invoked from the compute entry point
#[derive(Debug)]
pub struct VsAsComputePipeline {
    cfg: VsAsComputeConfig,
    empty_bg: wgpu::BindGroup,
    bgl_group3: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl VsAsComputePipeline {
    pub fn new(
        device: &wgpu::Device,
        vertex_pulling: &VertexPullingLayout,
        vs_bgl_group0: &wgpu::BindGroupLayout,
        vs_wgsl: &str,
        cfg: VsAsComputeConfig,
    ) -> Result<Self> {
        cfg.validate()?;

        let wgsl = build_vs_as_compute_wgsl(vertex_pulling, vs_wgsl, cfg)?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 VS-as-compute"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let bgl_group3 = create_vs_as_compute_bind_group_layout(device, vertex_pulling, cfg);

        // The pipeline layout must include group layouts for `0..=VERTEX_PULLING_GROUP`.
        //
        // - `@group(0)`: vertex shader resources (cbuffers/textures/samplers/etc).
        // - `@group(1..=2)`: unused placeholders (reserved by the D3D binding model).
        // - `@group(3)`: vertex pulling + index pulling + vs_out_regs.
        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero-d3d11 VS-as-compute empty bgl"),
            entries: &[],
        });
        let empty_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 VS-as-compute empty bind group"),
            layout: &empty_bgl,
            entries: &[],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d11 VS-as-compute pipeline layout"),
            // VERTEX_PULLING_GROUP is currently 3 (see `binding_model.rs`).
            bind_group_layouts: &[vs_bgl_group0, &empty_bgl, &empty_bgl, &bgl_group3],
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
            empty_bg,
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
    #[allow(clippy::too_many_arguments)]
    pub fn create_bind_group_group3(
        &self,
        device: &wgpu::Device,
        vertex_pulling: &VertexPullingLayout,
        vertex_buffers: &[&wgpu::Buffer],
        ia_uniform: wgpu::BufferBinding<'_>,
        index_params: Option<wgpu::BufferBinding<'_>>,
        index_buffer_words: Option<&wgpu::Buffer>,
        vs_out_regs: &ExpansionScratchAlloc,
    ) -> Result<wgpu::BindGroup> {
        if self.cfg.indexed && (index_params.is_none() || index_buffer_words.is_none()) {
            bail!("VS-as-compute: indexed pipeline requires index_params and index_buffer_words");
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
            resource: wgpu::BindingResource::Buffer(ia_uniform),
        });

        if self.cfg.indexed {
            entries.push(wgpu::BindGroupEntry {
                binding: VS_AS_COMPUTE_INDEX_PARAMS_BINDING,
                resource: wgpu::BindingResource::Buffer(index_params.unwrap()),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: INDEX_PULLING_BUFFER_BINDING,
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
        bind_group_group0: &wgpu::BindGroup,
        bind_group_group3: &wgpu::BindGroup,
    ) -> Result<()> {
        if invocations_per_instance == 0 || instance_count == 0 {
            bail!(
                "VS-as-compute: invalid dispatch size (invocations_per_instance={invocations_per_instance} instance_count={instance_count})"
            );
        }
        if !invocations_per_instance.is_multiple_of(self.cfg.control_point_count) {
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
        pass.set_bind_group(0, bind_group_group0, &[]);
        pass.set_bind_group(1, &self.empty_bg, &[]);
        pass.set_bind_group(2, &self.empty_bg, &[]);
        pass.set_bind_group(VERTEX_PULLING_GROUP, bind_group_group3, &[]);
        pass.dispatch_workgroups(invocations_per_instance, instance_count, 1);
        Ok(())
    }
}

/// Allocate a `vs_out_regs` scratch buffer large enough for the VS-as-compute dispatch.
///
/// The output layout is:
/// `[patch_id_total][control_point_id][out_register]` where each output register is `vec4<f32>`
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
            binding: INDEX_PULLING_PARAMS_BINDING,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(16),
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: INDEX_PULLING_BUFFER_BINDING,
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

fn build_vs_as_compute_wgsl(
    vertex_pulling: &VertexPullingLayout,
    vs_wgsl: &str,
    cfg: VsAsComputeConfig,
) -> Result<String> {
    let vs_in = parse_vs_in_struct(vs_wgsl)?;
    let vs_out = parse_vs_out_struct(vs_wgsl)?;
    let (rewritten_vs, vs_impl_takes_input) = rewrite_vs_wgsl_for_compute(vs_wgsl)?;

    if vs_impl_takes_input && vs_in.is_none() {
        bail!("VS-as-compute: VS takes an input parameter, but WGSL did not define struct VsIn");
    }

    // Validate output registers are in range.
    for field in &vs_out.location_fields {
        if field.location >= cfg.out_reg_count {
            bail!(
                "VS-as-compute: VS output @location({}) is out of range (out_reg_count={})",
                field.location,
                cfg.out_reg_count
            );
        }
    }

    let mut wgsl = String::new();
    wgsl.push_str(&vertex_pulling.wgsl_prelude());

    if cfg.indexed {
        wgsl.push_str(&wgsl_index_pulling_lib(
            VERTEX_PULLING_GROUP,
            INDEX_PULLING_PARAMS_BINDING,
            INDEX_PULLING_BUFFER_BINDING,
        ));
        wgsl.push('\n');
    }

    wgsl.push_str(&format!(
        r#"
// ---- Aero VS-as-compute ----
@group({group}) @binding({out_binding})
var<storage, read_write> vs_out_regs: array<vec4<f32>>;

const CONTROL_POINT_COUNT: u32 = {cp}u;
const OUT_REG_COUNT: u32 = {out_regs}u;
const POS_REG: u32 = {pos_reg}u;
"#,
        group = VERTEX_PULLING_GROUP,
        out_binding = VS_AS_COMPUTE_VS_OUT_REGS_BINDING,
        cp = cfg.control_point_count,
        out_regs = cfg.out_reg_count,
        pos_reg = cfg.pos_reg,
    ));

    // Generate per-location load helpers that match the vertex shader's `VsIn` struct.
    if let Some(vs_in) = &vs_in {
        let mut fields = vs_in.location_fields.clone();
        fields.sort_by_key(|f| f.location);
        for field in &fields {
            let attr = vertex_pulling
                .attributes
                .iter()
                .find(|a| a.shader_location == field.location)
                .ok_or_else(|| {
                    anyhow!(
                        "VS-as-compute: vertex pulling layout missing attribute for VS input @location({})",
                        field.location
                    )
                })?;
            wgsl.push_str(&wgsl_load_attr_fn(attr, field.location, &field.ty)?);
            wgsl.push('\n');
        }
    }

    wgsl.push_str(&rewritten_vs);
    wgsl.push('\n');

    // Compute entry point. Use a 2D dispatch:
    // - gid.x = vertex/index in instance
    // - gid.y = instance in draw
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

    // Ensure deterministic output for registers not written by the VS signature.
    wgsl.push_str(
        r#"
    for (var reg: u32 = 0u; reg < OUT_REG_COUNT; reg = reg + 1u) {
        vs_out_regs[base_out + reg] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
"#,
    );

    // Build `VsIn` and invoke the rewritten VS implementation.
    if vs_impl_takes_input {
        let vs_in = vs_in.as_ref().expect("validated above");
        wgsl.push_str("    var input: VsIn;\n");
        if let Some(name) = &vs_in.vertex_id_field {
            wgsl.push_str(&format!("    input.{name} = u32(vertex_id_i32);\n"));
        }
        if let Some(name) = &vs_in.instance_id_field {
            wgsl.push_str(&format!("    input.{name} = instance_id;\n"));
        }
        let mut fields = vs_in.location_fields.clone();
        fields.sort_by_key(|f| f.location);
        for field in &fields {
            wgsl.push_str(&format!(
                "    input.{field_name} = aero_vp_load_loc{loc}(vertex_id_i32, instance_id);\n",
                field_name = field.name,
                loc = field.location
            ));
        }
        wgsl.push_str("    let out: VsOut = aero_vs_impl(input);\n");
    } else {
        wgsl.push_str("    let out: VsOut = aero_vs_impl();\n");
    }

    wgsl.push_str(&format!(
        "    vs_out_regs[base_out + POS_REG] = out.{};\n",
        vs_out.pos_field
    ));
    for field in &vs_out.location_fields {
        wgsl.push_str(&format!(
            "    vs_out_regs[base_out + {}u] = out.{};\n",
            field.location, field.name
        ));
    }

    wgsl.push_str("}\n");
    Ok(wgsl)
}

#[derive(Debug, Clone)]
struct VsInLocationField {
    location: u32,
    name: String,
    ty: String,
}

#[derive(Debug, Clone, Default)]
struct VsInStructInfo {
    vertex_id_field: Option<String>,
    instance_id_field: Option<String>,
    location_fields: Vec<VsInLocationField>,
}

#[derive(Debug, Clone)]
struct VsOutLocationField {
    location: u32,
    name: String,
}

#[derive(Debug, Clone)]
struct VsOutStructInfo {
    pos_field: String,
    location_fields: Vec<VsOutLocationField>,
}

fn parse_location_attr(line: &str) -> Option<u32> {
    let idx = line.find("@location(")?;
    let rest = &line[idx + "@location(".len()..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

fn parse_struct_member_name_and_type(line: &str) -> Option<(String, String)> {
    // Expect something like:
    // `@location(0) a0: vec4<f32>,`
    // `@builtin(vertex_index) vertex_id: u32,`
    let after = line.split(')').next_back()?.trim();
    let (name, ty) = after.split_once(':')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let ty = ty.trim().trim_end_matches(',').trim();
    if ty.is_empty() {
        return None;
    }
    Some((name.to_owned(), ty.to_owned()))
}

fn parse_vs_in_struct(vs_wgsl: &str) -> Result<Option<VsInStructInfo>> {
    let mut lines = vs_wgsl.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "struct VsIn {" {
            continue;
        }
        let mut info = VsInStructInfo::default();
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed == "};" {
                break;
            }
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.contains("@builtin(vertex_index)") {
                if let Some((name, _ty)) = parse_struct_member_name_and_type(trimmed) {
                    info.vertex_id_field = Some(name);
                }
                continue;
            }
            if trimmed.contains("@builtin(instance_index)") {
                if let Some((name, _ty)) = parse_struct_member_name_and_type(trimmed) {
                    info.instance_id_field = Some(name);
                }
                continue;
            }
            if let Some(loc) = parse_location_attr(trimmed) {
                let Some((name, ty)) = parse_struct_member_name_and_type(trimmed) else {
                    continue;
                };
                info.location_fields.push(VsInLocationField {
                    location: loc,
                    name,
                    ty,
                });
            }
        }
        return Ok(Some(info));
    }
    Ok(None)
}

fn parse_vs_out_struct(vs_wgsl: &str) -> Result<VsOutStructInfo> {
    let mut lines = vs_wgsl.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "struct VsOut {" {
            continue;
        }
        let mut info = VsOutStructInfo {
            pos_field: "pos".to_owned(),
            location_fields: Vec::new(),
        };
        let mut found_pos = false;
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed == "};" {
                break;
            }
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.contains("@builtin(position)") {
                if let Some((name, _ty)) = parse_struct_member_name_and_type(trimmed) {
                    info.pos_field = name;
                    found_pos = true;
                }
                continue;
            }
            if let Some(loc) = parse_location_attr(trimmed) {
                let Some((name, _ty)) = parse_struct_member_name_and_type(trimmed) else {
                    continue;
                };
                info.location_fields.push(VsOutLocationField {
                    location: loc,
                    name,
                });
            }
        }
        if !found_pos {
            bail!("VS-as-compute: WGSL struct VsOut is missing a @builtin(position) member");
        }
        return Ok(info);
    }
    bail!("VS-as-compute: WGSL did not define struct VsOut")
}

fn rewrite_vs_wgsl_for_compute(vs_wgsl: &str) -> Result<(String, bool)> {
    // Rewrite:
    //   @vertex fn vs_main(..) -> VsOut { ... }
    // into:
    //   fn aero_vs_impl(..) -> VsOut { ... }
    //   @vertex fn vs_main(..) -> VsOut { return aero_vs_impl(..); }

    let mut out = String::with_capacity(vs_wgsl.len() + 128);
    let lines: Vec<&str> = vs_wgsl.lines().collect();

    let mut found = false;
    let mut vs_main_sig_line: Option<&str> = None;
    let mut vs_main_has_param = false;
    let mut vs_main_param_name: Option<String> = None;

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if !found && line.trim() == "@vertex" {
            let Some(next) = lines.get(i + 1) else {
                bail!("VS-as-compute: malformed WGSL (dangling @vertex)");
            };
            let next_trim = next.trim_start();
            if next_trim.starts_with("fn vs_main") {
                found = true;
                vs_main_sig_line = Some(next);

                // Parse param presence + name.
                let open = next
                    .find('(')
                    .ok_or_else(|| anyhow!("VS-as-compute: malformed vs_main signature"))?;
                let close = next[open + 1..]
                    .find(')')
                    .map(|p| p + open + 1)
                    .ok_or_else(|| anyhow!("VS-as-compute: malformed vs_main signature"))?;
                let params = next[open + 1..close].trim();
                if !params.is_empty() {
                    vs_main_has_param = true;
                    if let Some((name, _)) = params.split_once(':') {
                        vs_main_param_name = Some(name.trim().to_owned());
                    } else {
                        bail!("VS-as-compute: unexpected vs_main parameter list: {params}");
                    }
                }

                // Drop the `@vertex` attribute for the impl.
                // Rewrite `fn vs_main` -> `fn aero_vs_impl`.
                out.push_str(&next.replacen("fn vs_main", "fn aero_vs_impl", 1));
                out.push('\n');
                i += 2;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
        i += 1;
    }

    if !found {
        bail!("VS-as-compute: failed to find @vertex fn vs_main entry point in WGSL");
    }

    let sig = vs_main_sig_line.expect("found implies signature");

    out.push('\n');
    out.push_str("@vertex\n");
    out.push_str(sig);
    out.push('\n');
    if vs_main_has_param {
        let arg = vs_main_param_name.as_deref().unwrap_or("input");
        out.push_str(&format!("    return aero_vs_impl({arg});\n"));
    } else {
        out.push_str("    return aero_vs_impl();\n");
    }
    out.push_str("}\n");

    Ok((out, vs_main_has_param))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WgslScalarType {
    F32,
    U32,
    I32,
}

impl WgslScalarType {
    fn wgsl(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::U32 => "u32",
            Self::I32 => "i32",
        }
    }

    fn zero(self) -> &'static str {
        match self {
            Self::F32 => "0.0",
            Self::U32 => "0u",
            Self::I32 => "0",
        }
    }

    fn one(self) -> &'static str {
        match self {
            Self::F32 => "1.0",
            Self::U32 => "1u",
            Self::I32 => "1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WgslNumericType {
    scalar: WgslScalarType,
    count: u32,
}

fn parse_wgsl_numeric_type(ty: &str) -> Result<WgslNumericType> {
    let ty = ty.trim();
    let (count, scalar_str) = if matches!(ty, "f32" | "u32" | "i32") {
        (1u32, ty)
    } else if let Some(rest) = ty.strip_prefix("vec") {
        let (count_str, rest) = rest
            .split_once('<')
            .ok_or_else(|| anyhow!("invalid WGSL type syntax: {ty}"))?;
        let count: u32 = count_str
            .parse()
            .map_err(|_| anyhow!("invalid WGSL vector width in type: {ty}"))?;
        let scalar_str = rest
            .strip_suffix('>')
            .ok_or_else(|| anyhow!("invalid WGSL type syntax: {ty}"))?;
        (count, scalar_str.trim())
    } else {
        bail!("VS-as-compute: unsupported WGSL numeric type: {ty}");
    };

    let scalar = match scalar_str {
        "f32" => WgslScalarType::F32,
        "u32" => WgslScalarType::U32,
        "i32" => WgslScalarType::I32,
        other => bail!("VS-as-compute: unsupported WGSL numeric scalar type: {other}"),
    };
    if !(1..=4).contains(&count) {
        bail!("VS-as-compute: unsupported WGSL vector width {count} in type {ty}");
    }
    Ok(WgslNumericType { scalar, count })
}

fn wgsl_type_string(ty: WgslNumericType) -> String {
    if ty.count == 1 {
        ty.scalar.wgsl().to_owned()
    } else {
        format!("vec{}<{}>", ty.count, ty.scalar.wgsl())
    }
}

fn load_fn_for_format(
    component_type: DxgiFormatComponentType,
    requested_count: u32,
) -> (&'static str, u32, WgslScalarType) {
    let requested_count = requested_count.clamp(1, 4);
    match component_type {
        DxgiFormatComponentType::F32 => match requested_count {
            1 => ("load_attr_f32", 1, WgslScalarType::F32),
            2 => ("load_attr_f32x2", 2, WgslScalarType::F32),
            3 => ("load_attr_f32x3", 3, WgslScalarType::F32),
            _ => ("load_attr_f32x4", 4, WgslScalarType::F32),
        },
        DxgiFormatComponentType::F16 => {
            if requested_count <= 2 {
                ("load_attr_f16x2", 2, WgslScalarType::F32)
            } else {
                ("load_attr_f16x4", 4, WgslScalarType::F32)
            }
        }
        DxgiFormatComponentType::U32 => match requested_count {
            1 => ("load_attr_u32", 1, WgslScalarType::U32),
            2 => ("load_attr_u32x2", 2, WgslScalarType::U32),
            3 => ("load_attr_u32x3", 3, WgslScalarType::U32),
            _ => ("load_attr_u32x4", 4, WgslScalarType::U32),
        },
        DxgiFormatComponentType::I32 => match requested_count {
            1 => ("load_attr_i32", 1, WgslScalarType::I32),
            2 => ("load_attr_i32x2", 2, WgslScalarType::I32),
            3 => ("load_attr_i32x3", 3, WgslScalarType::I32),
            _ => ("load_attr_i32x4", 4, WgslScalarType::I32),
        },
        DxgiFormatComponentType::U16 => match requested_count {
            1 => ("load_attr_u16", 1, WgslScalarType::U32),
            2 => ("load_attr_u16x2", 2, WgslScalarType::U32),
            _ => ("load_attr_u16x4", 4, WgslScalarType::U32),
        },
        DxgiFormatComponentType::I16 => match requested_count {
            1 => ("load_attr_i16", 1, WgslScalarType::I32),
            2 => ("load_attr_i16x2", 2, WgslScalarType::I32),
            _ => ("load_attr_i16x4", 4, WgslScalarType::I32),
        },
        DxgiFormatComponentType::U8 => {
            if requested_count <= 2 {
                ("load_attr_u8x2", 2, WgslScalarType::U32)
            } else {
                ("load_attr_u8x4", 4, WgslScalarType::U32)
            }
        }
        DxgiFormatComponentType::I8 => {
            if requested_count <= 2 {
                ("load_attr_i8x2", 2, WgslScalarType::I32)
            } else {
                ("load_attr_i8x4", 4, WgslScalarType::I32)
            }
        }
        DxgiFormatComponentType::Unorm8 => {
            if requested_count <= 2 {
                ("load_attr_unorm8x2", 2, WgslScalarType::F32)
            } else {
                ("load_attr_unorm8x4", 4, WgslScalarType::F32)
            }
        }
        DxgiFormatComponentType::Snorm8 => {
            if requested_count <= 2 {
                ("load_attr_snorm8x2", 2, WgslScalarType::F32)
            } else {
                ("load_attr_snorm8x4", 4, WgslScalarType::F32)
            }
        }
        DxgiFormatComponentType::Unorm16 => {
            if requested_count <= 2 {
                ("load_attr_unorm16x2", 2, WgslScalarType::F32)
            } else {
                ("load_attr_unorm16x4", 4, WgslScalarType::F32)
            }
        }
        DxgiFormatComponentType::Snorm16 => {
            if requested_count <= 2 {
                ("load_attr_snorm16x2", 2, WgslScalarType::F32)
            } else {
                ("load_attr_snorm16x4", 4, WgslScalarType::F32)
            }
        }
        DxgiFormatComponentType::Unorm10_10_10_2 => {
            ("load_attr_unorm10_10_10_2", 4, WgslScalarType::F32)
        }
    }
}

fn extract_expr(var: &str, want_count: u32, have_count: u32) -> Result<String> {
    match (want_count, have_count) {
        (1, 1) => Ok(var.to_owned()),
        (1, 2..=4) => Ok(format!("{var}.x")),
        (2, 2) => Ok(var.to_owned()),
        (2, 3..=4) => Ok(format!("{var}.xy")),
        (3, 3) => Ok(var.to_owned()),
        (3, 4) => Ok(format!("{var}.xyz")),
        (4, 4) => Ok(var.to_owned()),
        _ => {
            bail!("VS-as-compute: cannot extract vec{want_count} from vec{have_count} (var={var})")
        }
    }
}

fn cast_expr(expr: &str, from: WgslScalarType, to: WgslScalarType, count: u32) -> Result<String> {
    if from == to {
        return Ok(expr.to_owned());
    }
    match (from, to) {
        (WgslScalarType::U32 | WgslScalarType::I32, WgslScalarType::F32) => {
            if count == 1 {
                Ok(format!("f32({expr})"))
            } else {
                Ok(format!("vec{count}<f32>({expr})"))
            }
        }
        _ => bail!(
            "VS-as-compute: unsupported type conversion from {:?} to {:?}",
            from,
            to
        ),
    }
}

fn wgsl_load_attr_fn(attr: &VertexPullingAttribute, loc: u32, target_ty: &str) -> Result<String> {
    let target = parse_wgsl_numeric_type(target_ty)?;

    let format_count = attr.format.component_count.clamp(1, 4);
    let data_count = target.count.min(format_count);

    let elem_index_expr = match attr.step_mode {
        wgpu::VertexStepMode::Vertex => "u32(vertex_id)".to_owned(),
        wgpu::VertexStepMode::Instance => {
            let step = attr.instance_step_rate.max(1);
            format!("instance_id / {step}u")
        }
    };

    let (load_fn, load_count, load_scalar) =
        load_fn_for_format(attr.format.component_type, data_count);
    let load_ty = wgsl_type_string(WgslNumericType {
        scalar: load_scalar,
        count: load_count,
    });

    let extracted = extract_expr("v", data_count, load_count)?;
    let casted = cast_expr(&extracted, load_scalar, target.scalar, data_count)?;
    let data_ty = wgsl_type_string(WgslNumericType {
        scalar: target.scalar,
        count: data_count,
    });

    let mut body = String::new();
    body.push_str(&format!(
        "fn aero_vp_load_loc{loc}(vertex_id: i32, instance_id: u32) -> {target_ty} {{\n",
        target_ty = target_ty.trim()
    ));
    body.push_str(&format!(
        "    let slot = aero_vp_ia.slots[{slot}u];\n",
        slot = attr.pulling_slot
    ));
    body.push_str(&format!("    let elem_index: u32 = {elem_index_expr};\n"));
    body.push_str(&format!(
        "    let addr: u32 = slot.base_offset_bytes + elem_index * slot.stride_bytes + {offset}u;\n",
        offset = attr.offset_bytes
    ));
    body.push_str(&format!(
        "    let v: {load_ty} = {load_fn}({slot}u, addr);\n",
        slot = attr.pulling_slot
    ));
    body.push_str(&format!("    let d: {data_ty} = {casted};\n"));

    // Expand to the target type with D3D IA default fill (0,0,0,1).
    let ret = match (target.count, data_count) {
        (1, 1) => "d".to_owned(),
        (2, 1) => format!(
            "vec2<{}>(d, {})",
            target.scalar.wgsl(),
            target.scalar.zero()
        ),
        (2, 2) => "d".to_owned(),
        (3, 1) => format!(
            "vec3<{}>(d, {}, {})",
            target.scalar.wgsl(),
            target.scalar.zero(),
            target.scalar.zero()
        ),
        (3, 2) => format!(
            "vec3<{}>(d.x, d.y, {})",
            target.scalar.wgsl(),
            target.scalar.zero()
        ),
        (3, 3) => "d".to_owned(),
        (4, 1) => format!(
            "vec4<{}>(d, {}, {}, {})",
            target.scalar.wgsl(),
            target.scalar.zero(),
            target.scalar.zero(),
            target.scalar.one()
        ),
        (4, 2) => format!(
            "vec4<{}>(d.x, d.y, {}, {})",
            target.scalar.wgsl(),
            target.scalar.zero(),
            target.scalar.one()
        ),
        (4, 3) => format!(
            "vec4<{}>(d.x, d.y, d.z, {})",
            target.scalar.wgsl(),
            target.scalar.one()
        ),
        (4, 4) => "d".to_owned(),
        _ => bail!(
            "VS-as-compute: unsupported type expansion from vec{data_count} to vec{}",
            target.count
        ),
    };
    body.push_str(&format!("    return {ret};\n"));
    body.push_str("}\n");
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_layout::{
        fnv1a_32, InputLayoutBinding, InputLayoutBlobHeader, InputLayoutDesc,
        InputLayoutElementDxgi, VsInputSignatureElement, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
        AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
    };
    use std::collections::BTreeMap;

    fn assert_wgsl_validates(wgsl: &str) {
        let module = naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("generated WGSL failed to validate");
    }

    const MINIMAL_VS_WGSL: &str = r#"
struct VsIn {
  @location(0) a0: vec4<f32>,
};

struct VsOut {
  @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
  var out: VsOut;
  out.pos = input.a0;
  return out;
}
"#;

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
            required_strides: vec![0; 3],
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

    #[test]
    fn generated_wgsl_parses_without_a_device() {
        // This ensures the WGSL generation is syntactically valid even on platforms where wgpu
        // compute tests are skipped (no adapter / no compute support).
        let layout_bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ilay_pos3_color.bin"
        ));
        let layout = InputLayoutDesc::parse(layout_bytes).expect("fixture ILAY should parse");

        let signature = [VsInputSignatureElement {
            semantic_name_hash: layout.elements[0].semantic_name_hash,
            semantic_index: layout.elements[0].semantic_index,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        }];

        let stride = 28u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling =
            VertexPullingLayout::new(&binding, &signature).expect("build VertexPullingLayout");

        let wgsl = build_vs_as_compute_wgsl(
            &pulling,
            MINIMAL_VS_WGSL,
            VsAsComputeConfig {
                control_point_count: 1,
                out_reg_count: 2,
                pos_reg: 0,
                indexed: false,
            },
        )
        .expect("build WGSL");

        naga::front::wgsl::parse_str(&wgsl).expect("generated VS-as-compute WGSL should parse");
    }

    #[test]
    fn vs_as_compute_uses_unorm10_loader_for_r10g10b10a2_unorm() {
        let fmt = crate::input_layout::dxgi_format_info(24).expect("DXGI_FORMAT_R10G10B10A2_UNORM");
        assert_eq!(fmt.component_type, DxgiFormatComponentType::Unorm10_10_10_2);
        assert_eq!(fmt.component_count, 4);

        let pulling = VertexPullingLayout {
            d3d_slot_to_pulling_slot: BTreeMap::from([(0u32, 0u32)]),
            pulling_slot_to_d3d_slot: vec![0u32],
            required_strides: vec![0u32],
            attributes: vec![VertexPullingAttribute {
                shader_location: 0,
                pulling_slot: 0,
                offset_bytes: 0,
                format: fmt,
                step_mode: wgpu::VertexStepMode::Vertex,
                instance_step_rate: 0,
            }],
        };

        let wgsl = build_vs_as_compute_wgsl(
            &pulling,
            MINIMAL_VS_WGSL,
            VsAsComputeConfig {
                control_point_count: 1,
                out_reg_count: 1,
                pos_reg: 0,
                indexed: false,
            },
        )
        .expect("build WGSL");

        assert!(
            wgsl.contains("load_attr_unorm10_10_10_2(0u, addr)"),
            "expected VS-as-compute WGSL to call load_attr_unorm10_10_10_2 for loc0, got:\n{wgsl}"
        );
    }

    #[test]
    fn vs_as_compute_uses_unorm8x2_loader_for_r8g8_unorm() {
        let fmt = crate::input_layout::dxgi_format_info(49).expect("DXGI_FORMAT_R8G8_UNORM");
        assert_eq!(fmt.component_type, DxgiFormatComponentType::Unorm8);
        assert_eq!(fmt.component_count, 2);

        let pulling = VertexPullingLayout {
            d3d_slot_to_pulling_slot: BTreeMap::from([(0u32, 0u32)]),
            pulling_slot_to_d3d_slot: vec![0u32],
            required_strides: vec![0u32],
            attributes: vec![VertexPullingAttribute {
                shader_location: 0,
                pulling_slot: 0,
                offset_bytes: 0,
                format: fmt,
                step_mode: wgpu::VertexStepMode::Vertex,
                instance_step_rate: 0,
            }],
        };

        let wgsl = build_vs_as_compute_wgsl(
            &pulling,
            MINIMAL_VS_WGSL,
            VsAsComputeConfig {
                control_point_count: 1,
                out_reg_count: 1,
                pos_reg: 0,
                indexed: false,
            },
        )
        .expect("build WGSL");

        assert!(
            wgsl.contains("load_attr_unorm8x2(0u, addr)"),
            "expected VS-as-compute WGSL to call load_attr_unorm8x2 for loc0, got:\n{wgsl}"
        );
    }

    #[test]
    fn wgsl_load_attr_expanded_supports_f16_u16_u32() {
        let tex_hash = fnv1a_32(b"TEXCOORD");

        // Layout: three attributes in one slot with explicit offsets.
        // - loc0: R16G16_FLOAT (F16x2) @ offset 0
        // - loc1: R16_UINT      (U16x1) @ offset 4 (padded to 4 bytes in our format map)
        // - loc2: R32_UINT      (U32x1) @ offset 8
        let layout = InputLayoutDesc {
            header: InputLayoutBlobHeader {
                magic: AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
                version: AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
                element_count: 3,
                flags: 0,
            },
            elements: vec![
                InputLayoutElementDxgi {
                    semantic_name_hash: tex_hash,
                    semantic_index: 0,
                    dxgi_format: 34, // DXGI_FORMAT_R16G16_FLOAT
                    input_slot: 0,
                    aligned_byte_offset: 0,
                    input_slot_class: 0,
                    instance_data_step_rate: 0,
                },
                InputLayoutElementDxgi {
                    semantic_name_hash: tex_hash,
                    semantic_index: 1,
                    dxgi_format: 57, // DXGI_FORMAT_R16_UINT
                    input_slot: 0,
                    aligned_byte_offset: 4,
                    input_slot_class: 0,
                    instance_data_step_rate: 0,
                },
                InputLayoutElementDxgi {
                    semantic_name_hash: tex_hash,
                    semantic_index: 2,
                    dxgi_format: 42, // DXGI_FORMAT_R32_UINT
                    input_slot: 0,
                    aligned_byte_offset: 8,
                    input_slot_class: 0,
                    instance_data_step_rate: 0,
                },
            ],
        };

        let signature = vec![
            VsInputSignatureElement {
                semantic_name_hash: tex_hash,
                semantic_index: 0,
                input_register: 0,
                mask: 0xF,
                shader_location: 0,
            },
            VsInputSignatureElement {
                semantic_name_hash: tex_hash,
                semantic_index: 1,
                input_register: 1,
                mask: 0xF,
                shader_location: 1,
            },
            VsInputSignatureElement {
                semantic_name_hash: tex_hash,
                semantic_index: 2,
                input_register: 2,
                mask: 0xF,
                shader_location: 2,
            },
        ];

        let strides = [12u32]; // 3 dwords
        let binding = InputLayoutBinding::new(&layout, &strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).expect("pulling layout");

        let mut wgsl = pulling.wgsl_prelude();
        for attr in &pulling.attributes {
            wgsl.push_str(
                &wgsl_load_attr_fn(attr, attr.shader_location, "vec4<f32>")
                    .expect("generate load attr"),
            );
        }

        wgsl.push_str(
            r#"
@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    // Call each generated loader so naga validates the full call graph.
    let _a0 = aero_vp_load_loc0(0, 0u);
    let _a1 = aero_vp_load_loc1(0, 0u);
    let _a2 = aero_vp_load_loc2(0, 0u);
    _ = gid;
}
"#,
        );

        assert!(
            wgsl.contains("load_attr_f16x2"),
            "expected WGSL to reference f16 loader: {wgsl}"
        );
        assert!(
            wgsl.contains("load_attr_u16"),
            "expected WGSL to reference u16 loader: {wgsl}"
        );
        assert!(
            wgsl.contains("load_attr_u32"),
            "expected WGSL to reference u32 loader: {wgsl}"
        );

        assert_wgsl_validates(&wgsl);
    }
}
