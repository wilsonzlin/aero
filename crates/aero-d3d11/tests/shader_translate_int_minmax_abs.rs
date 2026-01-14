use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, DxbcSignatureParameter, FourCC,
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn sig_param(name: &str, index: u32, register: u32, mask: u8) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type: 0,
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

fn src_imm_u32(bits: [u32; 4]) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32(bits),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
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
fn translates_integer_minmax_abs_neg_family_via_bitcasts() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let a = src_imm_u32([1, 2, 3, 4]);
    let b = src_imm_u32([5, 6, 7, 8]);
    let neg = src_imm_u32([0xffff_fffe, 0xffff_fffd, 1, 0x8000_0000]);

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            // Seed the untyped register file with raw integer bits (stored as `vec4<f32>`).
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: a.clone(),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: b.clone(),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
                src: neg.clone(),
            },
            Sm4Inst::IMin {
                dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::IMax {
                dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::UMin {
                dst: dst(RegFile::Temp, 5, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::UMax {
                dst: dst(RegFile::Temp, 6, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::IAbs {
                dst: dst(RegFile::Temp, 7, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::INeg {
                dst: dst(RegFile::Temp, 8, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 3),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure integer operations use bitcasts and the expected WGSL intrinsics.
    assert!(
        translated.wgsl.contains("bitcast<vec4<i32>>"),
        "expected signed integer ops to bitcast to i32:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<u32>>"),
        "expected unsigned integer ops to bitcast to u32:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("min("),
        "expected `min` call in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("max("),
        "expected `max` call in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("abs("),
        "expected `abs` call in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected integer results to be stored as raw bits via bitcast to f32:\n{}",
        translated.wgsl
    );
}
