use aero_d3d11::binding_model::{BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE};
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BindingKind, DstOperand, DxbcFile,
    DxbcSignatureParameter, FourCC, OperandModifier, RegFile, RegisterRef, SamplerRef, ShaderModel,
    ShaderStage, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, TextureRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_PSGN: FourCC = FourCC(*b"PSGN");
const FOURCC_PCSG: FourCC = FourCC(*b"PCSG");
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
            min_precision: u32::from(p.min_precision),
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn dst(file: RegFile, index: u32, mask: WriteMask) -> DstOperand {
    DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_cb(slot: u32, reg: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ConstantBuffer { slot, reg },
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

fn src_imm_f32(x: f32, y: f32, z: f32, w: f32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32([x.to_bits(), y.to_bits(), z.to_bits(), w.to_bits()]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

#[test]
fn translates_ds_cbuffer_in_group3_with_compute_visibility() {
    // Domain shaders require an output signature with SV_Position.
    let osgn_params = vec![sig_param("SV_Position", 0, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_PSGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Minimal DS body that reads cb0[0] and writes it to SV_Position.
    let module = Sm4Module {
        stage: ShaderStage::Domain,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_cb(0, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    naga::front::wgsl::parse_str(&translated.wgsl).expect("generated WGSL failed to parse");

    assert!(
        translated.wgsl.contains(&format!(
            "@group(3) @binding({BINDING_BASE_CBUFFER}) var<uniform> cb0:"
        )),
        "expected DS constant buffer binding to use @group(3):\n{}",
        translated.wgsl
    );

    let cb = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: 0, .. }))
        .expect("missing cbuffer reflection");
    assert_eq!(cb.group, 3);
    assert_eq!(cb.binding, BINDING_BASE_CBUFFER);
    assert_eq!(cb.visibility, wgpu::ShaderStages::COMPUTE);
}

#[test]
fn translates_hs_cbuffer_in_group3_with_compute_visibility() {
    // HS requires ISGN/OSGN + a patch-constant signature (PCSG or PSGN). Provide an OSGN entry so
    // the HS output commit path stores `o0` into the control-point output buffer.
    let osgn_params = vec![sig_param("TEXCOORD", 0, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
        (FOURCC_PCSG, build_signature_chunk(&[])),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    // Minimal HS body that reads cb0[0] and writes it to output register o0.
    let module = Sm4Module {
        stage: ShaderStage::Hull,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Mov {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                src: src_cb(0, 0),
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    naga::front::wgsl::parse_str(&translated.wgsl).expect("generated WGSL failed to parse");

    assert!(
        translated.wgsl.contains(&format!(
            "@group(3) @binding({BINDING_BASE_CBUFFER}) var<uniform> cb0:"
        )),
        "expected HS constant buffer binding to use @group(3):\n{}",
        translated.wgsl
    );

    let cb = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::ConstantBuffer { slot: 0, .. }))
        .expect("missing cbuffer reflection");
    assert_eq!(cb.group, 3);
    assert_eq!(cb.binding, BINDING_BASE_CBUFFER);
    assert_eq!(cb.visibility, wgpu::ShaderStages::COMPUTE);
}

#[test]
fn translates_ds_texture_sampler_in_group3_with_samplelevel() {
    // Domain shaders require an output signature with SV_Position.
    let osgn_params = vec![sig_param("SV_Position", 0, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_PSGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Domain,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                coord: src_imm_f32(0.5, 0.5, 0.0, 0.0),
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    naga::front::wgsl::parse_str(&translated.wgsl).expect("generated WGSL failed to parse");

    assert!(
        translated.wgsl.contains(&format!(
            "@group(3) @binding({BINDING_BASE_TEXTURE}) var t0: texture_2d<f32>;"
        )),
        "expected DS t0 to use @group(3):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(&format!(
            "@group(3) @binding({BINDING_BASE_SAMPLER}) var s0: sampler;"
        )),
        "expected DS s0 to use @group(3):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("textureSampleLevel(t0, s0"),
        "expected DS sampling to use textureSampleLevel:\n{}",
        translated.wgsl
    );

    let tex = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Texture2D { slot: 0 }))
        .expect("missing texture reflection");
    assert_eq!(tex.group, 3);
    assert_eq!(tex.binding, BINDING_BASE_TEXTURE);
    assert_eq!(tex.visibility, wgpu::ShaderStages::COMPUTE);

    let samp = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Sampler { slot: 0 }))
        .expect("missing sampler reflection");
    assert_eq!(samp.group, 3);
    assert_eq!(samp.binding, BINDING_BASE_SAMPLER);
    assert_eq!(samp.visibility, wgpu::ShaderStages::COMPUTE);
}

#[test]
fn translates_hs_texture_sampler_in_group3_with_samplelevel() {
    // HS requires ISGN/OSGN + a patch-constant signature (PCSG or PSGN). Provide an OSGN entry so
    // the HS output commit path stores `o0` into the control-point output buffer.
    let osgn_params = vec![sig_param("TEXCOORD", 0, 0, 0b1111)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_SHEX, Vec::new()),
        (FOURCC_ISGN, build_signature_chunk(&[])),
        (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
        (FOURCC_PCSG, build_signature_chunk(&[])),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let signatures = parse_signatures(&dxbc).expect("parse signatures");

    let module = Sm4Module {
        stage: ShaderStage::Hull,
        model: ShaderModel { major: 5, minor: 0 },
        decls: Vec::new(),
        instructions: vec![
            Sm4Inst::Sample {
                dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                coord: src_imm_f32(0.25, 0.75, 0.0, 0.0),
                texture: TextureRef { slot: 0 },
                sampler: SamplerRef { slot: 0 },
            },
            Sm4Inst::Ret,
        ],
    };

    let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
    naga::front::wgsl::parse_str(&translated.wgsl).expect("generated WGSL failed to parse");

    assert!(
        translated.wgsl.contains(&format!(
            "@group(3) @binding({BINDING_BASE_TEXTURE}) var t0: texture_2d<f32>;"
        )),
        "expected HS t0 to use @group(3):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains(&format!(
            "@group(3) @binding({BINDING_BASE_SAMPLER}) var s0: sampler;"
        )),
        "expected HS s0 to use @group(3):\n{}",
        translated.wgsl
    );
    assert!(
        translated.wgsl.contains("textureSampleLevel(t0, s0"),
        "expected HS sampling to use textureSampleLevel:\n{}",
        translated.wgsl
    );

    let tex = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Texture2D { slot: 0 }))
        .expect("missing texture reflection");
    assert_eq!(tex.group, 3);
    assert_eq!(tex.binding, BINDING_BASE_TEXTURE);
    assert_eq!(tex.visibility, wgpu::ShaderStages::COMPUTE);

    let samp = translated
        .reflection
        .bindings
        .iter()
        .find(|b| matches!(b.kind, BindingKind::Sampler { slot: 0 }))
        .expect("missing sampler reflection");
    assert_eq!(samp.group, 3);
    assert_eq!(samp.binding, BINDING_BASE_SAMPLER);
    assert_eq!(samp.visibility, wgpu::ShaderStages::COMPUTE);
}
