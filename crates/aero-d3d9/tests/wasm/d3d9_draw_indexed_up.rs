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
async fn d3d9_draw_indexed_primitive_up_uploads_u16_indices() {
    let mut rm = util::init_manager().await;
    rm.begin_frame();

    let vertices = [
        // Left triangle (red).
        Vertex {
            pos: [-1.0, -1.0],
            color: [1.0, 0.0, 0.0, 1.0],
        },
        Vertex {
            pos: [0.0, -1.0],
            color: [1.0, 0.0, 0.0, 1.0],
        },
        Vertex {
            pos: [-1.0, 1.0],
            color: [1.0, 0.0, 0.0, 1.0],
        },
        // Right triangle (green).
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
    ];

    let (vb, vb_offset, vb_size) = rm.upload_user_vertex_data(bytemuck::cast_slice(&vertices));

    // 3 u16 indices = 6 bytes (not 4-byte aligned); this exercises padding in the UP upload path.
    let (ib_red, ib_red_offset, ib_red_size) =
        rm.upload_user_index_data(bytemuck::cast_slice(&[0u16, 1, 2]));
    let (ib_green, ib_green_offset, ib_green_size) =
        rm.upload_user_index_data(bytemuck::cast_slice(&[3u16, 4, 5]));

    let device = rm.device();

    let make_out = |label: &str| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
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
        })
    };

    let out_red = make_out("out_red");
    let out_green = make_out("out_green");
    let out_red_view = out_red.create_view(&wgpu::TextureViewDescriptor::default());
    let out_green_view = out_green.create_view(&wgpu::TextureViewDescriptor::default());

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("shader"),
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
    rm.encode_uploads(&mut encoder);

    let draw = |pass: &mut wgpu::RenderPass<'_>, ib: &wgpu::Buffer, ib_offset: u64, ib_size: u64| {
        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, vb.slice(vb_offset..vb_offset + vb_size));
        pass.set_index_buffer(ib.slice(ib_offset..ib_offset + ib_size), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..3, 0, 0..1);
    };

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("red"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &out_red_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: true,
                },
            })],
            depth_stencil_attachment: None,
        });
        draw(&mut pass, &ib_red, ib_red_offset, ib_red_size);
    }

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("green"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &out_green_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: true,
                },
            })],
            depth_stencil_attachment: None,
        });
        draw(&mut pass, &ib_green, ib_green_offset, ib_green_size);
    }

    rm.submit(encoder);

    let red_data = util::read_texture_rgba8(rm.device(), rm.queue(), &out_red, 32, 32).await;
    let green_data = util::read_texture_rgba8(rm.device(), rm.queue(), &out_green, 32, 32).await;

    let sample = |data: &[u8], x: usize, y: usize| {
        let off = (y * 32 + x) * 4;
        [data[off], data[off + 1], data[off + 2], data[off + 3]]
    };

    let red_left = sample(&red_data, 4, 16);
    let red_right = sample(&red_data, 27, 16);
    assert!(red_left[0] >= 200 && red_left[1] <= 20 && red_left[2] <= 20, "red left {:?}", red_left);
    assert!(
        red_right[0] <= 20 && red_right[1] <= 20 && red_right[2] <= 20,
        "red right {:?}",
        red_right
    );

    let green_left = sample(&green_data, 4, 16);
    let green_right = sample(&green_data, 27, 16);
    assert!(
        green_left[0] <= 20 && green_left[1] <= 20 && green_left[2] <= 20,
        "green left {:?}",
        green_left
    );
    assert!(
        green_right[1] >= 200 && green_right[0] <= 20 && green_right[2] <= 20,
        "green right {:?}",
        green_right
    );
}

