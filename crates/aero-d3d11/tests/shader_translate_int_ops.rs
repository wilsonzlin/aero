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
    // Header: param_count + param_offset.
    let param_count = u32::try_from(params.len()).expect("too many signature params");
    let header_len = 8usize;
    let entry_size = 24usize;
    let table_len = params.len() * entry_size;

    // Strings appended after table.
    let mut strings = Vec::<u8>::new();
    let mut name_offsets = Vec::<u32>::with_capacity(params.len());
    for p in params {
        name_offsets.push((header_len + table_len + strings.len()) as u32);
        strings.extend_from_slice(p.semantic_name.as_bytes());
        strings.push(0);
    }

    let mut bytes = Vec::with_capacity(header_len + table_len + strings.len());
    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&(header_len as u32).to_le_bytes());

    for (p, &name_off) in params.iter().zip(name_offsets.iter()) {
        bytes.extend_from_slice(&name_off.to_le_bytes());
        bytes.extend_from_slice(&p.semantic_index.to_le_bytes());
        bytes.extend_from_slice(&p.system_value_type.to_le_bytes());
        bytes.extend_from_slice(&p.component_type.to_le_bytes());
        bytes.extend_from_slice(&p.register.to_le_bytes());
        bytes.push(p.mask);
        bytes.push(p.read_write_mask);
        bytes.push(p.stream);
        bytes.push(p.min_precision);
    }
    bytes.extend_from_slice(&strings);
    bytes
}

fn dst(file: RegFile, index: u32) -> aero_d3d11::DstOperand {
    aero_d3d11::DstOperand {
        reg: RegisterRef { file, index },
        mask: WriteMask::XYZW,
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

fn src_imm_u32(values: [u32; 4]) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32(values),
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
fn translates_integer_and_bitwise_ops() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            // Seed some temps with raw integer bit patterns.
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0),
                src: src_imm_u32([0x0000_00ff, 0, 0, 0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1),
                src: src_imm_u32([1, 1, 1, 1]),
            },
            // Integer arithmetic.
            Sm4Inst::IAdd {
                dst: dst(RegFile::Temp, 2),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::IMul {
                dst: dst(RegFile::Temp, 3),
                a: src_reg(RegFile::Temp, 2),
                b: src_reg(RegFile::Temp, 1),
            },
            // Bitwise ops (u32 space).
            Sm4Inst::And {
                dst: dst(RegFile::Temp, 4),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Or {
                dst: dst(RegFile::Temp, 5),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Xor {
                dst: dst(RegFile::Temp, 6),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Not {
                dst: dst(RegFile::Temp, 7),
                src: src_reg(RegFile::Temp, 0),
            },
            // Shifts.
            Sm4Inst::IShl {
                dst: dst(RegFile::Temp, 8),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::UShr {
                dst: dst(RegFile::Temp, 9),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::IShr {
                dst: dst(RegFile::Temp, 10),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0),
                src: src_reg(RegFile::Temp, 10),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Integer ops should reinterpret the register file as u32/i32 and emit native bitwise ops.
    assert!(
        translated.wgsl.contains("bitcast<vec4<u32>>"),
        "expected u32 bitcasts in WGSL:\n{}",
        translated.wgsl
    );
    assert!(translated.wgsl.contains("&"));
    assert!(translated.wgsl.contains("|"));
    assert!(translated.wgsl.contains("^"));
    assert!(translated.wgsl.contains("<<"));
    assert!(translated.wgsl.contains(">>"));
}
