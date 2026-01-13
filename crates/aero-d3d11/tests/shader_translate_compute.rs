use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BufferKind, BufferRef, DstOperand, DxbcFile,
    FourCC, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Decl, Sm4Inst,
    Sm4Module, SrcKind, SrcOperand, Swizzle, UavRef, WriteMask,
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

fn dst_temp(index: u32, mask: WriteMask) -> DstOperand {
    DstOperand {
        reg: RegisterRef {
            file: RegFile::Temp,
            index,
        },
        mask,
        saturate: false,
    }
}

fn src_temp(index: u32, swizzle: Swizzle) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef {
            file: RegFile::Temp,
            index,
        }),
        swizzle,
        modifier: OperandModifier::None,
    }
}

fn src_imm_u32_scalar(v: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32([v, v, v, v]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    }
}

#[test]
fn translates_compute_thread_group_size_and_entry_point() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 8, y: 4, z: 1 }],
        instructions: vec![Sm4Inst::Ret],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("fn cs_main"));
    assert!(translated.wgsl.contains("@workgroup_size(8, 4, 1)"));
}

#[test]
fn translates_compute_raw_buffer_load_store() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
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
                dst: dst_temp(0, WriteMask::XYZW),
                addr: src_imm_u32_scalar(0),
                buffer: BufferRef { slot: 0 },
            },
            Sm4Inst::StoreRaw {
                uav: UavRef { slot: 0 },
                addr: src_imm_u32_scalar(0),
                value: src_temp(0, Swizzle::XYZW),
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure the storage buffer bindings are referenced from the instruction stream.
    assert!(translated.wgsl.contains("t0.data["));
    assert!(translated.wgsl.contains("u0.data["));
}

#[test]
fn translates_compute_structured_buffer_load_store() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
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
                dst: dst_temp(0, WriteMask::XYZW),
                index: src_imm_u32_scalar(2),
                offset: src_imm_u32_scalar(4),
                buffer: BufferRef { slot: 0 },
            },
            Sm4Inst::StoreStructured {
                uav: UavRef { slot: 0 },
                index: src_imm_u32_scalar(2),
                offset: src_imm_u32_scalar(4),
                value: src_temp(0, Swizzle::XYZW),
                mask: WriteMask(0b0011),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("* 16u"),
        "expected stride 16 to participate in address calculation, wgsl={}",
        translated.wgsl
    );
}
