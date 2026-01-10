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

fn write_u16(dst: &mut [u8], v: u16) {
    dst.copy_from_slice(&v.to_le_bytes());
}

#[wasm_bindgen_test(async)]
async fn d3d9_u16_index_buffer_unaligned_locks_upload_correctly() {
    let mut rm = util::init_manager().await;
    rm.begin_frame();

    let ib_id = 1;
    rm.create_index_buffer(
        ib_id,
        IndexBufferDesc {
            // 3 indices * 2 bytes = 6 bytes (not 4-byte aligned).
            size_bytes: 6,
            format: IndexFormat::U16,
            pool: D3DPool::Default,
            usage: BufferUsageFlags::DYNAMIC,
        },
    )
    .unwrap();

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

    // Draw red triangle on the left (indices = 0,1,2).
    {
        let data = rm.lock_index_buffer(ib_id, 0, 0, LockFlags::DISCARD).unwrap();
        data.copy_from_slice(bytemuck::cast_slice(&[0u16, 1, 2]));
        rm.unlock_index_buffer(ib_id).unwrap();
    }

    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        rm.encode_uploads(&mut encoder);

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
            pass.set_vertex_buffer(0, vb.slice(vb_offset..vb_offset + vb_size));
            let ib = rm.index_buffer(ib_id).unwrap().gpu_buffer();
            pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..3, 0, 0..1);
        }

        rm.submit(encoder);
    }

    let data = util::read_texture_rgba8(rm.device(), rm.queue(), &out_tex, 32, 32).await;
    let sample = |x: usize, y: usize| {
        let off = (y * 32 + x) * 4;
        [data[off], data[off + 1], data[off + 2], data[off + 3]]
    };
    let left_px = sample(4, 16);
    let right_px = sample(27, 16);
    assert!(left_px[0] >= 200 && left_px[1] <= 20 && left_px[2] <= 20, "left {:?}", left_px);
    assert!(
        right_px[0] <= 20 && right_px[1] <= 20 && right_px[2] <= 20,
        "right {:?}",
        right_px
    );

    // Update indices to draw the green triangle on the right (indices = 3,4,5) using unaligned
    // sub-range locks (offset 2 bytes is not 4-byte aligned).
    {
        let data = rm.lock_index_buffer(ib_id, 0, 2, LockFlags::DISCARD).unwrap();
        write_u16(data, 3);
        rm.unlock_index_buffer(ib_id).unwrap();
    }
    {
        let data = rm.lock_index_buffer(ib_id, 2, 2, LockFlags::empty()).unwrap();
        write_u16(data, 4);
        rm.unlock_index_buffer(ib_id).unwrap();
    }
    {
        let data = rm.lock_index_buffer(ib_id, 4, 2, LockFlags::empty()).unwrap();
        write_u16(data, 5);
        rm.unlock_index_buffer(ib_id).unwrap();
    }

    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        rm.encode_uploads(&mut encoder);

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pass2"),
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
            pass.set_vertex_buffer(0, vb.slice(vb_offset..vb_offset + vb_size));
            let ib = rm.index_buffer(ib_id).unwrap().gpu_buffer();
            pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..3, 0, 0..1);
        }

        rm.submit(encoder);
    }

    let data2 = util::read_texture_rgba8(rm.device(), rm.queue(), &out_tex, 32, 32).await;
    let left_px2 = {
        let off = (16 * 32 + 4) * 4;
        [data2[off], data2[off + 1], data2[off + 2], data2[off + 3]]
    };
    let right_px2 = {
        let off = (16 * 32 + 27) * 4;
        [data2[off], data2[off + 1], data2[off + 2], data2[off + 3]]
    };

    assert!(
        left_px2[0] <= 20 && left_px2[1] <= 20 && left_px2[2] <= 20,
        "left2 {:?}",
        left_px2
    );
    assert!(
        right_px2[1] >= 200 && right_px2[0] <= 20 && right_px2[2] <= 20,
        "right2 {:?}",
        right_px2
    );
}

