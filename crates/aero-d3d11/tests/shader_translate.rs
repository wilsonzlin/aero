use aero_d3d11::binding_model::{
    BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, BINDING_BASE_UAV,
    D3D11_MAX_CONSTANT_BUFFER_SLOTS, MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS,
};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BindingKind, BufferKind, Builtin, CmpOp,
    CmpType, DxbcFile, DxbcSignatureParameter, FourCC, OperandModifier, RegFile, RegisterRef,
    SamplerRef, ShaderModel, ShaderStage, ShaderTranslateError, Sm4Decl, Sm4Inst, Sm4Module,
    Sm4TestBool, SrcKind, SrcOperand, StorageTextureFormat, Swizzle, TextureRef, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_RDEF: FourCC = FourCC(*b"RDEF");

fn build_minimal_rdef_cbuffer(name: &str, bind_point: u32, size_bytes: u32) -> Vec<u8> {
    // Header (8 DWORDs / 32 bytes) + constant buffer table (24 bytes) + resource binding table
    // (32 bytes) + string table.
    let header_len = 32u32;
    let cb_offset = header_len;
    let rb_offset = header_len + 24;
    let string_offset = header_len + 24 + 32;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes()); // cb_count
    bytes.extend_from_slice(&cb_offset.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes()); // rb_count
    bytes.extend_from_slice(&rb_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // target
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // creator_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // interface_slot_count

    // Constant buffer desc (24 bytes).
    bytes.extend_from_slice(&string_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // var_count
    bytes.extend_from_slice(&0u32.to_le_bytes()); // var_offset
    bytes.extend_from_slice(&size_bytes.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_type

    // Resource binding desc (32 bytes) for the cbuffer binding slot.
    bytes.extend_from_slice(&string_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // input_type (D3D_SIT_CBUFFER)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // return_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // dimension
    bytes.extend_from_slice(&0u32.to_le_bytes()); // sample_count
    bytes.extend_from_slice(&bind_point.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes()); // bind_count
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags

    // String table.
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    bytes
}

fn build_minimal_rdef_cbuffer_array(
    name: &str,
    bind_point: u32,
    bind_count: u32,
    size_bytes: u32,
) -> Vec<u8> {
    // Header (8 DWORDs / 32 bytes) + constant buffer table (24 bytes) + resource binding table
    // (32 bytes) + string table.
    let header_len = 32u32;
    let cb_offset = header_len;
    let rb_offset = header_len + 24;
    let string_offset = header_len + 24 + 32;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes()); // cb_count
    bytes.extend_from_slice(&cb_offset.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes()); // rb_count
    bytes.extend_from_slice(&rb_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // target
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // creator_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // interface_slot_count

    // Constant buffer desc (24 bytes).
    bytes.extend_from_slice(&string_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // var_count
    bytes.extend_from_slice(&0u32.to_le_bytes()); // var_offset
    bytes.extend_from_slice(&size_bytes.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_type

    // Resource binding desc (32 bytes) for the cbuffer binding slot.
    bytes.extend_from_slice(&string_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // input_type (D3D_SIT_CBUFFER)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // return_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // dimension
    bytes.extend_from_slice(&0u32.to_le_bytes()); // sample_count
    bytes.extend_from_slice(&bind_point.to_le_bytes());
    bytes.extend_from_slice(&bind_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags

    // String table.
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    bytes
}

fn build_minimal_rdef_texture_and_sampler_arrays(bind_point: u32, bind_count: u32) -> Vec<u8> {
    // Header (8 DWORDs / 32 bytes) + 2 resource binding descs (2*32 bytes) + string table.
    let header_len = 32u32;
    let rb_offset = header_len;
    let table_len = 2u32 * 32;
    let string_offset = header_len + table_len;

    let tex_name = "tex_array";
    let samp_name = "samp_array";

    let tex_name_offset = string_offset;
    let samp_name_offset = string_offset + (tex_name.len() as u32) + 1;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_count
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_offset
    bytes.extend_from_slice(&2u32.to_le_bytes()); // rb_count
    bytes.extend_from_slice(&rb_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // target
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // creator_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // interface_slot_count

    // Resource binding desc (texture array).
    bytes.extend_from_slice(&tex_name_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&2u32.to_le_bytes()); // input_type (D3D_SIT_TEXTURE)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // return_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // dimension
    bytes.extend_from_slice(&0u32.to_le_bytes()); // sample_count
    bytes.extend_from_slice(&bind_point.to_le_bytes());
    bytes.extend_from_slice(&bind_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags

    // Resource binding desc (sampler array).
    bytes.extend_from_slice(&samp_name_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&3u32.to_le_bytes()); // input_type (D3D_SIT_SAMPLER)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // return_type
    bytes.extend_from_slice(&0u32.to_le_bytes()); // dimension
    bytes.extend_from_slice(&0u32.to_le_bytes()); // sample_count
    bytes.extend_from_slice(&bind_point.to_le_bytes());
    bytes.extend_from_slice(&bind_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags

    // String table.
    bytes.extend_from_slice(tex_name.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(samp_name.as_bytes());
    bytes.push(0);
    bytes
}

fn build_minimal_rdef_texture2d_array(bind_point: u32, bind_count: u32) -> Vec<u8> {
    // Header (8 DWORDs / 32 bytes) + resource binding desc (32 bytes) + string table.
    let header_len = 32u32;
    let rb_offset = header_len;
    let string_offset = header_len + 32;
    let tex_name = "tex2d_array";
    let tex_name_offset = string_offset;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_count
    bytes.extend_from_slice(&0u32.to_le_bytes()); // cb_offset
    bytes.extend_from_slice(&1u32.to_le_bytes()); // rb_count
    bytes.extend_from_slice(&rb_offset.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // target
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&0u32.to_le_bytes()); // creator_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // interface_slot_count

    // Resource binding desc (Texture2DArray).
    bytes.extend_from_slice(&tex_name_offset.to_le_bytes()); // name_offset
    bytes.extend_from_slice(&2u32.to_le_bytes()); // input_type (D3D_SIT_TEXTURE)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // return_type
    bytes.extend_from_slice(&5u32.to_le_bytes()); // dimension (D3D_SRV_DIMENSION_TEXTURE2DARRAY)
    bytes.extend_from_slice(&0u32.to_le_bytes()); // sample_count
    bytes.extend_from_slice(&bind_point.to_le_bytes());
    bytes.extend_from_slice(&bind_count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // flags

    // String table.
    bytes.extend_from_slice(tex_name.as_bytes());
    bytes.push(0);
    bytes
}

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
            min_precision: u32::from(p.min_precision),
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

fn src_cb(slot: u32, reg: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ConstantBuffer { slot, reg },
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
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

fn src_imm_bits(bits: [u32; 4]) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32(bits),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

fn assert_wgsl_parses(wgsl: &str) {
    naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
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
fn translates_vertex_passthrough_signature_io() {
    let isgn_params = vec![
        sig_param("POSITION", 0, 0, 0b0011),
        sig_param("COLOR", 0, 1, 0b1111),
    ];
    let osgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("COLOR", 0, 1, 0b1111),
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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 1, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains("@vertex"));
    assert!(translated.wgsl.contains("fn vs_main"));
    assert!(translated.wgsl.contains("@location(0) a0: vec2<f32>"));
    assert!(translated
        .wgsl
        .contains("@builtin(position) pos: vec4<f32>"));
    assert!(translated.wgsl.contains("out.pos = o0;"));
    assert!(translated.wgsl.contains("out.o1 = o1;"));

    // Reflection should preserve the semantic ↔ register mapping.
    assert_eq!(translated.reflection.inputs.len(), 2);
    assert_eq!(translated.reflection.outputs.len(), 2);
    assert!(translated.reflection.bindings.is_empty());
}

#[test]
fn translates_vertex_packed_input_signature_by_splitting_locations() {
    // DXBC vertex input signatures can pack multiple semantics into a single input register (v#)
    // using disjoint component masks. WebGPU vertex attributes require unique `@location`s, so the
    // translator must split the packed register into multiple WGSL inputs and reconstruct the
    // original `v#` register when emitting instructions.
    let isgn_params = vec![
        sig_param("POSITION", 0, 0, 0b0011), // v0.xy
        sig_param("TEXCOORD", 0, 0, 0b1100), // v0.zw
    ];
    let osgn_params = vec![sig_param("SV_Position", 0, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let src_v0 = SrcOperand {
        kind: SrcKind::Register(RegisterRef {
            file: RegFile::Input,
            index: 0,
        }),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let src_v0_zw = SrcOperand {
        kind: SrcKind::Register(RegisterRef {
            file: RegFile::Input,
            index: 0,
        }),
        swizzle: Swizzle([2, 3, 2, 3]),
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Vertex,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            // o0.xy = v0.xy
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask(0b0011)),
                src: src_v0.clone(),
            },
            // o0.zw = v0.zw
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask(0b1100)),
                src: src_v0_zw,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Two signature parameters -> two distinct WGSL locations.
    assert!(translated.wgsl.contains("@location(0) a0: vec2<f32>"));
    assert!(translated.wgsl.contains("@location(1) a1: vec2<f32>"));

    // The packed register reconstruction should reference both inputs.
    assert!(
        translated.wgsl.contains("input.a1"),
        "expected packed input register to reference the second attribute:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_vertex_legacy_position_output_semantic() {
    let isgn_params = vec![sig_param("POSITION", 0, 0, 0b1111)];
    // Some SM4-era shaders still use `POSITION` for the clip-space output instead of `SV_Position`.
    let osgn_params = vec![sig_param("POSITION", 0, 0, 0b1111)];

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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("@builtin(position) pos: vec4<f32>"));
    assert!(translated.wgsl.contains("out.pos = o0;"));
}

#[test]
fn translates_pixel_texture_sample_and_bindings() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_reg(RegFile::Input, 1),
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef { slot: 0 },
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains("@fragment"));
    assert!(translated.wgsl.contains("textureSample(t0, s0"));
    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({BINDING_BASE_TEXTURE}) var t0: texture_2d<f32>;"
    )));
    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({BINDING_BASE_SAMPLER}) var s0: sampler;"
    )));

    // Reflection should surface required texture/sampler slots.
    let tex_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Texture2D { slot: 0 }))
        .expect("missing texture binding");
    assert_eq!(tex_binding.group, 1);
    assert_eq!(tex_binding.binding, BINDING_BASE_TEXTURE);

    let sampler_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Sampler { slot: 0 }))
        .expect("missing sampler binding");
    assert_eq!(sampler_binding.group, 1);
    assert_eq!(sampler_binding.binding, BINDING_BASE_SAMPLER);
}

#[test]
fn translates_pixel_texture_sample_at_max_slots() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let tex_slot = MAX_TEXTURE_SLOTS - 1;
    let samp_slot = MAX_SAMPLER_SLOTS - 1;
    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_reg(RegFile::Input, 1),
                texture: TextureRef { slot: tex_slot },
                sampler: SamplerRef { slot: samp_slot },
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains(&format!("textureSample(t{tex_slot}, s{samp_slot}")));

    let tex_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Texture2D { slot } if slot == tex_slot))
        .expect("missing texture binding");
    assert_eq!(tex_binding.binding, BINDING_BASE_TEXTURE + tex_slot);

    let sampler_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Sampler { slot } if slot == samp_slot))
        .expect("missing sampler binding");
    assert_eq!(sampler_binding.binding, BINDING_BASE_SAMPLER + samp_slot);
}

#[test]
fn translates_pixel_legacy_color_output_semantic() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b1111),
    ];
    // Some SM4-era pixel shaders use `COLOR` instead of `SV_Target`.
    let osgn_params = vec![sig_param("COLOR", 0, 0, 0b1111)];

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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains("@fragment"));
    assert!(translated.wgsl.contains("struct PsOut"));
    assert!(translated.wgsl.contains("@location(0) target0"));
    assert!(translated.wgsl.contains("out.target0 = o0"));
    assert!(translated.wgsl.contains("return out;"));
}

#[test]
fn translates_pixel_depth_output_sv_depth() {
    // Minimal depth-only pixel shader: write a constant depth value to SV_Depth.
    //
    // D3D10/11 map this via `@builtin(frag_depth)` in WGSL.
    let osgn_params = vec![sig_param("SV_Depth", 0, 0, 0b0001)];
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
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::X),
                src: src_imm([0.25, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("@builtin(frag_depth)"),
        "expected frag_depth output in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_pixel_depth_output_sv_depth_via_output_depth_register() {
    // Same as `translates_pixel_depth_output_sv_depth`, but exercise the dedicated `oDepth` register
    // file used by real DXBC (`D3D10_SB_OPERAND_TYPE_OUTPUT_DEPTH`).
    let osgn_params = vec![sig_param("SV_Depth", 0, 0, 0b0001)];
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
            Sm4Inst::Mov {
                dst: dst(RegFile::OutputDepth, 0, WriteMask::X),
                src: src_imm([0.25, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("@builtin(frag_depth)"),
        "expected frag_depth output in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_pixel_depth_output_sv_depth_less_equal_semantic() {
    // Conservative depth output (`SV_DepthLessEqual`) should still translate to `frag_depth`
    // (WGSL does not currently expose the conservative contract).
    let osgn_params = vec![sig_param("SV_DepthLessEqual", 0, 0, 0b0001)];
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
            Sm4Inst::Mov {
                dst: dst(RegFile::OutputDepth, 0, WriteMask::X),
                src: src_imm([0.25, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("@builtin(frag_depth)"),
        "expected frag_depth output in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_pixel_depth_output_sv_depth_greater_equal_semantic() {
    // Conservative depth output (`SV_DepthGreaterEqual`) should still translate to `frag_depth`.
    let osgn_params = vec![sig_param("SV_DepthGreaterEqual", 0, 0, 0b0001)];
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
            Sm4Inst::Mov {
                dst: dst(RegFile::OutputDepth, 0, WriteMask::X),
                src: src_imm([0.25, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("@builtin(frag_depth)"),
        "expected frag_depth output in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_pixel_depth_output_with_overlapping_signature_registers() {
    // Real DXBC signatures can assign `SV_Target0` and `SV_Depth` the same register number (they
    // live in different register files). Ensure our signature-driven translator doesn't treat this
    // as a conflict and still emits `@builtin(frag_depth)` alongside color outputs.
    let osgn_params = vec![
        sig_param("SV_Target", 0, 0, 0b1111),
        sig_param("SV_Depth", 0, 0, 0b0001),
    ];
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
            // o0 = red
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([1.0, 0.0, 0.0, 1.0]),
            },
            // oDepth.x = 0.25
            Sm4Inst::Mov {
                dst: dst(RegFile::OutputDepth, 0, WriteMask::X),
                src: src_imm([0.25, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("@location(0) target0"),
        "expected color output location 0:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("@builtin(frag_depth)"),
        "expected depth output builtin:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("var oDepth: vec4<f32>"),
        "expected dedicated oDepth temp when signature registers overlap:\n{}",
        translated.wgsl
    );

    assert!(
        translated.reflection.outputs.iter().any(|p| {
            p.semantic_name.eq_ignore_ascii_case("SV_Depth")
                && p.register == 0
                && p.location.is_none()
        }),
        "expected pixel depth output in reflection: {:#?}",
        translated.reflection.outputs
    );
}

#[test]
fn translates_cbuffer_and_arithmetic_ops() {
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
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_cb(0, 0),
            },
            Sm4Inst::Add {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_imm([1.0, 2.0, 3.0, 4.0]),
            },
            Sm4Inst::Mul {
                dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 1),
                b: src_imm([0.5, 0.5, 0.5, 0.5]),
            },
            Sm4Inst::Mad {
                dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 2),
                c: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Dp3 {
                dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 3),
                b: src_reg(RegFile::Temp, 3),
            },
            Sm4Inst::Dp4 {
                dst: dst(RegFile::Temp, 5, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 3),
                b: src_reg(RegFile::Temp, 3),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 5),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);

    assert!(translated.wgsl.contains("struct Cb0"));
    assert!(translated.wgsl.contains("var<uniform> cb0"));
    assert!(translated.wgsl.contains("dot((")); // dp3/dp4

    let cb_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: 0, .. }))
        .expect("missing constant buffer binding");
    assert_eq!(cb_binding.group, 1);
    assert_eq!(cb_binding.binding, BINDING_BASE_CBUFFER);
}

#[test]
fn translates_movc_to_wgsl_select_with_bitcast_condition() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // SM4/SM5 encodes boolean conditions as raw 32-bit lanes (commonly 0xffffffff for true, 0 for
    // false). Ensure the translator treats `movc`'s condition as a raw-bit non-zero test.
    let cond = SrcOperand {
        kind: SrcKind::ImmediateF32([0xffff_ffff, 0, 0xffff_ffff, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Movc {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                cond,
                a: src_imm([1.0, 1.0, 1.0, 1.0]),
                b: src_imm([0.0, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("select("),
        "expected WGSL to use `select` for movc:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<u32>>"),
        "expected movc condition to use a raw-bit non-zero test:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_uaddc_emits_carry_and_writes_both_destinations() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let a = SrcOperand {
        kind: SrcKind::ImmediateF32([0xffff_ffff; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let b = SrcOperand {
        kind: SrcKind::ImmediateF32([1; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::UAddC {
                dst_sum: dst(RegFile::Temp, 0, WriteMask::XYZW),
                dst_carry: dst(RegFile::Temp, 1, WriteMask::XYZW),
                a,
                b,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains(
            "let uaddc_carry_0 = select(vec4<u32>(0u), vec4<u32>(1u), uaddc_sum_0 < uaddc_a_0);"
        ),
        "expected carry computation using `sum < a`:\n{}",
        translated.wgsl
    );

    // Both destinations should be written back as raw bits.
    assert!(
        translated
            .wgsl
            .contains("r0.x = (bitcast<vec4<f32>>(uaddc_sum_0)).x;"),
        "expected sum to be written to r0:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("r1.x = (bitcast<vec4<f32>>(uaddc_carry_0)).x;"),
        "expected carry to be written to r1:\n{}",
        translated.wgsl
    );
}

#[test]
fn integer_arithmetic_sources_use_raw_bits_not_float_to_int_heuristics() {
    // Regression test: integer arithmetic ops (uaddc/usubb/udiv/...) should treat the SM4/SM5
    // register file as raw 32-bit lanes.
    //
    // Historically we had a "looks like an integer float" heuristic (based on `floor`) that
    // attempted to recover integer values from numeric `f32` lanes. This breaks when the *raw bits*
    // happen to look like integer-valued floats (e.g. 0x3f800000 == 1.0f).
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // 0x3f800000 is the IEEE-754 bit pattern for 1.0f. If we accidentally treat lanes as numeric
    // floats during integer ops, this would collapse to `1u` instead of preserving the bits.
    let a = SrcOperand {
        kind: SrcKind::ImmediateF32([0x3f80_0000; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let b = SrcOperand {
        kind: SrcKind::ImmediateF32([0; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::UAddC {
                dst_sum: dst(RegFile::Temp, 0, WriteMask::XYZW),
                dst_carry: dst(RegFile::Temp, 1, WriteMask::XYZW),
                a,
                b,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("0x3f800000u"),
        "expected raw u32 literal bits to flow into integer op:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("floor("),
        "expected integer ops to avoid float-to-int heuristics:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("vec4<u32>(vec4<f32>"),
        "expected integer ops to avoid numeric `vec4<u32>(f32)` conversions:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_umul_uses_raw_integer_bits_not_float_to_int_heuristics() {
    // Regression test: integer multiply sources must be interpreted as raw u32 lane bits, not as
    // numeric float values.
    //
    // This test specifically targets `umul` because it historically went through a separate
    // `emit_src_vec4_u32_int` helper.
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
            // r0 = 1.0f (bits 0x3f800000).
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm([1.0, 1.0, 1.0, 1.0]),
            },
            Sm4Inst::UMul {
                dst_lo: dst(RegFile::Temp, 1, WriteMask::XYZW),
                dst_hi: None,
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure `umul` consumes the raw bits of `r0` (bitcast), not a numeric float→u32 conversion.
    assert!(
        translated.wgsl.contains("bitcast<vec4<u32>>(r0)"),
        "expected umul sources to come from raw bits of r0:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("vec4<u32>(r0)"),
        "expected umul to avoid numeric vec4<u32>(r0) conversion:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("floor("),
        "expected umul to avoid float->int heuristics:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_usubb_emits_borrow_and_writes_both_destinations() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let a = SrcOperand {
        kind: SrcKind::ImmediateF32([0; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let b = SrcOperand {
        kind: SrcKind::ImmediateF32([1; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::USubB {
                dst_diff: dst(RegFile::Temp, 0, WriteMask::XYZW),
                dst_borrow: dst(RegFile::Temp, 1, WriteMask::XYZW),
                a,
                b,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains(
            "let usubb_borrow_0 = select(vec4<u32>(0u), vec4<u32>(1u), usubb_a_0 < usubb_b_0);"
        ),
        "expected borrow computation using `a < b`:\n{}",
        translated.wgsl
    );

    assert!(
        translated
            .wgsl
            .contains("r0.x = (bitcast<vec4<f32>>(usubb_diff_0)).x;"),
        "expected diff to be written to r0:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("r1.x = (bitcast<vec4<f32>>(usubb_borrow_0)).x;"),
        "expected borrow to be written to r1:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_isubc_emits_carry_and_writes_both_destinations() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let a = SrcOperand {
        kind: SrcKind::ImmediateF32([0; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let b = SrcOperand {
        kind: SrcKind::ImmediateF32([1; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::ISubC {
                dst_diff: dst(RegFile::Temp, 0, WriteMask::XYZW),
                dst_carry: dst(RegFile::Temp, 1, WriteMask::XYZW),
                a,
                b,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains(
            "let isubc_carry_0 = select(vec4<u32>(0u), vec4<u32>(1u), isubc_a_0 >= isubc_b_0);"
        ),
        "expected carry computation using `a >= b`:\n{}",
        translated.wgsl
    );

    assert!(
        translated
            .wgsl
            .contains("r0.x = (bitcast<vec4<f32>>(isubc_diff_0)).x;"),
        "expected diff to be written to r0:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("r1.x = (bitcast<vec4<f32>>(isubc_carry_0)).x;"),
        "expected carry to be written to r1:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_isubc_uses_raw_bits_without_float_int_heuristics() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Populate source registers with float values first to ensure the `isubc` lowering doesn't try
    // to "helpfully" numeric-cast f32->u32 based on `floor()` heuristics. DXBC registers are
    // untyped; integer ops must treat these as raw 32-bit lane bits.
    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm([1.0, 1.0, 1.0, 1.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: src_imm([2.0, 2.0, 2.0, 2.0]),
            },
            Sm4Inst::ISubC {
                dst_diff: dst(RegFile::Temp, 2, WriteMask::XYZW),
                dst_carry: dst(RegFile::Temp, 3, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated
            .wgsl
            .contains("let isubc_a_2 = bitcast<vec4<u32>>(r0);"),
        "expected isubc to consume raw u32 bits via bitcast for src0:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("let isubc_b_2 = bitcast<vec4<u32>>(r1);"),
        "expected isubc to consume raw u32 bits via bitcast for src1:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("floor("),
        "isubc lowering should not use float→int heuristics (floor):\n{}",
        translated.wgsl
    );
    assert!(
        // Numeric float->u32 conversions for register operands would look like `vec4<u32>(rN)`.
        !translated.wgsl.contains("vec4<u32>(r0)") && !translated.wgsl.contains("vec4<u32>(r1)"),
        "isubc lowering should not use numeric vec4<u32>(f32) conversions:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_float_cmp_to_predicate_mask_bits() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let a = src_imm([1.0, 2.0, 3.0, 4.0]);
    let b = src_imm([1.0, 1.0, 4.0, 0.0]);

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Cmp {
                op: CmpOp::Eq,
                ty: CmpType::F32,
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                a: a.clone(),
                b: b.clone(),
            },
            Sm4Inst::Cmp {
                op: CmpOp::Ne,
                ty: CmpType::F32,
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                a: a.clone(),
                b: b.clone(),
            },
            Sm4Inst::Cmp {
                op: CmpOp::Lt,
                ty: CmpType::F32,
                dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
                a: a.clone(),
                b: b.clone(),
            },
            Sm4Inst::Cmp {
                op: CmpOp::Ge,
                ty: CmpType::F32,
                dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
                a: a.clone(),
                b: b.clone(),
            },
            Sm4Inst::Cmp {
                op: CmpOp::Gt,
                ty: CmpType::F32,
                dst: dst(RegFile::Temp, 4, WriteMask::XYZW),
                a: a.clone(),
                b: b.clone(),
            },
            Sm4Inst::Cmp {
                op: CmpOp::Le,
                ty: CmpType::F32,
                dst: dst(RegFile::Temp, 5, WriteMask::XYZW),
                a,
                b,
            },
            // Ensure the predicate value is observed by the output path.
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("0xffffffffu"),
        "expected predicate-mask constant in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected predicate-mask bitcast in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn rdef_cbuffer_size_overrides_used_registers() {
    // Shader reads only cb0[0], but RDEF declares the cbuffer to be 128 bytes (8 registers).
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let rdef_bytes = build_minimal_rdef_cbuffer("CB0", 0, 128);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_RDEF, rdef_bytes),
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
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_cb(0, 0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains("array<vec4<u32>, 8>"));
    assert!(translated.reflection.rdef.is_some());

    let cb_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: 0, .. }))
        .expect("missing constant buffer binding");
    match cb_binding.kind {
        BindingKind::ConstantBuffer { reg_count, .. } => assert_eq!(reg_count, 8),
        _ => panic!("unexpected binding kind"),
    }
}

#[test]
fn rdef_resource_arrays_expand_used_texture_and_sampler_slots() {
    // Shader samples from t2/s2, but RDEF declares arrays bound at t0..t3 and s0..s3.
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let rdef_bytes = build_minimal_rdef_texture_and_sampler_arrays(0, 4);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_RDEF, rdef_bytes),
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
            Sm4Inst::Sample {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_imm([0.0, 0.0, 0.0, 0.0]),
                texture: TextureRef { slot: 2 },
                sampler: SamplerRef { slot: 2 },
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);

    // Texture array expansion should declare t0..t3.
    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({}) var t0: texture_2d<f32>;",
        BINDING_BASE_TEXTURE
    )));
    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({}) var t3: texture_2d<f32>;",
        BINDING_BASE_TEXTURE + 3
    )));

    // Sampler array expansion should declare s0..s3.
    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({}) var s0: sampler;",
        BINDING_BASE_SAMPLER
    )));
    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({}) var s3: sampler;",
        BINDING_BASE_SAMPLER + 3
    )));

    // Reflection should surface the expanded ranges.
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Texture2D { slot: 0 })));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Texture2D { slot: 3 })));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Sampler { slot: 0 })));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Sampler { slot: 3 })));
}

#[test]
fn rdef_cbuffer_arrays_expand_used_slots() {
    // Shader reads cb2[0], but RDEF declares an array bound at b0..b3.
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let rdef_bytes = build_minimal_rdef_cbuffer_array("CBArray", 0, 4, 64);

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_RDEF, rdef_bytes),
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
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_cb(2, 0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);

    // Should declare the full b0..b3 range.
    assert!(translated.wgsl.contains("struct Cb0"));
    assert!(translated.wgsl.contains("struct Cb3"));
    assert!(translated
        .wgsl
        .contains("@group(1) @binding(0) var<uniform> cb0"));
    assert!(translated
        .wgsl
        .contains("@group(1) @binding(3) var<uniform> cb3"));

    // Reflection should include the expanded bindings with the RDEF-derived size (64 bytes -> 4 regs).
    for slot in 0..4 {
        let cb_binding = translated
            .reflection
            .bindings
            .iter()
            .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: s, .. } if s == slot))
            .expect("missing constant buffer binding");
        match cb_binding.kind {
            BindingKind::ConstantBuffer { reg_count, .. } => assert_eq!(reg_count, 4),
            _ => panic!("unexpected binding kind"),
        }
    }
}

#[test]
fn translates_cbuffer_at_max_slot() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let slot = D3D11_MAX_CONSTANT_BUFFER_SLOTS - 1;
    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_cb(slot, 0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({slot}) var<uniform> cb{slot}: Cb{slot};"
    )));

    let cb_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: s, .. } if s == slot))
        .expect("missing constant buffer binding");
    assert_eq!(cb_binding.binding, BINDING_BASE_CBUFFER + slot);
}

#[test]
fn translates_sample_l() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::SampleL {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_reg(RegFile::Input, 1),
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef { slot: 0 },
                lod: src_imm([0.0, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert!(translated.wgsl.contains("textureSampleLevel"));
}

#[test]
fn translates_vertex_sample_uses_explicit_lod() {
    // WGSL forbids implicit-derivative sampling (`textureSample`) in the vertex stage. Ensure we
    // translate SM4/SM5 `sample` in a vertex shader to `textureSampleLevel(..., 0.0)` instead.
    let isgn_params = vec![
        sig_param("POSITION", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Position", 0, 0, 0b1111)];
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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_reg(RegFile::Input, 1),
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef { slot: 0 },
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_parses(&translated.wgsl);
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("textureSampleLevel(t0, s0"));
}

#[test]
fn translates_texture_load_ld() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // `ld` consumes integer coords/LOD. The DXBC operand stream stores 32-bit values without a
    // float/int tag; in practice lanes can contain either raw integer bit patterns (common compiler
    // output) or numeric floats that happen to be exact integers (common in hand-authored/test
    // token streams).
    //
    // Use raw integer bits here (1, 2, 0, 0). The translator should include a raw-bit fallback path
    // (`bitcast<i32>`) so these values aren't misinterpreted as tiny denormal floats.
    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([1, 2, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let lod = SrcOperand {
        kind: SrcKind::ImmediateF32([0.0f32.to_bits(); 4]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };
    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Ld {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                coord,
                texture: TextureRef { slot: 0 },
                lod,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("textureLoad(t0"));
    let load_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("textureLoad("))
        .expect("expected a textureLoad call");
    assert!(
        load_line.contains("vec2<i32>("),
        "expected textureLoad to use integer coordinates:\n{}",
        load_line
    );
    assert!(
        translated.wgsl.contains("bitcast<i32>(ld_x0_f)")
            && translated.wgsl.contains("bitcast<i32>(ld_y0_f)"),
        "expected raw-bit fallback when lowering ld coords:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<f32>(0x00000001u)"),
        "expected raw coordinate bits to be preserved as an f32 payload:\n{}",
        translated.wgsl
    );

    // Reflection should surface the referenced texture slot (no sampler needed for ld).
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Texture2D { slot: 0 })));
    assert!(!translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Sampler { .. })));
}

#[test]
fn translates_texture_load_ld_prefers_numeric_i32_for_integer_float_coords() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Use float bit patterns that look like exact integers as floats (e.g. 1.0 = 0x3f800000).
    //
    // The translator should prefer numeric `i32(f32)` conversion for these operands, while still
    // emitting a raw-bit fallback for cases where the lane does not represent an exact integer
    // float.
    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([1.0f32.to_bits(), 2.0f32.to_bits(), 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let lod = SrcOperand {
        kind: SrcKind::ImmediateF32([0.0f32.to_bits(); 4]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };
    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Ld {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                coord,
                texture: TextureRef { slot: 0 },
                lod,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("ld_x0 = i32(ld_x0_f);")
            && translated.wgsl.contains("ld_y0 = i32(ld_y0_f);"),
        "expected ld to use numeric i32(f32) conversion for integer float lanes:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<i32>(ld_x0_f)")
            && translated.wgsl.contains("bitcast<i32>(ld_y0_f)"),
        "expected ld to still emit a raw-bit fallback for non-integer float lanes:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("bitcast<i32>(0x3f800000u)"),
        "expected ld not to treat integer floats as raw u32 bits:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_texture_load_ld_texture2darray_uses_raw_integer_bits_for_slice() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let rdef_bytes = build_minimal_rdef_texture2d_array(0, 1);
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_RDEF, rdef_bytes),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Use a float bit pattern (1.0) for the array slice lane. `ld` must treat it as raw i32 bits.
    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([0, 0, 1.0f32.to_bits(), 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let lod = SrcOperand {
        kind: SrcKind::ImmediateF32([0.0f32.to_bits(); 4]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };
    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Ld {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                coord,
                texture: TextureRef { slot: 0 },
                lod,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("texture_2d_array<f32>"),
        "expected Texture2DArray binding in WGSL:\n{}",
        translated.wgsl
    );
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Texture2DArray { slot: 0 })));

    let load_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("textureLoad("))
        .expect("expected a textureLoad call");
    assert!(
        load_line.contains("bitcast<i32>(0x3f800000u)"),
        "expected raw slice bit pattern 0x3f800000 (f32 1.0) to flow into textureLoad array-index:\n{}",
        load_line
    );
    assert!(
        !translated.wgsl.contains("select(")
            && !translated.wgsl.contains("floor(")
            && !translated.wgsl.contains("i32("),
        "textureLoad array lowering should not use float-vs-bitcast heuristics or numeric f32->i32 conversions:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_store_uav_typed_as_storage_texture_write() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Coords are integer-typed in SM5; use raw integer bits for (1, 2).
    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([1, 2, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::UavTyped2D {
            slot: 0,
            // DXGI_FORMAT_R8G8B8A8_UNORM
            format: 28,
        }],
        instructions: vec![
            Sm4Inst::StoreUavTyped {
                uav: UavRef { slot: 0 },
                coord,
                value: src_imm([0.25, 0.5, 0.75, 1.0]),
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([0.0, 0.0, 0.0, 1.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({BINDING_BASE_UAV}) var u0: texture_storage_2d<rgba8unorm, write>;"
    )));
    assert!(translated.wgsl.contains("textureStore(u0, vec2<i32>("));
    let store_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("textureStore("))
        .expect("expected a textureStore call");
    assert!(
        store_line.contains("bitcast<i32>(0x00000001u)"),
        "expected raw integer bit-pattern 1 to flow into textureStore coordinate:\n{}",
        store_line
    );
    assert!(
        !store_line.contains("select(") && !store_line.contains("floor(") && !store_line.contains("i32("),
        "textureStore coordinate lowering should not use float-vs-bitcast heuristics or numeric f32->i32 conversions:\n{}",
        store_line
    );

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::UavTexture2DWriteOnly { slot: 0, .. }))
        .expect("missing uav binding");
    assert_eq!(uav_binding.group, 1);
    assert_eq!(uav_binding.binding, BINDING_BASE_UAV);
}

#[test]
fn translates_store_uav_typed_uint_format_uses_u32_value() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([0, 0, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let value = SrcOperand {
        // Raw u32 lane bits for 1,2,3,4.
        kind: SrcKind::ImmediateF32([1, 2, 3, 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::UavTyped2D {
            slot: 0,
            // DXGI_FORMAT_R32G32B32A32_UINT
            format: 3,
        }],
        instructions: vec![
            Sm4Inst::StoreUavTyped {
                uav: UavRef { slot: 0 },
                coord,
                value,
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([0.0, 0.0, 0.0, 1.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({BINDING_BASE_UAV}) var u0: texture_storage_2d<rgba32uint, write>;"
    )));
    assert!(
        translated
            .wgsl
            .contains("vec4<u32>(0x00000001u, 0x00000002u, 0x00000003u, 0x00000004u)"),
        "{}",
        translated.wgsl
    );

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| {
            matches!(
                b.kind,
                BindingKind::UavTexture2DWriteOnly {
                    slot: 0,
                    format: StorageTextureFormat::Rgba32Uint
                }
            )
        })
        .expect("missing uav binding");
    assert_eq!(uav_binding.group, 1);
    assert_eq!(uav_binding.binding, BINDING_BASE_UAV);
}

#[test]
fn translates_store_uav_typed_r32_float_accepts_x_mask() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([0, 0, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::UavTyped2D {
            slot: 0,
            // DXGI_FORMAT_R32_FLOAT
            format: 41,
        }],
        instructions: vec![
            Sm4Inst::StoreUavTyped {
                uav: UavRef { slot: 0 },
                coord,
                value: src_imm([0.25, 0.0, 0.0, 0.0]),
                mask: WriteMask::X,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([0.0, 0.0, 0.0, 1.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({BINDING_BASE_UAV}) var u0: texture_storage_2d<r32float, write>;"
    )));
    assert!(translated.wgsl.contains("textureStore(u0, vec2<i32>("));

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| {
            matches!(
                b.kind,
                BindingKind::UavTexture2DWriteOnly {
                    slot: 0,
                    format: StorageTextureFormat::R32Float
                }
            )
        })
        .expect("missing uav binding");
    assert_eq!(uav_binding.group, 1);
    assert_eq!(uav_binding.binding, BINDING_BASE_UAV);
}

#[test]
fn translates_store_uav_typed_rg32_float_accepts_xy_mask() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([0, 0, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::UavTyped2D {
            slot: 0,
            // DXGI_FORMAT_R32G32_FLOAT
            format: 16,
        }],
        instructions: vec![
            Sm4Inst::StoreUavTyped {
                uav: UavRef { slot: 0 },
                coord,
                value: src_imm([0.25, 0.5, 0.0, 0.0]),
                mask: WriteMask(WriteMask::X.0 | WriteMask::Y.0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([0.0, 0.0, 0.0, 1.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(translated.wgsl.contains(&format!(
        "@group(1) @binding({BINDING_BASE_UAV}) var u0: texture_storage_2d<rg32float, write>;"
    )));
    assert!(translated.wgsl.contains("textureStore(u0, vec2<i32>("));

    let uav_binding = translated
        .reflection
        .bindings
        .iter()
        .find(|b| {
            matches!(
                b.kind,
                BindingKind::UavTexture2DWriteOnly {
                    slot: 0,
                    format: StorageTextureFormat::Rg32Float
                }
            )
        })
        .expect("missing uav binding");
    assert_eq!(uav_binding.group, 1);
    assert_eq!(uav_binding.binding, BINDING_BASE_UAV);
}

#[test]
fn translates_store_uav_typed_uses_raw_integer_bits_not_float_heuristics() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Use float bit patterns that look like exact integers as floats (e.g. 1.0 = 0x3f800000).
    // Typed UAV stores must interpret these operands as *integer bits* in the untyped register
    // file, not as numeric floats that "happen to be integers".
    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([1.0f32.to_bits(), 2.0f32.to_bits(), 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: vec![Sm4Decl::UavTyped2D {
            slot: 0,
            // DXGI_FORMAT_R8G8B8A8_UNORM
            format: 28,
        }],
        instructions: vec![
            Sm4Inst::StoreUavTyped {
                uav: UavRef { slot: 0 },
                coord,
                value: src_imm([0.25, 0.5, 0.75, 1.0]),
                mask: WriteMask::XYZW,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([0.0, 0.0, 0.0, 1.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("textureStore("),
        "expected a textureStore call:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<i32>(0x3f800000u)")
            && translated.wgsl.contains("bitcast<i32>(0x40000000u)"),
        "expected raw bit patterns to flow into textureStore coords as i32 bits:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("select(")
            && !translated.wgsl.contains("floor(")
            && !translated.wgsl.contains("i32("),
        "textureStore lowering should not use float-vs-bitcast heuristics or numeric f32->i32 conversions:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_texture_load_with_nonzero_lod() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // `ld` expects integer coords/LOD. Use raw integer bits for coordinates here; the translator
    // preserves those bits when forming `textureLoad` arguments.
    let coord = SrcOperand {
        kind: SrcKind::ImmediateF32([1, 2, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let lod = SrcOperand {
        kind: SrcKind::ImmediateF32([3, 3, 3, 3]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };
    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Ld {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                coord,
                texture: TextureRef { slot: 0 },
                lod,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("textureLoad(t0"));
    assert!(
        translated.wgsl.contains("bitcast<f32>(0x00000003u)")
            && translated.wgsl.contains("bitcast<i32>(ld_lod_scalar0_f)"),
        "expected raw mip LOD bits (3) to be preserved via a bitcast fallback:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_texture_load_with_vertex_id_raw_bits_coords() {
    // `SV_VertexID` is surfaced to the translator as a `u32` builtin and expanded into our
    // internal `vec4<f32>` register model via `bitcast<f32>(input.vertex_id)` so the original
    // integer bit pattern is preserved (required for integer/bit ops).
    const D3D_NAME_VERTEX_ID: u32 = 6;

    let isgn_params = vec![DxbcSignatureParameter {
        semantic_name: "SV_VertexID".to_owned(),
        semantic_index: 0,
        system_value_type: D3D_NAME_VERTEX_ID,
        component_type: 0,
        register: 0,
        mask: 0b0001,
        read_write_mask: 0b0001,
        stream: 0,
        min_precision: 0,
    }];
    let osgn_params = vec![sig_param("SV_Position", 0, 0, 0b1111)];
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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Ld {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_reg(RegFile::Input, 0),
                texture: TextureRef { slot: 0 },
                lod: src_imm([0.0, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("textureLoad(t0"));
    assert!(translated.wgsl.contains("bitcast<f32>(input.vertex_id)"));
}

#[test]
fn translates_utof_from_vertex_id_bits() {
    // Demonstrate how to recover a float numeric value from the raw integer bit-pattern carried in
    // our `vec4<f32>` register model.
    //
    // In real DXBC this is handled by the `utof` instruction; until we translate more integer
    // instructions this test asserts the intended WGSL pattern (`bitcast` -> numeric conversion).
    const D3D_NAME_VERTEX_ID: u32 = 6;

    let isgn_params = vec![DxbcSignatureParameter {
        semantic_name: "SV_VertexID".to_owned(),
        semantic_index: 0,
        system_value_type: D3D_NAME_VERTEX_ID,
        component_type: 0,
        register: 0,
        mask: 0b0001,
        read_write_mask: 0b0001,
        stream: 0,
        min_precision: 0,
    }];
    // Only write X; the translator will apply D3D default fill for SV_Position.
    let osgn_params = vec![sig_param("SV_Position", 0, 0, 0b0001)];

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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Utof {
                dst: dst(RegFile::Temp, 0, WriteMask::X),
                src: src_reg(RegFile::Input, 0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::X),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("bitcast<f32>(input.vertex_id)"),
        "expected raw-bits vertex_id expansion:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("vec4<f32>(bitcast<vec4<u32>>"),
        "expected `utof` to convert from raw integer bits via bitcast:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_f32tof16_via_pack2x16float_masking_low_16_bits() {
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
            Sm4Inst::F32ToF16 {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm([1.0, 2.0, 3.0, 4.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("pack2x16float"),
        "expected f32tof16 lowering to use pack2x16float:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("& 0xffffu"),
        "expected f32tof16 lowering to mask low 16 bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(", 0.0)) & 0xffffu"),
        "expected f32tof16 lowering to pack per-component half bits (second lane = 0.0):\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_f16tof32_via_unpack2x16float() {
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
            // Round-trip through f32tof16 so the input bits are present in our untyped register
            // file model.
            Sm4Inst::F32ToF16 {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm([1.0, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::F16ToF32 {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("unpack2x16float"),
        "expected f16tof32 lowering to use unpack2x16float:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("& 0xffffu"),
        "expected f16tof32 lowering to mask low 16 bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("& 0xffffu).x"),
        "expected f16tof32 to unpack the low half of each lane via unpack2x16float(...).x:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_f16tof32_applies_operand_modifier_after_unpacking_half_bits() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Apply a source operand modifier to `f16tof32`.
    //
    // The operand is still read as raw bits from the untyped register file (so the half payload is
    // preserved), but the modifier should be applied to the numeric f32 result of the conversion.
    let mut half_bits = src_reg(RegFile::Temp, 0);
    half_bits.modifier = OperandModifier::Neg;

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::F32ToF16 {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm([1.0, 0.0, 0.0, 0.0]),
            },
            Sm4Inst::F16ToF32 {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: half_bits,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("unpack2x16float"),
        "expected f16tof32 lowering to use unpack2x16float:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("& 0xffffu"),
        "expected f16tof32 lowering to mask low 16 bits:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("& 0xffffu).x"),
        "expected f16tof32 to unpack the low half of each lane via unpack2x16float(...).x:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("-(vec4<f32>(unpack2x16float"),
        "expected f16tof32 to apply operand modifier after unpacking half bits:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("vec4<u32>(0u) -"),
        "expected f16tof32 operand modifier not to be applied in the u32 domain:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_f16tof32_sat_clamps_converted_values() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let mut dst_sat = dst(RegFile::Temp, 0, WriteMask::XYZW);
    dst_sat.saturate = true;

    // Use raw half-float bit patterns (in low 16 bits of each lane):
    // - 2.0  -> 0x4000
    // - -1.0 -> 0xbc00
    // - 0.5  -> 0x3800
    // - 0.0  -> 0x0000
    let half_bits = src_imm_bits([0x0000_4000, 0x0000_bc00, 0x0000_3800, 0x0000_0000]);

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::F16ToF32 {
                dst: dst_sat,
                src: half_bits,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("unpack2x16float"),
        "expected f16tof32 lowering to use unpack2x16float:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("clamp(("),
        "expected f16tof32_sat to clamp the converted float values:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_vs_system_value_builtins_from_siv_decls() {
    const D3D_NAME_POSITION: u32 = 1;
    const D3D_NAME_VERTEX_ID: u32 = 6;
    const D3D_NAME_INSTANCE_ID: u32 = 8;

    let isgn_params = vec![
        sig_param("VID", 0, 0, 0b0001),
        sig_param("IID", 0, 1, 0b0001),
        sig_param("POSITION", 0, 2, 0b1111),
    ];
    let osgn_params = vec![
        // Use a non-canonical semantic so the translator must rely on `dcl_output_siv`.
        sig_param("OUTPOS", 0, 0, 0b1111),
        sig_param("COLOR", 0, 1, 0b1111),
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
            Sm4Decl::Input {
                reg: 2,
                mask: WriteMask::XYZW,
            },
            Sm4Decl::OutputSiv {
                reg: 0,
                mask: WriteMask::XYZW,
                sys_value: D3D_NAME_POSITION,
            },
            Sm4Decl::Output {
                reg: 1,
                mask: WriteMask::XYZW,
            },
        ],
        instructions: vec![
            // o0 = v2 (position)
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 2),
            },
            // r0 = v0 (vertex id)
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 0),
            },
            // r1 = v1 (instance id)
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 1),
            },
            // o1 = r0 + r1
            Sm4Inst::Add {
                dst: dst(RegFile::Output, 1, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("@builtin(vertex_index) vertex_id: u32"));
    assert!(translated
        .wgsl
        .contains("@builtin(instance_index) instance_id: u32"));
    assert!(translated.wgsl.contains("@location(0) a0: vec4<f32>"));
    assert!(!translated.wgsl.contains("@location(0) v0:"));

    let v0 = translated
        .reflection
        .inputs
        .iter()
        .find(|p| p.register == 0)
        .expect("missing v0 reflection");
    assert_eq!(v0.builtin, Some(Builtin::VertexIndex));
    assert_eq!(v0.location, None);

    let v1 = translated
        .reflection
        .inputs
        .iter()
        .find(|p| p.register == 1)
        .expect("missing v1 reflection");
    assert_eq!(v1.builtin, Some(Builtin::InstanceIndex));
    assert_eq!(v1.location, None);

    let o0 = translated
        .reflection
        .outputs
        .iter()
        .find(|p| p.register == 0)
        .expect("missing o0 reflection");
    assert_eq!(o0.builtin, Some(Builtin::Position));
    assert_eq!(o0.location, None);
}

#[test]
fn translates_ps_front_facing_builtin_from_system_value_type() {
    const D3D_NAME_IS_FRONT_FACE: u32 = 9;

    let mut front_facing = sig_param("SIV", 0, 0, 0b0001);
    front_facing.system_value_type = D3D_NAME_IS_FRONT_FACE;

    let isgn_params = vec![front_facing, sig_param("TEXCOORD", 0, 1, 0b0011)];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];

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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Input, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated
        .wgsl
        .contains("@builtin(front_facing) front_facing: bool"));
    assert!(!translated.wgsl.contains("@location(0) v0:"));

    let v0 = translated
        .reflection
        .inputs
        .iter()
        .find(|p| p.register == 0)
        .expect("missing v0 reflection");
    assert_eq!(v0.builtin, Some(Builtin::FrontFacing));
    assert_eq!(v0.location, None);
}

#[test]
fn translates_front_facing_as_d3d_boolean_mask_for_bitwise_ops() {
    // Validate that `SV_IsFrontFace` is represented as a D3D-style boolean mask
    // (0xffffffff/0) in the untyped register file, so integer/bitwise code can
    // operate on it directly.
    const D3D_NAME_IS_FRONT_FACE: u32 = 9;

    let mut front_facing = sig_param("SV_IsFrontFace", 0, 0, 0b0001);
    front_facing.system_value_type = D3D_NAME_IS_FRONT_FACE;

    let isgn_params = vec![front_facing];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&isgn_params)),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // AND a constant mask with the front-facing value. If `SV_IsFrontFace` is
    // represented as 1.0/0.0, the AND would produce nonsensical masks.
    let mask_imm = SrcOperand {
        kind: SrcKind::ImmediateF32([0x000000ffu32; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::And {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                a: src_reg(RegFile::Input, 0),
                b: mask_imm,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated
            .wgsl
            .contains("select(0u, 0xffffffffu, input.front_facing)"),
        "expected `SV_IsFrontFace` to expand to a 0xffffffff/0 mask:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("&"),
        "expected bitwise AND to be present in generated WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<u32>>"),
        "expected integer bitcasts to be present in generated WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_if_bool_test_uses_raw_bits_for_boolean_masks() {
    // `if_z`/`if_nz` should test the raw 32-bit lane value, not the *numeric* `f32` value.
    //
    // This matters for D3D-style boolean masks: if we `and` a 0xffffffff/0 mask with a mask that
    // yields `0x80000000`, the result is `-0.0` as an `f32`. Numeric comparisons would treat this
    // as zero (false) even though the bit-pattern is non-zero (true).
    const D3D_NAME_IS_FRONT_FACE: u32 = 9;
    let mut front_facing = sig_param("SV_IsFrontFace", 0, 0, 0b0001);
    front_facing.system_value_type = D3D_NAME_IS_FRONT_FACE;

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[front_facing])),
        (
            FOURCC_OSGN,
            build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111)]),
        ),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let sign_bit = SrcOperand {
        kind: SrcKind::ImmediateF32([0x80000000u32; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            // r0 = front_facing & 0x80000000
            Sm4Inst::And {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                a: src_reg(RegFile::Input, 0),
                b: sign_bit,
            },
            // if_nz r0.x
            Sm4Inst::If {
                cond: src_reg(RegFile::Temp, 0),
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([1.0, 0.0, 0.0, 1.0]),
            },
            Sm4Inst::Else,
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_imm([0.0, 1.0, 0.0, 1.0]),
            },
            Sm4Inst::EndIf,
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("if (bitcast<u32>((r0).x) != 0u)"),
        "expected `if_nz` to test raw bits via `bitcast<u32>`:\n{}",
        translated.wgsl
    );
}

#[test]
fn rejects_texture_slot_out_of_range() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_reg(RegFile::Input, 1),
                texture: TextureRef { slot: 128 },
                sampler: SamplerRef { slot: 0 },
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::ResourceSlotOutOfRange {
            kind: "texture",
            slot: 128,
            max
        } if max == MAX_TEXTURE_SLOTS - 1
    ));
}

#[test]
fn rejects_constant_buffer_slot_out_of_range() {
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
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: SrcOperand {
                    kind: SrcKind::ConstantBuffer { slot: 14, reg: 0 },
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::ResourceSlotOutOfRange {
            kind: "cbuffer",
            slot: 14,
            max
        } if max == D3D11_MAX_CONSTANT_BUFFER_SLOTS - 1
    ));
}

#[test]
fn rejects_cbuffer_slot_14_d3d11_limit_message() {
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
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_cb(14, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("D3D11") && err.contains("max 13"),
        "expected error to mention D3D11 slot limit (max 13), got: {err}"
    );
}

#[test]
fn rejects_sampler_slot_out_of_range() {
    let isgn_params = vec![
        sig_param("SV_Position", 0, 0, 0b1111),
        sig_param("TEXCOORD", 0, 1, 0b0011),
    ];
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
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
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                coord: src_reg(RegFile::Input, 1),
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef { slot: 16 },
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).unwrap_err();
    assert!(matches!(
        err,
        ShaderTranslateError::ResourceSlotOutOfRange {
            kind: "sampler",
            slot: 16,
            max
        } if max == MAX_SAMPLER_SLOTS - 1
    ));
}

#[test]
fn translates_compute_store_raw_to_storage_buffer() {
    // Compute shaders do not have ISGN/OSGN signatures.
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
        instructions: vec![
            Sm4Inst::StoreRaw {
                uav: UavRef { slot: 0 },
                addr: src_imm([0.0, 0.0, 0.0, 0.0]),
                // Regression: store_raw values must preserve raw u32 lane bits even when the bits
                // *look* like an integer-valued float (0x3f800000 == 1.0f). Numeric float→int
                // conversions would incorrectly collapse this to `1u`.
                value: src_imm_bits([0x3f80_0000u32, 0, 0, 0]),
                mask: WriteMask::X,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(translated.wgsl.contains("@compute"));
    assert!(translated.wgsl.contains("fn cs_main"));
    assert!(translated
        .wgsl
        .contains(&format!("@binding({})", BINDING_BASE_UAV)));
    assert!(translated
        .wgsl
        .contains("var<storage, read_write> u0: AeroStorageBufferU32"));
    assert!(translated.wgsl.contains("u0.data["));
    let val_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("let store_raw_val"))
        .expect("expected store_raw value temp");
    assert!(
        val_line.contains("0x3f800000u"),
        "expected raw-bit u32 literal to flow into store_raw value:\n{val_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    assert!(
        !val_line.contains("bitcast<f32>(0x3f800000u)"),
        "store_raw value should not route through float numeric conversions:\n{val_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    assert!(
        !val_line.contains("vec4<u32>(vec4<f32>"),
        "store_raw value should not cast vec4<f32> to vec4<u32> numerically:\n{val_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_compute_store_raw_uses_raw_bit_byte_address() {
    // Compute shaders do not have ISGN/OSGN signatures.
    let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Use a float immediate (`16.0`) for the byte address.
    //
    // DXBC register lanes are untyped 32-bit values. Integer operations (including buffer
    // addresses) must consume raw bits; numeric float→int conversion must be expressed explicitly
    // via `ftou`/`ftoi`, not inferred heuristically.
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
        instructions: vec![
            Sm4Inst::StoreRaw {
                uav: UavRef { slot: 0 },
                addr: src_imm([16.0, 16.0, 16.0, 16.0]),
                value: src_imm_bits([0xdead_beefu32, 0, 0, 0]),
                mask: WriteMask::X,
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("0x41800000u"),
        "expected raw float bit-pattern 0x41800000 to be used as a byte address:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("floor("),
        "expected strict raw-bit address handling (no float->u32 heuristics) in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_udiv_and_idiv_to_integer_division_and_modulo() {
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
            // Seed temp regs with float values that look like exact integers (e.g. 1.0).
            //
            // Integer ops must still consume the *raw bits* (0x3f800000, 0x40000000, ...), not
            // numeric f32->i32/u32 conversions.
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm([1.0, 1.0, 1.0, 1.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: src_imm([2.0, 2.0, 2.0, 2.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
                src: src_imm([-1.0, -1.0, -1.0, -1.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 3, WriteMask::XYZW),
                src: src_imm([2.0, 2.0, 2.0, 2.0]),
            },
            Sm4Inst::UDiv {
                dst_quot: dst(RegFile::Temp, 4, WriteMask::XYZW),
                dst_rem: dst(RegFile::Temp, 5, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
            },
            Sm4Inst::IDiv {
                dst_quot: dst(RegFile::Temp, 6, WriteMask::XYZW),
                dst_rem: dst(RegFile::Temp, 7, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 2),
                b: src_reg(RegFile::Temp, 3),
            },
            // Ensure at least one result flows to the output.
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 4),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Integer division should be performed on integer vectors, not floats.
    assert!(
        translated.wgsl.contains("vec4<u32>"),
        "expected udiv to operate on vec4<u32>:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("vec4<i32>"),
        "expected idiv to operate on vec4<i32>:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(" / "),
        "expected integer division operator in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(" % "),
        "expected integer modulo operator in WGSL:\n{}",
        translated.wgsl
    );

    // Integer sources should be derived purely via bitcasts from the untyped register file, with
    // no float->int heuristics.
    let udiv_a_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("let udiv_a"))
        .expect("expected udiv_a line");
    assert!(
        udiv_a_line.contains("bitcast<vec4<u32>>(r0)"),
        "expected udiv_a to come from raw bits of r0:\n{udiv_a_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    let udiv_b_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("let udiv_b"))
        .expect("expected udiv_b line");
    assert!(
        udiv_b_line.contains("bitcast<vec4<u32>>(r1)"),
        "expected udiv_b to come from raw bits of r1:\n{udiv_b_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    let idiv_a_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("let idiv_a"))
        .expect("expected idiv_a line");
    assert!(
        idiv_a_line.contains("bitcast<vec4<i32>>(r2)"),
        "expected idiv_a to come from raw bits of r2:\n{idiv_a_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    let idiv_b_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("let idiv_b"))
        .expect("expected idiv_b line");
    assert!(
        idiv_b_line.contains("bitcast<vec4<i32>>(r3)"),
        "expected idiv_b to come from raw bits of r3:\n{idiv_b_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("floor(") && !translated.wgsl.contains("select("),
        "udiv/idiv lowering should not use float-vs-bitcast heuristics:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_udiv_uses_raw_integer_bits_not_float_heuristics() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Use float bit patterns that look like exact integers as floats (e.g. 1.0 = 0x3f800000).
    // Integer ops must interpret these operands as *raw integer bits* in the untyped register
    // file, not as numeric floats that "happen to be integers".
    let a = src_imm_bits([1.0f32.to_bits(); 4]);
    let b = src_imm_bits([2.0f32.to_bits(); 4]);

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::UDiv {
                dst_quot: dst(RegFile::Temp, 0, WriteMask::XYZW),
                dst_rem: dst(RegFile::Temp, 1, WriteMask::XYZW),
                a,
                b,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let a_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("let udiv_a"))
        .expect("expected udiv_a temp");
    let b_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("let udiv_b"))
        .expect("expected udiv_b temp");
    assert!(
        !a_line.contains("select(") && !a_line.contains("floor("),
        "udiv lowering should not use float-vs-bitcast heuristics:\n{}",
        a_line
    );
    assert!(
        !b_line.contains("select(") && !b_line.contains("floor("),
        "udiv lowering should not use float-vs-bitcast heuristics:\n{}",
        b_line
    );
    assert!(translated.wgsl.contains("0x3f800000u"));
    assert!(translated.wgsl.contains("0x40000000u"));
}

#[test]
fn translates_switch_selector_uses_raw_integer_bits_not_float_heuristics() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Selector is `0x3f800000` (1.0f bits). If switch lowering incorrectly uses float→int heuristics
    // the selector would become `1` and match `case 1`. Correct lowering uses raw integer bits.
    let selector = src_imm_bits([1.0f32.to_bits(); 4]);

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            // Default value (will be overwritten in the matched clause).
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm_bits([0.0f32.to_bits(); 4]),
            },
            Sm4Inst::Switch { selector },
            Sm4Inst::Case { value: 1 },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm_bits([10.0f32.to_bits(); 4]),
            },
            Sm4Inst::Break,
            Sm4Inst::Default,
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm_bits([20.0f32.to_bits(); 4]),
            },
            Sm4Inst::EndSwitch,
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    let switch_line = translated
        .wgsl
        .lines()
        .find(|l| l.contains("switch("))
        .expect("expected switch line");
    assert!(
        !switch_line.contains("select(") && !switch_line.contains("floor("),
        "switch selector lowering should not use float-vs-bitcast heuristics:\n{switch_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
    assert!(
        switch_line.contains("0x3f800000u"),
        "expected switch selector to preserve raw bits (0x3f800000):\n{switch_line}\n\nWGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_udiv_respects_independent_dest_write_masks() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let a = SrcOperand {
        kind: SrcKind::ImmediateF32([7, 7, 7, 7]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };
    let b = SrcOperand {
        kind: SrcKind::ImmediateF32([3, 3, 3, 3]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::UDiv {
                dst_quot: dst(RegFile::Temp, 0, WriteMask::X),
                dst_rem: dst(RegFile::Temp, 1, WriteMask::Y),
                a,
                b,
            },
            // Ensure quotient reaches the output.
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Quotient and remainder write masks are independent in DXBC; ensure we only write r0.x and
    // r1.y for this instruction.
    assert!(
        translated.wgsl.contains("r0.x = (udiv_qf0).x;"),
        "expected udiv quotient to write only x lane:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("r1.y = (udiv_rf0).y;"),
        "expected udiv remainder to write only y lane:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_nested_structured_ifs() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let cond_true = src_imm([1.0, 1.0, 1.0, 1.0]);
    let red = src_imm([1.0, 0.0, 0.0, 1.0]);
    let green = src_imm([0.0, 1.0, 0.0, 1.0]);
    let blue = src_imm([0.0, 0.0, 1.0, 1.0]);

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::If {
                cond: cond_true.clone(),
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::If {
                cond: cond_true.clone(),
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: red,
            },
            Sm4Inst::Else,
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: green,
            },
            Sm4Inst::EndIf,
            Sm4Inst::Else,
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: blue,
            },
            Sm4Inst::EndIf,
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.matches("if (").count() >= 2,
        "expected nested if statements in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.matches("} else {").count() >= 2,
        "expected else branches in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_ubfe_to_extract_bits() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let imm_scalar_bits = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v; 4]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Ubfe {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                width: imm_scalar_bits(8),
                offset: imm_scalar_bits(0),
                src: imm_scalar_bits(0x1234_5678),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("extractBits"),
        "expected ubfe translation to use WGSL extractBits:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_ibfe_to_extract_bits() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let imm_scalar_bits = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v; 4]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Ibfe {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                width: imm_scalar_bits(8),
                offset: imm_scalar_bits(0),
                // Extract from a negative `i32` bit-pattern.
                src: imm_scalar_bits(0x8765_4321),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("extractBits"),
        "expected ibfe translation to use WGSL extractBits:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_bfi_to_insert_bits() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let imm_scalar_bits = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v; 4]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Bfi {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                width: imm_scalar_bits(8),
                offset: imm_scalar_bits(0),
                insert: imm_scalar_bits(0xaa),
                base: imm_scalar_bits(0x1234_5678),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("insertBits"),
        "expected bfi translation to use WGSL insertBits:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_unsigned_integer_compare_to_predicate_mask() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Compare raw integer bits: 1u < 2u.
    let imm_u32 = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: imm_u32(1),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: imm_u32(2),
            },
            Sm4Inst::Cmp {
                dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
                op: CmpOp::Lt,
                ty: CmpType::U32,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("bitcast<vec4<u32>>"),
        "expected compare operands to be interpreted as u32 via bitcast:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("select(vec4<u32>(0u), vec4<u32>(0xffffffffu)"),
        "expected bool->mask conversion in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected predicate mask stored as vec4<f32> bitcast:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_signed_integer_compare_to_predicate_mask() {
    let osgn_params = vec![sig_param("SV_Target", 0, 0, 0b1111)];
    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Compare raw integer bits: -1 < 0 (i32).
    let imm_u32 = |v: u32| SrcOperand {
        kind: SrcKind::ImmediateF32([v; 4]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    };

    let module = Sm4Module {
        stage: ShaderStage::Pixel,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: imm_u32(0xffff_ffff),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: imm_u32(0),
            },
            Sm4Inst::Cmp {
                dst: dst(RegFile::Temp, 2, WriteMask::XYZW),
                a: src_reg(RegFile::Temp, 0),
                b: src_reg(RegFile::Temp, 1),
                op: CmpOp::Lt,
                ty: CmpType::I32,
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_reg(RegFile::Temp, 2),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    assert!(
        translated.wgsl.contains("bitcast<vec4<i32>>"),
        "expected compare operands to be interpreted as i32 via bitcast:\n{}",
        translated.wgsl
    );
    assert!(
        translated
            .wgsl
            .contains("select(vec4<u32>(0u), vec4<u32>(0xffffffffu)"),
        "expected bool->mask conversion in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<f32>>"),
        "expected predicate mask stored as vec4<f32> bitcast:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_atomic_add_uav_buffer() {
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
            // atomicAdd(u0[0], 1) -> r0.x (original value)
            Sm4Inst::AtomicAdd {
                dst: Some(dst(RegFile::Temp, 0, WriteMask::X)),
                uav: UavRef { slot: 0 },
                addr: src_imm([0.0, 0.0, 0.0, 0.0]),
                value: src_imm([1.0, 1.0, 1.0, 1.0]),
            },
            // Discarded atomicAdd(u0[0], 1).
            Sm4Inst::AtomicAdd {
                dst: None,
                uav: UavRef { slot: 0 },
                addr: src_imm([0.0, 0.0, 0.0, 0.0]),
                value: src_imm([1.0, 1.0, 1.0, 1.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);
    assert!(
        translated.wgsl.contains("atomicAdd"),
        "expected atomicAdd call in WGSL:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(&format!(
            "@group(1) @binding({BINDING_BASE_UAV}) var<storage, read_write> u0: AeroStorageBufferAtomicU32;"
        )),
        "expected u0 storage buffer binding declaration:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<f32>(atomic_old_0)"),
        "expected atomic return value to be written via bitcast:\n{}",
        translated.wgsl
    );

    assert!(
        translated
            .reflection
            .bindings
            .iter()
            .any(|b| matches!(b.kind, BindingKind::UavBuffer { slot: 0 })),
        "expected reflection to include u0 binding"
    );
}

#[test]
fn translates_sm4_bool_tests_consistently() {
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
            // r0 = 1.0 (used as a non-zero condition), r1 = 0.0 (zero condition).
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 0, WriteMask::XYZW),
                src: src_imm([1.0, 1.0, 1.0, 1.0]),
            },
            Sm4Inst::Mov {
                dst: dst(RegFile::Temp, 1, WriteMask::XYZW),
                src: src_imm([0.0, 0.0, 0.0, 0.0]),
            },
            // Scalar test site: `if_nz`.
            Sm4Inst::If {
                cond: src_reg(RegFile::Temp, 0),
                test: Sm4TestBool::NonZero,
            },
            // Scalar test site: `discard_nz`.
            Sm4Inst::Discard {
                cond: src_reg(RegFile::Temp, 1),
                test: Sm4TestBool::NonZero,
            },
            Sm4Inst::EndIf,
            // Vector test site: `movc`.
            Sm4Inst::Movc {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                cond: src_reg(RegFile::Temp, 0),
                a: src_imm([10.0, 20.0, 30.0, 40.0]),
                b: src_imm([50.0, 60.0, 70.0, 80.0]),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    assert_wgsl_validates(&translated.wgsl);

    // Ensure all SM4 boolean-test sites use the same raw-bit test semantics (bitcast to u32).
    assert!(
        translated.wgsl.contains("bitcast<u32>"),
        "expected scalar bool tests to use bitcast<u32>:\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("bitcast<vec4<u32>>"),
        "expected vector bool tests to use bitcast<vec4<u32>>:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("== 0.0"),
        "found numeric == 0.0 test:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("!= 0.0"),
        "found numeric != 0.0 test:\n{}",
        translated.wgsl
    );
}
