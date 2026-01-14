use aero_d3d11::binding_model::{BINDING_BASE_TEXTURE, BINDING_BASE_UAV};
use aero_d3d11::DxbcFile;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, BufferRef, DstOperand, OperandModifier, RegFile,
    RegisterRef, ShaderModel, ShaderSignatures, ShaderStage, ShaderTranslateError, Sm4Decl,
    Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

fn dummy_dxbc_bytes() -> Vec<u8> {
    dxbc_test_utils::build_container(&[])
}

fn src_imm_u32_bits(bits: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32([bits; 4]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    }
}

#[test]
fn translates_structured_buffer_address_math() {
    let dxbc_bytes = dummy_dxbc_bytes();
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC parse");
    let signatures = ShaderSignatures::default();

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
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                index: src_imm_u32_bits(1),
                offset: src_imm_u32_bits(0),
                buffer: BufferRef { slot: 0 },
            },
            Sm4Inst::StoreStructured {
                uav: UavRef { slot: 0 },
                index: src_imm_u32_bits(1),
                offset: src_imm_u32_bits(0),
                value: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    naga::front::wgsl::parse_str(&translated.wgsl).expect("WGSL should parse");

    // Ensure the generated bindings match the binding model.
    assert!(translated.wgsl.contains(&format!(
        "@binding({}) var<storage, read> t0",
        BINDING_BASE_TEXTURE
    )));
    assert!(translated.wgsl.contains(&format!(
        "@binding({}) var<storage, read_write> u0",
        BINDING_BASE_UAV
    )));

    // Address calculation: base_word = (index * stride_bytes + byte_offset) / 4.
    assert!(
        translated.wgsl.contains("* 16u") && translated.wgsl.contains("/ 4u"),
        "expected stride and /4 scaling in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("t0.data[ld_struct_base0 + 0u]"),
        "expected structured load to index into t0 with ld_struct_base0"
    );
    assert!(
        translated.wgsl.contains("u0.data[store_struct_base1 + 0u]"),
        "expected structured store to index into u0 with store_struct_base1"
    );
}

#[test]
fn rejects_structured_buffer_without_stride_declaration() {
    let dxbc_bytes = dummy_dxbc_bytes();
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC parse");
    let signatures = ShaderSignatures::default();

    // Use a structured buffer instruction without declaring `dcl_resource_structured` / stride.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::LdStructured {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                index: src_imm_u32_bits(0),
                offset: src_imm_u32_bits(0),
                buffer: BufferRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .expect_err("expected structured buffer stride error");
    assert!(
        matches!(
            err,
            ShaderTranslateError::MissingStructuredBufferStride {
                kind: "srv_buffer",
                slot: 0
            }
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn rejects_structured_buffer_stride_not_multiple_of4() {
    let dxbc_bytes = dummy_dxbc_bytes();
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC parse");
    let signatures = ShaderSignatures::default();

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::ResourceBuffer {
                slot: 0,
                stride: 6,
                kind: BufferKind::Structured,
            },
        ],
        instructions: vec![
            Sm4Inst::LdStructured {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Temp,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                index: src_imm_u32_bits(0),
                offset: src_imm_u32_bits(0),
                buffer: BufferRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .expect_err("expected structured buffer stride error");
    assert!(
        matches!(
            err,
            ShaderTranslateError::StructuredBufferStrideNotMultipleOf4 {
                kind: "srv_buffer",
                slot: 0,
                stride_bytes: 6
            }
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn rejects_uav_structured_buffer_without_stride_declaration() {
    let dxbc_bytes = dummy_dxbc_bytes();
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC parse");
    let signatures = ShaderSignatures::default();

    // Use a structured UAV store without declaring `dcl_uav_structured` / stride.
    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::StoreStructured {
                uav: UavRef { slot: 0 },
                index: src_imm_u32_bits(0),
                offset: src_imm_u32_bits(0),
                value: src_imm_u32_bits(0),
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .expect_err("expected structured UAV stride error");
    assert!(
        matches!(
            err,
            ShaderTranslateError::MissingStructuredBufferStride {
                kind: "uav_buffer",
                slot: 0
            }
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn rejects_uav_structured_buffer_stride_not_multiple_of4() {
    let dxbc_bytes = dummy_dxbc_bytes();
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC parse");
    let signatures = ShaderSignatures::default();

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 6,
                kind: BufferKind::Structured,
            },
        ],
        instructions: vec![
            Sm4Inst::StoreStructured {
                uav: UavRef { slot: 0 },
                index: src_imm_u32_bits(0),
                offset: src_imm_u32_bits(0),
                value: src_imm_u32_bits(0),
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .expect_err("expected structured UAV stride error");
    assert!(
        matches!(
            err,
            ShaderTranslateError::StructuredBufferStrideNotMultipleOf4 {
                kind: "uav_buffer",
                slot: 0,
                stride_bytes: 6
            }
        ),
        "unexpected error: {err:?}"
    );
}
