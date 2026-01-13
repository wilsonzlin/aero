use aero_d3d11::binding_model::{
    BINDING_BASE_TEXTURE, BINDING_BASE_UAV, MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS, MAX_UAV_SLOTS,
};
use aero_d3d11::shader_translate::reflect_resource_bindings;
use aero_d3d11::{
    BindingKind, BufferRef, DstOperand, OperandModifier, RegFile, RegisterRef, SamplerRef,
    ShaderModel, ShaderStage, ShaderTranslateError, Sm4Decl, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, TextureRef, UavRef, WriteMask,
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

#[test]
fn reflect_resource_bindings_includes_srv_and_uav_buffers() {
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::LdRaw {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                addr: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                buffer: BufferRef { slot: 0 },
            },
            Sm4Inst::StoreRaw {
                uav: UavRef { slot: 0 },
                addr: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                value: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let bindings = reflect_resource_bindings(&module).expect("reflect bindings");
    let srv = bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::SrvBuffer { slot: 0 }))
        .expect("expected srv buffer binding");
    let uav = bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::UavBuffer { slot: 0 }))
        .expect("expected uav buffer binding");

    assert_eq!(srv.group, 2);
    assert_eq!(srv.binding, BINDING_BASE_TEXTURE);
    assert!(srv.visibility.contains(wgpu::ShaderStages::COMPUTE));

    assert_eq!(uav.group, 2);
    assert_eq!(uav.binding, BINDING_BASE_UAV);
    assert!(uav.visibility.contains(wgpu::ShaderStages::COMPUTE));
}

#[test]
fn reflect_resource_bindings_rejects_out_of_range_srv_buffer_slot() {
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::LdRaw {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                addr: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                buffer: BufferRef {
                    slot: MAX_TEXTURE_SLOTS,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = reflect_resource_bindings(&module).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::ResourceSlotOutOfRange {
            kind: "srv_buffer",
            slot,
            max,
        } if slot == MAX_TEXTURE_SLOTS && max == MAX_TEXTURE_SLOTS - 1
    ));
}

#[test]
fn reflect_resource_bindings_rejects_out_of_range_uav_buffer_slot() {
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::StoreRaw {
                uav: UavRef {
                    slot: MAX_UAV_SLOTS,
                },
                addr: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                value: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let err = reflect_resource_bindings(&module).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::ResourceSlotOutOfRange {
            kind: "uav_buffer",
            slot,
            max,
        } if slot == MAX_UAV_SLOTS && max == MAX_UAV_SLOTS - 1
    ));
}
