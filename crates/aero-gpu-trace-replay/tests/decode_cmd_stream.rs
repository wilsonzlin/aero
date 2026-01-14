use aero_dxbc::test_utils as dxbc_test_utils;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuIndexFormat, AerogpuPrimitiveTopology,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_PRESENT_FLAG_VSYNC,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn push_u32_le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64_le(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_packet(out: &mut Vec<u8>, opcode: u32, payload: &[u8]) {
    let size_bytes = 8u32 + payload.len() as u32;
    assert_eq!(size_bytes % 4, 0, "packet size must be 4-byte aligned");
    push_u32_le(out, opcode);
    push_u32_le(out, size_bytes);
    out.extend_from_slice(payload);
}

fn build_fixture_cmd_stream() -> Vec<u8> {
    let mut out = Vec::new();
    push_u32_le(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32_le(&mut out, AEROGPU_ABI_VERSION_U32);
    push_u32_le(&mut out, 0); // patched later
    push_u32_le(&mut out, 0); // flags
    push_u32_le(&mut out, 0); // reserved0
    push_u32_le(&mut out, 0); // reserved1
    assert_eq!(out.len(), 24);

    // CREATE_BUFFER(buffer_handle=1, usage_flags=0x1234, size_bytes=64, backing_alloc_id=2, backing_offset=16).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 0x1234);
    push_u64_le(&mut payload, 64);
    push_u32_le(&mut payload, 2);
    push_u32_le(&mut payload, 16);
    push_u64_le(&mut payload, 0); // reserved0
    assert_eq!(payload.len(), 32);
    push_packet(&mut out, AerogpuCmdOpcode::CreateBuffer as u32, &payload);

    // UPLOAD_RESOURCE(resource_handle=1, offset=0, size=4, data=DE AD BE EF).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 0); // reserved0
    push_u64_le(&mut payload, 0);
    push_u64_le(&mut payload, 4);
    payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(payload.len(), 28);
    push_packet(&mut out, AerogpuCmdOpcode::UploadResource as u32, &payload);

    // CLEAR(COLOR=red).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, AEROGPU_CLEAR_COLOR);
    push_u32_le(&mut payload, 1.0f32.to_bits());
    push_u32_le(&mut payload, 0.0f32.to_bits());
    push_u32_le(&mut payload, 0.0f32.to_bits());
    push_u32_le(&mut payload, 1.0f32.to_bits());
    push_u32_le(&mut payload, 1.0f32.to_bits()); // depth
    push_u32_le(&mut payload, 0); // stencil
    assert_eq!(payload.len(), 28);
    push_packet(&mut out, AerogpuCmdOpcode::Clear as u32, &payload);

    // DRAW(3 verts).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 3); // vertex_count
    push_u32_le(&mut payload, 1); // instance_count
    push_u32_le(&mut payload, 0); // first_vertex
    push_u32_le(&mut payload, 0); // first_instance
    assert_eq!(payload.len(), 16);
    push_packet(&mut out, AerogpuCmdOpcode::Draw as u32, &payload);

    // PRESENT(scanout_id=0, flags=VSYNC).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0);
    push_u32_le(&mut payload, AEROGPU_PRESENT_FLAG_VSYNC);
    assert_eq!(payload.len(), 8);
    push_packet(&mut out, AerogpuCmdOpcode::Present as u32, &payload);

    // Unknown opcode with 4-byte payload.
    push_packet(&mut out, 0xDEAD_BEEF, &[0, 1, 2, 3]);

    // DESTROY_RESOURCE(resource_handle=1).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 0);
    assert_eq!(payload.len(), 8);
    push_packet(&mut out, AerogpuCmdOpcode::DestroyResource as u32, &payload);

    // SET_SHADER_RESOURCE_BUFFERS(shader_stage=2, start_slot=0, buffer_count=1, stage_ex=0, binding0={buffer=7, offset=32, size=64}).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // shader_stage=Compute
    push_u32_le(&mut payload, 0); // start_slot
    push_u32_le(&mut payload, 1); // buffer_count
    push_u32_le(&mut payload, 0); // reserved0 / stage_ex
    push_u32_le(&mut payload, 7); // binding[0].buffer
    push_u32_le(&mut payload, 32); // binding[0].offset_bytes
    push_u32_le(&mut payload, 64); // binding[0].size_bytes
    push_u32_le(&mut payload, 0); // binding[0].reserved0
    assert_eq!(payload.len(), 32);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetShaderResourceBuffers as u32,
        &payload,
    );

    // SET_UNORDERED_ACCESS_BUFFERS(shader_stage=2, start_slot=0, uav_count=1, stage_ex=0, binding0={buffer=8, offset=0, size=16, initial_count=123}).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // shader_stage=Compute
    push_u32_le(&mut payload, 0); // start_slot
    push_u32_le(&mut payload, 1); // uav_count
    push_u32_le(&mut payload, 0); // reserved0 / stage_ex
    push_u32_le(&mut payload, 8); // binding[0].buffer
    push_u32_le(&mut payload, 0); // binding[0].offset_bytes
    push_u32_le(&mut payload, 16); // binding[0].size_bytes
    push_u32_le(&mut payload, 123); // binding[0].initial_count
    assert_eq!(payload.len(), 32);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32,
        &payload,
    );

    // DISPATCH(1,2,3).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1); // group_count_x
    push_u32_le(&mut payload, 2); // group_count_y
    push_u32_le(&mut payload, 3); // group_count_z
    push_u32_le(&mut payload, 0); // reserved0
    assert_eq!(payload.len(), 16);
    push_packet(&mut out, AerogpuCmdOpcode::Dispatch as u32, &payload);

    // SET_CONSTANT_BUFFERS(shader_stage=2, start_slot=1, buffer_count=1, stage_ex=0, binding0={buffer=9, offset=64, size=256}).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // shader_stage=Compute
    push_u32_le(&mut payload, 1); // start_slot
    push_u32_le(&mut payload, 1); // buffer_count
    push_u32_le(&mut payload, 0); // reserved0 / stage_ex
    push_u32_le(&mut payload, 9); // binding[0].buffer
    push_u32_le(&mut payload, 64); // binding[0].offset_bytes
    push_u32_le(&mut payload, 256); // binding[0].size_bytes
    push_u32_le(&mut payload, 0); // binding[0].reserved0
    assert_eq!(payload.len(), 32);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetConstantBuffers as u32,
        &payload,
    );

    // CREATE_SHADER_DXBC(shader_handle=0x1234, stage=Compute, stage_ex=Hull, dxbc="DXBC"+3 bytes + padding).
    //
    // `stage_ex` is encoded via `reserved0` when `stage == Compute`.
    let dxbc: [u8; 7] = [0x44, 0x58, 0x42, 0x43, 0x01, 0x02, 0x03]; // "DXBC" + payload bytes
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x1234); // shader_handle
    push_u32_le(&mut payload, 2); // stage=Compute
    push_u32_le(&mut payload, dxbc.len() as u32);
    push_u32_le(&mut payload, 3); // reserved0 / stage_ex = Hull (DXBC program type)
    payload.extend_from_slice(&dxbc);
    while payload.len() % 4 != 0 {
        payload.push(0);
    }
    push_packet(
        &mut out,
        AerogpuCmdOpcode::CreateShaderDxbc as u32,
        &payload,
    );

    // SET_TEXTURE(shader_stage=Compute, slot=0, texture=0x2222, stage_ex=Geometry (2)).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // shader_stage=Compute
    push_u32_le(&mut payload, 0); // slot
    push_u32_le(&mut payload, 0x2222); // texture handle
    push_u32_le(&mut payload, 2); // reserved0 / stage_ex = Geometry
    assert_eq!(payload.len(), 16);
    push_packet(&mut out, AerogpuCmdOpcode::SetTexture as u32, &payload);

    // SET_SAMPLERS(shader_stage=Compute, start_slot=0, sampler_count=1, stage_ex=Hull (3), handles=[0x3333]).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // shader_stage=Compute
    push_u32_le(&mut payload, 0); // start_slot
    push_u32_le(&mut payload, 1); // sampler_count
    push_u32_le(&mut payload, 3); // reserved0 / stage_ex = Hull
    push_u32_le(&mut payload, 0x3333); // sampler0
    assert_eq!(payload.len(), 20);
    push_packet(&mut out, AerogpuCmdOpcode::SetSamplers as u32, &payload);

    // SET_SHADER_CONSTANTS_F(stage=Compute, start_register=0, vec4_count=1, stage_ex=Domain (4), values=[1,2,3,4]).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // stage=Compute
    push_u32_le(&mut payload, 0); // start_register
    push_u32_le(&mut payload, 1); // vec4_count
    push_u32_le(&mut payload, 4); // reserved0 / stage_ex = Domain
    for f in [1.0f32, 2.0, 3.0, 4.0] {
        push_u32_le(&mut payload, f.to_bits());
    }
    assert_eq!(payload.len(), 32);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetShaderConstantsF as u32,
        &payload,
    );

    // CREATE_TEXTURE2D(texture_handle=0x2000, format=R8G8B8A8_UNORM).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x2000); // texture_handle
    push_u32_le(&mut payload, 0); // usage_flags
    push_u32_le(&mut payload, AerogpuFormat::R8G8B8A8Unorm as u32);
    push_u32_le(&mut payload, 4); // width
    push_u32_le(&mut payload, 4); // height
    push_u32_le(&mut payload, 1); // mip_levels
    push_u32_le(&mut payload, 1); // array_layers
    push_u32_le(&mut payload, 16); // row_pitch_bytes
    push_u32_le(&mut payload, 0); // backing_alloc_id
    push_u32_le(&mut payload, 0); // backing_offset_bytes
    push_u64_le(&mut payload, 0); // reserved0
    assert_eq!(payload.len(), 48);
    push_packet(&mut out, AerogpuCmdOpcode::CreateTexture2d as u32, &payload);

    // CREATE_TEXTURE_VIEW(view_handle=0x1000, texture_handle=0x2000, format=R8G8B8A8_UNORM, mip 0..1, layer 0..1).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x1000);
    push_u32_le(&mut payload, 0x2000);
    push_u32_le(&mut payload, AerogpuFormat::R8G8B8A8Unorm as u32);
    push_u32_le(&mut payload, 0); // base_mip_level
    push_u32_le(&mut payload, 1); // mip_level_count
    push_u32_le(&mut payload, 0); // base_array_layer
    push_u32_le(&mut payload, 1); // array_layer_count
    push_u64_le(&mut payload, 0); // reserved0
    assert_eq!(payload.len(), 36);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::CreateTextureView as u32,
        &payload,
    );

    // DESTROY_TEXTURE_VIEW(view_handle=0x1000).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x1000);
    push_u32_le(&mut payload, 0); // reserved0
    assert_eq!(payload.len(), 8);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::DestroyTextureView as u32,
        &payload,
    );

    // SET_SHADER_CONSTANTS_I(stage=Compute, start_register=0, vec4_count=1, stage_ex=Hull (3), values=[1,2,3,4]).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // stage=Compute
    push_u32_le(&mut payload, 0); // start_register
    push_u32_le(&mut payload, 1); // vec4_count
    push_u32_le(&mut payload, 3); // reserved0 / stage_ex = Hull
    for i in [1u32, 2, 3, 4] {
        push_u32_le(&mut payload, i);
    }
    assert_eq!(payload.len(), 32);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetShaderConstantsI as u32,
        &payload,
    );

    // SET_SHADER_CONSTANTS_B(stage=Compute, start_register=0, bool_count=2, stage_ex=Domain (4), values=[0,1]).
    //
    // Bool constants are encoded as scalar u32 values (0/1), one per bool register.
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // stage=Compute
    push_u32_le(&mut payload, 0); // start_register
    push_u32_le(&mut payload, 2); // bool_count
    push_u32_le(&mut payload, 4); // reserved0 / stage_ex = Domain
    push_u32_le(&mut payload, 0);
    push_u32_le(&mut payload, 1);
    assert_eq!(payload.len(), 24);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetShaderConstantsB as u32,
        &payload,
    );

    // SET_INPUT_LAYOUT(input_layout_handle=0x9999).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x9999);
    push_u32_le(&mut payload, 0);
    assert_eq!(payload.len(), 8);
    push_packet(&mut out, AerogpuCmdOpcode::SetInputLayout as u32, &payload);

    // SET_VERTEX_BUFFERS(start_slot=0, buffer_count=1, binding0={buffer=0x4444, stride=16, offset=32}).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0); // start_slot
    push_u32_le(&mut payload, 1); // buffer_count
    push_u32_le(&mut payload, 0x4444); // binding[0].buffer
    push_u32_le(&mut payload, 16); // binding[0].stride_bytes
    push_u32_le(&mut payload, 32); // binding[0].offset_bytes
    push_u32_le(&mut payload, 0); // binding[0].reserved0
    assert_eq!(payload.len(), 24);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetVertexBuffers as u32,
        &payload,
    );

    // SET_INDEX_BUFFER(buffer=0x5555, format=1, offset_bytes=64).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x5555);
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 64);
    push_u32_le(&mut payload, 0);
    assert_eq!(payload.len(), 16);
    push_packet(&mut out, AerogpuCmdOpcode::SetIndexBuffer as u32, &payload);

    // SET_PRIMITIVE_TOPOLOGY(topology=4).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 4);
    push_u32_le(&mut payload, 0);
    assert_eq!(payload.len(), 8);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetPrimitiveTopology as u32,
        &payload,
    );

    // CREATE_SAMPLER(sampler_handle=0x6666, filter=1, address_u=2, address_v=3, address_w=4).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x6666);
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 2);
    push_u32_le(&mut payload, 3);
    push_u32_le(&mut payload, 4);
    assert_eq!(payload.len(), 20);
    push_packet(&mut out, AerogpuCmdOpcode::CreateSampler as u32, &payload);

    // SET_SAMPLER_STATE(shader_stage=Pixel, slot=0, state=5, value=0x7777).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 0);
    push_u32_le(&mut payload, 5);
    push_u32_le(&mut payload, 0x7777);
    assert_eq!(payload.len(), 16);
    push_packet(&mut out, AerogpuCmdOpcode::SetSamplerState as u32, &payload);

    // SET_RENDER_STATE(state=7, value=0x8888).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 7);
    push_u32_le(&mut payload, 0x8888);
    assert_eq!(payload.len(), 8);
    push_packet(&mut out, AerogpuCmdOpcode::SetRenderState as u32, &payload);

    // DESTROY_SAMPLER(sampler_handle=0x6666).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x6666);
    push_u32_le(&mut payload, 0);
    assert_eq!(payload.len(), 8);
    push_packet(&mut out, AerogpuCmdOpcode::DestroySampler as u32, &payload);

    // SET_BLEND_STATE(enable=1, src_factor=2, dst_factor=3, blend_op=4, color_write_mask=0x0F, sample_mask=0xFFFF_FFFF).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1); // enable
    push_u32_le(&mut payload, 2); // src_factor
    push_u32_le(&mut payload, 3); // dst_factor
    push_u32_le(&mut payload, 4); // blend_op
    push_u32_le(&mut payload, 0x0F); // color_write_mask + reserved0
    push_u32_le(&mut payload, 5); // src_factor_alpha
    push_u32_le(&mut payload, 6); // dst_factor_alpha
    push_u32_le(&mut payload, 7); // blend_op_alpha
    push_u32_le(&mut payload, 1.0f32.to_bits());
    push_u32_le(&mut payload, 0.5f32.to_bits());
    push_u32_le(&mut payload, 0.25f32.to_bits());
    push_u32_le(&mut payload, 0.0f32.to_bits());
    push_u32_le(&mut payload, 0xFFFF_FFFF); // sample_mask
    assert_eq!(payload.len(), 52);
    push_packet(&mut out, AerogpuCmdOpcode::SetBlendState as u32, &payload);

    // SET_DEPTH_STENCIL_STATE(depth_enable=1, depth_write_enable=0, depth_func=2, stencil_enable=1, read_mask=0xAA, write_mask=0x55).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1); // depth_enable
    push_u32_le(&mut payload, 0); // depth_write_enable
    push_u32_le(&mut payload, 2); // depth_func
    push_u32_le(&mut payload, 1); // stencil_enable
    push_u32_le(&mut payload, 0x0000_55AA); // stencil_read_mask/stencil_write_mask + reserved0
    assert_eq!(payload.len(), 20);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetDepthStencilState as u32,
        &payload,
    );

    // SET_RASTERIZER_STATE(fill_mode=1, cull_mode=2, front_ccw=1, scissor_enable=1, depth_bias=-1, flags=1).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1); // fill_mode
    push_u32_le(&mut payload, 2); // cull_mode
    push_u32_le(&mut payload, 1); // front_ccw
    push_u32_le(&mut payload, 1); // scissor_enable
    push_u32_le(&mut payload, (-1i32) as u32); // depth_bias
    push_u32_le(&mut payload, 1); // flags
    assert_eq!(payload.len(), 24);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::SetRasterizerState as u32,
        &payload,
    );

    // DESTROY_INPUT_LAYOUT(input_layout_handle=0x9999).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x9999);
    push_u32_le(&mut payload, 0);
    assert_eq!(payload.len(), 8);
    push_packet(
        &mut out,
        AerogpuCmdOpcode::DestroyInputLayout as u32,
        &payload,
    );

    // DESTROY_SHADER(shader_handle=0x1234).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x1234);
    push_u32_le(&mut payload, 0);
    assert_eq!(payload.len(), 8);
    push_packet(&mut out, AerogpuCmdOpcode::DestroyShader as u32, &payload);

    // COPY_TEXTURE2D(dst_texture=0xAAAA, src_texture=0xBBBB, dst=5,6, src=7,8, size=9x10, flags=2).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0xAAAA); // dst_texture
    push_u32_le(&mut payload, 0xBBBB); // src_texture
    push_u32_le(&mut payload, 1); // dst_mip_level
    push_u32_le(&mut payload, 2); // dst_array_layer
    push_u32_le(&mut payload, 3); // src_mip_level
    push_u32_le(&mut payload, 4); // src_array_layer
    push_u32_le(&mut payload, 5); // dst_x
    push_u32_le(&mut payload, 6); // dst_y
    push_u32_le(&mut payload, 7); // src_x
    push_u32_le(&mut payload, 8); // src_y
    push_u32_le(&mut payload, 9); // width
    push_u32_le(&mut payload, 10); // height
    push_u32_le(&mut payload, 2); // flags
    push_u32_le(&mut payload, 0); // reserved0
    assert_eq!(payload.len(), 56);
    push_packet(&mut out, AerogpuCmdOpcode::CopyTexture2d as u32, &payload);

    // DEBUG_MARKER("MARK").
    push_packet(&mut out, AerogpuCmdOpcode::DebugMarker as u32, b"MARK");

    // SET_SCISSOR(x=1, y=2, width=3, height=4).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 2);
    push_u32_le(&mut payload, 3);
    push_u32_le(&mut payload, 4);
    assert_eq!(payload.len(), 16);
    push_packet(&mut out, AerogpuCmdOpcode::SetScissor as u32, &payload);

    // BIND_SHADERS(vs=0x1111, ps=0x2222, cs=0x3333, ex={gs=0x4444, hs=0x5555, ds=0x6666}).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x1111); // vs
    push_u32_le(&mut payload, 0x2222); // ps
    push_u32_le(&mut payload, 0x3333); // cs
    push_u32_le(&mut payload, 0); // reserved0
    push_u32_le(&mut payload, 0x4444); // gs
    push_u32_le(&mut payload, 0x5555); // hs
    push_u32_le(&mut payload, 0x6666); // ds
    assert_eq!(payload.len(), 28);
    push_packet(&mut out, AerogpuCmdOpcode::BindShaders as u32, &payload);

    // Patch header.size_bytes.
    let size_bytes = out.len() as u32;
    out[8..12].copy_from_slice(&size_bytes.to_le_bytes());
    out
}

#[test]
fn decodes_cmd_stream_dump_to_stable_listing() {
    let bytes = build_fixture_cmd_stream();
    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed in non-strict mode");

    // Header line.
    assert!(listing.contains("header magic=0x444D4341"));
    let abi_major = (AEROGPU_ABI_VERSION_U32 >> 16) as u16;
    let abi_minor = (AEROGPU_ABI_VERSION_U32 & 0xFFFF) as u16;
    let expected_abi = format!("abi={abi_major}.{abi_minor}");
    assert!(
        listing.contains(&expected_abi),
        "listing missing {expected_abi}: {listing}"
    );

    // Packet listing includes offsets, opcode names, and packet sizes.
    assert!(listing.contains("0x00000018 CreateBuffer size_bytes=40"));
    assert!(listing.contains("buffer_handle=1"));
    assert!(listing.contains("usage_flags=0x00001234"));
    assert!(listing.contains("buffer_size_bytes=64"));

    assert!(listing.contains("0x00000040 UploadResource size_bytes=36"));
    assert!(listing.contains("data_len=4"));
    assert!(listing.contains("data_prefix=deadbeef"));

    assert!(listing.contains("0x000000A0 Present size_bytes=16"));
    assert!(listing.contains("flags=0x00000001"));

    // Unknown opcode is shown but does not fail by default.
    assert!(listing.contains("0x000000B0 Unknown"));
    assert!(listing.contains("opcode_id=0xDEADBEEF"));

    // Decoder continues after unknown opcodes.
    assert!(listing.contains("0x000000BC DestroyResource size_bytes=16"));

    // New ABI opcodes should also decode their fields.
    assert!(listing.contains("0x000000CC SetShaderResourceBuffers size_bytes=40"));
    assert!(listing.contains("shader_stage=2"));
    assert!(listing.contains("shader_stage_name=Compute"));
    assert!(listing.contains("buffer_count=1"));
    assert!(listing.contains("srv0_buffer=7"));

    assert!(listing.contains("0x000000F4 SetUnorderedAccessBuffers size_bytes=40"));
    assert!(listing.contains("uav_count=1"));
    assert!(listing.contains("uav0_buffer=8"));
    assert!(listing.contains("uav0_initial_count=123"));

    assert!(listing.contains("0x0000011C Dispatch size_bytes=24"));
    assert!(listing.contains("group_count_x=1"));
    assert!(listing.contains("group_count_y=2"));
    assert!(listing.contains("group_count_z=3"));

    assert!(listing.contains("0x00000134 SetConstantBuffers size_bytes=40"));
    assert!(listing.contains("start_slot=1"));
    assert!(listing.contains("cb0_buffer=9"));
    assert!(listing.contains("cb0_offset_bytes=64"));
    assert!(listing.contains("cb0_size_bytes=256"));

    // CREATE_SHADER_DXBC should surface its `stage_ex` tag when present.
    assert!(listing.contains("CreateShaderDxbc"));
    assert!(listing.contains("stage_name=Compute"));
    assert!(listing.contains("stage_ex=3"));
    assert!(listing.contains("stage_ex_name=Hull"));

    // stage_ex-capable binding packets should also surface stage_ex tags.
    assert!(listing.contains("SetTexture"));
    assert!(listing.contains("texture=8738")); // 0x2222
    assert!(listing.contains("stage_ex=2")); // Geometry
    assert!(listing.contains("stage_ex_name=Geometry"));

    assert!(listing.contains("SetSamplers"));
    assert!(listing.contains("sampler0=13107")); // 0x3333
    assert!(listing.contains("stage_ex=3")); // Hull
    assert!(listing.contains("stage_ex_name=Hull"));

    assert!(listing.contains("SetShaderConstantsF"));
    assert!(listing.contains("vec4_count=1"));
    assert!(listing.contains("stage_ex=4")); // Domain
    assert!(listing.contains("stage_ex_name=Domain"));
    assert!(listing.contains("data_len=16"));
    assert!(listing.contains("data_prefix=0000803f000000400000404000008040"));

    assert!(listing.contains("SetShaderConstantsI"));
    assert!(listing.contains("data_len=16"));
    assert!(listing.contains("data_prefix=01000000020000000300000004000000"));

    assert!(listing.contains("SetShaderConstantsB"));
    assert!(listing.contains("bool_count=2"));
    assert!(listing.contains("data_len=8"));
    assert!(listing.contains("data_prefix=0000000001000000"));

    // Texture creation packets should decode their payload fields.
    assert!(listing.contains("CreateTexture2d"), "{listing}");
    assert!(listing.contains("texture_handle=8192"), "{listing}"); // 0x2000
    assert!(listing.contains("format_name=R8G8B8A8Unorm"), "{listing}");

    // Texture view packets should decode their payload fields.
    let format = AerogpuFormat::R8G8B8A8Unorm as u32;
    let format_hex = format!("format=0x{format:08X}");
    assert!(listing.contains("CreateTextureView"), "{listing}");
    assert!(listing.contains("view_handle=4096"), "{listing}");
    assert!(listing.contains("texture_handle=8192"), "{listing}");
    assert!(listing.contains(&format_hex), "{listing}");
    assert!(listing.contains("format_name=R8G8B8A8Unorm"), "{listing}");
    assert!(listing.contains("DestroyTextureView"), "{listing}");

    // Pipeline/state opcodes should decode their fields (not just payload_len).
    assert!(listing.contains("SetInputLayout"));
    assert!(listing.contains("input_layout_handle=39321")); // 0x9999

    assert!(listing.contains("SetVertexBuffers"));
    assert!(listing.contains("start_slot=0"));
    assert!(listing.contains("vb0_buffer=17476")); // 0x4444
    assert!(listing.contains("vb0_stride_bytes=16"));
    assert!(listing.contains("vb0_offset_bytes=32"));

    assert!(listing.contains("SetIndexBuffer"));
    assert!(listing.contains("buffer=21845")); // 0x5555
    assert!(listing.contains("format=1"));
    assert!(listing.contains("offset_bytes=64"));

    assert!(listing.contains("SetPrimitiveTopology"));
    assert!(listing.contains("topology=4"));

    assert!(listing.contains("CreateSampler"));
    assert!(listing.contains("sampler_handle=26214")); // 0x6666
    assert!(listing.contains("filter=1"));
    assert!(listing.contains("filter_name=Linear"));
    assert!(listing.contains("address_u_name=MirrorRepeat"));

    assert!(listing.contains("SetSamplerState"));
    assert!(listing.contains("shader_stage=1"));
    assert!(listing.contains("state=5"));
    assert!(listing.contains("state_name=D3DSAMP_MAGFILTER"));
    assert!(listing.contains("value=0x00007777"));

    assert!(listing.contains("SetRenderState"));
    assert!(listing.contains("state=7"));
    assert!(listing.contains("state_name=D3DRS_ZENABLE"));
    assert!(listing.contains("value=0x00008888"));

    assert!(listing.contains("DestroySampler"));
    assert!(listing.contains("sampler_handle=26214"));

    assert!(listing.contains("SetBlendState"));
    assert!(listing.contains("enable=1"));
    assert!(listing.contains("src_factor_name=SrcAlpha"), "{listing}");
    assert!(listing.contains("dst_factor_name=InvSrcAlpha"), "{listing}");
    assert!(listing.contains("blend_op_name=Max"), "{listing}");
    assert!(
        listing.contains("src_factor_alpha_name=InvDestAlpha"),
        "{listing}"
    );
    assert!(
        listing.contains("dst_factor_alpha_name=Constant"),
        "{listing}"
    );
    assert!(listing.contains("color_write_mask=0x0F"));
    assert!(listing.contains("sample_mask=0xFFFFFFFF"));

    assert!(listing.contains("SetDepthStencilState"));
    assert!(listing.contains("depth_func_name=Equal"), "{listing}");
    assert!(listing.contains("stencil_read_mask=0xAA"));
    assert!(listing.contains("stencil_write_mask=0x55"));

    assert!(listing.contains("SetRasterizerState"));
    assert!(listing.contains("fill_mode_name=Wireframe"), "{listing}");
    assert!(listing.contains("cull_mode_name=Back"), "{listing}");
    assert!(
        listing.contains("flags_names=DepthClipDisable"),
        "{listing}"
    );
    assert!(listing.contains("depth_bias=-1"));
    assert!(listing.contains("flags=0x00000001"));

    assert!(listing.contains("DestroyInputLayout"));
    assert!(listing.contains("DestroyShader"));

    // COPY_TEXTURE2D should decode region fields.
    assert!(listing.contains("CopyTexture2d"));
    assert!(listing.contains("dst_texture=43690")); // 0xAAAA
    assert!(listing.contains("src_texture=48059")); // 0xBBBB
    assert!(listing.contains("dst_xy=5,6"));
    assert!(listing.contains("src_xy=7,8"));
    assert!(listing.contains("size=9x10"));
    assert!(listing.contains("flags=0x00000002"));

    assert!(listing.contains("DebugMarker"));
    assert!(listing.contains("marker=\"MARK\""));

    assert!(listing.contains("SetScissor"));
    assert!(listing.contains("x=1 y=2 width=3 height=4"));

    assert!(listing.contains("BindShaders"));
    assert!(listing.contains("vs=4369")); // 0x1111
    assert!(listing.contains("ps=8738")); // 0x2222
    assert!(listing.contains("cs=13107")); // 0x3333
    assert!(listing.contains("gs=17476")); // 0x4444
    assert!(listing.contains("hs=21845")); // 0x5555
    assert!(listing.contains("ds=26214")); // 0x6666
}

#[test]
fn stage_ex_is_gated_by_cmd_stream_abi_minor_in_listings() {
    // Build a stream that contains non-zero reserved0/stage_ex values in several packets, then
    // force the stream ABI minor to 2 (pre-stage_ex). Decoders should treat those fields as
    // reserved/ignored so garbage does not get misinterpreted as an extended stage.
    let mut bytes = build_fixture_cmd_stream();
    bytes[4..8].copy_from_slice(&0x0001_0002u32.to_le_bytes()); // ABI 1.2

    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed");
    assert!(listing.contains("abi=1.2"));
    assert!(
        !listing.contains("stage_ex="),
        "stage_ex tags should be suppressed for ABI minor<3 (listing={listing})"
    );

    let json_listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&json_listing).expect("parse json listing");

    let records = v["records"].as_array().expect("records array");
    let create_shader = records
        .iter()
        .find(|r| r["type"] == "packet" && r["opcode"] == "CreateShaderDxbc")
        .expect("missing CreateShaderDxbc packet");
    assert!(
        create_shader["decoded"].get("stage_ex").is_none(),
        "stage_ex should be omitted from JSON decode for ABI minor<3"
    );
}

#[test]
fn dispatch_stage_ex_is_decoded_in_listings() {
    // `DISPATCH.reserved0` is repurposed as a `stage_ex` selector for extended-stage compute
    // execution. Ensure both the stable and JSON listings surface it when ABI permits, and gate it
    // for older captures.
    let mut bytes = Vec::new();
    push_u32_le(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
    push_u32_le(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32_le(&mut bytes, 0); // patched later
    push_u32_le(&mut bytes, 0); // flags
    push_u32_le(&mut bytes, 0); // reserved0
    push_u32_le(&mut bytes, 0); // reserved1
    assert_eq!(bytes.len(), AerogpuCmdStreamHeader::SIZE_BYTES);

    let mut payload = Vec::new();
    // DISPATCH(1,2,3) with stage_ex=Hull (3) encoded in reserved0.
    push_u32_le(&mut payload, 1);
    push_u32_le(&mut payload, 2);
    push_u32_le(&mut payload, 3);
    push_u32_le(&mut payload, 3); // reserved0 / stage_ex = Hull
    assert_eq!(payload.len(), 16);
    push_packet(&mut bytes, AerogpuCmdOpcode::Dispatch as u32, &payload);

    // Patch header.size_bytes.
    let size_bytes = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());

    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed");
    assert!(listing.contains("Dispatch"), "{listing}");
    assert!(listing.contains("stage_ex=3"), "{listing}");
    assert!(listing.contains("stage_ex_name=Hull"), "{listing}");

    let json_listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&json_listing).expect("parse json listing");
    let records = v["records"].as_array().expect("records array");
    let dispatch = records
        .iter()
        .find(|r| r["type"] == "packet" && r["opcode"] == "Dispatch")
        .expect("missing Dispatch packet");
    assert_eq!(dispatch["decoded"]["stage_ex"], 3);
    assert_eq!(dispatch["decoded"]["stage_ex_name"], "Hull");

    // ABI 1.2: stage_ex must be ignored and instead exposed as a non-zero reserved0 field.
    let mut legacy = bytes.clone();
    legacy[4..8].copy_from_slice(&0x0001_0002u32.to_le_bytes()); // ABI 1.2
    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&legacy, false)
        .expect("decode should succeed");
    assert!(listing.contains("abi=1.2"), "{listing}");
    assert!(
        !listing.contains("stage_ex="),
        "stage_ex tags should be suppressed for ABI minor<3 (listing={listing})"
    );

    let json_listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &legacy,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&json_listing).expect("parse json listing");
    let records = v["records"].as_array().expect("records array");
    let dispatch = records
        .iter()
        .find(|r| r["type"] == "packet" && r["opcode"] == "Dispatch")
        .expect("missing Dispatch packet");
    assert!(dispatch["decoded"].get("stage_ex").is_none());
    assert!(dispatch["decoded"].get("stage_ex_name").is_none());
    assert_eq!(dispatch["decoded"]["reserved0"], 3);
}

#[test]
fn shader_constants_i_b_stage_ex_is_decoded_in_listings() {
    // `SET_SHADER_CONSTANTS_{I,B}.reserved0` is repurposed as `stage_ex` (like the float variant).
    // Ensure the trace tooling surfaces it when ABI permits.
    let mut bytes = Vec::new();
    push_u32_le(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
    push_u32_le(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32_le(&mut bytes, 0); // patched later
    push_u32_le(&mut bytes, 0); // flags
    push_u32_le(&mut bytes, 0); // reserved0
    push_u32_le(&mut bytes, 0); // reserved1
    assert_eq!(bytes.len(), 24);

    // SET_SHADER_CONSTANTS_I(stage=Compute, start_register=0, vec4_count=1, stage_ex=Hull (3)).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // stage=Compute
    push_u32_le(&mut payload, 0); // start_register
    push_u32_le(&mut payload, 1); // vec4_count
    push_u32_le(&mut payload, 3); // reserved0 / stage_ex = Hull
    for i in [1u32, 2, 3, 4] {
        push_u32_le(&mut payload, i);
    }
    assert_eq!(payload.len(), 32);
    push_packet(
        &mut bytes,
        AerogpuCmdOpcode::SetShaderConstantsI as u32,
        &payload,
    );

    // SET_SHADER_CONSTANTS_B(stage=Compute, start_register=0, bool_count=1, stage_ex=Domain (4)).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 2); // stage=Compute
    push_u32_le(&mut payload, 0); // start_register
    push_u32_le(&mut payload, 1); // bool_count
    push_u32_le(&mut payload, 4); // reserved0 / stage_ex = Domain
    push_u32_le(&mut payload, 1);
    assert_eq!(payload.len(), 20);
    push_packet(
        &mut bytes,
        AerogpuCmdOpcode::SetShaderConstantsB as u32,
        &payload,
    );

    // Patch header.size_bytes.
    let size_bytes = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());

    // ABI 1.3+ stream: stage_ex must be shown.
    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed");
    assert!(listing.contains("SetShaderConstantsI"), "{listing}");
    assert!(listing.contains("stage_ex=3"), "{listing}");
    assert!(listing.contains("stage_ex_name=Hull"), "{listing}");
    assert!(listing.contains("SetShaderConstantsB"), "{listing}");
    assert!(listing.contains("stage_ex=4"), "{listing}");
    assert!(listing.contains("stage_ex_name=Domain"), "{listing}");

    let json_listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&json_listing).expect("parse json listing");
    let records = v["records"].as_array().expect("records array");
    let find_packet = |opcode: &str| {
        records
            .iter()
            .find(|r| r["type"] == "packet" && r["opcode"] == opcode)
            .unwrap_or_else(|| panic!("missing {opcode} packet"))
    };
    let sci = find_packet("SetShaderConstantsI");
    assert_eq!(sci["decoded"]["stage_ex"], 3);
    assert_eq!(sci["decoded"]["stage_ex_name"], "Hull");
    let scb = find_packet("SetShaderConstantsB");
    assert_eq!(scb["decoded"]["stage_ex"], 4);
    assert_eq!(scb["decoded"]["stage_ex_name"], "Domain");

    // ABI 1.2 stream: reserved0 must not be interpreted as stage_ex.
    let mut legacy = bytes.clone();
    legacy[4..8].copy_from_slice(&0x0001_0002u32.to_le_bytes()); // ABI 1.2
    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&legacy, false)
        .expect("decode should succeed");
    assert!(listing.contains("abi=1.2"), "{listing}");
    assert!(
        !listing.contains("stage_ex="),
        "stage_ex tags should be suppressed for ABI minor<3 (listing={listing})"
    );
    assert!(listing.contains("reserved0=0x00000003"), "{listing}");
    assert!(listing.contains("reserved0=0x00000004"), "{listing}");

    let json_listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &legacy,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&json_listing).expect("parse json listing");
    let records = v["records"].as_array().expect("records array");
    let find_packet = |opcode: &str| {
        records
            .iter()
            .find(|r| r["type"] == "packet" && r["opcode"] == opcode)
            .unwrap_or_else(|| panic!("missing {opcode} packet"))
    };
    let sci = find_packet("SetShaderConstantsI");
    assert!(sci["decoded"].get("stage_ex").is_none());
    assert_eq!(sci["decoded"]["reserved0"], 3);
    let scb = find_packet("SetShaderConstantsB");
    assert!(scb["decoded"].get("stage_ex").is_none());
    assert_eq!(scb["decoded"]["reserved0"], 4);
}

#[test]
fn stage_ex_vertex_program_type_is_reported_as_invalid() {
    // `stage_ex=1` matches the DXBC Vertex program type, but it is intentionally invalid in AeroGPU:
    // Vertex shaders must be encoded via the legacy `shader_stage = VERTEX` encoding.
    //
    // Ensure the trace tooling does not mislabel this as a valid stage selector.
    let mut bytes = Vec::new();
    push_u32_le(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
    push_u32_le(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32_le(&mut bytes, 0); // patched later
    push_u32_le(&mut bytes, 0); // flags
    push_u32_le(&mut bytes, 0); // reserved0
    push_u32_le(&mut bytes, 0); // reserved1
    assert_eq!(bytes.len(), 24);

    // CREATE_SHADER_DXBC(shader_handle=1, stage=Compute, stage_ex=1, dxbc=<empty DXBC container>).
    let dxbc = dxbc_test_utils::build_container(&[]);
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 1); // shader_handle
    push_u32_le(&mut payload, 2); // stage=Compute
    push_u32_le(&mut payload, dxbc.len() as u32); // dxbc_size_bytes
    push_u32_le(&mut payload, 1); // reserved0 / stage_ex = 1 (invalid Vertex program type)
    payload.extend_from_slice(&dxbc);
    while payload.len() % 4 != 0 {
        payload.push(0);
    }
    push_packet(
        &mut bytes,
        AerogpuCmdOpcode::CreateShaderDxbc as u32,
        &payload,
    );

    // Patch header.size_bytes.
    let size_bytes = bytes.len() as u32;
    bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());

    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed");
    assert!(listing.contains("stage_ex=1"), "{listing}");
    assert!(
        listing.contains("stage_ex_name=InvalidVertex"),
        "listing should label stage_ex=1 as invalid: {listing}"
    );

    let json_listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&json_listing).expect("parse json listing");

    let records = v["records"].as_array().expect("records array");
    let create_shader = records
        .iter()
        .find(|r| r["type"] == "packet" && r["opcode"] == "CreateShaderDxbc")
        .expect("missing CreateShaderDxbc packet");
    assert_eq!(create_shader["decoded"]["stage_ex"], 1);
    assert_eq!(create_shader["decoded"]["stage_ex_name"], "InvalidVertex");
}

#[test]
fn strict_mode_fails_on_unknown_opcode() {
    let bytes = build_fixture_cmd_stream();
    let err = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, true)
        .expect_err("strict mode should fail on unknown opcode");
    let msg = err.to_string();
    assert!(msg.contains("unknown opcode_id=0xDEADBEEF"));
    assert!(msg.contains("0x000000B0"));
}

#[test]
fn decodes_cmd_stream_built_by_writer() {
    let mut w = AerogpuCmdWriter::new();
    w.create_buffer(1, 0, 4, 0, 0);
    w.set_viewport(0.0, 0.0, 64.0, 64.0, 0.0, 1.0);
    w.draw(3, 1, 0, 0);
    w.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let bytes = w.finish();

    let listing =
        aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false).expect("decode cmd stream");

    assert!(listing.contains("CreateBuffer"), "{listing}");
    assert!(listing.contains("SetViewport"), "{listing}");
    assert!(listing.contains("Draw"), "{listing}");
    assert!(listing.contains("Present"), "{listing}");
}

#[test]
fn stable_listing_decodes_vertex_and_index_buffers() {
    let mut w = AerogpuCmdWriter::new();
    w.set_vertex_buffers(
        0,
        &[AerogpuVertexBufferBinding {
            buffer: 7,
            stride_bytes: 16,
            offset_bytes: 32,
            reserved0: 0,
        }],
    );
    w.set_index_buffer(8, AerogpuIndexFormat::Uint16, 12);
    let bytes = w.finish();

    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed in non-strict mode");

    assert!(
        listing.contains("SetVertexBuffers")
            && listing.contains(
                "start_slot=0 buffer_count=1 vb0_buffer=7 vb0_stride_bytes=16 vb0_offset_bytes=32"
            ),
        "{listing}"
    );
    assert!(
        listing.contains("SetIndexBuffer")
            && listing.contains("buffer=8 format=0 offset_bytes=12 format_name=Uint16"),
        "{listing}"
    );
}

#[test]
fn json_listing_decodes_new_opcodes() {
    let bytes = build_fixture_cmd_stream();
    let listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&listing).expect("parse json listing");

    let records = v["records"].as_array().expect("records array");
    let find_packet = |opcode: &str| {
        records
            .iter()
            .find(|r| r["type"] == "packet" && r["opcode"] == opcode)
            .unwrap_or_else(|| panic!("missing packet {opcode}"))
    };

    let upload = find_packet("UploadResource");
    assert_eq!(upload["decoded"]["data_len"], 4);
    assert_eq!(upload["decoded"]["data_prefix"], "deadbeef");

    let clear = find_packet("Clear");
    assert_eq!(clear["decoded"]["flags"], AEROGPU_CLEAR_COLOR);
    assert_eq!(
        clear["decoded"]["color_rgba"][0]
            .as_f64()
            .expect("color_rgba[0]"),
        1.0
    );
    assert_eq!(clear["decoded"]["depth"].as_f64().expect("depth"), 1.0);
    assert_eq!(clear["decoded"]["stencil"], 0);

    let srv = find_packet("SetShaderResourceBuffers");
    assert_eq!(srv["decoded"]["shader_stage"], 2);
    assert_eq!(srv["decoded"]["shader_stage_name"], "Compute");
    assert_eq!(srv["decoded"]["buffer_count"], 1);
    assert_eq!(srv["decoded"]["srv0_buffer"], 7);

    let uav = find_packet("SetUnorderedAccessBuffers");
    assert_eq!(uav["decoded"]["uav_count"], 1);
    assert_eq!(uav["decoded"]["uav0_buffer"], 8);
    assert_eq!(uav["decoded"]["uav0_initial_count"], 123);

    let dispatch = find_packet("Dispatch");
    assert_eq!(dispatch["decoded"]["group_count_x"], 1);
    assert_eq!(dispatch["decoded"]["group_count_y"], 2);
    assert_eq!(dispatch["decoded"]["group_count_z"], 3);

    let cbs = find_packet("SetConstantBuffers");
    assert_eq!(cbs["decoded"]["shader_stage"], 2);
    assert_eq!(cbs["decoded"]["shader_stage_name"], "Compute");
    assert_eq!(cbs["decoded"]["start_slot"], 1);
    assert_eq!(cbs["decoded"]["buffer_count"], 1);
    assert_eq!(cbs["decoded"]["cb0_buffer"], 9);
    assert_eq!(cbs["decoded"]["cb0_offset_bytes"], 64);
    assert_eq!(cbs["decoded"]["cb0_size_bytes"], 256);

    let create_shader = find_packet("CreateShaderDxbc");
    assert_eq!(create_shader["decoded"]["stage"], 2);
    assert_eq!(create_shader["decoded"]["stage_name"], "Compute");
    assert_eq!(create_shader["decoded"]["stage_ex"], 3);
    assert_eq!(create_shader["decoded"]["stage_ex_name"], "Hull");
    assert_eq!(create_shader["decoded"]["dxbc_prefix"], "44584243010203");

    let set_texture = find_packet("SetTexture");
    assert_eq!(set_texture["decoded"]["shader_stage"], 2);
    assert_eq!(set_texture["decoded"]["shader_stage_name"], "Compute");
    assert_eq!(set_texture["decoded"]["texture"], 0x2222);
    assert_eq!(set_texture["decoded"]["stage_ex"], 2);
    assert_eq!(set_texture["decoded"]["stage_ex_name"], "Geometry");

    let set_samplers = find_packet("SetSamplers");
    assert_eq!(set_samplers["decoded"]["shader_stage"], 2);
    assert_eq!(set_samplers["decoded"]["shader_stage_name"], "Compute");
    assert_eq!(set_samplers["decoded"]["sampler_count"], 1);
    assert_eq!(set_samplers["decoded"]["sampler0"], 0x3333);
    assert_eq!(set_samplers["decoded"]["stage_ex"], 3);
    assert_eq!(set_samplers["decoded"]["stage_ex_name"], "Hull");

    let set_consts = find_packet("SetShaderConstantsF");
    assert_eq!(set_consts["decoded"]["stage"], 2);
    assert_eq!(set_consts["decoded"]["stage_name"], "Compute");
    assert_eq!(set_consts["decoded"]["vec4_count"], 1);
    assert_eq!(set_consts["decoded"]["stage_ex"], 4);
    assert_eq!(set_consts["decoded"]["stage_ex_name"], "Domain");
    assert_eq!(set_consts["decoded"]["data_len"], 16);
    assert_eq!(
        set_consts["decoded"]["data_prefix"],
        "0000803f000000400000404000008040"
    );

    let set_consts_i = find_packet("SetShaderConstantsI");
    assert_eq!(set_consts_i["decoded"]["stage"], 2);
    assert_eq!(set_consts_i["decoded"]["stage_name"], "Compute");
    assert_eq!(set_consts_i["decoded"]["vec4_count"], 1);
    assert_eq!(set_consts_i["decoded"]["stage_ex"], 3);
    assert_eq!(set_consts_i["decoded"]["stage_ex_name"], "Hull");
    assert_eq!(set_consts_i["decoded"]["data_len"], 16);
    assert_eq!(
        set_consts_i["decoded"]["data_prefix"],
        "01000000020000000300000004000000"
    );

    let set_consts_b = find_packet("SetShaderConstantsB");
    assert_eq!(set_consts_b["decoded"]["stage"], 2);
    assert_eq!(set_consts_b["decoded"]["stage_name"], "Compute");
    assert_eq!(set_consts_b["decoded"]["bool_count"], 2);
    assert_eq!(set_consts_b["decoded"]["stage_ex"], 4);
    assert_eq!(set_consts_b["decoded"]["stage_ex_name"], "Domain");
    assert_eq!(set_consts_b["decoded"]["data_len"], 8);
    assert_eq!(set_consts_b["decoded"]["data_prefix"], "0000000001000000");

    let create_texture = find_packet("CreateTexture2d");
    assert_eq!(create_texture["decoded"]["texture_handle"], 0x2000);
    assert_eq!(
        create_texture["decoded"]["format"],
        AerogpuFormat::R8G8B8A8Unorm as u32
    );
    assert_eq!(create_texture["decoded"]["format_name"], "R8G8B8A8Unorm");
    assert_eq!(create_texture["decoded"]["width"], 4);
    assert_eq!(create_texture["decoded"]["height"], 4);
    assert_eq!(create_texture["decoded"]["row_pitch_bytes"], 16);

    let create_view = find_packet("CreateTextureView");
    assert_eq!(create_view["decoded"]["view_handle"], 0x1000);
    assert_eq!(create_view["decoded"]["texture_handle"], 0x2000);
    assert_eq!(
        create_view["decoded"]["format"],
        AerogpuFormat::R8G8B8A8Unorm as u32
    );
    assert_eq!(create_view["decoded"]["format_name"], "R8G8B8A8Unorm");
    assert_eq!(create_view["decoded"]["base_mip_level"], 0);
    assert_eq!(create_view["decoded"]["mip_level_count"], 1);
    assert_eq!(create_view["decoded"]["base_array_layer"], 0);
    assert_eq!(create_view["decoded"]["array_layer_count"], 1);

    let destroy_view = find_packet("DestroyTextureView");
    assert_eq!(destroy_view["decoded"]["view_handle"], 0x1000);

    let set_input_layout = find_packet("SetInputLayout");
    assert_eq!(set_input_layout["decoded"]["input_layout_handle"], 0x9999);

    let set_vertex_buffers = find_packet("SetVertexBuffers");
    assert_eq!(set_vertex_buffers["decoded"]["start_slot"], 0);
    assert_eq!(set_vertex_buffers["decoded"]["buffer_count"], 1);
    assert_eq!(set_vertex_buffers["decoded"]["vb0_buffer"], 0x4444);
    assert_eq!(set_vertex_buffers["decoded"]["vb0_stride_bytes"], 16);
    assert_eq!(set_vertex_buffers["decoded"]["vb0_offset_bytes"], 32);

    let set_index_buffer = find_packet("SetIndexBuffer");
    assert_eq!(set_index_buffer["decoded"]["buffer"], 0x5555);
    assert_eq!(set_index_buffer["decoded"]["format"], 1);
    assert_eq!(set_index_buffer["decoded"]["offset_bytes"], 64);

    let set_topology = find_packet("SetPrimitiveTopology");
    assert_eq!(set_topology["decoded"]["topology"], 4);

    let create_sampler = find_packet("CreateSampler");
    assert_eq!(create_sampler["decoded"]["sampler_handle"], 0x6666);
    assert_eq!(create_sampler["decoded"]["filter"], 1);
    assert_eq!(create_sampler["decoded"]["filter_name"], "Linear");
    assert_eq!(create_sampler["decoded"]["address_u"], 2);
    assert_eq!(create_sampler["decoded"]["address_u_name"], "MirrorRepeat");
    assert_eq!(create_sampler["decoded"]["address_v"], 3);
    assert_eq!(create_sampler["decoded"]["address_w"], 4);

    let sampler_state = find_packet("SetSamplerState");
    assert_eq!(sampler_state["decoded"]["shader_stage"], 1);
    assert_eq!(sampler_state["decoded"]["shader_stage_name"], "Pixel");
    assert_eq!(sampler_state["decoded"]["slot"], 0);
    assert_eq!(sampler_state["decoded"]["state"], 5);
    assert_eq!(sampler_state["decoded"]["state_name"], "D3DSAMP_MAGFILTER");
    assert_eq!(sampler_state["decoded"]["value"], 0x7777);

    let render_state = find_packet("SetRenderState");
    assert_eq!(render_state["decoded"]["state"], 7);
    assert_eq!(render_state["decoded"]["state_name"], "D3DRS_ZENABLE");
    assert_eq!(render_state["decoded"]["value"], 0x8888);

    let destroy_sampler = find_packet("DestroySampler");
    assert_eq!(destroy_sampler["decoded"]["sampler_handle"], 0x6666);

    let blend = find_packet("SetBlendState");
    assert_eq!(blend["decoded"]["enable"], 1);
    assert_eq!(blend["decoded"]["src_factor"], 2);
    assert_eq!(blend["decoded"]["src_factor_name"], "SrcAlpha");
    assert_eq!(blend["decoded"]["dst_factor"], 3);
    assert_eq!(blend["decoded"]["dst_factor_name"], "InvSrcAlpha");
    assert_eq!(blend["decoded"]["blend_op"], 4);
    assert_eq!(blend["decoded"]["blend_op_name"], "Max");
    assert_eq!(blend["decoded"]["color_write_mask"], 0x0F);
    assert_eq!(blend["decoded"]["src_factor_alpha"], 5);
    assert_eq!(blend["decoded"]["src_factor_alpha_name"], "InvDestAlpha");
    assert_eq!(blend["decoded"]["dst_factor_alpha"], 6);
    assert_eq!(blend["decoded"]["dst_factor_alpha_name"], "Constant");
    assert_eq!(blend["decoded"]["blend_op_alpha"], 7);
    assert_eq!(blend["decoded"]["sample_mask"], 0xFFFF_FFFFu64);
    assert_eq!(
        blend["decoded"]["blend_constant_rgba"][0]
            .as_f64()
            .expect("blend_constant_rgba[0]"),
        1.0
    );

    let depth_stencil = find_packet("SetDepthStencilState");
    assert_eq!(depth_stencil["decoded"]["depth_enable"], 1);
    assert_eq!(depth_stencil["decoded"]["depth_write_enable"], 0);
    assert_eq!(depth_stencil["decoded"]["depth_func"], 2);
    assert_eq!(depth_stencil["decoded"]["depth_func_name"], "Equal");
    assert_eq!(depth_stencil["decoded"]["stencil_read_mask"], 0xAA);
    assert_eq!(depth_stencil["decoded"]["stencil_write_mask"], 0x55);

    let raster = find_packet("SetRasterizerState");
    assert_eq!(raster["decoded"]["fill_mode"], 1);
    assert_eq!(raster["decoded"]["fill_mode_name"], "Wireframe");
    assert_eq!(raster["decoded"]["cull_mode"], 2);
    assert_eq!(raster["decoded"]["cull_mode_name"], "Back");
    assert_eq!(raster["decoded"]["depth_bias"], -1);
    assert_eq!(raster["decoded"]["flags"], 1);
    assert_eq!(raster["decoded"]["flags_names"], "DepthClipDisable");

    let destroy_input_layout = find_packet("DestroyInputLayout");
    assert_eq!(
        destroy_input_layout["decoded"]["input_layout_handle"],
        0x9999
    );

    let destroy_shader = find_packet("DestroyShader");
    assert_eq!(destroy_shader["decoded"]["shader_handle"], 0x1234);

    let copy_texture = find_packet("CopyTexture2d");
    assert_eq!(copy_texture["decoded"]["dst_texture"], 0xAAAA);
    assert_eq!(copy_texture["decoded"]["src_texture"], 0xBBBB);
    assert_eq!(copy_texture["decoded"]["dst_mip_level"], 1);
    assert_eq!(copy_texture["decoded"]["dst_array_layer"], 2);
    assert_eq!(copy_texture["decoded"]["src_mip_level"], 3);
    assert_eq!(copy_texture["decoded"]["src_array_layer"], 4);
    assert_eq!(copy_texture["decoded"]["dst_x"], 5);
    assert_eq!(copy_texture["decoded"]["dst_y"], 6);
    assert_eq!(copy_texture["decoded"]["src_x"], 7);
    assert_eq!(copy_texture["decoded"]["src_y"], 8);
    assert_eq!(copy_texture["decoded"]["width"], 9);
    assert_eq!(copy_texture["decoded"]["height"], 10);
    assert_eq!(copy_texture["decoded"]["flags"], 2);

    let marker = find_packet("DebugMarker");
    assert_eq!(marker["decoded"]["marker"], "MARK");

    let scissor = find_packet("SetScissor");
    assert_eq!(scissor["decoded"]["x"], 1);
    assert_eq!(scissor["decoded"]["y"], 2);
    assert_eq!(scissor["decoded"]["width"], 3);
    assert_eq!(scissor["decoded"]["height"], 4);

    let bind = find_packet("BindShaders");
    assert_eq!(bind["decoded"]["vs"], 0x1111);
    assert_eq!(bind["decoded"]["ps"], 0x2222);
    assert_eq!(bind["decoded"]["cs"], 0x3333);
    assert_eq!(bind["decoded"]["gs"], 0x4444);
    assert_eq!(bind["decoded"]["hs"], 0x5555);
    assert_eq!(bind["decoded"]["ds"], 0x6666);
}

#[test]
fn json_listing_decodes_index_buffer_format_name() {
    let mut w = AerogpuCmdWriter::new();
    w.set_index_buffer(8, AerogpuIndexFormat::Uint32, 4);
    let bytes = w.finish();

    let listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&listing).expect("parse json listing");

    let records = v["records"].as_array().expect("records array");
    let pkt = records
        .iter()
        .find(|r| r["type"] == "packet" && r["opcode"] == "SetIndexBuffer")
        .expect("missing SetIndexBuffer packet");

    assert_eq!(pkt["decoded"]["buffer"], 8);
    assert_eq!(pkt["decoded"]["format"], 1);
    assert_eq!(pkt["decoded"]["format_name"], "Uint32");
    assert_eq!(pkt["decoded"]["offset_bytes"], 4);
}

#[test]
fn json_listing_decodes_topology_names_for_adjacency_and_patchlists() {
    let mut w = AerogpuCmdWriter::new();
    w.set_primitive_topology(AerogpuPrimitiveTopology::TriangleStripAdj);
    w.set_primitive_topology(AerogpuPrimitiveTopology::PatchList32);
    let bytes = w.finish();

    let listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&listing).expect("parse json listing");

    let records = v["records"].as_array().expect("records array");
    let topo_packets: Vec<&serde_json::Value> = records
        .iter()
        .filter(|r| r["type"] == "packet" && r["opcode"] == "SetPrimitiveTopology")
        .collect();
    assert_eq!(topo_packets.len(), 2);

    assert_eq!(topo_packets[0]["decoded"]["topology"], 13);
    assert_eq!(
        topo_packets[0]["decoded"]["topology_name"],
        "TriangleStripAdj"
    );

    assert_eq!(topo_packets[1]["decoded"]["topology"], 64);
    assert_eq!(topo_packets[1]["decoded"]["topology_name"], "PatchList32");
}

#[test]
fn stable_listing_decodes_topology_names_for_adjacency_and_patchlists() {
    let mut w = AerogpuCmdWriter::new();
    w.set_primitive_topology(AerogpuPrimitiveTopology::TriangleStripAdj);
    w.set_primitive_topology(AerogpuPrimitiveTopology::PatchList32);
    let bytes = w.finish();

    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed in non-strict mode");

    assert!(listing.contains("SetPrimitiveTopology"), "{listing}");
    assert!(
        listing.contains("topology=13 topology_name=TriangleStripAdj"),
        "{listing}"
    );
    assert!(
        listing.contains("topology=64 topology_name=PatchList32"),
        "{listing}"
    );
}

#[test]
fn stage_ex_fields_are_gated_by_abi_minor() {
    // Build a fixture stream that contains stage_ex tags (via the modern ABI),
    // then downgrade the header ABI minor to ensure the decoders do not interpret
    // reserved fields as stage_ex for older captures.
    let mut bytes = build_fixture_cmd_stream();

    // Patch header ABI to 1.2 (stage_ex was introduced in 1.3).
    let abi_version = (1u32 << 16) | 2u32;
    bytes[4..8].copy_from_slice(&abi_version.to_le_bytes());

    let listing = aero_gpu_trace_replay::decode_cmd_stream_listing(&bytes, false)
        .expect("decode should succeed in non-strict mode");
    assert!(listing.contains("abi=1.2"), "{listing}");
    assert!(listing.contains("SetTexture"), "{listing}");
    assert!(
        !listing.contains("stage_ex="),
        "stage_ex should be ignored for ABI < 1.3:\n{listing}"
    );

    let json_listing = aero_gpu_trace_replay::cmd_stream_decode::render_cmd_stream_listing(
        &bytes,
        aero_gpu_trace_replay::cmd_stream_decode::CmdStreamListingFormat::Json,
    )
    .expect("render json listing");
    let v: serde_json::Value = serde_json::from_str(&json_listing).expect("parse json listing");
    let records = v["records"].as_array().expect("records array");
    assert!(!records.is_empty());
    for rec in records {
        if rec["type"] != "packet" {
            continue;
        }
        let decoded = rec["decoded"].as_object().expect("decoded object");
        assert!(
            !decoded.contains_key("stage_ex") && !decoded.contains_key("stage_ex_name"),
            "stage_ex should be gated by ABI minor; found keys in {rec}"
        );
    }
}
