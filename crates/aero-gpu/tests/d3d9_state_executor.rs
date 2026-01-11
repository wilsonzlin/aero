use aero_gpu::command_processor_d3d9::{CommandProcessor, ProcessorConfig};
use aero_gpu::protocol_d3d9::{
    BufferUsage, ShaderStage, StreamEncoder, TextureFormat, TextureUsage, VertexAttributeWire,
    VertexFormat,
};

const DEVICE_ID: u32 = 1;
const CONTEXT_ID: u32 = 1;
const SWAPCHAIN_ID: u32 = 1;

// D3D9 render state IDs (subset).
const D3DRS_ALPHABLENDENABLE: u32 = 27;
const D3DRS_SRCBLEND: u32 = 19;
const D3DRS_DESTBLEND: u32 = 20;
const D3DRS_BLENDOP: u32 = 171;
const D3DRS_SCISSORTESTENABLE: u32 = 174;

// D3D9 blend factors/ops.
const D3DBLEND_SRCALPHA: u32 = 5;
const D3DBLEND_INVSRCALPHA: u32 = 6;
const D3DBLENDOP_ADD: u32 = 1;

// D3D9 sampler state IDs (subset).
const D3DSAMP_ADDRESSU: u32 = 1;
const D3DSAMP_ADDRESSV: u32 = 2;
const D3DSAMP_MAGFILTER: u32 = 5;
const D3DSAMP_MINFILTER: u32 = 6;
const D3DSAMP_MIPFILTER: u32 = 7;

// D3D9 address/filter enums.
const D3DTADDRESS_WRAP: u32 = 1;
const D3DTADDRESS_CLAMP: u32 = 3;
const D3DTEXF_NONE: u32 = 0;
const D3DTEXF_POINT: u32 = 1;

fn pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    [pixels[idx], pixels[idx + 1], pixels[idx + 2], pixels[idx + 3]]
}

fn run_and_readback(stream: StreamEncoder) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = stream.finish();
    let mut processor = CommandProcessor::new(ProcessorConfig { validation: true });
    let report = pollster::block_on(processor.process(&bytes));
    if !report.is_ok() {
        let msg = format!("{:?}", report.events);
        if msg.contains("wgpu adapter not found") {
            eprintln!("skipping D3D9 state tests: {msg}");
            return None;
        }
        panic!("unexpected processor events: {:?}", report.events);
    }

    let (width, height, pixels) =
        pollster::block_on(processor.readback_swapchain_rgba8(DEVICE_ID, SWAPCHAIN_ID))
            .expect("readback failed");
    Some((width, height, pixels))
}

fn base_stream(width: u32, height: u32) -> StreamEncoder {
    let mut stream = StreamEncoder::new();
    stream.device_create(DEVICE_ID);
    stream.context_create(DEVICE_ID, CONTEXT_ID);
    stream.swapchain_create(
        CONTEXT_ID,
        SWAPCHAIN_ID,
        width,
        height,
        TextureFormat::Rgba8Unorm,
    );
    stream.set_render_targets_swapchain(CONTEXT_ID, SWAPCHAIN_ID);
    stream
}

fn upload_fullscreen_quad(stream: &mut StreamEncoder, vb_id: u32, uv: [[f32; 2]; 6]) {
    let vertices: [[f32; 4]; 6] = [
        [-1.0, -1.0, uv[0][0], uv[0][1]],
        [-1.0, 1.0, uv[1][0], uv[1][1]],
        [1.0, -1.0, uv[2][0], uv[2][1]],
        [-1.0, 1.0, uv[3][0], uv[3][1]],
        [1.0, 1.0, uv[4][0], uv[4][1]],
        [1.0, -1.0, uv[5][0], uv[5][1]],
    ];

    let mut vb = Vec::with_capacity(vertices.len() * 16);
    for v in vertices {
        for f in v {
            vb.extend_from_slice(&f.to_le_bytes());
        }
    }

    stream.buffer_create(
        CONTEXT_ID,
        vb_id,
        vb.len() as u64,
        BufferUsage::Vertex as u32,
    );
    stream.buffer_update(CONTEXT_ID, vb_id, 0, &vb);
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
    stream.set_vertex_stream(CONTEXT_ID, 0, vb_id, 0, 16);
}

#[test]
fn d3d9_state_alpha_blend_srcalpha_invsrcalpha() {
    let mut stream = base_stream(64, 64);

    // Full-screen quad with UVs spanning [0, 1].
    upload_fullscreen_quad(
        &mut stream,
        1,
        [
            [0.0, 0.0],
            [0.0, 1.0],
            [1.0, 0.0],
            [0.0, 1.0],
            [1.0, 1.0],
            [1.0, 0.0],
        ],
    );

    // Texture: two texels, left alpha=0, right alpha=255 (both green).
    stream.texture_create(
        CONTEXT_ID,
        1,
        2,
        1,
        1,
        TextureFormat::Rgba8Unorm,
        TextureUsage::Sampled as u32,
    );
    stream.texture_update_full_mip(
        CONTEXT_ID,
        1,
        0,
        2,
        1,
        &[0, 255, 0, 0, 0, 255, 0, 255],
    );

    stream.set_texture(CONTEXT_ID, ShaderStage::Fragment, 0, 1);
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MINFILTER,
        D3DTEXF_POINT,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MAGFILTER,
        D3DTEXF_POINT,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MIPFILTER,
        D3DTEXF_NONE,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_ADDRESSU,
        D3DTADDRESS_CLAMP,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_ADDRESSV,
        D3DTADDRESS_CLAMP,
    );

    // Background: solid red.
    stream.set_shader_key(CONTEXT_ID, ShaderStage::Vertex, 1);
    stream.set_shader_key(CONTEXT_ID, ShaderStage::Fragment, 1);
    stream.set_render_state_u32(CONTEXT_ID, D3DRS_ALPHABLENDENABLE, 0);
    stream.set_constants_f32(CONTEXT_ID, ShaderStage::Fragment, 0, 1, &[1.0, 0.0, 0.0, 1.0]);
    stream.draw(CONTEXT_ID, 6, 0);

    // Overlay: textured green with alpha; enable blending.
    stream.set_shader_key(CONTEXT_ID, ShaderStage::Fragment, 2);
    stream.set_render_state_u32(CONTEXT_ID, D3DRS_ALPHABLENDENABLE, 1);
    stream.set_render_state_u32(CONTEXT_ID, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    stream.set_render_state_u32(CONTEXT_ID, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    stream.set_render_state_u32(CONTEXT_ID, D3DRS_BLENDOP, D3DBLENDOP_ADD);
    stream.draw(CONTEXT_ID, 6, 0);
    stream.present(CONTEXT_ID, SWAPCHAIN_ID);

    let Some((width, _height, pixels)) = run_and_readback(stream) else {
        return;
    };

    // Left side samples alpha=0 texel → should remain red after blending.
    assert_eq!(pixel_at(&pixels, width, 8, 32), [255, 0, 0, 255]);
    // Right side samples alpha=255 texel → should be green.
    assert_eq!(pixel_at(&pixels, width, 56, 32), [0, 255, 0, 255]);
}

#[test]
fn d3d9_state_scissor_rect_clips_draw() {
    let mut stream = base_stream(64, 64);

    upload_fullscreen_quad(
        &mut stream,
        1,
        [
            [0.0, 0.0],
            [0.0, 1.0],
            [1.0, 0.0],
            [0.0, 1.0],
            [1.0, 1.0],
            [1.0, 0.0],
        ],
    );

    // Two 1x1 textures: red background and green overlay.
    stream.texture_create(
        CONTEXT_ID,
        1,
        1,
        1,
        1,
        TextureFormat::Rgba8Unorm,
        TextureUsage::Sampled as u32,
    );
    stream.texture_update_full_mip(CONTEXT_ID, 1, 0, 1, 1, &[255, 0, 0, 255]);

    stream.texture_create(
        CONTEXT_ID,
        2,
        1,
        1,
        1,
        TextureFormat::Rgba8Unorm,
        TextureUsage::Sampled as u32,
    );
    stream.texture_update_full_mip(CONTEXT_ID, 2, 0, 1, 1, &[0, 255, 0, 255]);

    stream.set_shader_key(CONTEXT_ID, ShaderStage::Vertex, 1);
    stream.set_shader_key(CONTEXT_ID, ShaderStage::Fragment, 2);

    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MINFILTER,
        D3DTEXF_POINT,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MAGFILTER,
        D3DTEXF_POINT,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MIPFILTER,
        D3DTEXF_NONE,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_ADDRESSU,
        D3DTADDRESS_CLAMP,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_ADDRESSV,
        D3DTADDRESS_CLAMP,
    );

    // Background red.
    stream.set_texture(CONTEXT_ID, ShaderStage::Fragment, 0, 1);
    stream.draw(CONTEXT_ID, 6, 0);

    // Enable scissor and draw green in the left half only.
    stream.set_render_state_u32(CONTEXT_ID, D3DRS_SCISSORTESTENABLE, 1);
    stream.set_scissor_rect(CONTEXT_ID, 0, 0, 32, 64);
    stream.set_texture(CONTEXT_ID, ShaderStage::Fragment, 0, 2);
    stream.draw(CONTEXT_ID, 6, 0);
    stream.present(CONTEXT_ID, SWAPCHAIN_ID);

    let Some((width, _height, pixels)) = run_and_readback(stream) else {
        return;
    };

    assert_eq!(pixel_at(&pixels, width, 16, 32), [0, 255, 0, 255]);
    assert_eq!(pixel_at(&pixels, width, 48, 32), [255, 0, 0, 255]);
}

#[test]
fn d3d9_state_sampler_wrap_vs_clamp() {
    let mut stream = base_stream(64, 64);

    // Constant UV outside [0,1] to exercise address modes.
    upload_fullscreen_quad(
        &mut stream,
        1,
        [
            [1.1, 0.5],
            [1.1, 0.5],
            [1.1, 0.5],
            [1.1, 0.5],
            [1.1, 0.5],
            [1.1, 0.5],
        ],
    );

    // Texture: four texels, distinct colors.
    stream.texture_create(
        CONTEXT_ID,
        1,
        4,
        1,
        1,
        TextureFormat::Rgba8Unorm,
        TextureUsage::Sampled as u32,
    );
    stream.texture_update_full_mip(
        CONTEXT_ID,
        1,
        0,
        4,
        1,
        &[
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 0, 255, // yellow
        ],
    );

    stream.set_shader_key(CONTEXT_ID, ShaderStage::Vertex, 1);
    stream.set_shader_key(CONTEXT_ID, ShaderStage::Fragment, 2);
    stream.set_texture(CONTEXT_ID, ShaderStage::Fragment, 0, 1);

    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MINFILTER,
        D3DTEXF_POINT,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MAGFILTER,
        D3DTEXF_POINT,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_MIPFILTER,
        D3DTEXF_NONE,
    );
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_ADDRESSV,
        D3DTADDRESS_CLAMP,
    );

    // Left half: clamp, so u=1.1 should clamp to last texel (yellow).
    stream.set_render_state_u32(CONTEXT_ID, D3DRS_SCISSORTESTENABLE, 1);
    stream.set_scissor_rect(CONTEXT_ID, 0, 0, 32, 64);
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_ADDRESSU,
        D3DTADDRESS_CLAMP,
    );
    stream.draw(CONTEXT_ID, 6, 0);

    // Right half: wrap, so u=1.1 wraps to 0.1 (red).
    stream.set_scissor_rect(CONTEXT_ID, 32, 0, 32, 64);
    stream.set_sampler_state_u32(
        CONTEXT_ID,
        ShaderStage::Fragment,
        0,
        D3DSAMP_ADDRESSU,
        D3DTADDRESS_WRAP,
    );
    stream.draw(CONTEXT_ID, 6, 0);
    stream.present(CONTEXT_ID, SWAPCHAIN_ID);

    let Some((width, _height, pixels)) = run_and_readback(stream) else {
        return;
    };

    assert_eq!(pixel_at(&pixels, width, 16, 32), [255, 255, 0, 255]);
    assert_eq!(pixel_at(&pixels, width, 48, 32), [255, 0, 0, 255]);
}
