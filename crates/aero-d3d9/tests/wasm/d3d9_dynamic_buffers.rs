#![cfg(target_arch = "wasm32")]

mod util;

use aero_d3d9::resources::*;
use bytemuck::{Pod, Zeroable};
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
}

const VERT_SHADER: &str = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(in.pos, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

#[wasm_bindgen_test(async)]
async fn d3d9_dynamic_vertex_buffer_discard_does_not_corrupt_in_flight_draws() {
    let mut rm = util::init_manager().await;
    rm.begin_frame();

    // Dynamic vertex buffer large enough for 6 vertices (two triangles).
    let vb_id = 1;
    let vb_size = (std::mem::size_of::<Vertex>() * 6) as u32;
    rm.create_vertex_buffer(
        vb_id,
        VertexBufferDesc {
            size_bytes: vb_size,
            pool: D3DPool::Default,
            usage: BufferUsageFlags::DYNAMIC,
        },
    )
    .unwrap();

    // Output render target.
    let device = rm.device();
    let out_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("out"),
        size: wgpu::Extent3d {
            width: 32,
            height: 32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let out_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("vb shader"),
        source: wgpu::ShaderSource::Wgsl(VERT_SHADER.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pl"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    let vb_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[vb_layout],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

    // Draw 1: left half, red quad.
    {
        let data = rm
            .lock_vertex_buffer(vb_id, 0, 0, LockFlags::DISCARD)
            .unwrap();
        let verts = [
            Vertex {
                pos: [-1.0, -1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [0.0, -1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, -1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [0.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        data.copy_from_slice(bytemuck::cast_slice(&verts));
        rm.unlock_vertex_buffer(vb_id).unwrap();
    }
    rm.encode_uploads(&mut encoder);
    let vb_buf_1 = std::sync::Arc::clone(rm.vertex_buffer(vb_id).unwrap().gpu_buffer());

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass1"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &out_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: true,
                },
            })],
            depth_stencil_attachment: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, vb_buf_1.slice(..));
        pass.draw(0..6, 0..1);
    }

    // Draw 2: right half, green quad. DISCARD should not overwrite vb_buf_1.
    {
        let data = rm
            .lock_vertex_buffer(vb_id, 0, 0, LockFlags::DISCARD)
            .unwrap();
        let verts = [
            Vertex {
                pos: [0.0, -1.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [1.0, -1.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [1.0, 1.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [0.0, -1.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [1.0, 1.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            Vertex {
                pos: [0.0, 1.0],
                color: [0.0, 1.0, 0.0, 1.0],
            },
        ];
        data.copy_from_slice(bytemuck::cast_slice(&verts));
        rm.unlock_vertex_buffer(vb_id).unwrap();
    }
    rm.encode_uploads(&mut encoder);
    let vb_buf_2 = std::sync::Arc::clone(rm.vertex_buffer(vb_id).unwrap().gpu_buffer());

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass2"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &out_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: true,
                },
            })],
            depth_stencil_attachment: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, vb_buf_2.slice(..));
        pass.draw(0..6, 0..1);
    }

    rm.submit(encoder);

    let data = util::read_texture_rgba8(rm.device(), rm.queue(), &out_tex, 32, 32).await;

    let sample = |x: usize, y: usize| {
        let off = (y * 32 + x) * 4;
        [data[off], data[off + 1], data[off + 2], data[off + 3]]
    };
    let left_px = sample(4, 16);
    let right_px = sample(27, 16);

    assert!(left_px[0] >= 200 && left_px[1] <= 20 && left_px[2] <= 20, "left {:?}", left_px);
    assert!(right_px[1] >= 200 && right_px[0] <= 20 && right_px[2] <= 20, "right {:?}", right_px);

    rm.destroy_vertex_buffer(vb_id);
}
