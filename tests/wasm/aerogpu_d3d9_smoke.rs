#![cfg(target_arch = "wasm32")]

use aero_gpu::command_processor_d3d9::{CommandProcessor, ProcessorConfig};
use aero_gpu::protocol_d3d9::{
    BufferUsage, IndexFormat, ShaderStage, StreamEncoder, TextureFormat, VertexAttributeWire,
    VertexFormat,
};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test(async)]
async fn aerogpu_d3d9_triangle_smoke() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let mut stream = StreamEncoder::new();
    stream.device_create(1);
    stream.context_create(1, 1);
    stream.swapchain_create(1, 1, 64, 64, TextureFormat::Rgba8Unorm);
    stream.set_render_targets_swapchain(1, 1);

    // Full-screen triangle with UVs for the built-in vertex shader.
    let vertices: [f32; 12] = [
        // pos       uv
        -1.0, -1.0, 0.0, 0.0, //
        3.0, -1.0, 2.0, 0.0, //
        -1.0, 3.0, 0.0, 2.0, //
    ];
    let mut vb = Vec::with_capacity(vertices.len() * 4);
    for v in vertices {
        vb.extend_from_slice(&v.to_le_bytes());
    }

    let indices: [u16; 3] = [0, 1, 2];
    let mut ib = Vec::with_capacity(indices.len() * 2);
    for idx in indices {
        ib.extend_from_slice(&idx.to_le_bytes());
    }

    stream.buffer_create(1, 1, vb.len() as u64, BufferUsage::Vertex as u32);
    stream.buffer_update(1, 1, 0, &vb);
    stream.buffer_create(1, 2, ib.len() as u64, BufferUsage::Index as u32);
    stream.buffer_update(1, 2, 0, &ib);

    stream.set_vertex_declaration(
        1,
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
    stream.set_vertex_stream(1, 0, 1, 0, 16);
    stream.set_index_buffer(1, 2, 0, IndexFormat::U16);

    stream.set_shader_key(1, ShaderStage::Vertex, 1);
    stream.set_shader_key(1, ShaderStage::Fragment, 1);
    stream.set_constants_f32(1, ShaderStage::Fragment, 0, 1, &[1.0, 0.0, 0.0, 1.0]);

    stream.draw_indexed(1, 3, 0, 0);
    stream.present(1, 1);

    let bytes = stream.finish();

    let mut processor = CommandProcessor::new(ProcessorConfig { validation: true });
    let report = processor.process(&bytes).await;
    assert!(
        report.is_ok(),
        "unexpected processor events: {:?}",
        report.events
    );

    let (width, height, pixels) = processor
        .readback_swapchain_rgba8(1, 1)
        .await
        .expect("readback failed");
    assert_eq!((width, height), (64, 64));

    let mut expected = vec![0u8; (width * height * 4) as usize];
    for pixel in expected.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[255, 0, 0, 255]);
    }
    assert_eq!(pixels, expected);
}
