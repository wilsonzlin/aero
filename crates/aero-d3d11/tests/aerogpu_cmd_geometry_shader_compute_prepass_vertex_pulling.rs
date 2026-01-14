mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdBindShaders, AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
};

const CMD_TRIANGLE_SM4: &[u8] = include_bytes!("fixtures/cmd_triangle_sm4.bin");

fn patch_first_bind_shaders_set_dummy_gs(bytes: &mut [u8], gs_handle: u32) {
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut patched = false;
    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size == 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::BindShaders as u32 {
            let off = cursor + core::mem::offset_of!(AerogpuCmdBindShaders, reserved0);
            bytes[off..off + 4].copy_from_slice(&gs_handle.to_le_bytes());
            patched = true;
            break;
        }

        cursor += size;
    }
    assert!(patched, "failed to patch BindShaders reserved0 as dummy GS");
}

/// Force the executor down the GS/HS/DS compute-prepass path while still using a real
/// input layout + vertex buffers.
///
/// This exercises the vertex pulling bind-group plumbing used by the eventual VS-as-compute
/// implementation.
#[test]
fn aerogpu_cmd_geometry_shader_compute_prepass_vertex_pulling_smoke() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        patch_first_bind_shaders_set_dummy_gs(&mut stream, 0xCAFE_BABE);

        let mut guest_mem = VecGuestMemory::new(0);
        let report = match exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            Ok(report) => report,
            Err(err) => {
                if common::skip_if_compute_or_indirect_unsupported(module_path!(), &err) {
                    return;
                }
                panic!("execute_cmd_stream failed: {err:#}");
            }
        };
        exec.poll_wait();

        let render_target = report
            .presents
            .last()
            .and_then(|p| p.presented_render_target)
            .expect("fixture should present a render target");

        let (width, height) = exec
            .texture_size(render_target)
            .expect("presented render target should exist");
        let pixels = exec
            .read_texture_rgba8(render_target)
            .await
            .expect("readback should succeed");
        assert_eq!(pixels.len(), width as usize * height as usize * 4);

        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}
