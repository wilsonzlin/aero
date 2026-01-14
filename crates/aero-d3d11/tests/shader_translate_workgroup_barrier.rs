use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, FourCC, ShaderModel, ShaderStage,
    Sm4Decl, Sm4Inst, Sm4Module,
};
use aero_d3d11::sm4::opcode::{
    SYNC_FLAG_THREAD_GROUP_SHARED_MEMORY, SYNC_FLAG_THREAD_GROUP_SYNC, SYNC_FLAG_UAV_MEMORY,
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
    // Fence-only `sync` variants must NOT be translated to WGSL `workgroupBarrier()` since that
    // introduces a control barrier and can deadlock when used in divergent control flow.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![Sm4Inst::Sync { flags: SYNC_FLAG_UAV_MEMORY }, Sm4Inst::Ret],
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
