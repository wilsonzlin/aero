use aero_d3d9::fixed_function::fvf::Fvf;
use aero_d3d9::fixed_function::shader_gen::{
    generate_fixed_function_shaders, FixedFunctionShaderDesc,
};
use aero_d3d9::fixed_function::tss::{
    AlphaTestState, CompareFunc, FogState, LightingState, TextureArg, TextureOp,
    TextureResultTarget, TextureStageState, TextureTransform,
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
fn colorop_disable_disables_stage_even_if_alphaop_enabled() {
    // D3D9 stage disabling is keyed off COLOROP; ALPHAOP should not be able to “keep the stage
    // alive” once COLOROP is DISABLE.
    let mut stages_a = [TextureStageState::default(); 8];
    stages_a[0] = TextureStageState {
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
    // Stage1: COLOROP disables pipeline, but ALPHAOP is enabled (should still be ignored).
    stages_a[1] = TextureStageState {
        color_op: TextureOp::Disable,
        alpha_op: TextureOp::Modulate,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Current,
        alpha_arg2: TextureArg::Diffuse,
        ..Default::default()
    };
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
    stages_b[2].color_op = TextureOp::Subtract;

    let desc_a = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | (1 << Fvf::TEXCOUNT_SHIFT)),
        stages: stages_a,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };
    let desc_b = FixedFunctionShaderDesc {
        stages: stages_b,
        ..desc_a.clone()
    };

    assert_eq!(
        desc_a.state_hash(),
        desc_b.state_hash(),
        "stages after COLOROP=Disable must not influence the hash, even if ALPHAOP is enabled"
    );

    let wgsl = generate_fixed_function_shaders(&desc_a).fragment_wgsl;
    assert!(
        !wgsl.contains("let tex1_color"),
        "unexpected stage1 emission:\n{wgsl}"
    );
    assert!(
        !wgsl.contains("let tex2_color"),
        "unexpected stage2 emission:\n{wgsl}"
    );
}

#[test]
fn state_hash_ignores_texcoord_state_when_stage_does_not_sample_texture() {
    // If a stage doesn't sample from `D3DTA_TEXTURE` (and its op doesn't implicitly sample),
    // TEXCOORDINDEX and TEXTURETRANSFORMFLAGS should not affect shader generation nor cache keys.
    let base_stage = TextureStageState {
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

    let mut stages_a = [TextureStageState::default(); 8];
    stages_a[0] = TextureStageState {
        texcoord_index: None,
        texture_transform: TextureTransform::Disable,
        ..base_stage
    };
    let mut stages_b = stages_a;
    stages_b[0].texcoord_index = Some(3);
    stages_b[0].texture_transform = TextureTransform::Count2Projected;

    let desc_a = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | Fvf::DIFFUSE | ((4u32) << Fvf::TEXCOUNT_SHIFT)),
        stages: stages_a,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };
    let desc_b = FixedFunctionShaderDesc {
        stages: stages_b,
        ..desc_a.clone()
    };

    assert_eq!(
        desc_a.state_hash(),
        desc_b.state_hash(),
        "texcoord state should not affect the state hash when the stage does not sample textures"
    );

    let wgsl = generate_fixed_function_shaders(&desc_a).fragment_wgsl;
    assert!(
        !wgsl.contains("textureSample("),
        "unexpected texture sampling in WGSL:\n{wgsl}"
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

    // RESULTARG (TEMP vs CURRENT) should influence the hash and generated code.
    //
    // When TEMP is not read by later stages, writes to TEMP are dead and the stage can be treated
    // as a no-op (avoid sampling textures / declaring temp).
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
        !wgsl_temp.contains("var temp = current"),
        "expected temp register to be omitted when TEMP is never read:\n{wgsl_temp}"
    );
    assert!(
        !wgsl_temp.contains("textureSample("),
        "expected no texture sampling when stage0 only writes TEMP that is never read:\n{wgsl_temp}"
    );
}

#[test]
fn result_target_temp_emits_temp_when_read_later() {
    // If a later stage reads TEMP, we must emit the temp register and preserve the write.
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
        result_target: TextureResultTarget::Temp,
        ..Default::default()
    };
    stages[1] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Temp,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Temp,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };

    let desc = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | (1 << Fvf::TEXCOUNT_SHIFT)),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let wgsl = generate_fixed_function_shaders(&desc).fragment_wgsl;
    assert!(
        wgsl.contains("var temp = current"),
        "expected temp register to be declared:\n{wgsl}"
    );
    assert!(
        wgsl.contains("temp = vec4<f32>"),
        "expected stage0 to write to temp:\n{wgsl}"
    );
    assert!(
        wgsl.contains("let rgb_raw = temp.rgb"),
        "expected later stage to read temp:\n{wgsl}"
    );
}

#[test]
fn dead_temp_writes_after_last_temp_read_are_ignored() {
    // Stage0 writes TEMP, Stage1 reads TEMP into CURRENT, Stage2 writes TEMP again but nobody reads
    // it. Stage2 should be skipped entirely (no tex2 sampling and no hash impact).
    let mut stages_a = [TextureStageState::default(); 8];
    stages_a[0] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Texture,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Texture,
        alpha_arg2: TextureArg::Current,
        result_target: TextureResultTarget::Temp,
        ..Default::default()
    };
    stages_a[1] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Temp,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Temp,
        alpha_arg2: TextureArg::Current,
        result_target: TextureResultTarget::Current,
        ..Default::default()
    };
    stages_a[2] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Texture,
        color_arg2: TextureArg::Current,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Texture,
        alpha_arg2: TextureArg::Current,
        result_target: TextureResultTarget::Temp,
        ..Default::default()
    };

    let mut stages_b = stages_a;
    stages_b[2].color_op = TextureOp::Subtract;

    let desc_a = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | (1 << Fvf::TEXCOUNT_SHIFT)),
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

    let wgsl = generate_fixed_function_shaders(&desc_a).fragment_wgsl;
    assert!(
        !wgsl.contains("let tex2_color"),
        "expected stage2 to be skipped:\n{wgsl}"
    );
    assert!(
        !wgsl.contains("textureSample(tex2"),
        "expected stage2 not to sample tex2:\n{wgsl}"
    );
}

#[test]
fn blendtexturealpha_op_implicitly_samples_texture() {
    // BLENDTEXTUREALPHA uses the current stage texture alpha as its interpolant, even if none of
    // the args explicitly reference `D3DTA_TEXTURE`. Ensure we still emit a textureSample and
    // include texcoord state in the cache key.
    let base_stage = TextureStageState {
        color_op: TextureOp::BlendTextureAlpha,
        color_arg0: TextureArg::Current, // unused by this op
        color_arg1: TextureArg::Current,
        color_arg2: TextureArg::Diffuse,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Current,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };

    let mut stages_tc0 = [TextureStageState::default(); 8];
    stages_tc0[0] = TextureStageState {
        texcoord_index: None,
        ..base_stage
    };
    let mut stages_tc1 = stages_tc0;
    stages_tc1[0].texcoord_index = Some(1);

    let desc_tc0 = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | ((2u32) << Fvf::TEXCOUNT_SHIFT)),
        stages: stages_tc0,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };
    let desc_tc1 = FixedFunctionShaderDesc {
        stages: stages_tc1,
        ..desc_tc0.clone()
    };

    assert_ne!(
        desc_tc0.state_hash(),
        desc_tc1.state_hash(),
        "texcoord_index should affect the state hash when BLENDTEXTUREALPHA is used"
    );

    let wgsl = generate_fixed_function_shaders(&desc_tc0).fragment_wgsl;
    assert!(
        wgsl.contains("textureSample(tex0, samp0"),
        "expected BLENDTEXTUREALPHA to emit a texture sample:\n{wgsl}"
    );
    assert!(
        wgsl.contains("tex0_color.a"),
        "expected BLENDTEXTUREALPHA to reference stage texture alpha:\n{wgsl}"
    );
}

#[test]
fn blendtexturealpha_in_alphaop_implicitly_samples_texture() {
    // Like the RGB op, ALPHAOP=BLENDTEXTUREALPHA should still cause a texture sample, even if the
    // COLOROP path does not use `D3DTA_TEXTURE` at all.
    let base_stage = TextureStageState {
        // No texture usage on the RGB path.
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Diffuse,
        color_arg2: TextureArg::Current,
        // But alpha path uses texture alpha implicitly.
        alpha_op: TextureOp::BlendTextureAlpha,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Current,
        alpha_arg2: TextureArg::Diffuse,
        ..Default::default()
    };

    let mut stages_tc0 = [TextureStageState::default(); 8];
    stages_tc0[0] = TextureStageState {
        texcoord_index: None,
        ..base_stage
    };
    let mut stages_tc1 = stages_tc0;
    stages_tc1[0].texcoord_index = Some(1);

    let desc_tc0 = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | ((2u32) << Fvf::TEXCOUNT_SHIFT)),
        stages: stages_tc0,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };
    let desc_tc1 = FixedFunctionShaderDesc {
        stages: stages_tc1,
        ..desc_tc0.clone()
    };

    assert_ne!(
        desc_tc0.state_hash(),
        desc_tc1.state_hash(),
        "texcoord_index should affect the state hash when ALPHAOP=BLENDTEXTUREALPHA is used"
    );

    let wgsl = generate_fixed_function_shaders(&desc_tc0).fragment_wgsl;
    assert!(
        wgsl.contains("textureSample(tex0, samp0"),
        "expected ALPHAOP=BLENDTEXTUREALPHA to emit a texture sample:\n{wgsl}"
    );
    assert!(
        wgsl.contains("tex0_color.a"),
        "expected ALPHAOP=BLENDTEXTUREALPHA to reference stage texture alpha:\n{wgsl}"
    );
}

#[test]
fn blendtexturealphapm_op_implicitly_samples_texture() {
    // `BLENDTEXTUREALPHAPM` implicitly uses the stage texture alpha even if args do not reference
    // `D3DTA_TEXTURE`.
    let base_stage = TextureStageState {
        color_op: TextureOp::BlendTextureAlphaPm,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Current,
        color_arg2: TextureArg::Diffuse,
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Current,
        alpha_arg2: TextureArg::Current,
        ..Default::default()
    };

    let mut stages_tc0 = [TextureStageState::default(); 8];
    stages_tc0[0] = TextureStageState {
        texcoord_index: None,
        ..base_stage
    };
    let mut stages_tc1 = stages_tc0;
    stages_tc1[0].texcoord_index = Some(1);

    let desc_tc0 = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ | ((2u32) << Fvf::TEXCOUNT_SHIFT)),
        stages: stages_tc0,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState::default(),
    };
    let desc_tc1 = FixedFunctionShaderDesc {
        stages: stages_tc1,
        ..desc_tc0.clone()
    };

    assert_ne!(
        desc_tc0.state_hash(),
        desc_tc1.state_hash(),
        "texcoord_index should affect the state hash when BLENDTEXTUREALPHAPM is used"
    );

    let wgsl = generate_fixed_function_shaders(&desc_tc0).fragment_wgsl;
    assert!(
        wgsl.contains("textureSample(tex0, samp0"),
        "expected BLENDTEXTUREALPHAPM to emit a texture sample:\n{wgsl}"
    );
    assert!(
        wgsl.contains("tex0_color.a"),
        "expected BLENDTEXTUREALPHAPM to reference stage texture alpha:\n{wgsl}"
    );
    assert!(
        wgsl.contains("1.0 - (tex0_color.a)"),
        "expected BLENDTEXTUREALPHAPM to use (1 - tex_a) weight:\n{wgsl}"
    );
}

#[test]
fn unused_texture_args_do_not_trigger_sampling_or_hash_changes() {
    // If a stage's operation does not consume ARG2, setting ARG2 to `D3DTA_TEXTURE` should not
    // affect shader generation or caching.
    let mut stages_a = [TextureStageState::default(); 8];
    stages_a[0] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Current,
        color_arg1: TextureArg::Diffuse,
        color_arg2: TextureArg::Texture, // unused by SelectArg1
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Current,
        alpha_arg1: TextureArg::Diffuse,
        alpha_arg2: TextureArg::Texture, // unused by SelectArg1
        ..Default::default()
    };

    let mut stages_b = stages_a;
    stages_b[0].color_arg2 = TextureArg::Current;
    stages_b[0].alpha_arg2 = TextureArg::Current;

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

    let shaders = generate_fixed_function_shaders(&desc_a);
    assert!(
        !shaders.fragment_wgsl.contains("textureSample("),
        "unexpected texture sampling in fragment shader:\n{}",
        shaders.fragment_wgsl
    );
}

#[test]
fn unused_temp_args_do_not_trigger_temp_register() {
    let mut stages = [TextureStageState::default(); 8];
    stages[0] = TextureStageState {
        color_op: TextureOp::SelectArg1,
        color_arg0: TextureArg::Temp, // unused
        color_arg1: TextureArg::Diffuse,
        color_arg2: TextureArg::Temp, // unused
        alpha_op: TextureOp::SelectArg1,
        alpha_arg0: TextureArg::Temp, // unused
        alpha_arg1: TextureArg::Diffuse,
        alpha_arg2: TextureArg::Temp, // unused
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
fn state_hash_ignores_alpha_test_func_when_disabled() {
    let stages = [TextureStageState::default(); 8];

    let desc_a = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ),
        stages,
        alpha_test: AlphaTestState {
            enabled: false,
            func: CompareFunc::Always,
        },
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let desc_b = FixedFunctionShaderDesc {
        alpha_test: AlphaTestState {
            enabled: false,
            func: CompareFunc::Less,
        },
        ..desc_a.clone()
    };

    assert_eq!(
        desc_a.state_hash(),
        desc_b.state_hash(),
        "alpha-test func must not affect the hash when alpha testing is disabled"
    );
}

#[test]
fn alpha_test_always_is_treated_as_disabled_for_hash_and_wgsl() {
    let stages = [TextureStageState::default(); 8];

    let base = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ),
        stages,
        alpha_test: AlphaTestState {
            enabled: false,
            func: CompareFunc::Less,
        },
        fog: FogState::default(),
        lighting: LightingState::default(),
    };

    let always_enabled = FixedFunctionShaderDesc {
        alpha_test: AlphaTestState {
            enabled: true,
            func: CompareFunc::Always,
        },
        ..base.clone()
    };

    assert_eq!(
        base.state_hash(),
        always_enabled.state_hash(),
        "ALPHATEST=ENABLED + FUNC=ALWAYS is a no-op and should not change the shader hash"
    );

    let wgsl_base = generate_fixed_function_shaders(&base).fragment_wgsl;
    let wgsl_always = generate_fixed_function_shaders(&always_enabled).fragment_wgsl;
    assert_eq!(
        wgsl_base, wgsl_always,
        "ALPHATEST=ENABLED + FUNC=ALWAYS should generate identical WGSL"
    );
    assert!(
        !wgsl_always.contains("discard"),
        "unexpected discard:\n{wgsl_always}"
    );
}

#[test]
fn state_hash_ignores_lighting_when_fvf_has_no_normals() {
    let stages = [TextureStageState::default(); 8];

    let base = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZ),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState { enabled: false },
    };
    let lighting_enabled = FixedFunctionShaderDesc {
        lighting: LightingState { enabled: true },
        ..base.clone()
    };

    assert_eq!(
        base.state_hash(),
        lighting_enabled.state_hash(),
        "lighting has no observable effect without normals and must not affect the shader cache key"
    );

    let wgsl_base = generate_fixed_function_shaders(&base).vertex_wgsl;
    let wgsl_enabled = generate_fixed_function_shaders(&lighting_enabled).vertex_wgsl;
    assert_eq!(
        wgsl_base, wgsl_enabled,
        "enabling lighting without normals should generate identical WGSL"
    );
}

#[test]
fn state_hash_ignores_lighting_when_vertices_are_xyzrhw() {
    let stages = [TextureStageState::default(); 8];

    let base = FixedFunctionShaderDesc {
        fvf: Fvf(Fvf::XYZRHW | Fvf::NORMAL),
        stages,
        alpha_test: AlphaTestState::default(),
        fog: FogState::default(),
        lighting: LightingState { enabled: false },
    };
    let lighting_enabled = FixedFunctionShaderDesc {
        lighting: LightingState { enabled: true },
        ..base.clone()
    };

    assert_eq!(
        base.state_hash(),
        lighting_enabled.state_hash(),
        "lighting has no observable effect for XYZRHW vertices and must not affect the cache key"
    );

    let wgsl_base = generate_fixed_function_shaders(&base).vertex_wgsl;
    let wgsl_enabled = generate_fixed_function_shaders(&lighting_enabled).vertex_wgsl;
    assert_eq!(
        wgsl_base, wgsl_enabled,
        "enabling lighting for XYZRHW vertices should generate identical WGSL"
    );
}
