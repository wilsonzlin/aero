mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::FourCC;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AerogpuShaderStage, AerogpuShaderStageEx, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

use aero_d3d11::sm4::opcode::{OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, OPCODE_LEN_SHIFT, OPCODE_RET};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

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
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
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

fn build_sm5_program_bytes(program_type: u32, body_tokens: &[u32]) -> Vec<u8> {
    let major = 5u32;
    let minor = 0u32;
    let version = (program_type << 16) | (major << 4) | minor;

    let mut tokens = Vec::with_capacity(2 + body_tokens.len());
    tokens.push(version);
    tokens.push(0); // declared length patched below
    tokens.extend_from_slice(body_tokens);
    tokens[1] = tokens.len() as u32;

    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn build_hs_dxbc_input_control_points(count: u32) -> Vec<u8> {
    // Hull shader program type = 3.
    let body = [
        opcode_token(OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, 2),
        count,
        opcode_token(OPCODE_RET, 1),
    ];
    let shex = build_sm5_program_bytes(3, &body);
    build_dxbc(&[(FOURCC_SHEX, shex)])
}

fn build_ds_dxbc_minimal() -> Vec<u8> {
    // Domain shader program type = 4.
    let body = [opcode_token(OPCODE_RET, 1)];
    let shex = build_sm5_program_bytes(4, &body);
    build_dxbc(&[(FOURCC_SHEX, shex)])
}

fn bind_shaders_ex(bytes: &[u8], hs: u32, ds: u32) -> Vec<u8> {
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut patched = false;

    let mut out = Vec::with_capacity(bytes.len() + 12);
    out.extend_from_slice(&bytes[..ProtocolCmdStreamHeader::SIZE_BYTES]);

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size == 0 || cursor + size > bytes.len() {
            break;
        }
        let pkt = &bytes[cursor..cursor + size];

        if !patched && opcode == AerogpuCmdOpcode::BindShaders as u32 {
            let vs = u32::from_le_bytes(pkt[8..12].try_into().unwrap());
            let ps = u32::from_le_bytes(pkt[12..16].try_into().unwrap());
            let cs = u32::from_le_bytes(pkt[16..20].try_into().unwrap());
            let reserved0 = u32::from_le_bytes(pkt[20..24].try_into().unwrap());

            let mut new_pkt = Vec::with_capacity(36);
            new_pkt.extend_from_slice(&opcode.to_le_bytes());
            new_pkt.extend_from_slice(&(36u32).to_le_bytes());
            new_pkt.extend_from_slice(&vs.to_le_bytes());
            new_pkt.extend_from_slice(&ps.to_le_bytes());
            new_pkt.extend_from_slice(&cs.to_le_bytes());
            new_pkt.extend_from_slice(&reserved0.to_le_bytes());
            new_pkt.extend_from_slice(&0u32.to_le_bytes()); // gs
            new_pkt.extend_from_slice(&hs.to_le_bytes());
            new_pkt.extend_from_slice(&ds.to_le_bytes());

            out.extend_from_slice(&new_pkt);
            patched = true;
        } else {
            out.extend_from_slice(pkt);
        }

        cursor += size;
    }

    assert!(patched, "failed to patch BindShaders to extended form");

    // Update stream header size_bytes.
    let size_bytes = u32::try_from(out.len()).expect("stream too large for u32");
    out[8..12].copy_from_slice(&size_bytes.to_le_bytes());
    out
}

#[test]
fn aerogpu_cmd_rejects_patchlist_when_hs_inputcontrolpoints_mismatch() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const RT: u32 = 1;
        const VS: u32 = 2;
        const PS: u32 = 3;
        const HS: u32 = 4;
        const DS: u32 = 5;

        let hs_dxbc = build_hs_dxbc_input_control_points(3);
        let ds_dxbc = build_ds_dxbc_minimal();

        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            1,
            1,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        writer.create_shader_dxbc_ex(HS, AerogpuShaderStageEx::Hull, &hs_dxbc);
        writer.create_shader_dxbc_ex(DS, AerogpuShaderStageEx::Domain, &ds_dxbc);
        writer.bind_shaders(VS, PS, 0);
        writer.set_primitive_topology(AerogpuPrimitiveTopology::PatchList4);
        // PatchList4 groups 4 control points per patch; submit a full patch worth of vertices so
        // the draw is non-empty and reaches patchlist validation.
        writer.draw(4, 1, 0, 0);

        let stream = writer.finish();
        let stream = bind_shaders_ex(&stream, HS, DS);

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("PatchList4 draw should fail when HS expects 3 control points");

        let msg = err.to_string();
        assert!(
            msg.contains("control point") && msg.contains("3") && msg.contains("4"),
            "unexpected error: {msg:#}"
        );
    });
}
