use aero_d3d11::{
    binding_model::{BINDING_BASE_TEXTURE, BINDING_BASE_UAV},
    parse_signatures, translate_sm4_module_to_wgsl, BindingKind, BufferKind, BufferRef, DstOperand,
    DxbcFile, FourCC, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage,
    ShaderTranslateError, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, UavRef,
    WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_RDEF: FourCC = FourCC(*b"RDEF");

fn build_minimal_rdef_single_resource(
    name: &str,
    input_type: u32,
    bind_point: u32,
    bind_count: u32,
) -> Vec<u8> {
    // Header (8 DWORDs / 32 bytes) + resource binding table entry (32 bytes) + string table.
    let header_len = 32u32;
    let rb_offset = header_len;
    let string_offset = header_len + 32;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_count
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_offset
    bytes.extend_from_slice(&1u32.to_le_bytes()); // rb_count
    bytes.extend_from_slice(&rb_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // target
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // creator_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // interface_slot_count

    // Resource binding desc (32 bytes).
    bytes.extend_from_slice(&string_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&input_type.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // return_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // dimension
    bytes.extend_from_slice(&0u32.to_le_bytes()); // sample_count
    bytes.extend_from_slice(&bind_point.to_le_bytes());
    bytes.extend_from_slice(&bind_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags

    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    bytes
}

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

#[test]
fn rejects_store_structured_with_empty_write_mask() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 16,
                kind: BufferKind::Structured,
            },
        ],
        instructions: vec![
            Sm4Inst::StoreStructured {
                uav: UavRef { slot: 0 },
                index: src_imm_u32_scalar(0),
                offset: src_imm_u32_scalar(0),
                value: src_imm_u32_scalar(0),
                mask: WriteMask(0),
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::UnsupportedWriteMask {
            opcode: "store_structured",
            mask,
            ..
        } if mask.0 == 0
    ));
}

#[test]
fn rdef_expands_srv_buffer_array_slots() {
    // Shader reads from t2, but RDEF declares an array bound at t0..t3.
    let rdef_bytes = build_minimal_rdef_single_resource("buf_array", 7, 0, 4); // D3D_SIT_BYTEADDRESS

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new()), (FOURCC_RDEF, rdef_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::LdRaw {
                dst: dst_temp(0, WriteMask::XYZW),
                addr: src_imm_u32_scalar(0),
                buffer: BufferRef { slot: 2 },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure the full t0..t3 binding range is declared.
    assert!(translated.wgsl.contains(&format!(
        "@group(2) @binding({}) var<storage, read> t0: AeroStorageBufferU32;",
        BINDING_BASE_TEXTURE
    )));
    assert!(translated.wgsl.contains(&format!(
        "@group(2) @binding({}) var<storage, read> t3: AeroStorageBufferU32;",
        BINDING_BASE_TEXTURE + 3
    )));
}

#[test]
fn rdef_expands_uav_buffer_array_slots() {
    // Shader writes to u2, but RDEF declares an array bound at u0..u3.
    let rdef_bytes = build_minimal_rdef_single_resource("uav_array", 8, 0, 4); // D3D_SIT_UAV_RWBYTEADDRESS

    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new()), (FOURCC_RDEF, rdef_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
        instructions: vec![
            Sm4Inst::StoreRaw {
                uav: UavRef { slot: 2 },
                addr: src_imm_u32_scalar(0),
                value: src_imm_u32_scalar(0x1234_5678),
                mask: WriteMask::X,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure the full u0..u3 binding range is declared.
    assert!(translated.wgsl.contains(&format!(
        "@group(2) @binding({}) var<storage, read_write> u0: AeroStorageBufferU32;",
        BINDING_BASE_UAV
    )));
    assert!(translated.wgsl.contains(&format!(
        "@group(2) @binding({}) var<storage, read_write> u3: AeroStorageBufferU32;",
        BINDING_BASE_UAV + 3
    )));
}

#[test]
fn compute_translation_wgsl_group_matches_reflection() {
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Compute,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![
            Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
            Sm4Decl::UavBuffer {
                slot: 0,
                stride: 0,
                kind: BufferKind::Raw,
            },
        ],
        instructions: vec![Sm4Inst::Ret],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::UavBuffer { slot: 0 }))
        .expect("missing u0 binding in reflection");

    let needle = format!(
        "@group({}) @binding({}) var<storage, read_write> u0",
        uav_binding.group, uav_binding.binding
    );
    assert!(
        translated.wgsl.contains(&needle),
        "compute WGSL uses a different bind-group index than reflection\nneedle: {needle}\nwgsl:\n{}",
        translated.wgsl
    );
}
