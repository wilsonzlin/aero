use aero_d3d11::sm4::opcode::{
    SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY, SYNC_FLAG_THREAD_GROUP_SYNC, SYNC_FLAG_UAV_MEMORY,
};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, FourCC, OperandModifier,
    PredicateOperand, PredicateRef, ShaderModel, ShaderStage, ShaderTranslateError, Sm4Decl,
    Sm4Inst, Sm4Module, Sm4TestBool, SrcKind, SrcOperand, Swizzle,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

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
fn translates_sync_with_group_sync_to_wgsl() {
    // Translator-only test: build the decoded IR directly.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_THREAD_GROUP_SYNC | SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    // Minimal DXBC container; compute translation does not currently rely on signatures.
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_eq!(translated.stage, ShaderStage::Compute);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("workgroupBarrier()"));
    assert!(
        !translated.wgsl.contains("storageBarrier()"),
        "TGSM-only barriers should not emit a storage barrier"
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn translates_sync_uav_with_group_sync_to_wgsl() {
    // Translator-only test: build the decoded IR directly.
    //
    // This corresponds to `DeviceMemoryBarrierWithGroupSync()`.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_THREAD_GROUP_SYNC | SYNC_FLAG_UAV_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(translated.wgsl.contains("storageBarrier()"));
    assert!(
        !translated.wgsl.contains("workgroupBarrier()"),
        "UAV-only barriers should not emit a workgroup barrier"
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn translates_sync_all_memory_with_group_sync_to_wgsl() {
    // Translator-only test: build the decoded IR directly.
    //
    // This corresponds to `AllMemoryBarrierWithGroupSync()`.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_THREAD_GROUP_SYNC
                    | SYNC_FLAG_UAV_MEMORY
                    | SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(translated.wgsl.contains("storageBarrier()"));
    assert!(translated.wgsl.contains("workgroupBarrier()"));
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn translates_sync_uav_fence_only_to_wgsl() {
    // Translator-only test: build the decoded IR directly.
    //
    // Fence-only `sync` variants must not be translated to WGSL `workgroupBarrier()`, since that
    // introduces a workgroup execution barrier that is not present in D3D's fence-only forms.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_UAV_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_eq!(translated.stage, ShaderStage::Compute);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("storageBarrier()"));
    assert!(
        !translated.wgsl.contains("workgroupBarrier()"),
        "fence-only sync must not introduce a workgroup barrier"
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn translates_sync_with_group_sync_inside_control_flow_to_wgsl() {
    // Translator-only test: build the decoded IR directly.
    //
    // Group-sync barriers are expected to be used in uniform control flow in D3D, but can legally
    // appear inside structured control-flow constructs such as loops and `if` blocks. Ensure we can
    // translate such shaders without introducing extra restrictions here.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::If {
                cond: SrcOperand {
                    kind: SrcKind::ImmediateF32([1.0f32.to_bits(); 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::Sync {
                flags: SYNC_FLAG_THREAD_GROUP_SYNC | SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY,
            },
            Sm4Inst::EndIf,
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(translated.wgsl.contains("if ("));
    assert!(
        !translated.wgsl.contains("storageBarrier()"),
        "TGSM-only barriers should not emit a storage barrier"
    );
    assert!(translated.wgsl.contains("workgroupBarrier()"));
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn rejects_sync_with_group_sync_after_conditional_return() {
    // Even at top-level, a barrier after a conditional return may not be executed by all
    // invocations. We reject to avoid generating WGSL that can deadlock.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::If {
                cond: SrcOperand {
                    kind: SrcKind::ImmediateF32([1.0f32.to_bits(); 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::Ret,
            Sm4Inst::EndIf,
            Sm4Inst::Sync {
                flags: SYNC_FLAG_THREAD_GROUP_SYNC | SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    match err {
        ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
            assert_eq!(inst_index, 3);
            assert_eq!(opcode, "sync_group_sync_after_conditional_return");
        }
        other => panic!("expected UnsupportedInstruction for sync, got {other:?}"),
    }
}

#[test]
fn rejects_sync_uav_fence_only_inside_control_flow() {
    // Fence-only `sync` instructions are allowed in divergent control flow in DXBC, but WGSL's
    // `storageBarrier()` is lowered by WebGPU/Naga as a workgroup-level barrier, which can
    // deadlock when not executed uniformly.
    //
    // We conservatively reject the translation when the fence-only sync appears inside structured
    // control flow.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::If {
                cond: SrcOperand {
                    kind: SrcKind::ImmediateF32([1.0f32.to_bits(); 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::Sync {
                flags: SYNC_FLAG_UAV_MEMORY,
            },
            Sm4Inst::EndIf,
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    match err {
        ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
            assert_eq!(inst_index, 1);
            assert_eq!(opcode, "sync_fence_only_in_control_flow");
        }
        other => panic!("expected UnsupportedInstruction for sync, got {other:?}"),
    }
}

#[test]
fn rejects_predicated_sync_uav_fence_only() {
    // Predication is lowered to WGSL `if` control flow; barriers must be executed uniformly.
    // We conservatively reject predicated `sync` when it would emit a barrier built-in.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Predicated {
                pred: PredicateOperand {
                    reg: PredicateRef { index: 0 },
                    component: 0,
                    invert: false,
                },
                inner: Box::new(Sm4Inst::Sync {
                    flags: SYNC_FLAG_UAV_MEMORY,
                }),
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    match err {
        ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
            assert_eq!(inst_index, 0);
            assert_eq!(opcode, "predicated_sync");
        }
        other => panic!("expected UnsupportedInstruction for predicated sync, got {other:?}"),
    }
}

#[test]
fn rejects_sync_uav_fence_only_after_conditional_return() {
    // Even if the `sync` itself is at top-level, earlier conditional returns can cause some
    // invocations to skip it. Since WebGPU/Naga lower `storageBarrier()` as a workgroup-level
    // barrier, this can deadlock, so we reject the translation.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::If {
                cond: SrcOperand {
                    kind: SrcKind::ImmediateF32([1.0f32.to_bits(); 4]),
                    swizzle: Swizzle::XXXX,
                    modifier: OperandModifier::None,
                },
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::Ret,
            Sm4Inst::EndIf,
            Sm4Inst::Sync {
                flags: SYNC_FLAG_UAV_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    match err {
        ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
            assert_eq!(inst_index, 3);
            assert_eq!(opcode, "sync_fence_only_after_conditional_return");
        }
        other => panic!("expected UnsupportedInstruction for sync, got {other:?}"),
    }
}

#[test]
fn translates_sync_all_memory_fence_only_to_wgsl() {
    // "AllMemoryBarrier()" fence-only form: both UAV+TGSM bits set, but no group sync.
    //
    // The translator can currently only model the UAV/storage fence portion (via `storageBarrier()`),
    // but must still avoid introducing a control barrier (`workgroupBarrier()`).
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_UAV_MEMORY | SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(translated.wgsl.contains("storageBarrier()"));
    assert!(
        !translated.wgsl.contains("workgroupBarrier()"),
        "fence-only sync must not introduce a workgroup barrier"
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn translates_sync_tgsm_fence_only_is_noop() {
    // "GroupMemoryBarrier()" fence-only form: TGSM bit set, but no group sync.
    //
    // We currently don't model TGSM/workgroup shared memory, so this becomes a no-op (but must still
    // avoid introducing a control barrier).
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert!(
        !translated.wgsl.contains("storageBarrier()"),
        "TGSM-only fence-only sync should not emit a storage barrier"
    );
    assert!(
        !translated.wgsl.contains("workgroupBarrier()"),
        "TGSM-only fence-only sync must not introduce a workgroup barrier"
    );
    assert_wgsl_validates(&translated.wgsl);
}

#[test]
fn rejects_sync_in_non_compute_stage() {
    // `sync` instructions are only valid in compute shaders in SM5. The translator should reject
    // them in non-compute stages (instead of emitting unsafe barriers).
    let module = Sm4Module {
        stage: ShaderStage::Vertex,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_UAV_MEMORY,
            },
            Sm4Inst::Ret,
        ],
    };

    let isgn = dxbc_test_utils::build_signature_chunk_v0(&[]);
    let osgn = dxbc_test_utils::build_signature_chunk_v0(&[dxbc_test_utils::SignatureEntryDesc {
        // Use legacy `POSITION` with `system_value_type = 0` to trigger the translator's
        // `SV_Position` fallback path.
        semantic_name: "POSITION",
        semantic_index: 0,
        system_value_type: 0,
        component_type: 0,
        register: 0,
        mask: 0xF,
        read_write_mask: 0xF,
        stream: 0,
        min_precision: 0,
    }]);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, isgn),
        (FOURCC_OSGN, osgn),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    match err {
        ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
            assert_eq!(inst_index, 0);
            assert_eq!(opcode, "sync");
        }
        other => panic!("expected UnsupportedInstruction for sync, got {other:?}"),
    }
}

#[test]
fn rejects_sync_with_unknown_flags() {
    // The translator only models a subset of `D3D11_SB_SYNC_FLAGS`; unknown bits should be rejected
    // to avoid silently dropping ordering semantics.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::Sync {
                flags: SYNC_FLAG_UAV_MEMORY | 0x8,
            },
            Sm4Inst::Ret,
        ],
    };

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    match err {
        ShaderTranslateError::UnsupportedInstruction { inst_index, opcode } => {
            assert_eq!(inst_index, 0);
            assert_eq!(opcode, "sync_unknown_flags(0x8)");
        }
        other => panic!("expected UnsupportedInstruction for sync, got {other:?}"),
    }
}
