use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BindingKind, DxbcFile, DxbcSignatureParameter,
    FourCC, ShaderModel, ShaderStage, Sm4Inst, Sm4Module,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_PCSG: FourCC = FourCC(*b"PCSG");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn build_signature_chunk(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name.as_str(),
            semantic_index: p.semantic_index,
            system_value_type: p.system_value_type,
            component_type: p.component_type,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.read_write_mask,
            stream: u32::from(p.stream),
            min_precision: u32::from(p.min_precision),
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

#[test]
fn hs_translation_reflects_internal_stage_interface_buffers() {
    // Minimal HS container with empty signatures. The shader itself doesn't read or write any
    // user-facing resources; we only care about the expansion-internal bindings.
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&[])),
        (FOURCC_PCSG, build_signature_chunk(&[])),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Hull,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![Sm4Inst::Ret],
    };
    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

    let internal_group = aero_d3d11::binding_model::BIND_GROUP_INTERNAL_EMULATION;
    let expect = [
        aero_d3d11::runtime::tessellation::BINDING_VS_OUT_REGS,
        aero_d3d11::runtime::tessellation::BINDING_HS_OUT_REGS,
        aero_d3d11::runtime::tessellation::BINDING_HS_PATCH_CONSTANTS,
        aero_d3d11::runtime::tessellation::BINDING_HS_TESS_FACTORS,
    ];

    for binding_num in expect {
        let b = translated
            .reflection
            .bindings
            .iter()
            .find(|b| b.group == internal_group && b.binding == binding_num)
            .unwrap_or_else(|| {
                panic!(
                    "missing expansion-internal HS binding @group({internal_group}) @binding({binding_num}); got bindings: {:?}",
                    translated.reflection.bindings
                )
            });
        assert_eq!(b.visibility, wgpu::ShaderStages::COMPUTE);
        assert!(
            matches!(
                b.kind,
                BindingKind::ExpansionStorageBuffer { read_only: false }
            ),
            "unexpected binding kind for @binding({binding_num}): {:?}",
            b.kind
        );
    }
}

