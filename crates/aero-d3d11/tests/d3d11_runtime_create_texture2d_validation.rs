mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{CmdWriter, DxgiFormat, Texture2dDesc, TextureUsage};

#[test]
fn d3d11_runtime_create_texture2d_rejects_mip_levels_beyond_chain_length() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_create_texture2d_rejects_mip_levels_beyond_chain_length"
        );
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        // 4x4 textures only have 3 mip levels (4x4, 2x2, 1x1). Requesting 4 should be rejected
        // before it reaches wgpu validation.
        let mut w = CmdWriter::new();
        w.create_texture2d(
            1,
            Texture2dDesc {
                width: 4,
                height: 4,
                array_layers: 1,
                mip_level_count: 4, // invalid
                format: DxgiFormat::R8G8B8A8Unorm,
                usage: TextureUsage::TEXTURE_BINDING,
            },
        );

        let err = rt
            .execute(&w.finish())
            .expect_err("expected CreateTexture2D with too many mips to be rejected");
        assert!(
            err.to_string().contains("mip_level_count"),
            "unexpected error: {err}"
        );
    });
}

