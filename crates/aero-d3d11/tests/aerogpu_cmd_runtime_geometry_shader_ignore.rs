mod common;

use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{
    IndexBufferBinding, PrimitiveTopology, RasterizerState, VertexBufferBinding,
};
use aero_d3d11::sm4::opcode as sm4_opcode;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};

const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_GS_POINT_TO_TRIANGLE: &[u8] = include_bytes!("fixtures/gs_point_to_triangle.dxbc");
const DXBC_GS_CUT: &[u8] = include_bytes!("fixtures/gs_cut.dxbc");
const DXBC_PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name,
            semantic_index: p.semantic_index,
            system_value_type: 0,
            component_type: 0,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.mask,
            stream: 0,
            min_precision: 0,
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn build_ps_passthrough_color_in_v2_dxbc() -> Vec<u8> {
    // ps_4_0:
    //   mov o0, v2
    //   ret
    //
    // This forces the translated WGSL to declare `@location(2)` in `PsIn`, which
    // the runtime GS emulation draw path must support.
    let isgn = build_signature_chunk(&[SigParam {
        semantic_name: "COLOR",
        semantic_index: 0,
        register: 2,
        mask: 0x0f,
    }]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_token = sm4_opcode::OPCODE_MOV | (5u32 << sm4_opcode::OPCODE_LEN_SHIFT);
    let ret_token = sm4_opcode::OPCODE_RET | (1u32 << sm4_opcode::OPCODE_LEN_SHIFT);

    // Operand encodings are shared across the SM4 fixtures.
    let dst_o0 = 0x0010_f022u32;
    let src_v = 0x001e_4016u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        src_v,
        2, // v2 index
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    dxbc_test_utils::build_container_owned(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << sm4_opcode::OPCODE_LEN_SHIFT)
}

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
) -> u32 {
    let mut token = 0u32;
    token |= num_components & sm4_opcode::OPERAND_NUM_COMPONENTS_MASK;
    token |= (selection_mode & sm4_opcode::OPERAND_SELECTION_MODE_MASK)
        << sm4_opcode::OPERAND_SELECTION_MODE_SHIFT;
    token |= (ty & sm4_opcode::OPERAND_TYPE_MASK) << sm4_opcode::OPERAND_TYPE_SHIFT;
    token |= (component_sel & sm4_opcode::OPERAND_COMPONENT_SELECTION_MASK)
        << sm4_opcode::OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & sm4_opcode::OPERAND_INDEX_DIMENSION_MASK)
        << sm4_opcode::OPERAND_INDEX_DIMENSION_SHIFT;
    token |= sm4_opcode::OPERAND_INDEX_REP_IMMEDIATE32 << sm4_opcode::OPERAND_INDEX0_REP_SHIFT;
    token |= sm4_opcode::OPERAND_INDEX_REP_IMMEDIATE32 << sm4_opcode::OPERAND_INDEX1_REP_SHIFT;
    token |= sm4_opcode::OPERAND_INDEX_REP_IMMEDIATE32 << sm4_opcode::OPERAND_INDEX2_REP_SHIFT;
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: u32) -> Vec<u32> {
    vec![
        operand_token(ty, 2, sm4_opcode::OPERAND_SEL_MASK, mask, 1),
        idx,
    ]
}

fn imm_f32x4(v: [f32; 4]) -> Vec<u32> {
    vec![
        operand_token(
            sm4_opcode::OPERAND_TYPE_IMMEDIATE32,
            2,
            sm4_opcode::OPERAND_SEL_SWIZZLE,
            swizzle_bits([0, 1, 2, 3]),
            0,
        ),
        v[0].to_bits(),
        v[1].to_bits(),
        v[2].to_bits(),
        v[3].to_bits(),
    ]
}

fn build_gs_point_to_triangle_solid_red_in_o2_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive point
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   dcl_output o0.xyzw
    //   dcl_output o2.xyzw
    //
    //   mov o2, l(1,0,0,1)
    //   mov o0, l(-0.5, -0.5, 0, 1); emit
    //   mov o0, l( 0.5, -0.5, 0, 1); emit
    //   mov o0, l( 0.0,  0.5, 0, 1); emit
    //   cut; ret
    let version_token = 0x0002_0040u32; // gs_4_0
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(1); // point
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // trianglestrip
    tokens.push(opcode_token(
        sm4_opcode::OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
        2,
    ));
    tokens.push(3);

    // dcl_output o0.xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_INPUT, 3));
    tokens.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 0, 0xF));
    // dcl_output o2.xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_INPUT, 3));
    tokens.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 2, 0xF));

    // mov o2, l(1,0,0,1)
    let mut inst = vec![0u32];
    inst.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 2, 0xF));
    inst.extend_from_slice(&imm_f32x4([1.0, 0.0, 0.0, 1.0]));
    inst[0] = opcode_token(sm4_opcode::OPCODE_MOV, inst.len() as u32);
    tokens.extend_from_slice(&inst);

    for pos in [
        [-0.5, -0.5, 0.0, 1.0],
        [0.5, -0.5, 0.0, 1.0],
        [0.0, 0.5, 0.0, 1.0],
    ] {
        let mut inst = vec![0u32];
        inst.extend_from_slice(&reg_dst(sm4_opcode::OPERAND_TYPE_OUTPUT, 0, 0xF));
        inst.extend_from_slice(&imm_f32x4(pos));
        inst[0] = opcode_token(sm4_opcode::OPCODE_MOV, inst.len() as u32);
        tokens.extend_from_slice(&inst);
        tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));
    }

    tokens.push(opcode_token(sm4_opcode::OPCODE_CUT, 1));
    tokens.push(opcode_token(sm4_opcode::OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    dxbc_test_utils::build_container_owned(&[(FourCC(*b"SHDR"), tokens_to_bytes(&tokens))])
}

fn build_ps_solid_green_dxbc() -> Vec<u8> {
    // ps_4_0: mov o0, l(0,1,0,1); ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_token = 0x01u32 | (8u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;

    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        zero,
        one,
        zero,
        one,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let shdr = tokens_to_bytes(&tokens);
    dxbc_test_utils::build_container_owned(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

fn rgba_at(pixels: &[u8], width: usize, x: usize, y: usize) -> &[u8] {
    let idx = (y * width + x) * 4;
    &pixels[idx..idx + 4]
}

#[test]
fn aerogpu_cmd_runtime_geometry_shader_emulation_renders_triangle() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_runtime_geometry_shader_emulation_renders_triangle"
        );

        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        if !rt.supports_compute() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires compute shaders, but this wgpu backend does not support compute",
            );
            return;
        }
        if !rt.supports_indirect_execution() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires indirect execution, but this wgpu backend does not support indirect draws",
            );
            return;
        }

        // Draw a single point near the top-right. Without GS emulation the point does not cover
        // the center pixel. With GS emulation, the GS emits a centered triangle.
        let vertex = VertexPos3Color4 {
            pos: [0.75, 0.75, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(vb_bytes.len(), 28);

        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 3;
        const GS: u32 = 4;
        const PS: u32 = 5;
        const IL: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(GS, DXBC_GS_POINT_TO_TRIANGLE)
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_solid_green_dxbc())
            .unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        rt.create_buffer(
            VB,
            vb_bytes.len() as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, vb_bytes).unwrap();

        let (width, height) = (64u32, 64u32);
        rt.create_texture2d(
            RT,
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RT);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(GS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: 28,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::PointList);
        // Disable face culling so the test does not depend on winding conventions.
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(1, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        // Center pixel should be shaded green from the emitted triangle.
        let w = width as usize;
        assert_eq!(
            rgba_at(&pixels, w, (width / 2) as usize, (height / 2) as usize),
            &[0, 255, 0, 255]
        );

        // Corner should remain at clear color (black).
        assert_eq!(rgba_at(&pixels, w, 0, 0), &[0, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_runtime_geometry_shader_emulation_draw_indexed_respects_index_buffer_offset() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_runtime_geometry_shader_emulation_draw_indexed_respects_index_buffer_offset"
        );

        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        if !rt.supports_compute() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires compute shaders, but this wgpu backend does not support compute",
            );
            return;
        }
        if !rt.supports_indirect_execution() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires indirect execution, but this wgpu backend does not support indirect draws",
            );
            return;
        }

        // Index buffer selects the *center* point via an IASetIndexBuffer byte offset. If the
        // runtime ignores the index buffer offset, it will instead use the off-screen point and the
        // center pixel will remain clear.
        let vertices = [
            VertexPos3Color4 {
                pos: [2.0, 2.0, 0.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [0.0, 0.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::cast_slice(&vertices);
        assert_eq!(vb_bytes.len(), 56);

        // Two packed u16 indices: [0, 1]. We set the IA offset to 2 bytes so the first read is the
        // second index (1).
        let indices: [u16; 2] = [0, 1];
        let ib_bytes = bytemuck::cast_slice(&indices);
        assert_eq!(ib_bytes.len(), 4);

        const VB: u32 = 10;
        const IB: u32 = 11;
        const RT: u32 = 12;
        const VS: u32 = 13;
        const GS: u32 = 14;
        const PS: u32 = 15;
        const IL: u32 = 16;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(GS, DXBC_GS_CUT).unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_PASSTHROUGH).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        rt.create_buffer(
            VB,
            vb_bytes.len() as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, vb_bytes).unwrap();

        rt.create_buffer(
            IB,
            ib_bytes.len() as u64,
            wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(IB, 0, ib_bytes).unwrap();

        let (width, height) = (64u32, 64u32);
        rt.create_texture2d(
            RT,
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RT);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(GS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: 28,
                offset: 0,
            }],
        );
        rt.set_index_buffer(Some(IndexBufferBinding {
            buffer: IB,
            format: wgpu::IndexFormat::Uint16,
            offset: 2,
        }));
        rt.set_primitive_topology(PrimitiveTopology::PointList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw_indexed(1, 1, 0, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        let w = width as usize;
        assert_eq!(
            rgba_at(&pixels, w, (width / 2) as usize, (height / 2) as usize),
            &[255, 0, 0, 255]
        );
        assert_eq!(rgba_at(&pixels, w, 0, 0), &[0, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_runtime_geometry_shader_emulation_supports_location2_varying() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_runtime_geometry_shader_emulation_supports_location2_varying"
        );

        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        if !rt.supports_compute() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires compute shaders, but this wgpu backend does not support compute",
            );
            return;
        }
        if !rt.supports_indirect_execution() {
            common::skip_or_panic(
                test_name,
                "geometry shader prepass requires indirect execution, but this wgpu backend does not support indirect draws",
            );
            return;
        }

        let vertex = VertexPos3Color4 {
            pos: [0.0, 0.0, 0.0],
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let vb_bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(vb_bytes.len(), 28);

        const VB: u32 = 101;
        const RT: u32 = 102;
        const VS: u32 = 103;
        const GS: u32 = 104;
        const PS: u32 = 105;
        const IL: u32 = 106;

        let gs_dxbc = build_gs_point_to_triangle_solid_red_in_o2_dxbc();
        let ps_dxbc = build_ps_passthrough_color_in_v2_dxbc();

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(GS, &gs_dxbc).unwrap();
        rt.create_shader_dxbc(PS, &ps_dxbc).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        rt.create_buffer(
            VB,
            vb_bytes.len() as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, vb_bytes).unwrap();

        let (width, height) = (64u32, 64u32);
        rt.create_texture2d(
            RT,
            width,
            height,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RT);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(GS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: 28,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::PointList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(1, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), (width * height * 4) as usize);

        let w = width as usize;
        assert_eq!(
            rgba_at(&pixels, w, (width / 2) as usize, (height / 2) as usize),
            &[255, 0, 0, 255]
        );
    });
}
