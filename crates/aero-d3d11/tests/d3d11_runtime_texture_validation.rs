mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{
    CmdWriter, DxgiFormat, Texture2dDesc, Texture2dUpdate, TextureUsage,
};

#[test]
fn d3d11_runtime_update_texture2d_rejects_out_of_range_mip_level() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_update_texture2d_rejects_out_of_range_mip_level"
        );
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        let mut w = CmdWriter::new();
        w.create_texture2d(
            1,
            Texture2dDesc {
                width: 4,
                height: 4,
                array_layers: 1,
                mip_level_count: 1,
                format: DxgiFormat::R8G8B8A8Unorm,
                usage: TextureUsage::COPY_DST,
            },
        );
        w.update_texture2d(
            1,
            Texture2dUpdate {
                mip_level: 1, // out of range
                array_layer: 0,
                width: 1,
                height: 1,
                bytes_per_row: 0,
                data: &[0u8; 4],
            },
        );

        let err = rt
            .execute(&w.finish())
            .expect_err("expected UpdateTexture2D with out-of-range mip_level to be rejected");
        assert!(
            err.to_string().contains("mip_level"),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn d3d11_runtime_create_texture_view_rejects_out_of_range_base_mip() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_create_texture_view_rejects_out_of_range_base_mip"
        );
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        let mut w = CmdWriter::new();
        w.create_texture2d(
            1,
            Texture2dDesc {
                width: 4,
                height: 4,
                array_layers: 1,
                mip_level_count: 1,
                format: DxgiFormat::R8G8B8A8Unorm,
                usage: TextureUsage::TEXTURE_BINDING,
            },
        );
        w.create_texture_view(
            2, // view_id
            1, // texture_id
            1, // base_mip_level (out of range)
            1, // mip_level_count
            0, // base_array_layer
            1, // array_layer_count
        );

        let err = rt.execute(&w.finish()).expect_err(
            "expected CreateTextureView with out-of-range base_mip_level to be rejected",
        );
        assert!(
            err.to_string().contains("base_mip_level"),
            "unexpected error: {err}"
        );
    });
}
