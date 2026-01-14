use aero_d3d9::fixed_function::fvf::Fvf;
use aero_d3d9::fixed_function::shader_gen::{
    generate_fixed_function_shaders, FixedFunctionShaderDesc,
};
use aero_d3d9::fixed_function::tss::{
    AlphaTestState, FogState, LightingState, TextureArg, TextureOp, TextureStageState,
};

fn shaders_snapshot(desc: &FixedFunctionShaderDesc) -> String {
    let shaders = generate_fixed_function_shaders(desc);
    format!(
        "// hash: {:#x}\n\n// --- vertex.wgsl ---\n{}\n\n// --- fragment.wgsl ---\n{}\n",
        shaders.hash, shaders.vertex_wgsl, shaders.fragment_wgsl
    )
}

#[test]
fn wgsl_single_stage_modulate_texture_diffuse() {
    let mut stages = [TextureStageState::default(); 8];
    stages[0] = TextureStageState {
        color_op: TextureOp::Modulate,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Texture,
        color_arg2: TextureArg::Diffuse,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Texture,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };
    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | (1 << 8)),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    insta::assert_snapshot!(shaders_snapshot(&desc));
}

#[test]
fn wgsl_two_stage_add_current_texture() {
    let mut stages = [TextureStageState::default(); 8];
    stages[0] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Texture,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Texture,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };
    stages[1] = TextureStageState {
        color_op: TextureOp::Add,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Current,
        color_arg2: TextureArg::Texture,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Current,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };
    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | (2 << 8)),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    insta::assert_snapshot!(shaders_snapshot(&desc));
}

#[test]
fn wgsl_modulate2x_with_complement_and_alpha_replicate() {
    let mut stages = [TextureStageState::default(); 8];
    stages[0] = TextureStageState {
        color_op: TextureOp::Modulate2x,
        // Lerp factor unused; keep deterministic.
        color_arg0: TextureArg::Current,
        // RGB uses (1 - texture.rgb) * specular.aaa * 2.
        color_arg1: TextureArg::Texture.complement(),
        color_arg2: TextureArg::Specular.alpha_replicate(),
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Diffuse,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };
    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | Fvf::SPECULAR | (1 << 8)),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    insta::assert_snapshot!(shaders_snapshot(&desc));
}
