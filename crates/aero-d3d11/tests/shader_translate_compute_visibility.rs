use aero_d3d11::binding_model::{MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS};
use aero_d3d11::shader_translate::reflect_resource_bindings;
use aero_d3d11::{
    BindingKind, DstOperand, OperandModifier, RegFile, RegisterRef, SamplerRef, ShaderModel,
    ShaderStage, ShaderTranslateError, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle,
    TextureRef, WriteMask,
};

#[test]
fn compute_stage_resource_bindings_use_compute_visibility() {
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ConstantBuffer {
            slot: 0,
            reg_count: 1,
        }],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ConstantBuffer { slot: 0, reg: 0 },
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let bindings = reflect_resource_bindings(&module).expect("reflect bindings");
    let cb = bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: 0, .. }))
        .expect("expected cb0 binding");

    assert!(cb.visibility.contains(wgpu::ShaderStages::COMPUTE));
    assert_eq!(cb.group, 2, "compute-stage bindings should use @group(2)");
}

#[test]
fn reflect_resource_bindings_rejects_out_of_range_texture_slot() {
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                coord: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
                texture: TextureRef {
                    slot: MAX_TEXTURE_SLOTS,
                },
                sampler: SamplerRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = reflect_resource_bindings(&module).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::ResourceSlotOutOfRange {
            kind: "texture",
            slot,
            max,
        } if slot == MAX_TEXTURE_SLOTS && max == MAX_TEXTURE_SLOTS - 1
    ));
}

#[test]
fn reflect_resource_bindings_rejects_out_of_range_sampler_slot() {
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                coord: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef {
                    slot: MAX_SAMPLER_SLOTS,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = reflect_resource_bindings(&module).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::ResourceSlotOutOfRange {
            kind: "sampler",
            slot,
            max,
        } if slot == MAX_SAMPLER_SLOTS && max == MAX_SAMPLER_SLOTS - 1
    ));
}
