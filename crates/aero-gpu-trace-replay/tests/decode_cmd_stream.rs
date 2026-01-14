use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdOpcode, AerogpuPrimitiveTopology, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC,
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
    push_packet(&mut out, AerogpuCmdOpcode::CreateTextureView as u32, &payload);

    // DESTROY_TEXTURE_VIEW(view_handle=0x1000).
    let mut payload = Vec::new();
    push_u32_le(&mut payload, 0x1000);
    push_u32_le(&mut payload, 0); // reserved0
    assert_eq!(payload.len(), 8);
    push_packet(&mut out, AerogpuCmdOpcode::DestroyTextureView as u32, &payload);

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
    assert!(listing.contains("abi=1.3"));

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
    assert!(listing.contains("stage_ex=3"));

    // stage_ex-capable binding packets should also surface stage_ex tags.
    assert!(listing.contains("SetTexture"));
    assert!(listing.contains("texture=8738")); // 0x2222
    assert!(listing.contains("stage_ex=2")); // Geometry

    assert!(listing.contains("SetSamplers"));
    assert!(listing.contains("sampler0=13107")); // 0x3333
    assert!(listing.contains("stage_ex=3")); // Hull

    assert!(listing.contains("SetShaderConstantsF"));
    assert!(listing.contains("vec4_count=1"));
    assert!(listing.contains("stage_ex=4")); // Domain
    assert!(listing.contains("data_len=16"));
    assert!(listing.contains("data_prefix=0000803f000000400000404000008040"));

    // Texture view packets should decode their payload fields.
    let format = AerogpuFormat::R8G8B8A8Unorm as u32;
    let format_hex = format!("format=0x{format:08X}");
    assert!(listing.contains("CreateTextureView"), "{listing}");
    assert!(listing.contains("view_handle=4096"), "{listing}");
    assert!(listing.contains("texture_handle=8192"), "{listing}");
    assert!(listing.contains(&format_hex), "{listing}");
    assert!(listing.contains("DestroyTextureView"), "{listing}");
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

    let srv = find_packet("SetShaderResourceBuffers");
    assert_eq!(srv["decoded"]["shader_stage"], 2);
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
    assert_eq!(cbs["decoded"]["start_slot"], 1);
    assert_eq!(cbs["decoded"]["buffer_count"], 1);
    assert_eq!(cbs["decoded"]["cb0_buffer"], 9);
    assert_eq!(cbs["decoded"]["cb0_offset_bytes"], 64);
    assert_eq!(cbs["decoded"]["cb0_size_bytes"], 256);

    let create_shader = find_packet("CreateShaderDxbc");
    assert_eq!(create_shader["decoded"]["stage"], 2);
    assert_eq!(create_shader["decoded"]["stage_ex"], 3);

    let set_texture = find_packet("SetTexture");
    assert_eq!(set_texture["decoded"]["shader_stage"], 2);
    assert_eq!(set_texture["decoded"]["texture"], 0x2222);
    assert_eq!(set_texture["decoded"]["stage_ex"], 2);

    let set_samplers = find_packet("SetSamplers");
    assert_eq!(set_samplers["decoded"]["shader_stage"], 2);
    assert_eq!(set_samplers["decoded"]["sampler_count"], 1);
    assert_eq!(set_samplers["decoded"]["sampler0"], 0x3333);
    assert_eq!(set_samplers["decoded"]["stage_ex"], 3);

    let set_consts = find_packet("SetShaderConstantsF");
    assert_eq!(set_consts["decoded"]["stage"], 2);
    assert_eq!(set_consts["decoded"]["vec4_count"], 1);
    assert_eq!(set_consts["decoded"]["stage_ex"], 4);
    assert_eq!(set_consts["decoded"]["data_len"], 16);
    assert_eq!(
        set_consts["decoded"]["data_prefix"],
        "0000803f000000400000404000008040"
    );

    let create_view = find_packet("CreateTextureView");
    assert_eq!(create_view["decoded"]["view_handle"], 0x1000);
    assert_eq!(create_view["decoded"]["texture_handle"], 0x2000);
    assert_eq!(
        create_view["decoded"]["format"],
        AerogpuFormat::R8G8B8A8Unorm as u32
    );
    assert_eq!(create_view["decoded"]["base_mip_level"], 0);
    assert_eq!(create_view["decoded"]["mip_level_count"], 1);
    assert_eq!(create_view["decoded"]["base_array_layer"], 0);
    assert_eq!(create_view["decoded"]["array_layer_count"], 1);

    let destroy_view = find_packet("DestroyTextureView");
    assert_eq!(destroy_view["decoded"]["view_handle"], 0x1000);
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
    assert_eq!(topo_packets[0]["decoded"]["topology_name"], "TriangleStripAdj");

    assert_eq!(topo_packets[1]["decoded"]["topology"], 64);
    assert_eq!(topo_packets[1]["decoded"]["topology_name"], "PatchList32");
}
