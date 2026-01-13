mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdSetPrimitiveTopology,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
    AerogpuPrimitiveTopology, AerogpuShaderStage, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const PS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/ps_passthrough.dxbc");

fn patch_first_set_primitive_topology(bytes: &mut [u8], topology: u32) {
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    let mut patched = false;
    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size == 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::SetPrimitiveTopology as u32 {
            let off = cursor + core::mem::offset_of!(AerogpuCmdSetPrimitiveTopology, topology);
            bytes[off..off + 4].copy_from_slice(&topology.to_le_bytes());
            patched = true;
            break;
        }

        cursor += size;
    }
    assert!(patched, "failed to patch SetPrimitiveTopology");
}

#[test]
fn aerogpu_cmd_geometry_shader_compute_prepass_smoke() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_geometry_shader_compute_prepass_smoke"
        );
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };
        if !exec.supports_compute() {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }
        if !exec.capabilities().supports_indirect_execution {
            common::skip_or_panic(module_path!(), "indirect unsupported");
            return;
        }

        if !common::require_gs_prepass_or_skip(&exec, test_name) {
            return;
        }

        const RT: u32 = 1;
        const VS: u32 = 2;
        const PS: u32 = 3;

        let mut writer = AerogpuCmdWriter::new();
        writer.create_texture2d(
            RT,
            AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            AerogpuFormat::R8G8B8A8Unorm as u32,
            8,
            8,
            1,
            1,
            0,
            0,
            0,
        );
        writer.set_render_targets(&[RT], 0);
        writer.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);
        writer.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, VS_PASSTHROUGH);
        writer.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, PS_PASSTHROUGH);
        // Bind a dummy GS handle in the legacy `BIND_SHADERS.reserved0` field to force the
        // compute-prepass path (the executor interprets this field as `gs`).
        writer.bind_shaders_with_gs(VS, 0xCAFE_BABE, PS, 0);
        // Emit a supported topology in the protocol stream, then patch it to a D3D11-only value
        // (patchlist) to ensure the compute-prepass path does not depend on WebGPU's topology
        // support.
        writer.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        writer.draw(3, 1, 0, 0);

        let mut stream = writer.finish();
        // Patch to D3D11 `D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST` (35) so the
        // primitive count remains 1 for a 3-vertex draw.
        patch_first_set_primitive_topology(&mut stream, 35);

        let mut guest_mem = VecGuestMemory::new(0);
        if let Err(err) = exec.execute_cmd_stream(&stream, None, &mut guest_mem) {
            if common::skip_if_compute_or_indirect_unsupported(module_path!(), &err) {
                return;
            }
            panic!("execute_cmd_stream failed: {err:#}");
        }
        exec.poll_wait();

        let (width, height) = exec.texture_size(RT).expect("render target should exist");
        assert_eq!((width, height), (8, 8));

        let pixels = exec
            .read_texture_rgba8(RT)
            .await
            .expect("readback should succeed");
        let idx = ((height as usize / 2) * width as usize + (width as usize / 2)) * 4;
        let center = &pixels[idx..idx + 4];
        assert_eq!(center, &[255, 0, 0, 255]);
    });
}
