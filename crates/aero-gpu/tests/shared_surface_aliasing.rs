mod common;

use aero_gpu::command_processor_d3d9::{CommandProcessor, ProcessorConfig, ProcessorEvent};
use aero_gpu::protocol_d3d9::{
    BufferUsage, RenderTarget, RenderTargetKind, ShaderStage, StreamEncoder, TextureCreateDesc,
    TextureFormat, TextureUsage, VertexAttributeWire, VertexFormat,
};

fn ensure_xdg_runtime_dir() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir =
                std::env::temp_dir().join(format!("aero-gpu-xdg-runtime-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create XDG_RUNTIME_DIR");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .expect("chmod XDG_RUNTIME_DIR");
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }
}

#[test]
fn shared_surface_import_export_aliases_texture_handles() {
    pollster::block_on(async {
        ensure_xdg_runtime_dir();

        const DEVICE_ID: u32 = 1;
        const CONTEXT_ID: u32 = 1;

        const TEX_ORIGINAL: u32 = 0x10;
        const TEX_ALIAS_A: u32 = 0x20;
        const TEX_ALIAS_B: u32 = 0x21;
        const VB_ID: u32 = 0x30;

        const TOKEN_A: u64 = 0x1122_3344_5566_7788;
        const TOKEN_B: u64 = 0x8877_6655_4433_2211;

        let mut processor = CommandProcessor::new(ProcessorConfig { validation: true });

        let mut stream = StreamEncoder::new();
        stream.device_create(DEVICE_ID);
        stream.context_create(DEVICE_ID, CONTEXT_ID);

        // Create a render-target texture, export it, then import it under two alias handles.
        stream.texture_create(
            CONTEXT_ID,
            TEX_ORIGINAL,
            TextureCreateDesc {
                width: 4,
                height: 4,
                mip_level_count: 1,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsage::RenderTarget as u32,
            },
        );
        stream.export_shared_surface(TEX_ORIGINAL, TOKEN_A);
        stream.import_shared_surface(TEX_ALIAS_A, TOKEN_A);

        // Re-export the alias to ensure EXPORT_SHARED_SURFACE resolves aliases to the underlying
        // texture handle.
        stream.export_shared_surface(TEX_ALIAS_A, TOKEN_B);
        stream.import_shared_surface(TEX_ALIAS_B, TOKEN_B);

        // Drop the original handle; aliases should keep the underlying texture alive.
        stream.texture_destroy(CONTEXT_ID, TEX_ORIGINAL);

        stream.set_render_targets(
            CONTEXT_ID,
            RenderTarget {
                kind: RenderTargetKind::Texture,
                id: TEX_ALIAS_B,
            },
            None,
        );
        stream.set_shader_key(CONTEXT_ID, ShaderStage::Vertex, 1);
        stream.set_shader_key(CONTEXT_ID, ShaderStage::Fragment, 1);
        stream.set_constants_f32(
            CONTEXT_ID,
            ShaderStage::Fragment,
            0,
            1,
            &[1.0, 0.0, 0.0, 1.0],
        );

        // Clockwise fullscreen triangle with UVs for the built-in vertex shader.
        let fullscreen_triangle: [f32; 12] = [
            // pos       uv
            -1.0, -1.0, 0.0, 0.0, //
            -1.0, 3.0, 0.0, 2.0, //
            3.0, -1.0, 2.0, 0.0, //
        ];
        stream.buffer_create(
            CONTEXT_ID,
            VB_ID,
            (fullscreen_triangle.len() * 4) as u64,
            BufferUsage::Vertex as u32,
        );
        stream.buffer_update(
            CONTEXT_ID,
            VB_ID,
            0,
            bytemuck::cast_slice(&fullscreen_triangle),
        );
        stream.set_vertex_declaration(
            CONTEXT_ID,
            16,
            &[
                VertexAttributeWire {
                    location: 0,
                    format: VertexFormat::Float32x2,
                    offset: 0,
                },
                VertexAttributeWire {
                    location: 1,
                    format: VertexFormat::Float32x2,
                    offset: 8,
                },
            ],
        );
        stream.set_vertex_stream(CONTEXT_ID, 0, VB_ID, 0, 16);
        stream.draw(CONTEXT_ID, 3, 0);

        // Flush the encoder so a subsequent readback sees the draw results.
        stream.fence_signal(CONTEXT_ID, 1, 1);

        let report = processor.process(&stream.finish()).await;
        if !report.is_ok() {
            let adapter_missing = report.events.iter().any(|event| match event {
                ProcessorEvent::Error { message, .. } => message.contains("adapter not found"),
                _ => false,
            });

            if adapter_missing {
                common::skip_or_panic(module_path!(), "wgpu adapter not found");
                return;
            }

            panic!("command processor failed: {report:?}");
        }

        let (_width, _height, rgba) = processor
            .readback_texture_rgba8(DEVICE_ID, TEX_ALIAS_B)
            .await
            .expect("readback texture");
        assert_eq!(rgba.len(), 4 * 4 * 4);
        let center = ((2 * 4) + 2) * 4;
        assert_eq!(&rgba[center..center + 4], &[255, 0, 0, 255]);

        // Destroy both aliases; this should drop the last references to the underlying texture.
        let mut teardown = StreamEncoder::new();
        teardown.texture_destroy(CONTEXT_ID, TEX_ALIAS_A);
        teardown.texture_destroy(CONTEXT_ID, TEX_ALIAS_B);
        teardown.context_destroy(CONTEXT_ID);
        teardown.device_destroy(DEVICE_ID);

        let teardown_report = processor.process(&teardown.finish()).await;
        assert!(
            teardown_report.is_ok(),
            "teardown failed: {teardown_report:?}"
        );
    });
}

#[test]
fn shared_surface_import_into_destroyed_original_handle_is_rejected() {
    pollster::block_on(async {
        ensure_xdg_runtime_dir();

        const DEVICE_ID: u32 = 1;
        const CONTEXT_ID: u32 = 1;

        const TEX_ORIGINAL: u32 = 0x10;
        const TEX_ALIAS: u32 = 0x20;

        const TOKEN: u64 = 0x0123_4567_89AB_CDEF;

        let mut processor = CommandProcessor::new(ProcessorConfig { validation: true });

        let mut stream = StreamEncoder::new();
        stream.device_create(DEVICE_ID);
        stream.context_create(DEVICE_ID, CONTEXT_ID);
        stream.texture_create(
            CONTEXT_ID,
            TEX_ORIGINAL,
            TextureCreateDesc {
                width: 4,
                height: 4,
                mip_level_count: 1,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsage::RenderTarget as u32,
            },
        );
        stream.export_shared_surface(TEX_ORIGINAL, TOKEN);
        stream.import_shared_surface(TEX_ALIAS, TOKEN);

        // Destroy the original handle; alias keeps the underlying texture alive.
        stream.texture_destroy(CONTEXT_ID, TEX_ORIGINAL);

        // Buggy guest behavior: attempt to re-import into the destroyed original handle value.
        stream.import_shared_surface(TEX_ORIGINAL, TOKEN);

        let report = processor.process(&stream.finish()).await;
        if report.is_ok() {
            panic!("expected import into destroyed original handle to fail validation");
        }

        let adapter_missing = report.events.iter().any(|event| match event {
            ProcessorEvent::Error { message, .. } => message.contains("adapter not found"),
            _ => false,
        });
        if adapter_missing {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }

        let msg = report
            .events
            .iter()
            .find_map(|event| match event {
                ProcessorEvent::Error { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .expect("expected an error event");
        assert!(
            msg.contains("still in use"),
            "expected reserved-handle reuse to be rejected, got: {msg}"
        );
    });
}
