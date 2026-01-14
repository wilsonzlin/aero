use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BufferKind, DxbcFile, FourCC, ShaderModel,
    ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

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

#[test]
fn translates_compute_system_value_builtins_to_wgsl_builtins() {
    // Translation-only test: ensure compute-stage `dcl_input_siv` system values map to the correct
    // WGSL builtins, and preserve raw integer bits via `bitcast<f32>(...)` into the untyped
    // `vec4<f32>` register file model.
    const D3D_NAME_DISPATCH_THREAD_ID: u32 = 20;
    const D3D_NAME_GROUP_ID: u32 = 21;
    const D3D_NAME_GROUP_INDEX: u32 = 22;
    const D3D_NAME_GROUP_THREAD_ID: u32 = 23;

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: D3D_NAME_DISPATCH_THREAD_ID,
            },
            Sm4Decl::InputSiv {
                reg: 1,
                mask: WriteMask::XYZW,
                sys_value: D3D_NAME_GROUP_THREAD_ID,
            },
            Sm4Decl::InputSiv {
                reg: 2,
                mask: WriteMask::XYZW,
                sys_value: D3D_NAME_GROUP_ID,
            },
            Sm4Decl::InputSiv {
                reg: 3,
                mask: WriteMask::X,
                sys_value: D3D_NAME_GROUP_INDEX,
            },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Input,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
            },
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 1,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Input,
                        index: 1,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
            },
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 2,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Input,
                        index: 2,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
            },
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 3,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Input,
                        index: 3,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("@builtin(global_invocation_id)"),
        "expected dispatch thread ID lowering to global_invocation_id:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("@builtin(local_invocation_id)"),
        "expected group thread ID lowering to local_invocation_id:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("@builtin(workgroup_id)"),
        "expected group ID lowering to workgroup_id:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("@builtin(local_invocation_index)"),
        "expected group index lowering to local_invocation_index:\n{}",
        translated.wgsl
    );

    assert!(
        translated.wgsl.contains(
            "vec4<f32>(bitcast<f32>(input.global_invocation_id.x), bitcast<f32>(input.global_invocation_id.y), bitcast<f32>(input.global_invocation_id.z), bitcast<f32>(1u))"
        ),
        "expected dispatch thread ID lanes to be expanded as raw bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(
            "vec4<f32>(bitcast<f32>(input.local_invocation_id.x), bitcast<f32>(input.local_invocation_id.y), bitcast<f32>(input.local_invocation_id.z), bitcast<f32>(1u))"
        ),
        "expected group thread ID lanes to be expanded as raw bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(
            "vec4<f32>(bitcast<f32>(input.workgroup_id.x), bitcast<f32>(input.workgroup_id.y), bitcast<f32>(input.workgroup_id.z), bitcast<f32>(1u))"
        ),
        "expected group ID lanes to be expanded as raw bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(
            "vec4<f32>(bitcast<f32>(input.local_invocation_index), 0.0, 0.0, bitcast<f32>(1u))"
        ),
        "expected group index to be expanded as scalar raw bits:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_compute_builtin_operand_types_to_wgsl_builtins() {
    // Compute shaders can reference thread IDs using dedicated operand types in the token stream
    // (`OPERAND_TYPE_INPUT_THREAD_ID`, etc). In our IR those lower to `SrcKind::ComputeBuiltin`
    // rather than an `InputSiv`/`RegFile::Input` register.
    //
    // Ensure these still result in the correct WGSL `@builtin(...)` declarations.
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let src_builtin = |builtin: aero_d3d11::sm4_ir::ComputeBuiltin| SrcOperand {
        kind: SrcKind::ComputeBuiltin(builtin),
        swizzle: Swizzle::XYZW,
        modifier: aero_d3d11::OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: src_builtin(aero_d3d11::sm4_ir::ComputeBuiltin::DispatchThreadId),
            },
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 1,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: src_builtin(aero_d3d11::sm4_ir::ComputeBuiltin::GroupThreadId),
            },
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 2,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: src_builtin(aero_d3d11::sm4_ir::ComputeBuiltin::GroupId),
            },
            Sm4Inst::Mov {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 3,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: src_builtin(aero_d3d11::sm4_ir::ComputeBuiltin::GroupIndex),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(translated.wgsl.contains("@builtin(global_invocation_id)"));
    assert!(translated.wgsl.contains("@builtin(local_invocation_id)"));
    assert!(translated.wgsl.contains("@builtin(workgroup_id)"));
    assert!(translated.wgsl.contains("@builtin(local_invocation_index)"));
}

#[test]
fn translates_compute_buffer_load_store_raw() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let addr_from_thread_id = SrcOperand {
        kind: SrcKind::Register(aero_d3d11::RegisterRef {
            file: aero_d3d11::RegFile::Input,
            index: 0,
        }),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            // Bind v0 to SV_DispatchThreadID so translation emits global_invocation_id.
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: 20,
            },
            Sm4Decl::ResourceBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
        ],
        instructions: vec![
            Sm4Inst::LdRaw {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                addr: addr_from_thread_id.clone(),
                buffer: aero_d3d11::BufferRef { slot: 0 },
            },
            Sm4Inst::StoreRaw {
                uav: aero_d3d11::UavRef { slot: 0 },
                addr: addr_from_thread_id,
                value: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("@builtin(global_invocation_id)"),
        "expected CsIn to contain global_invocation_id:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("bitcast<f32>(input.global_invocation_id.x)"),
        "expected compute sysvalue lowering to preserve integer bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("t0.data["),
        "expected buffer SRV access in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("u0.data["),
        "expected buffer UAV access in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_compute_buffer_load_store_structured() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let index_from_thread_id = SrcOperand {
        kind: SrcKind::Register(aero_d3d11::RegisterRef {
            file: aero_d3d11::RegFile::Input,
            index: 0,
        }),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };
    let zero = SrcOperand {
        kind: SrcKind::ImmediateF32([0; 4]),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: 20,
            },
            Sm4Decl::ResourceBuffer {
                slot: 0,
                stride: 16,
                kind: BufferKind::Structured,
            },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 16,
                kind: BufferKind::Structured,
            },
        ],
        instructions: vec![
            Sm4Inst::LdStructured {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                index: index_from_thread_id.clone(),
                offset: zero.clone(),
                buffer: aero_d3d11::BufferRef { slot: 0 },
            },
            Sm4Inst::StoreStructured {
                uav: aero_d3d11::UavRef { slot: 0 },
                index: index_from_thread_id,
                offset: zero,
                value: SrcOperand {
                    kind: SrcKind::Register(aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: aero_d3d11::OperandModifier::None,
                },
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("* 16u"),
        "expected structured stride (16 bytes) to appear in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("t0.data["),
        "expected structured SRV access in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("u0.data["),
        "expected structured UAV access in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_compute_buffer_uav_load_raw() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let zero = SrcOperand {
        kind: SrcKind::ImmediateF32([0; 4]),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
        ],
        instructions: vec![
            Sm4Inst::LdUavRaw {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                addr: zero,
                uav: aero_d3d11::UavRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("u0.data["),
        "expected UAV load to index u0.data:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("t0.data["),
        "unexpected SRV buffer access in UAV-only shader:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_compute_buffer_uav_load_structured() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let zero = SrcOperand {
        kind: SrcKind::ImmediateF32([0; 4]),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };
    let one_u32 = SrcOperand {
        // Structured buffer indices are integer-typed. The DXBC register file is untyped, so encode
        // an integer `1` as raw lane bits.
        kind: SrcKind::ImmediateF32([1; 4]),
        swizzle: Swizzle::XXXX,
        modifier: aero_d3d11::OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 16,
                kind: BufferKind::Structured,
            },
        ],
        instructions: vec![
            Sm4Inst::LdStructuredUav {
                dst: aero_d3d11::DstOperand {
                    reg: aero_d3d11::RegisterRef {
                        file: aero_d3d11::RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                index: one_u32,
                offset: zero,
                uav: aero_d3d11::UavRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("* 16u"),
        "expected structured stride (16 bytes) to appear in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("0x00000001u"),
        "expected raw integer bits 0x00000001 to be used for structured index:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("u0.data["),
        "expected structured UAV load to index u0.data:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("t0.data["),
        "unexpected SRV buffer access in UAV-only shader:\n{}",
        translated.wgsl
    );
}
