use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, DxbcSignatureParameter, FourCC,
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Module, SrcKind,
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

fn src_imm_bits(bits: [u32; 4]) -> SrcOperand {
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

fn minimal_pixel_shader_dxbc() -> (DxbcFile<'static>, aero_d3d11::ShaderSignatures) {
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);

    // The DXBC is owned by this function, but the returned `DxbcFile` borrows it. Leak the bytes
    // for test simplicity; each test case is tiny.
    let leaked: &'static [u8] = Box::leak(dxbc_bytes.into_boxed_slice());
    let dxbc = DxbcFile::parse(leaked).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");
    (dxbc, signatures)
}

#[test]
fn translates_bfrev_to_reverse_bits() {
    let (dxbc, signatures) = minimal_pixel_shader_dxbc();

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            aero_d3d11::Sm4Inst::Bfrev {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm_bits([1, 2, 3, 4]),
            },
            aero_d3d11::Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("reverseBits("),
        "expected reverseBits builtin in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_countbits_to_count_one_bits() {
    let (dxbc, signatures) = minimal_pixel_shader_dxbc();

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            aero_d3d11::Sm4Inst::CountBits {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm_bits([0xffff_ffff, 0, 0xdead_beef, 0x0123_4567]),
            },
            aero_d3d11::Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("countOneBits("),
        "expected countOneBits builtin in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_firstbit_hi_to_first_leading_bit() {
    let (dxbc, signatures) = minimal_pixel_shader_dxbc();

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            aero_d3d11::Sm4Inst::FirstbitHi {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm_bits([0x8000_0000, 0, 0x0000_0010, 0xffff_ffff]),
            },
            aero_d3d11::Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("firstLeadingBit("),
        "expected firstLeadingBit builtin in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_firstbit_lo_to_first_trailing_bit() {
    let (dxbc, signatures) = minimal_pixel_shader_dxbc();

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            aero_d3d11::Sm4Inst::FirstbitLo {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm_bits([0x8000_0000, 0, 0x0000_0010, 0xffff_ffff]),
            },
            aero_d3d11::Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("firstTrailingBit("),
        "expected firstTrailingBit builtin in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_firstbit_shi_to_first_leading_bit_signed() {
    let (dxbc, signatures) = minimal_pixel_shader_dxbc();

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            aero_d3d11::Sm4Inst::FirstbitShi {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                // Signed -1 should return -1; this path should use signed i32 semantics.
                src: src_imm_bits([0xffff_ffff, 0x8000_0000, 0, 1]),
            },
            aero_d3d11::Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("firstLeadingBit("),
        "expected firstLeadingBit builtin in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<i32>"),
        "expected signed i32 bitcasts in WGSL for firstbit_shi:\n{}",
        translated.wgsl
    );
}
