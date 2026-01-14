use std::fmt::Write;

use super::fvf::{Fvf, FvfLayout, PositionType, TexCoordSize};
use super::tss::{
    AlphaTestState, CompareFunc, FogState, LightingState, TextureArg, TextureArgFlags,
    TextureArgSource, TextureOp, TextureResultTarget, TextureStageState, TextureTransform,
};

const MAX_TEXTURE_STAGES: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FixedFunctionGlobals {
    /// Column-major matrix used for fixed-function world/view/projection transform.
    pub world_view_proj: [[f32; 4]; 4],
    /// (x, y, width, height) in pixels. Used for XYZRHW vertices.
    pub viewport: [f32; 4],
    /// (alpha_ref, _, _, _).
    pub alpha_test: [f32; 4],
    pub fog_color: [f32; 4],
    /// (fog_start, fog_end, _, _).
    pub fog_params: [f32; 4],
    /// Material diffuse (RGBA).
    pub material_diffuse: [f32; 4],
    /// Material ambient (RGBA). This is treated as an ambient contribution in fixed-function
    /// lighting.
    pub material_ambient: [f32; 4],
    /// Directional light direction (xyz). This matches D3D9's `D3DLIGHT9::Direction` convention:
    /// direction *of* the light rays. The shader uses `-light_dir` in the Lambert dot product.
    pub light_dir: [f32; 4],
    /// Directional light diffuse color (RGBA).
    pub light_color: [f32; 4],
    /// (lighting_enabled, light0_enabled, _, _).
    pub lighting_flags: [u32; 4],
    /// `D3DRS_TEXTUREFACTOR` converted to linear RGBA floats.
    pub texture_factor: [f32; 4],
    /// `D3DTSS_CONSTANT` values for stages 0..7.
    pub stage_constants: [[f32; 4]; MAX_TEXTURE_STAGES],
    /// `D3DTS_TEXTUREn` matrices for stages 0..7 (column-major).
    pub texture_transforms: [[[f32; 4]; 4]; MAX_TEXTURE_STAGES],
}

impl FixedFunctionGlobals {
    pub fn identity() -> Self {
        let identity_mat = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        Self {
            world_view_proj: identity_mat,
            viewport: [0.0, 0.0, 1.0, 1.0],
            alpha_test: [0.0, 0.0, 0.0, 0.0],
            fog_color: [0.0, 0.0, 0.0, 0.0],
            fog_params: [0.0, 1.0, 0.0, 0.0],
            material_diffuse: [1.0, 1.0, 1.0, 1.0],
            material_ambient: [0.0, 0.0, 0.0, 0.0],
            light_dir: [0.0, 0.0, -1.0, 0.0],
            light_color: [1.0, 1.0, 1.0, 1.0],
            lighting_flags: [0, 0, 0, 0],
            texture_factor: [1.0, 1.0, 1.0, 1.0],
            stage_constants: [[0.0, 0.0, 0.0, 0.0]; MAX_TEXTURE_STAGES],
            texture_transforms: [identity_mat; MAX_TEXTURE_STAGES],
        }
    }

    /// Raw bytes suitable for uploading into a `wgpu` uniform buffer.
    ///
    /// This is safe because `FixedFunctionGlobals` is `#[repr(C)]` and only
    /// contains plain old data (`f32`/`u32` arrays).
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                (self as *const Self).cast::<u8>(),
                std::mem::size_of::<Self>(),
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedFunctionShaderDesc {
    pub fvf: Fvf,
    pub stages: [TextureStageState; MAX_TEXTURE_STAGES],
    pub alpha_test: AlphaTestState,
    pub fog: FogState,
    pub lighting: LightingState,
}

impl FixedFunctionShaderDesc {
    /// Deterministic 64-bit hash used for shader caching.
    pub fn state_hash(&self) -> u64 {
        let mut hash = FNV1A_OFFSET_BASIS;
        fn write_u8(hash: &mut u64, v: u8) {
            *hash ^= v as u64;
            *hash = hash.wrapping_mul(FNV1A_PRIME);
        }

        fn write_u32(hash: &mut u64, v: u32) {
            for b in v.to_le_bytes() {
                write_u8(hash, b);
            }
        }

        write_u32(&mut hash, self.fvf.0);

        fn write_tex_arg(hash: &mut u64, arg: TextureArg) {
            write_u8(hash, arg.source as u8);
            write_u8(hash, arg.flags.bits());
        }

        fn write_stage(hash: &mut u64, stage: &TextureStageState) {
            write_u8(hash, stage.color_op as u8);
            // Hash only args that can affect shader generation. For unused args, write a fixed
            // placeholder so irrelevant state changes do not cause cache misses.
            let color_mask = op_arg_mask(stage.color_op, Component::Rgb);
            write_tex_arg(
                hash,
                if (color_mask & ARG0_MASK) != 0 {
                    stage.color_arg0
                } else {
                    TextureArg::Current
                },
            );
            write_tex_arg(
                hash,
                if (color_mask & ARG1_MASK) != 0 {
                    stage.color_arg1
                } else {
                    TextureArg::Current
                },
            );
            write_tex_arg(
                hash,
                if (color_mask & ARG2_MASK) != 0 {
                    stage.color_arg2
                } else {
                    TextureArg::Current
                },
            );

            write_u8(hash, stage.alpha_op as u8);
            let alpha_mask = op_arg_mask(stage.alpha_op, Component::Alpha);
            write_tex_arg(
                hash,
                if (alpha_mask & ARG0_MASK) != 0 {
                    stage.alpha_arg0
                } else {
                    TextureArg::Current
                },
            );
            write_tex_arg(
                hash,
                if (alpha_mask & ARG1_MASK) != 0 {
                    stage.alpha_arg1
                } else {
                    TextureArg::Current
                },
            );
            write_tex_arg(
                hash,
                if (alpha_mask & ARG2_MASK) != 0 {
                    stage.alpha_arg2
                } else {
                    TextureArg::Current
                },
            );

            // Only hash texture coordinate state when it can affect shader generation (i.e. when
            // this stage actually samples from `D3DTA_TEXTURE`, either explicitly via an arg or
            // implicitly via the op).
            if stage_uses_texture(stage) {
                write_u8(hash, stage.texcoord_index.unwrap_or(0xFF));
                write_u8(hash, stage.texture_transform as u8);
            } else {
                write_u8(hash, 0xFF);
                write_u8(hash, TextureTransform::Disable as u8);
            }
            write_u8(hash, stage.result_target as u8);
        }

        // `TEMP` is only meaningful when later stages actually *read* it. If nothing reads TEMP,
        // then writes to TEMP are dead and don't affect shader generation (we can omit them).
        let temp_is_ever_read = shader_reads_temp(self);

        // D3D9 fixed-function stage disabling is keyed off COLOROP: if stage N has
        // `D3DTSS_COLOROP = D3DTOP_DISABLE`, that stage and all subsequent stages are disabled.
        // Hash only stages that can affect output.
        for stage in &self.stages {
            if stage.color_op == TextureOp::Disable {
                break;
            }
            if !temp_is_ever_read && stage.result_target == TextureResultTarget::Temp {
                continue;
            }
            write_stage(&mut hash, stage);
        }

        // Alpha testing is only observable when enabled *and* the compare func can actually
        // discard fragments. `ALWAYS` is equivalent to alpha test disabled, so normalize it away
        // for hashing purposes (avoids shader-cache misses for no-op state changes).
        let alpha_test_effective =
            self.alpha_test.enabled && self.alpha_test.func != CompareFunc::Always;
        write_u8(&mut hash, alpha_test_effective as u8);
        // Only hash the alpha-test compare function when alpha testing is actually effective.
        write_u8(
            &mut hash,
            if alpha_test_effective {
                self.alpha_test.func as u8
            } else {
                CompareFunc::Always as u8
            },
        );

        write_u8(&mut hash, self.fog.enabled as u8);
        write_u8(&mut hash, self.lighting.enabled as u8);

        hash
    }
}

pub struct GeneratedFixedFunctionShaders {
    pub hash: u64,
    pub vertex_wgsl: String,
    pub fragment_wgsl: String,
    pub fvf_layout: FvfLayout,
}

impl GeneratedFixedFunctionShaders {
    pub fn vertex_buffer_layout(&self) -> wgpu::VertexBufferLayout<'_> {
        wgpu::VertexBufferLayout {
            array_stride: self.fvf_layout.vertex_stride,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &self.fvf_layout.vertex_attributes,
        }
    }
}

const FNV1A_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV1A_PRIME: u64 = 0x00000100000001B3;

pub fn generate_fixed_function_shaders(
    desc: &FixedFunctionShaderDesc,
) -> GeneratedFixedFunctionShaders {
    let fvf_layout =
        FvfLayout::new(desc.fvf).expect("FVF layout must be valid for shader generation");
    let hash = desc.state_hash();

    let vertex_wgsl = generate_vertex_wgsl(desc, &fvf_layout);
    let fragment_wgsl = generate_fragment_wgsl(desc, &fvf_layout);

    GeneratedFixedFunctionShaders {
        hash,
        vertex_wgsl,
        fragment_wgsl,
        fvf_layout,
    }
}

fn generate_vertex_wgsl(desc: &FixedFunctionShaderDesc, layout: &FvfLayout) -> String {
    let mut wgsl = String::new();

    wgsl.push_str(WGSL_SHARED);

    wgsl.push_str("struct VertexIn {\n");
    let mut attr_index = 0usize;

    // position
    {
        let attr = &layout.vertex_attributes[attr_index];
        attr_index += 1;
        let ty = wgsl_vertex_input_type(attr.format);
        let _ = writeln!(
            wgsl,
            "  @location({}) position: {},",
            attr.shader_location, ty
        );
    }

    if layout.has_normal {
        let attr = &layout.vertex_attributes[attr_index];
        attr_index += 1;
        let ty = wgsl_vertex_input_type(attr.format);
        let _ = writeln!(
            wgsl,
            "  @location({}) normal: {},",
            attr.shader_location, ty
        );
    }

    if layout.has_diffuse {
        let attr = &layout.vertex_attributes[attr_index];
        attr_index += 1;
        let ty = wgsl_vertex_input_type(attr.format);
        let _ = writeln!(
            wgsl,
            "  @location({}) diffuse: {},",
            attr.shader_location, ty
        );
    }

    if layout.has_specular {
        let attr = &layout.vertex_attributes[attr_index];
        attr_index += 1;
        let ty = wgsl_vertex_input_type(attr.format);
        let _ = writeln!(
            wgsl,
            "  @location({}) specular: {},",
            attr.shader_location, ty
        );
    }

    for (i, size) in layout.texcoords.iter().enumerate() {
        let attr = &layout.vertex_attributes[attr_index];
        attr_index += 1;
        let ty = match size {
            TexCoordSize::One => "f32",
            TexCoordSize::Two => "vec2<f32>",
            TexCoordSize::Three => "vec3<f32>",
            TexCoordSize::Four => "vec4<f32>",
        };
        let _ = writeln!(
            wgsl,
            "  @location({}) tex{}: {},",
            attr.shader_location, i, ty
        );
    }
    wgsl.push_str("}\n\n");

    wgsl.push_str("struct VertexOut {\n  @builtin(position) position: vec4<f32>,\n  @location(0) diffuse: vec4<f32>,\n");
    wgsl.push_str("  @location(1) specular: vec4<f32>,\n");

    let mut out_location = 2u32;
    for i in 0..layout.texcoords.len() {
        let _ = writeln!(wgsl, "  @location({}) tex{}: vec4<f32>,", out_location, i);
        out_location += 1;
    }

    if desc.fog.enabled {
        let _ = writeln!(wgsl, "  @location({}) fog_factor: f32,", out_location);
    }

    wgsl.push_str("}\n\n");

    wgsl.push_str("@vertex\nfn vs_main(input: VertexIn) -> VertexOut {\n  var out: VertexOut;\n");

    // Diffuse color
    if layout.has_diffuse {
        wgsl.push_str("  let base_diffuse = d3dcolor_bgra_to_rgba(input.diffuse);\n");
    } else {
        wgsl.push_str("  let base_diffuse = globals.material_diffuse;\n");
    }

    if desc.lighting.enabled && layout.has_normal && layout.position == PositionType::Xyz {
        wgsl.push_str("  let n = normalize(input.normal);\n");
        wgsl.push_str("  let light_enabled = f32(globals.lighting_flags.y);\n");
        wgsl.push_str(
            "  let lambert = max(dot(n, -globals.light_dir.xyz), 0.0) * light_enabled;\n",
        );
        wgsl.push_str("  let lit_rgb = clamp(globals.material_ambient.rgb + (base_diffuse.rgb * globals.light_color.rgb * lambert), vec3<f32>(0.0), vec3<f32>(1.0));\n");
        wgsl.push_str("  out.diffuse = vec4<f32>(lit_rgb, base_diffuse.a);\n");
    } else {
        wgsl.push_str("  out.diffuse = base_diffuse;\n");
    }

    if layout.has_specular {
        wgsl.push_str("  out.specular = d3dcolor_bgra_to_rgba(input.specular);\n");
    } else {
        wgsl.push_str("  out.specular = vec4<f32>(0.0, 0.0, 0.0, 0.0);\n");
    }

    // Texcoords
    for (i, size) in layout.texcoords.iter().enumerate() {
        let expr = match size {
            TexCoordSize::One => format!("vec4<f32>(input.tex{}, 0.0, 0.0, 1.0)", i),
            TexCoordSize::Two => format!("vec4<f32>(input.tex{}, 0.0, 1.0)", i),
            TexCoordSize::Three => format!("vec4<f32>(input.tex{}, 1.0)", i),
            TexCoordSize::Four => format!("input.tex{}", i),
        };
        let _ = writeln!(wgsl, "  out.tex{} = {};", i, expr);
    }

    match layout.position {
        PositionType::Xyz => {
            wgsl.push_str(
                "  out.position = globals.world_view_proj * vec4<f32>(input.position, 1.0);\n",
            );
        }
        PositionType::XyzRhw => {
            // D3D9 XYZRHW provides screen-space x/y in pixels and `rhw = 1/w`. Convert back to
            // clip-space so WebGPU can perform perspective-correct interpolation.
            wgsl.push_str(
                "  let w = select(1.0, 1.0 / input.position.w, input.position.w != 0.0);\n",
            );
            wgsl.push_str("  let vp = globals.viewport;\n");
            wgsl.push_str("  let ndc_x = ((input.position.x - vp.x) / vp.z) * 2.0 - 1.0;\n");
            wgsl.push_str("  let ndc_y = 1.0 - ((input.position.y - vp.y) / vp.w) * 2.0;\n");
            wgsl.push_str("  let ndc_z = input.position.z;\n");
            wgsl.push_str("  out.position = vec4<f32>(ndc_x * w, ndc_y * w, ndc_z * w, w);\n");
        }
    }

    if desc.fog.enabled {
        // Approximate fog depth using post-projection z.
        wgsl.push_str("  let z = out.position.z / out.position.w;\n");
        wgsl.push_str(
            "  let fog_start = globals.fog_params.x;\n  let fog_end = globals.fog_params.y;\n",
        );
        wgsl.push_str("  out.fog_factor = clamp((fog_end - z) / max(fog_end - fog_start, 0.00001), 0.0, 1.0);\n");
    }

    wgsl.push_str("  return out;\n}\n");
    wgsl
}

fn generate_fragment_wgsl(desc: &FixedFunctionShaderDesc, layout: &FvfLayout) -> String {
    let mut wgsl = String::new();
    wgsl.push_str(WGSL_SHARED);

    for stage in 0..MAX_TEXTURE_STAGES {
        let _ = writeln!(
            wgsl,
            "@group(1) @binding({}) var tex{}: texture_2d<f32>;\n@group(1) @binding({}) var samp{}: sampler;\n",
            stage * 2,
            stage,
            stage * 2 + 1,
            stage,
        );
    }

    wgsl.push_str("struct FragmentIn {\n  @location(0) diffuse: vec4<f32>,\n  @location(1) specular: vec4<f32>,\n");
    let mut location = 2u32;
    for i in 0..layout.texcoords.len() {
        let _ = writeln!(wgsl, "  @location({}) tex{}: vec4<f32>,", location, i);
        location += 1;
    }

    if desc.fog.enabled {
        let _ = writeln!(wgsl, "  @location({}) fog_factor: f32,", location);
    }

    wgsl.push_str("}\n\n");

    wgsl.push_str("@fragment\nfn fs_main(input: FragmentIn) -> @location(0) vec4<f32> {\n");

    let temp_is_ever_read = shader_reads_temp(desc);

    wgsl.push_str("  var current = input.diffuse;\n");
    if temp_is_ever_read {
        wgsl.push_str("  var temp = current;\n");
    }
    // D3D9 stage disabling: `D3DTOP_DISABLE` on stage N disables stage N and all subsequent
    // stages. D3D9 stage disabling is keyed off COLOROP: if `D3DTOP_DISABLE` is set on stage N,
    // that stage and all subsequent stages are disabled.
    for (stage_index, stage) in desc.stages.iter().enumerate() {
        if stage.color_op == TextureOp::Disable {
            break;
        }
        if !temp_is_ever_read && stage.result_target == TextureResultTarget::Temp {
            continue;
        }
        emit_tss_stage(&mut wgsl, desc, layout, stage_index, stage);
    }

    if desc.alpha_test.enabled && desc.alpha_test.func != CompareFunc::Always {
        let cond = wgsl_compare_func(desc.alpha_test.func, "current.a", "globals.alpha_test.x");
        let _ = writeln!(wgsl, "  if (!({})) {{ discard; }}", cond);
    }

    if desc.fog.enabled {
        wgsl.push_str("  current = vec4<f32>(mix(globals.fog_color.rgb, current.rgb, input.fog_factor), current.a);\n");
    }

    wgsl.push_str("  return current;\n}\n");
    wgsl
}

fn emit_tss_stage(
    wgsl: &mut String,
    desc: &FixedFunctionShaderDesc,
    layout: &FvfLayout,
    stage_index: usize,
    stage: &TextureStageState,
) {
    // In D3D9, `D3DTOP_DISABLE` on stage N disables that stage and all subsequent stages.
    if stage.color_op == TextureOp::Disable && stage.alpha_op == TextureOp::Disable {
        return;
    }

    let tex_var = format!("tex{}_color", stage_index);
    assert!(
        stage_index < MAX_TEXTURE_STAGES,
        "stage_index {stage_index} out of range"
    );
    let tex_name = format!("tex{}", stage_index);
    let samp_name = format!("samp{}", stage_index);

    if stage_uses_texture(stage) {
        // Default texcoord mapping: TEXCOORDn feeds stage n. This can be overridden with
        // `D3DTSS_TEXCOORDINDEX`; we only support the common "pass-through another set of
        // texcoords" behavior here.
        //
        // If the vertex format provides fewer sets than requested, fall back to TEXCOORD0 (common
        // for UI that reuses the same UVs).
        let texcoord_index = stage.texcoord_index.unwrap_or(stage_index as u8) as usize;
        let tc_expr = if layout.texcoords.is_empty() {
            "vec4<f32>(0.0, 0.0, 0.0, 1.0)".to_string()
        } else if texcoord_index < layout.texcoords.len() {
            format!("input.tex{}", texcoord_index)
        } else {
            "input.tex0".to_string()
        };
        match stage.texture_transform {
            TextureTransform::Disable => {
                let uv_expr = format!("{}.xy", tc_expr);
                let _ = writeln!(
                    wgsl,
                    "  let {} = textureSample({}, {}, {});",
                    tex_var, tex_name, samp_name, uv_expr
                );
            }
            TextureTransform::Count2 => {
                let tc_var = format!("tc{}_xform", stage_index);
                let _ = writeln!(
                    wgsl,
                    "  let {} = globals.texture_transforms[{}] * {};",
                    tc_var, stage_index, tc_expr
                );
                let _ = writeln!(
                    wgsl,
                    "  let {} = textureSample({}, {}, {}.xy);",
                    tex_var, tex_name, samp_name, tc_var
                );
            }
            TextureTransform::Count2Projected => {
                let tc_var = format!("tc{}_xform", stage_index);
                let w_var = format!("tc{}_w", stage_index);
                let _ = writeln!(
                    wgsl,
                    "  let {} = globals.texture_transforms[{}] * {};",
                    tc_var, stage_index, tc_expr
                );
                // Avoid NaNs when w==0.
                let _ = writeln!(
                    wgsl,
                    "  let {} = select(1.0, {}.w, {}.w != 0.0);",
                    w_var, tc_var, tc_var
                );
                let _ = writeln!(
                    wgsl,
                    "  let {} = textureSample({}, {}, {}.xy / {});",
                    tex_var, tex_name, samp_name, tc_var, w_var
                );
            }
        }
    } else {
        let _ = writeln!(wgsl, "  let {} = vec4<f32>(1.0, 1.0, 1.0, 1.0);", tex_var);
    }

    let color_arg0 = wgsl_arg_component(stage.color_arg0, stage_index, Component::Rgb);
    let color_arg1 = wgsl_arg_component(stage.color_arg1, stage_index, Component::Rgb);
    let color_arg2 = wgsl_arg_component(stage.color_arg2, stage_index, Component::Rgb);
    let alpha_arg0 = wgsl_arg_component(stage.alpha_arg0, stage_index, Component::Alpha);
    let alpha_arg1 = wgsl_arg_component(stage.alpha_arg1, stage_index, Component::Alpha);
    let alpha_arg2 = wgsl_arg_component(stage.alpha_arg2, stage_index, Component::Alpha);

    let rgb_raw = wgsl_op_expr(
        stage.color_op,
        stage_index,
        &color_arg0,
        &color_arg1,
        &color_arg2,
        Component::Rgb,
    );
    let a_raw = wgsl_op_expr(
        stage.alpha_op,
        stage_index,
        &alpha_arg0,
        &alpha_arg1,
        &alpha_arg2,
        Component::Alpha,
    );

    wgsl.push_str("  {\n");
    let _ = writeln!(wgsl, "    let rgb_raw = {};", rgb_raw);
    let _ = writeln!(wgsl, "    let a_raw = {};", a_raw);
    wgsl.push_str("    let rgb = clamp(rgb_raw, vec3<f32>(0.0), vec3<f32>(1.0));\n");
    wgsl.push_str("    let a = clamp(a_raw, 0.0, 1.0);\n");
    let dst_var = match stage.result_target {
        TextureResultTarget::Current => "current",
        TextureResultTarget::Temp => "temp",
    };
    let _ = writeln!(wgsl, "    {} = vec4<f32>(rgb, a);", dst_var);
    wgsl.push_str("  }\n");

    if desc.fog.enabled {
        // Fog mixes after the full texture pipeline; handled after all stages.
    }
}

fn stage_uses_texture(stage: &TextureStageState) -> bool {
    // Some ops implicitly consume the current stage texture even if none of the args explicitly
    // reference `D3DTA_TEXTURE` (e.g. `BLENDTEXTUREALPHA` uses texture alpha as its interpolant).
    let implicit_texture =
        op_implicitly_uses_texture(stage.color_op) || op_implicitly_uses_texture(stage.alpha_op);
    if implicit_texture {
        return true;
    }

    let color_mask = op_arg_mask(stage.color_op, Component::Rgb);
    if (color_mask & ARG0_MASK) != 0 && stage.color_arg0.source == TextureArgSource::Texture {
        return true;
    }
    if (color_mask & ARG1_MASK) != 0 && stage.color_arg1.source == TextureArgSource::Texture {
        return true;
    }
    if (color_mask & ARG2_MASK) != 0 && stage.color_arg2.source == TextureArgSource::Texture {
        return true;
    }

    let alpha_mask = op_arg_mask(stage.alpha_op, Component::Alpha);
    if (alpha_mask & ARG0_MASK) != 0 && stage.alpha_arg0.source == TextureArgSource::Texture {
        return true;
    }
    if (alpha_mask & ARG1_MASK) != 0 && stage.alpha_arg1.source == TextureArgSource::Texture {
        return true;
    }
    if (alpha_mask & ARG2_MASK) != 0 && stage.alpha_arg2.source == TextureArgSource::Texture {
        return true;
    }

    false
}

fn stage_reads_temp(stage: &TextureStageState) -> bool {
    let color_mask = op_arg_mask(stage.color_op, Component::Rgb);
    if (color_mask & ARG0_MASK) != 0 && stage.color_arg0.source == TextureArgSource::Temp {
        return true;
    }
    if (color_mask & ARG1_MASK) != 0 && stage.color_arg1.source == TextureArgSource::Temp {
        return true;
    }
    if (color_mask & ARG2_MASK) != 0 && stage.color_arg2.source == TextureArgSource::Temp {
        return true;
    }

    let alpha_mask = op_arg_mask(stage.alpha_op, Component::Alpha);
    if (alpha_mask & ARG0_MASK) != 0 && stage.alpha_arg0.source == TextureArgSource::Temp {
        return true;
    }
    if (alpha_mask & ARG1_MASK) != 0 && stage.alpha_arg1.source == TextureArgSource::Temp {
        return true;
    }
    if (alpha_mask & ARG2_MASK) != 0 && stage.alpha_arg2.source == TextureArgSource::Temp {
        return true;
    }

    false
}

fn shader_reads_temp(desc: &FixedFunctionShaderDesc) -> bool {
    // Respect D3D9 stage disabling semantics (COLOROP disables the stage and all subsequent ones).
    for stage in &desc.stages {
        if stage.color_op == TextureOp::Disable {
            break;
        }
        if stage_reads_temp(stage) {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Component {
    Rgb,
    Alpha,
}

const ARG0_MASK: u8 = 1 << 0;
const ARG1_MASK: u8 = 1 << 1;
const ARG2_MASK: u8 = 1 << 2;

fn op_arg_mask(op: TextureOp, component: Component) -> u8 {
    match op {
        TextureOp::Disable => 0,
        TextureOp::SelectArg1 => ARG1_MASK,
        TextureOp::SelectArg2 => ARG2_MASK,
        TextureOp::MultiplyAdd | TextureOp::Lerp => ARG0_MASK | ARG1_MASK | ARG2_MASK,
        TextureOp::DotProduct3 => match component {
            Component::Rgb => ARG1_MASK | ARG2_MASK,
            // DOTPRODUCT3 does not define alpha; the shader preserves current alpha and does not
            // consume any args.
            Component::Alpha => 0,
        },
        _ => ARG1_MASK | ARG2_MASK,
    }
}

fn op_implicitly_uses_texture(op: TextureOp) -> bool {
    matches!(
        op,
        TextureOp::BlendTextureAlpha | TextureOp::BlendTextureAlphaPm
    )
}

fn wgsl_arg_component(arg: TextureArg, stage_index: usize, component: Component) -> String {
    let base = match arg.source {
        TextureArgSource::Current => "current".to_string(),
        TextureArgSource::Temp => "temp".to_string(),
        TextureArgSource::Diffuse => "input.diffuse".to_string(),
        TextureArgSource::Specular => "input.specular".to_string(),
        TextureArgSource::Texture => format!("tex{}_color", stage_index),
        TextureArgSource::TextureFactor => "globals.texture_factor".to_string(),
        TextureArgSource::Factor => format!("globals.stage_constants[{}]", stage_index),
    };

    let mut expr = match component {
        Component::Rgb => {
            if arg.flags.contains(TextureArgFlags::ALPHA_REPLICATE) {
                format!("vec3<f32>({}.a)", base)
            } else {
                format!("{}.rgb", base)
            }
        }
        Component::Alpha => format!("{}.a", base),
    };

    if arg.flags.contains(TextureArgFlags::COMPLEMENT) {
        expr = match component {
            Component::Rgb => format!("(vec3<f32>(1.0) - ({}))", expr),
            Component::Alpha => format!("(1.0 - ({}))", expr),
        };
    }

    expr
}

fn wgsl_op_expr(
    op: TextureOp,
    stage_index: usize,
    arg0: &str,
    arg1: &str,
    arg2: &str,
    component: Component,
) -> String {
    match op {
        TextureOp::Disable => match component {
            Component::Rgb => "current.rgb".to_string(),
            Component::Alpha => "current.a".to_string(),
        },
        TextureOp::SelectArg1 => arg1.to_string(),
        TextureOp::SelectArg2 => arg2.to_string(),
        TextureOp::Modulate => format!("(({}) * ({}))", arg1, arg2),
        TextureOp::Modulate2x => format!("(2.0 * ({}) * ({}))", arg1, arg2),
        TextureOp::Modulate4x => format!("(4.0 * ({}) * ({}))", arg1, arg2),
        TextureOp::Add => format!("(({}) + ({}))", arg1, arg2),
        TextureOp::AddSigned => match component {
            Component::Rgb => format!("(({}) + ({}) - vec3<f32>(0.5))", arg1, arg2),
            Component::Alpha => format!("(({}) + ({}) - 0.5)", arg1, arg2),
        },
        TextureOp::AddSigned2x => match component {
            Component::Rgb => format!("(2.0 * (({}) + ({}) - vec3<f32>(0.5)))", arg1, arg2),
            Component::Alpha => format!("(2.0 * (({}) + ({}) - 0.5))", arg1, arg2),
        },
        TextureOp::Subtract => format!("(({}) - ({}))", arg1, arg2),
        TextureOp::AddSmooth => format!("(({}) + ({}) - (({}) * ({})))", arg1, arg2, arg1, arg2),
        TextureOp::BlendDiffuseAlpha => {
            format!(
                "((({}) * input.diffuse.a) + (({}) * (1.0 - input.diffuse.a)))",
                arg1, arg2
            )
        }
        TextureOp::BlendTextureAlpha => {
            let tex_a = format!("tex{}_color.a", stage_index);
            format!(
                "((({}) * ({})) + (({}) * (1.0 - ({}))))",
                arg1, tex_a, arg2, tex_a
            )
        }
        TextureOp::BlendFactorAlpha => {
            let f = "globals.texture_factor.a";
            format!("((({}) * ({})) + (({}) * (1.0 - ({}))))", arg1, f, arg2, f)
        }
        TextureOp::BlendTextureAlphaPm => {
            let tex_a = format!("tex{}_color.a", stage_index);
            format!("(({}) + (({}) * (1.0 - ({}))))", arg1, arg2, tex_a)
        }
        TextureOp::BlendCurrentAlpha => {
            let f = "current.a";
            format!("((({}) * ({})) + (({}) * (1.0 - ({}))))", arg1, f, arg2, f)
        }
        TextureOp::MultiplyAdd => format!("(({}) + (({}) * ({})))", arg0, arg1, arg2),
        TextureOp::Lerp => format!("mix(({}), ({}), ({}))", arg2, arg1, arg0),
        TextureOp::DotProduct3 => match component {
            Component::Rgb => {
                // D3D9 fixed-function DOTPRODUCT3:
                //   out = dot((arg1.rgb - 0.5), (arg2.rgb - 0.5)) * 4 + 0.5
                format!(
                    "vec3<f32>(dot((({}) - vec3<f32>(0.5)), (({}) - vec3<f32>(0.5))) * 4.0 + 0.5)",
                    arg1, arg2
                )
            }
            // DOTPRODUCT3 is not defined for alpha in D3D9's fixed-function pipeline. Preserve
            // alpha to avoid surprising changes in content that only uses DP3 for RGB.
            Component::Alpha => "current.a".to_string(),
        },
    }
}

fn wgsl_compare_func(func: CompareFunc, lhs: &str, rhs: &str) -> String {
    match func {
        CompareFunc::Never => "false".to_string(),
        CompareFunc::Less => format!("{} < {}", lhs, rhs),
        CompareFunc::Equal => format!("{} == {}", lhs, rhs),
        CompareFunc::LessEqual => format!("{} <= {}", lhs, rhs),
        CompareFunc::Greater => format!("{} > {}", lhs, rhs),
        CompareFunc::NotEqual => format!("{} != {}", lhs, rhs),
        CompareFunc::GreaterEqual => format!("{} >= {}", lhs, rhs),
        CompareFunc::Always => "true".to_string(),
    }
}

fn wgsl_vertex_input_type(format: wgpu::VertexFormat) -> &'static str {
    match format {
        wgpu::VertexFormat::Float32 => "f32",
        wgpu::VertexFormat::Float32x2 => "vec2<f32>",
        wgpu::VertexFormat::Float32x3 => "vec3<f32>",
        wgpu::VertexFormat::Float32x4 => "vec4<f32>",
        wgpu::VertexFormat::Unorm8x4 => "vec4<f32>",
        wgpu::VertexFormat::Uint32 => "u32",
        _ => panic!("Unsupported vertex format in FVF layout: {:?}", format),
    }
}

const WGSL_SHARED: &str = r#"
struct Globals {
  world_view_proj: mat4x4<f32>,
  viewport: vec4<f32>,
  alpha_test: vec4<f32>,
  fog_color: vec4<f32>,
  fog_params: vec4<f32>,
  material_diffuse: vec4<f32>,
  material_ambient: vec4<f32>,
  light_dir: vec4<f32>,
  light_color: vec4<f32>,
  lighting_flags: vec4<u32>,
  texture_factor: vec4<f32>,
  stage_constants: array<vec4<f32>, 8>,
  texture_transforms: array<mat4x4<f32>, 8>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

fn d3dcolor_bgra_to_rgba(c: vec4<f32>) -> vec4<f32> {
  // `D3DCOLOR` is 0xAARRGGBB in little-endian memory; when consumed as `unorm8x4` it arrives as
  // (b, g, r, a).
  return c.zyxw;
}

"#;
