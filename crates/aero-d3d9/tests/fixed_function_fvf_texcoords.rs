use aero_d3d9::fixed_function::fvf::{Fvf, FvfLayout, TexCoordSize};
use aero_d3d9::fixed_function::shader_gen::{
    generate_fixed_function_shaders, FixedFunctionShaderDesc,
};
use aero_d3d9::fixed_function::tss::{
    AlphaTestState, FogState, LightingState, TextureArg, TextureOp, TextureStageState,
};
use aero_gpu_utils::validate_wgsl_render_shader;

fn validate_wgsl_module(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn fixed_function_fvf_layout_allows_5_to_8_texcoords() {
    for tex_count in 5..=8usize {
        let fvf = Fvf(Fvf::XYZ | ((tex_count as u32) << Fvf::TEXCOUNT_SHIFT));
        let layout = FvfLayout::new(fvf).expect("layout builds");

        assert_eq!(layout.texcoords.len(), tex_count);
        assert!(layout
            .texcoords
            .iter()
            .all(|&size| size == TexCoordSize::Two));

        assert_eq!(layout.vertex_stride, 12 + (tex_count as u64) * 8);
        assert_eq!(layout.vertex_attributes.len(), 1 + tex_count);

        // Position is always first, at location 0.
        assert_eq!(layout.vertex_attributes[0].shader_location, 0);
        assert_eq!(layout.vertex_attributes[0].offset, 0);
        assert_eq!(
            layout.vertex_attributes[0].format,
            wgpu::VertexFormat::Float32x3
        );

        // Default texcoord size is 2 components, packed sequentially.
        for i in 0..tex_count {
            let attr = &layout.vertex_attributes[1 + i];
            assert_eq!(attr.shader_location, 5 + i as u32);
            assert_eq!(attr.offset, 12 + (i as u64) * 8);
            assert_eq!(attr.format, wgpu::VertexFormat::Float32x2);
        }
    }
}

#[test]
fn fixed_function_fvf_texcoord_size_bits_work_up_to_tex7() {
    // 8 texcoords with varying component counts, including entries past tex3.
    let expected_sizes = vec![
        TexCoordSize::One,
        TexCoordSize::Two,
        TexCoordSize::Three,
        TexCoordSize::Four,
        TexCoordSize::One,
        TexCoordSize::Two,
        TexCoordSize::Three,
        TexCoordSize::Four,
    ];

    let mut fvf = Fvf::XYZ | ((expected_sizes.len() as u32) << Fvf::TEXCOUNT_SHIFT);
    for (i, size) in expected_sizes.iter().enumerate() {
        let bits = match size {
            TexCoordSize::Two => 0u32,
            TexCoordSize::Three => 1u32,
            TexCoordSize::Four => 2u32,
            TexCoordSize::One => 3u32,
        };
        fvf |= bits << (16 + (i as u32) * 2);
    }

    let layout = FvfLayout::new(Fvf(fvf)).expect("layout builds");
    assert_eq!(layout.texcoords, expected_sizes);

    // Position is always 12 bytes for XYZ; then texcoords follow with sizes specified above.
    let mut offset = 12u64;
    for (i, size) in layout.texcoords.iter().enumerate() {
        let attr = &layout.vertex_attributes[1 + i];
        assert_eq!(attr.shader_location, 5 + i as u32);
        assert_eq!(attr.offset, offset);

        let (format, byte_len) = match size.components() {
            1 => (wgpu::VertexFormat::Float32, 4u64),
            2 => (wgpu::VertexFormat::Float32x2, 8u64),
            3 => (wgpu::VertexFormat::Float32x3, 12u64),
            4 => (wgpu::VertexFormat::Float32x4, 16u64),
            _ => unreachable!(),
        };
        assert_eq!(attr.format, format);
        offset += byte_len;
    }
    assert_eq!(layout.vertex_stride, offset);
}

#[test]
fn fixed_function_shader_generation_supports_8_texcoords() {
    let mut stages = [TextureStageState::default(); 8];
    stages[0] = TextureStageState {
        color_op: TextureOp::Modulate,
        color_arg1: TextureArg::Texture,
        color_arg2: TextureArg::Diffuse,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg1: TextureArg::Texture,
        ..TextureStageState::default()
    };
    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | Fvf::SPECULAR | ((8u32) << Fvf::TEXCOUNT_SHIFT)),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState { enabled: true },
        lighting: LightingState::default(),
    };

    let shaders = generate_fixed_function_shaders(&desc);

    // `validate_wgsl_render_shader` expects a single WGSL source containing both stages. Our
    // fixed-function pipeline produces separate WGSL modules; for this unit test we simply
    // concatenate them (the helper is a lightweight sanity check and does not parse WGSL).
    let combined = format!("{}\n{}", shaders.vertex_wgsl, shaders.fragment_wgsl);
    validate_wgsl_render_shader(&combined).expect("wgsl render shader sanity check");

    // Additional parsing/validation via naga to ensure the generated WGSL is well-formed.
    validate_wgsl_module(&shaders.vertex_wgsl);
    validate_wgsl_module(&shaders.fragment_wgsl);

    // With 8 texcoords, `tex7` should be present and fog factor should be assigned the next
    // available varying location: 2 (diffuse/specular) + 8 texcoords = 10.
    assert!(shaders.vertex_wgsl.contains("tex7"));
    assert!(shaders.fragment_wgsl.contains("tex7"));
    assert!(shaders.vertex_wgsl.contains("@location(10) fog_factor"));
    assert!(shaders.fragment_wgsl.contains("@location(10) fog_factor"));
}
