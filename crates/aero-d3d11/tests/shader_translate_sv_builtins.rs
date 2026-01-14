use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, DxbcSignatureParameter, FourCC,
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module,
    SrcKind, SrcOperand, Swizzle, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

// D3D_NAME values (from `d3dcommon.h` / `d3d11shader.h`).
const D3D_NAME_POSITION: u32 = 1;
const D3D_NAME_VERTEX_ID: u32 = 6;
const D3D_NAME_INSTANCE_ID: u32 = 8;
const D3D_NAME_IS_FRONT_FACE: u32 = 9;
const D3D_NAME_TARGET: u32 = 64;

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn sig_param(
    name: &str,
    index: u32,
    system_value_type: u32,
    register: u32,
    mask: u8,
) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type,
        component_type: 0,
        register,
        mask,
        read_write_mask: mask,
        stream: 0,
        min_precision: 0,
    }
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
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn dst(file: RegFile, index: u32, mask: WriteMask) -> aero_d3d11::DstOperand {
    aero_d3d11::DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_reg(file: RegFile, index: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef { file, index }),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

fn src_reg_swizzle(file: RegFile, index: u32, swizzle: Swizzle) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef { file, index }),
        swizzle,
        modifier: OperandModifier::None,
    }
}

fn assert_wgsl_parses(wgsl: &str) {
    naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
}

#[test]
fn translates_vertex_system_values_from_siv_decls() {
    // Non-canonical semantic strings + intentionally wrong `system_value_type` values:
    // the translator should prefer `dcl_input_siv` over the signature.
    let isgn_params = vec![
        sig_param("NOT_SV_VERTEXID", 0, D3D_NAME_INSTANCE_ID, 0, 0b0001),
        sig_param("NOT_SV_INSTANCEID", 0, D3D_NAME_VERTEX_ID, 1, 0b0001),
    ];
    let osgn_params = vec![
        sig_param("NOT_SV_POSITION", 0, 0, 0, 0b1111),
        sig_param("VARYING", 0, 0, 1, 0b0011),
    ];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Vertex,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::X,
                sys_value: D3D_NAME_VERTEX_ID,
            },
            Sm4Decl::InputSiv {
                reg: 1,
                mask: WriteMask::X,
                sys_value: D3D_NAME_INSTANCE_ID,
            },
            Sm4Decl::OutputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: D3D_NAME_POSITION,
            },
        ],
        instructions: vec![
            // Copy vertex_index into o1.x.
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 1, WriteMask::X),
                src: src_reg(RegFile::Input, 0),
            },
            // Copy instance_index into o1.y (swizzle xxxx so rhs.y == rhs.x).
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 1, WriteMask::Y),
                src: src_reg_swizzle(RegFile::Input, 1, Swizzle::XXXX),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);

    assert!(translated.wgsl.contains("@builtin(vertex_index)"));
    assert!(translated.wgsl.contains("@builtin(instance_index)"));
    assert!(translated
        .wgsl
        .contains("@builtin(position) pos: vec4<f32>"));
}

#[test]
fn translates_pixel_system_values_front_facing() {
    // Input semantics are non-canonical; `dcl_input_siv` should drive builtin mapping.
    let isgn_params = vec![
        sig_param("NOT_SV_POSITION", 0, 0, 0, 0b1111),
        sig_param("NOT_SV_ISFRONTFACE", 0, 0, 1, 0b0001),
    ];
    // Output semantic is non-canonical; `system_value_type` should be sufficient.
    let osgn_params = vec![sig_param("NOT_SV_TARGET", 0, D3D_NAME_TARGET, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::InputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: D3D_NAME_POSITION,
            },
            Sm4Decl::InputSiv {
                reg: 1,
                mask: WriteMask::X,
                sys_value: D3D_NAME_IS_FRONT_FACE,
            },
        ],
        instructions: vec![
            // Write `SV_IsFrontFace` into the output colour.
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::X),
                src: src_reg(RegFile::Input, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);

    assert!(translated.wgsl.contains("@builtin(front_facing)"));
    assert!(translated
        .wgsl
        .contains("@builtin(position) pos: vec4<f32>"));
}
