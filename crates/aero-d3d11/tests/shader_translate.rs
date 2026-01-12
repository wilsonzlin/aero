use aero_d3d11::binding_model::{
    BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE, MAX_CBUFFER_SLOTS,
    MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS,
};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BindingKind, Builtin, DxbcFile,
    DxbcSignatureParameter, FourCC, OperandModifier, RegFile, RegisterRef, SamplerRef, ShaderModel,
    ShaderStage, ShaderTranslateError, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle,
    TextureRef, WriteMask,
};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }
    let total_size = cursor as u32;

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored by parser)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved/unknown
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    for (fourcc, data) in chunks {
        bytes.extend_from_slice(&fourcc.0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }
    assert_eq!(bytes.len(), total_size as usize);
    bytes
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
    assert!(translated.wgsl.contains("@location(0) v0: vec2<f32>"));
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
    assert!(translated.wgsl.contains("return o0;"));
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
    // float/int tag, so use raw integer bits here (1, 2, 0, 0).
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
    assert!(translated.wgsl.contains("vec2<i32>(select("));
    assert!(translated.wgsl.contains("bitcast<i32>(0x00000001u)"));

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
        translated.wgsl.contains("bitcast<i32>(0x00000003u)"),
        "expected mip LOD 3 (0x00000003) to be present in WGSL:\n{}",
        translated.wgsl
    );
}

#[test]
fn translates_texture_load_with_vertex_id_numeric_coords() {
    // `SV_VertexID` is surfaced to the translator as a `u32` builtin and expanded into our
    // internal `vec4<f32>` register model via `f32(input.vertex_id)`. This means that integer
    // texel coordinates may sometimes appear as numeric floats (not raw integer bits), so `ld`
    // emission must handle both.
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
    assert!(translated.wgsl.contains("f32(input.vertex_id)"));
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
    assert!(translated.wgsl.contains("@location(2) v2: vec4<f32>"));
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
                    kind: SrcKind::ConstantBuffer { slot: 32, reg: 0 },
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
            slot: 32,
            max
        } if max == MAX_CBUFFER_SLOTS - 1
    ));
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
