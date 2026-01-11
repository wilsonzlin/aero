use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BindingKind, DxbcFile, DxbcSignatureParameter,
    FourCC, OperandModifier, RegFile, RegisterRef, SamplerRef, ShaderStage, Sm4Inst, Sm4Module,
    SrcKind, SrcOperand, Swizzle, TextureRef, WriteMask,
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

    // Reflection should surface required texture/sampler slots.
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Texture2D { slot: 0 })));
    assert!(translated
        .reflection
        .bindings
        .iter()
        .any(|b| matches!(b.kind, BindingKind::Sampler { slot: 0 })));
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
