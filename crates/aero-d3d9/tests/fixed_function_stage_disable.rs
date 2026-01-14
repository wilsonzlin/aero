use aero_d3d9::fixed_function::fvf::Fvf;
use aero_d3d9::fixed_function::shader_gen::{generate_fixed_function_shaders, FixedFunctionShaderDesc};
use aero_d3d9::fixed_function::tss::{
    AlphaTestState, FogState, LightingState, TextureArg, TextureOp, TextureResultTarget,
    TextureStageState, TextureTransform,
};

#[test]
fn state_hash_ignores_stages_after_first_disabled_colorop() {
    let mut stages_a = [TextureStageState::default(); 8];
    stages_a[0] = TextureStageState {
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
    // stage1 left as Disable (default), which should disable all subsequent stages.
    stages_a[2] = TextureStageState {
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

    let mut stages_b = stages_a;
    // Mutate stage2; hash should be unchanged because stage1 disables it.
    stages_b[2].color_op = TextureOp::Subtract;

    let desc_a = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | (1 << Fvf::TEXCOUNT_SHIFT)),
        stages: stages_a,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };
    let desc_b = FixedFunctionShaderDesc {
        stages: stages_b,
        ..desc_a.clone()
    };

    assert_eq!(desc_a.state_hash(), desc_b.state_hash());

    // Shader generation should also stop at stage1 and never emit `tex2_color`.
    let shaders = generate_fixed_function_shaders(&desc_a);
    assert!(
        !shaders.fragment_wgsl.contains("let tex2_color"),
        "unexpected stage2 emission:\n{}",
        shaders.fragment_wgsl
    );
}

#[test]
fn temp_register_is_not_emitted_for_disabled_stages() {
    // Regression test for `shader_uses_temp`: if a later stage uses TEMP but is disabled by an
    // earlier `COLOROP=Disable`, the shader should not declare the temp register at all.
    let mut stages = [TextureStageState::default(); 8];
    stages[0] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Diffuse,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Diffuse,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };
    // stage1 left as Disable (default).
    stages[2] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Temp,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Temp,
        alpha_arg2: TextureArg::Current,
        result_target: TextureResultTarget::Temp,
        ..Default::default()
    };

    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let shaders = generate_fixed_function_shaders(&desc);
    assert!(
        !shaders.fragment_wgsl.contains("var temp = current"),
        "unexpected temp register in fragment shader:\n{}",
        shaders.fragment_wgsl
    );
}

#[test]
fn state_hash_includes_texcoord_index_texture_transform_and_result_target() {
    let base_stage = TextureStageState {
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

    // TEXCOORDINDEX should affect the hash and WGSL when a different texcoord set exists.
    let mut stages_tex0 = [TextureStageState::default(); 8];
    stages_tex0[0] = TextureStageState {
        texcoord_index: None,
        ..base_stage
    };
    let mut stages_tex1 = stages_tex0;
    stages_tex1[0].texcoord_index = Some(1);

    let desc_tex0 = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | ((2u32) << Fvf::TEXCOUNT_SHIFT)),
        stages: stages_tex0,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };
    let desc_tex1 = FixedFunctionShaderDesc {
        stages: stages_tex1,
        ..desc_tex0.clone()
    };

    assert_ne!(desc_tex0.state_hash(), desc_tex1.state_hash());
    let wgsl_tex0 = generate_fixed_function_shaders(&desc_tex0).fragment_wgsl;
    let wgsl_tex1 = generate_fixed_function_shaders(&desc_tex1).fragment_wgsl;
    assert!(
        wgsl_tex0.contains("input.tex0") && !wgsl_tex0.contains("input.tex1"),
        "expected stage0 to sample from TEXCOORD0:\n{wgsl_tex0}"
    );
    assert!(
        wgsl_tex1.contains("input.tex1"),
        "expected stage0 to sample from TEXCOORD1:\n{wgsl_tex1}"
    );

    // Texture transform flags should also influence the hash and generated code.
    let mut stages_xform = [TextureStageState::default(); 8];
    stages_xform[0] = TextureStageState {
        texture_transform: TextureTransform::Count2,
        ..base_stage
    };
    let desc_xform = FixedFunctionShaderDesc {
        fvf: desc_tex0.fvf,
        stages: stages_xform,
        alpha_test: desc_tex0.alpha_test,
        fog: desc_tex0.fog,
        lighting: desc_tex0.lighting,
    };
    assert_ne!(desc_tex0.state_hash(), desc_xform.state_hash());
    let wgsl_xform = generate_fixed_function_shaders(&desc_xform).fragment_wgsl;
    assert!(
        wgsl_xform.contains("globals.texture_transforms[0]"),
        "expected stage0 to apply a texture transform:\n{wgsl_xform}"
    );

    // RESULTARG (TEMP vs CURRENT) should influence the hash and stage write target.
    let mut stages_temp = [TextureStageState::default(); 8];
    stages_temp[0] = TextureStageState {
        result_target: TextureResultTarget::Temp,
        ..base_stage
    };
    let desc_temp = FixedFunctionShaderDesc {
        fvf: desc_tex0.fvf,
        stages: stages_temp,
        alpha_test: desc_tex0.alpha_test,
        fog: desc_tex0.fog,
        lighting: desc_tex0.lighting,
    };
    assert_ne!(desc_tex0.state_hash(), desc_temp.state_hash());
    let wgsl_temp = generate_fixed_function_shaders(&desc_temp).fragment_wgsl;
    assert!(
        wgsl_temp.contains("temp = vec4<f32>"),
        "expected stage0 to write to temp:\n{wgsl_temp}"
    );
}
