use aero_d3d11::sm4::opcode::{
    SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY, SYNC_FLAG_THREAD_GROUP_SYNC, SYNC_FLAG_UAV_MEMORY,
};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, FourCC, ShaderModel, ShaderStage,
    ShaderTranslateError, Sm4Decl, Sm4Inst, Sm4Module,
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
    assert!(translated.wgsl.contains("storageBarrier()"));
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
