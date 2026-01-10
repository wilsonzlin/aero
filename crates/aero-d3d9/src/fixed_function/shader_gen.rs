use std::fmt::Write;

use super::fvf::{Fvf, FvfLayout, PositionType, TexCoordSize};
use super::tss::{AlphaTestState, CompareFunc, FogState, TextureArg, TextureOp, TextureStageState};

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
}

impl FixedFunctionGlobals {
    pub fn identity() -> Self {
        Self {
            world_view_proj: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            viewport: [0.0, 0.0, 1.0, 1.0],
            alpha_test: [0.0, 0.0, 0.0, 0.0],
            fog_color: [0.0, 0.0, 0.0, 0.0],
            fog_params: [0.0, 1.0, 0.0, 0.0],
        }
    }

    /// Raw bytes suitable for uploading into a `wgpu` uniform buffer.
    ///
    /// This is safe because `FixedFunctionGlobals` is `#[repr(C)]` and only
    /// contains plain old data (`f32` arrays).
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
    pub stage0: TextureStageState,
    pub alpha_test: AlphaTestState,
    pub fog: FogState,
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

        write_u8(&mut hash, self.stage0.color_op as u8);
        write_u8(&mut hash, self.stage0.color_arg1 as u8);
        write_u8(&mut hash, self.stage0.color_arg2 as u8);
        write_u8(&mut hash, self.stage0.alpha_op as u8);
        write_u8(&mut hash, self.stage0.alpha_arg1 as u8);
        write_u8(&mut hash, self.stage0.alpha_arg2 as u8);

        write_u8(&mut hash, self.alpha_test.enabled as u8);
        write_u8(&mut hash, self.alpha_test.func as u8);

        write_u8(&mut hash, self.fog.enabled as u8);

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
        wgsl.push_str("  let diffuse = unpack_argb8(input.diffuse);\n");
    } else {
        wgsl.push_str("  let diffuse = vec4<f32>(1.0, 1.0, 1.0, 1.0);\n");
    }
    wgsl.push_str("  out.diffuse = diffuse;\n");

    if layout.has_specular {
        wgsl.push_str("  out.specular = unpack_argb8(input.specular);\n");
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

    wgsl.push_str("@group(1) @binding(0) var tex0: texture_2d<f32>;\n");
    wgsl.push_str("@group(1) @binding(1) var samp0: sampler;\n\n");

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

    wgsl.push_str("  var current = input.diffuse;\n");

    let uses_texture = matches!(desc.stage0.color_arg1, TextureArg::Texture)
        || matches!(desc.stage0.color_arg2, TextureArg::Texture)
        || matches!(desc.stage0.alpha_arg1, TextureArg::Texture)
        || matches!(desc.stage0.alpha_arg2, TextureArg::Texture);

    if uses_texture {
        if layout.texcoords.is_empty() {
            wgsl.push_str("  let tex_color = textureSample(tex0, samp0, vec2<f32>(0.0, 0.0));\n");
        } else {
            wgsl.push_str("  let tex_color = textureSample(tex0, samp0, input.tex0.xy);\n");
        }
    } else {
        wgsl.push_str("  let tex_color = vec4<f32>(1.0, 1.0, 1.0, 1.0);\n");
    }

    let arg1 = wgsl_tex_arg(&desc.stage0.color_arg1);
    let arg2 = wgsl_tex_arg(&desc.stage0.color_arg2);
    let color_expr = wgsl_color_op(desc.stage0.color_op, arg1, arg2, ".rgb");
    let alpha_arg1 = wgsl_tex_arg(&desc.stage0.alpha_arg1);
    let alpha_arg2 = wgsl_tex_arg(&desc.stage0.alpha_arg2);
    let alpha_expr = wgsl_color_op(desc.stage0.alpha_op, alpha_arg1, alpha_arg2, ".a");

    wgsl.push_str("  {\n");
    let _ = writeln!(wgsl, "    let rgb = {};", color_expr);
    let _ = writeln!(wgsl, "    let a = {};", alpha_expr);
    wgsl.push_str("    current = vec4<f32>(rgb, a);\n  }\n");

    if desc.alpha_test.enabled {
        let cond = wgsl_compare_func(desc.alpha_test.func, "current.a", "globals.alpha_test.x");
        let _ = writeln!(wgsl, "  if (!({})) {{ discard; }}", cond);
    }

    if desc.fog.enabled {
        wgsl.push_str("  current = vec4<f32>(mix(globals.fog_color.rgb, current.rgb, input.fog_factor), current.a);\n");
    }

    wgsl.push_str("  return current;\n}\n");
    wgsl
}

fn wgsl_tex_arg(arg: &TextureArg) -> &'static str {
    match arg {
        TextureArg::Current => "current",
        TextureArg::Diffuse => "input.diffuse",
        TextureArg::Texture => "tex_color",
    }
}

fn wgsl_color_op(op: TextureOp, arg1: &str, arg2: &str, component: &str) -> String {
    match op {
        TextureOp::Disable => format!("current{}", component),
        TextureOp::SelectArg1 => format!("{}{}", arg1, component),
        TextureOp::SelectArg2 => format!("{}{}", arg2, component),
        TextureOp::Modulate => format!("({}{} * {}{})", arg1, component, arg2, component),
        TextureOp::Add => {
            if component == ".rgb" {
                format!(
                    "clamp({}{} + {}{}, vec3<f32>(0.0), vec3<f32>(1.0))",
                    arg1, component, arg2, component
                )
            } else {
                format!(
                    "clamp({}{} + {}{}, 0.0, 1.0)",
                    arg1, component, arg2, component
                )
            }
        }
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
};

@group(0) @binding(0) var<uniform> globals: Globals;

fn unpack_argb8(c: u32) -> vec4<f32> {
  let a = f32((c >> 24u) & 255u) / 255.0;
  let r = f32((c >> 16u) & 255u) / 255.0;
  let g = f32((c >> 8u) & 255u) / 255.0;
  let b = f32(c & 255u) / 255.0;
  return vec4<f32>(r, g, b, a);
}

"#;
