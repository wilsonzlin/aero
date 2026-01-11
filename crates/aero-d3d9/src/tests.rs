use pretty_assertions::assert_eq;

use std::collections::HashMap;

use crate::{dxbc, shader, software, state};

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_src_mod(reg_type: u8, reg_num: u16, swizzle: u8, modifier: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16) | ((modifier as u32) << 24)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    let token = (opcode as u32) | ((params.len() as u32) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn enc_inst_with_extra(opcode: u16, extra: u32, params: &[u32]) -> Vec<u32> {
    let token = (opcode as u32) | ((params.len() as u32) << 24) | extra;
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn assemble_vs_passthrough() -> Vec<u32> {
    // vs_2_0
    let mut out = vec![0xFFFE0200];
    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov oT0, v1
    out.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    // mov oD0, v2
    out.extend(enc_inst(0x0001, &[enc_dst(5, 0, 0xF), enc_src(1, 2, 0xE4)]));
    // end
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_texture_modulate() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // mul r0, r0, v0 (modulate by color)
    out.extend(enc_inst(
        0x0005,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src(1, 0, 0xE4), // v0 treated as input (color)
        ],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_color_passthrough() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // mov oC0, v0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(1, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_math_ops() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];

    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // min r0, r0, c1
    out.extend(enc_inst(
        0x000A,
        &[enc_dst(0, 0, 0xF), enc_src(0, 0, 0xE4), enc_src(2, 1, 0xE4)],
    ));
    // max r0, r0, c2
    out.extend(enc_inst(
        0x000B,
        &[enc_dst(0, 0, 0xF), enc_src(0, 0, 0xE4), enc_src(2, 2, 0xE4)],
    ));
    // rcp r1, c3
    out.extend(enc_inst(0x0006, &[enc_dst(0, 1, 0xF), enc_src(2, 3, 0xE4)]));
    // rsq r2, c4
    out.extend(enc_inst(0x0007, &[enc_dst(0, 2, 0xF), enc_src(2, 4, 0xE4)]));
    // frc r3, c5
    out.extend(enc_inst(0x0013, &[enc_dst(0, 3, 0xF), enc_src(2, 5, 0xE4)]));
    // slt r4, c6, c7
    out.extend(enc_inst(
        0x000C,
        &[enc_dst(0, 4, 0xF), enc_src(2, 6, 0xE4), enc_src(2, 7, 0xE4)],
    ));
    // sge r5, c8, c9
    out.extend(enc_inst(
        0x000D,
        &[enc_dst(0, 5, 0xF), enc_src(2, 8, 0xE4), enc_src(2, 9, 0xE4)],
    ));
    // cmp r6, c10, c11, c12
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(0, 6, 0xF),
            enc_src(2, 10, 0xE4),
            enc_src(2, 11, 0xE4),
            enc_src(2, 12, 0xE4),
        ],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps_mov_sat_neg_c0() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // mov_sat oC0, -c0
    out.extend(enc_inst_with_extra(
        0x0001,
        1u32 << 20, // saturate
        &[
            enc_dst(8, 0, 0xF),
            enc_src_mod(2, 0, 0xE4, 1), // -c0
        ],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_vs_passthrough_sm3() -> Vec<u32> {
    let mut out = assemble_vs_passthrough();
    out[0] = 0xFFFE0300; // vs_3_0
    out
}

fn assemble_ps3_tex_ifc_def() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.5, 0.0, 1.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F00_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // ifc_lt c0.x, r0.x (compare op 3 = lt)
    out.extend(enc_inst_with_extra(
        0x0029,
        3u32 << 16,
        &[enc_src(2, 0, 0x00), enc_src(0, 0, 0x00)],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // mov oC0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    out.push(0x0000FFFF);
    out
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

#[test]
fn dxbc_container_roundtrip_extracts_shdr() {
    let vs = to_bytes(&assemble_vs_passthrough());
    let container = dxbc::build_container(&[(dxbc::FourCC::SHDR, &vs)]);
    let extracted = dxbc::extract_shader_bytecode(&container).unwrap();
    assert_eq!(extracted, vs);
}

#[test]
fn translates_simple_vs_to_wgsl() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough());
    let dxbc = dxbc::build_container(&[(dxbc::FourCC::SHDR, &vs_bytes)]);
    let program = shader::parse(&dxbc).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir);

    // Validate WGSL via naga to ensure WebGPU compatibility.
    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    let _info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("@vertex"));
    assert!(wgsl.wgsl.contains("fn vs_main"));
    assert!(wgsl.wgsl.contains("@builtin(position)"));
}

#[test]
fn shader_cache_dedupes_by_hash() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough());
    let dxbc = dxbc::build_container(&[(dxbc::FourCC::SHDR, &vs_bytes)]);

    let mut cache = shader::ShaderCache::default();
    let a = cache.get_or_translate(&dxbc).unwrap().hash;
    let b = cache.get_or_translate(&dxbc).unwrap().hash;
    assert_eq!(a, b);
}

#[test]
fn state_defaults_are_stable() {
    let blend = state::BlendState::default();
    assert_eq!(blend.enabled, false);

    let depth = state::DepthState::default();
    assert_eq!(depth.enabled, false);

    let raster = state::RasterState::default();
    assert_eq!(raster.cull, state::CullMode::Back);
}

#[test]
fn translates_simple_ps_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps_texture_modulate());
    let dxbc = dxbc::build_container(&[(dxbc::FourCC::SHDR, &ps_bytes)]);
    let program = shader::parse(&dxbc).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir);

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("@fragment"));
    assert!(wgsl.wgsl.contains("textureSample"));
}

#[test]
fn translates_additional_ps_ops_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps_math_ops());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir);

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("min("));
    assert!(wgsl.wgsl.contains("max("));
    assert!(wgsl.wgsl.contains("inverseSqrt"));
    assert!(wgsl.wgsl.contains("fract("));
    assert!(wgsl.wgsl.contains("select("));
}

#[test]
fn translates_ps3_ifc_def_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps3_tex_ifc_def());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir);

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("if ("));
    assert!(wgsl.wgsl.contains("} else {"));
    assert!(wgsl.wgsl.contains("let c0: vec4<f32>"));
}

fn build_vertex_decl_pos_tex_color() -> state::VertexDecl {
    state::VertexDecl::new(
        40,
        vec![
            state::VertexElement {
                offset: 0,
                ty: state::VertexElementType::Float4,
                usage: state::VertexUsage::Position,
                usage_index: 0,
            },
            state::VertexElement {
                offset: 16,
                ty: state::VertexElementType::Float2,
                usage: state::VertexUsage::TexCoord,
                usage_index: 0,
            },
            state::VertexElement {
                offset: 24,
                ty: state::VertexElementType::Float4,
                usage: state::VertexUsage::Color,
                usage_index: 0,
            },
        ],
    )
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_vec4(out: &mut Vec<u8>, v: software::Vec4) {
    push_f32(out, v.x);
    push_f32(out, v.y);
    push_f32(out, v.z);
    push_f32(out, v.w);
}

fn push_vec2(out: &mut Vec<u8>, x: f32, y: f32) {
    push_f32(out, x);
    push_f32(out, y);
}

fn zero_constants() -> [software::Vec4; 256] {
    [software::Vec4::ZERO; 256]
}

#[test]
fn micro_triangle_solid_color_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_color_passthrough())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let red = software::Vec4::new(1.0, 0.0, 0.0, 1.0);

    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, red);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    software::draw(
        &mut rt,
        &vs,
        &ps,
        &decl,
        &vb,
        None,
        &zero_constants(),
        &HashMap::new(),
        &HashMap::new(),
        state::BlendState::default(),
    );

    let rgba = rt.to_rgba8();
    let hash = blake3::hash(&rgba);
    // Stable output signature for regression testing.
    assert_eq!(
        hash.to_hex().as_str(),
        "f319f67af7e26fb3e108840dfe953de674f251a9542b12738334ad592fbff483"
    );
    assert_eq!(rt.get(8, 8).to_rgba8(), [255, 0, 0, 255]);
}

#[test]
fn micro_textured_quad_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_texture_modulate())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    let verts = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)), // bottom-left
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),  // bottom-right
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),   // top-right
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),  // top-left
    ];
    for (pos, (u, v)) in verts {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let tex_bytes: [u8; 16] = [
        255, 0, 0, 255, // red (top-left)
        0, 255, 0, 255, // green (top-right)
        0, 0, 255, 255, // blue (bottom-left)
        255, 255, 255, 255, // white (bottom-right)
    ];
    let tex = software::Texture2D::from_rgba8(2, 2, &tex_bytes);

    let mut textures = HashMap::new();
    textures.insert(0u16, tex);

    let mut sampler_states = HashMap::new();
    sampler_states.insert(
        0u16,
        state::SamplerState {
            min_filter: state::FilterMode::Point,
            mag_filter: state::FilterMode::Point,
            address_u: state::AddressMode::Clamp,
            address_v: state::AddressMode::Clamp,
        },
    );

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    software::draw(
        &mut rt,
        &vs,
        &ps,
        &decl,
        &vb,
        Some(&indices),
        &zero_constants(),
        &textures,
        &sampler_states,
        state::BlendState::default(),
    );

    assert_eq!(rt.get(1, 1).to_rgba8(), [255, 0, 0, 255]); // top-left
    assert_eq!(rt.get(6, 1).to_rgba8(), [0, 255, 0, 255]); // top-right
    assert_eq!(rt.get(1, 6).to_rgba8(), [0, 0, 255, 255]); // bottom-left
    assert_eq!(rt.get(6, 6).to_rgba8(), [255, 255, 255, 255]); // bottom-right

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "6fa50059441133e99a2414be50f613190809d5373953a6e414c373be772438f7"
    );
}

#[test]
fn micro_ps3_ifc_def_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps3_tex_ifc_def())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    let verts = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)), // bottom-left
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),  // bottom-right
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),   // top-right
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),  // top-left
    ];
    for (pos, (u, v)) in verts {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    // 2x2 texture with red in the left column and black in the right column.
    let tex_bytes: [u8; 16] = [
        255, 0, 0, 255, // top-left red
        0, 0, 0, 255, // top-right black
        255, 0, 0, 255, // bottom-left red
        0, 0, 0, 255, // bottom-right black
    ];
    let tex = software::Texture2D::from_rgba8(2, 2, &tex_bytes);

    let mut textures = HashMap::new();
    textures.insert(0u16, tex);

    let mut sampler_states = HashMap::new();
    sampler_states.insert(
        0u16,
        state::SamplerState {
            min_filter: state::FilterMode::Point,
            mag_filter: state::FilterMode::Point,
            address_u: state::AddressMode::Clamp,
            address_v: state::AddressMode::Clamp,
        },
    );

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    software::draw(
        &mut rt,
        &vs,
        &ps,
        &decl,
        &vb,
        Some(&indices),
        &zero_constants(),
        &textures,
        &sampler_states,
        state::BlendState::default(),
    );

    // Left side: r0.x is 1.0 so branch returns the sampled texel (red).
    assert_eq!(rt.get(1, 4).to_rgba8(), [255, 0, 0, 255]);
    // Right side: r0.x is 0.0 so branch returns the embedded constant c0 = (0.5, 0.0, 1.0, 1.0).
    assert_eq!(rt.get(6, 4).to_rgba8(), [128, 0, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "fa291c33b86c387331d23b7163e6622bb9553e866980db89570ac967770c0ee3"
    );
}

#[test]
fn micro_alpha_blending_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_color_passthrough())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)),
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let make_vb = |color: software::Vec4| {
        let mut vb = Vec::new();
        for (pos, (u, v)) in quad {
            push_vec4(&mut vb, pos);
            push_vec2(&mut vb, u, v);
            push_vec4(&mut vb, color);
        }
        vb
    };

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    // Pass 1: opaque red.
    software::draw(
        &mut rt,
        &vs,
        &ps,
        &decl,
        &make_vb(software::Vec4::new(1.0, 0.0, 0.0, 1.0)),
        Some(&indices),
        &zero_constants(),
        &HashMap::new(),
        &HashMap::new(),
        state::BlendState::default(),
    );

    // Pass 2: green with alpha=0.5 blended over.
    let blend = state::BlendState {
        enabled: true,
        src_factor: state::BlendFactor::SrcAlpha,
        dst_factor: state::BlendFactor::OneMinusSrcAlpha,
        op: state::BlendOp::Add,
    };
    software::draw(
        &mut rt,
        &vs,
        &ps,
        &decl,
        &make_vb(software::Vec4::new(0.0, 1.0, 0.0, 0.5)),
        Some(&indices),
        &zero_constants(),
        &HashMap::new(),
        &HashMap::new(),
        blend,
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [128, 128, 0, 191]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "22e5d8454f12677044ceb24de7c5da02e285d7a6b347c7ed4bfb7b2209dadb0a"
    );
}

#[test]
fn translates_src_and_result_modifiers_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps_mov_sat_neg_c0());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir);

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("clamp("));
    assert!(wgsl.wgsl.contains("constants.c[0u]"));
}

#[test]
fn micro_ps2_src_and_result_modifiers_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_mov_sat_neg_c0())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut constants = zero_constants();
    constants[0] = software::Vec4::new(-0.5, 0.5, -2.0, -1.0);

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    software::draw(
        &mut rt,
        &vs,
        &ps,
        &decl,
        &vb,
        None,
        &constants,
        &HashMap::new(),
        &HashMap::new(),
        state::BlendState::default(),
    );

    // `oC0 = clamp(-c0, 0..1)`, with c0 = (-0.5, 0.5, -2.0, -1.0).
    assert_eq!(rt.get(8, 8).to_rgba8(), [128, 0, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "ab477a03b69b374481c3b6cba362a9b6e9cfb0dd038252a06a610b4c058e3f26"
    );
}

#[test]
fn supports_shader_model_3() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough_sm3());
    let program = shader::parse(&vs_bytes).unwrap();
    assert_eq!(program.version.model.major, 3);
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[test]
fn parses_isgn_signature_chunk() {
    // Minimal ISGN-like chunk with a single POSITION element.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 1); // element count
    push_u32(&mut chunk, 8); // table offset

    // Entry (24 bytes).
    push_u32(&mut chunk, 32); // name offset
    push_u32(&mut chunk, 0); // semantic index
    push_u32(&mut chunk, 0); // system value type
    push_u32(&mut chunk, 0); // component type
    push_u32(&mut chunk, 0); // register
    chunk.push(0xF); // mask
    chunk.push(0xF); // rw mask
    chunk.extend_from_slice(&[0, 0]); // padding

    chunk.extend_from_slice(b"POSITION\0");

    let sig = dxbc::parse_signature(&chunk).unwrap();
    assert_eq!(
        sig,
        vec![dxbc::SignatureElement {
            semantic: "POSITION".to_string(),
            semantic_index: 0,
            register: 0,
            mask: 0xF,
        }]
    );
}

#[test]
fn parses_rdef_resource_bindings() {
    // Minimal RDEF-like chunk with a single texture bound at t3.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // cb count
    push_u32(&mut chunk, 0); // cb offset
    push_u32(&mut chunk, 1); // resource count
    push_u32(&mut chunk, 28); // resource offset (header size)
    push_u32(&mut chunk, 0); // shader model
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 0); // creator offset

    // Resource entry (32 bytes).
    push_u32(&mut chunk, 60); // name offset
    push_u32(&mut chunk, 0); // type
    push_u32(&mut chunk, 0); // return type
    push_u32(&mut chunk, 0); // dimension
    push_u32(&mut chunk, 0); // num samples
    push_u32(&mut chunk, 3); // bind point
    push_u32(&mut chunk, 1); // bind count
    push_u32(&mut chunk, 0); // flags

    chunk.extend_from_slice(b"tex0\0");

    let rdef = dxbc::parse_rdef(&chunk).unwrap();
    assert_eq!(rdef.resources.len(), 1);
    assert_eq!(rdef.resources[0].name, "tex0");
    assert_eq!(rdef.resources[0].bind_point, 3);
}

#[test]
fn parses_ctab_constant_table() {
    // Minimal CTAB chunk with a single constant c0 and target string.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // size (ignored)
    push_u32(&mut chunk, 0); // creator offset
    push_u32(&mut chunk, 0); // version
    push_u32(&mut chunk, 1); // constant count
    push_u32(&mut chunk, 28); // constant info offset
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 48); // target offset (after entry)

    // Constant info entry (20 bytes).
    push_u32(&mut chunk, 55); // name offset (after target string)
    push_u16(&mut chunk, 0); // register set
    push_u16(&mut chunk, 0); // register index
    push_u16(&mut chunk, 1); // register count
    push_u16(&mut chunk, 0); // reserved
    push_u32(&mut chunk, 0); // type info offset
    push_u32(&mut chunk, 0); // default value offset

    chunk.extend_from_slice(b"ps_2_0\0"); // 7 bytes -> next offset 55
    chunk.extend_from_slice(b"C0\0");

    let ctab = dxbc::parse_ctab(&chunk).unwrap();
    assert_eq!(ctab.target.as_deref(), Some("ps_2_0"));
    assert_eq!(ctab.constants.len(), 1);
    assert_eq!(ctab.constants[0].name, "C0");
    assert_eq!(ctab.constants[0].register_index, 0);
    assert_eq!(ctab.constants[0].register_count, 1);
}

#[test]
fn converts_guest_textures_to_rgba8() {
    let rgba = state::convert_guest_texture_to_rgba8(
        state::TextureFormat::A8R8G8B8,
        1,
        1,
        4,
        &[0x01, 0x02, 0x03, 0x04], // BGRA
    );
    assert_eq!(rgba, vec![0x03, 0x02, 0x01, 0x04]);

    let rgba = state::convert_guest_texture_to_rgba8(
        state::TextureFormat::X8R8G8B8,
        1,
        1,
        4,
        &[0x10, 0x20, 0x30, 0x00], // BGRX
    );
    assert_eq!(rgba, vec![0x30, 0x20, 0x10, 0xFF]);

    let rgba = state::convert_guest_texture_to_rgba8(state::TextureFormat::A8, 1, 1, 1, &[0x7F]);
    assert_eq!(rgba, vec![0xFF, 0xFF, 0xFF, 0x7F]);
}
