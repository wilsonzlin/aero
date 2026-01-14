use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, DxbcSignatureParameter, FourCC,
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_PCG1: FourCC = FourCC(*b"PCG1");

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

fn dst(file: RegFile, index: u32, mask: WriteMask) -> aero_d3d11::DstOperand {
    aero_d3d11::DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_imm(vals: [f32; 4]) -> SrcOperand {
    let bits = vals.map(f32::to_bits);
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
fn hs_patch_constants_route_tess_factors_to_compact_buffer() {
    // Patch-constant signature:
    // - 3 outer factors (SV_TessFactor[0..2])
    // - 1 inner factor (SV_InsideTessFactor[0])
    // - 1 user patch constant (FOO0)
    //
    // The signature intentionally packs tess factors into two output registers to ensure the
    // translator extracts scalars correctly.
    let pcsg_params = vec![
        sig_param("SV_TessFactor", 0, 0, 0b0001), // o0.x -> outer[0]
        sig_param("SV_TessFactor", 1, 0, 0b0010), // o0.y -> outer[1]
        sig_param("SV_TessFactor", 2, 1, 0b0001), // o1.x -> outer[2]
        sig_param("SV_InsideTessFactor", 0, 1, 0b0010), // o1.y -> inner[0]
        sig_param("FOO", 0, 2, 0b1111),           // o2.xyzw -> patch constant
    ];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&[])),
        (FOURCC_PCG1, build_signature_chunk(&pcsg_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Hull,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            // Control-point phase terminator. FXC emits separate `ret`s per HS phase; the translator
            // uses the first top-level `ret` to split control-point vs patch-constant execution.
            Sm4Inst::Ret,
            // Tess factors.
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::X),
                src: src_imm([1.0, 1.0, 1.0, 1.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::Y),
                src: src_imm([2.0, 2.0, 2.0, 2.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 1, WriteMask::X),
                src: src_imm([3.0, 3.0, 3.0, 3.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 1, WriteMask::Y),
                src: src_imm([4.0, 4.0, 4.0, 4.0]),
            },
            // User patch constant.
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 2, WriteMask::XYZW),
                src: src_imm([10.0, 11.0, 12.0, 13.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure tess factors are written to the dedicated tess-factor buffer with the expected
    // compact layout: {outer0, outer1, outer2, inner0} packed into one vec4 per patch.
    let tess_vec4s_per_patch = aero_d3d11::runtime::tessellation::HS_TESS_FACTOR_VEC4S_PER_PATCH;
    assert!(translated.wgsl.contains(&format!(
        "const HS_TESS_FACTOR_STRIDE: u32 = {tess_vec4s_per_patch}u;"
    )));
    assert!(translated
        .wgsl
        .contains("let tf_base: u32 = hs_primitive_id * HS_TESS_FACTOR_STRIDE;"));
    assert!(translated.wgsl.contains("tf0.x = o0.x;"));
    assert!(translated.wgsl.contains("tf0.y = o0.y;"));
    assert!(translated.wgsl.contains("tf0.z = o1.x;"));
    assert!(translated.wgsl.contains("tf0.w = o1.y;"));
    assert!(translated
        .wgsl
        .contains("hs_store_tess_factors(tf_base + 0u, tf0);"));

    // User patch constants should land in the patch-constant register file (and not include the
    // tess-factor-only registers).
    assert!(translated
        .wgsl
        .contains("hs_store_patch_constants(hs_out_base + 2u, o2);"));
    assert!(
        !translated
            .wgsl
            .contains("hs_store_patch_constants(hs_out_base + 0u"),
        "tess-factor register o0 should not be written into hs_patch_constants:\n{}",
        translated.wgsl
    );
    assert!(
        !translated
            .wgsl
            .contains("hs_store_patch_constants(hs_out_base + 1u"),
        "tess-factor register o1 should not be written into hs_patch_constants:\n{}",
        translated.wgsl
    );
}
