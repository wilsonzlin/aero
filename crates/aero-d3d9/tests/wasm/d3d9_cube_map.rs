#![cfg(target_arch = "wasm32")]

mod util;

use aero_d3d9::resources::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const CUBE_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let p = positions[idx];
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    return out;
}

@group(0) @binding(0) var t: texture_cube<f32>;
@group(0) @binding(1) var s: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let idx = u32(floor(in.pos.x));
    var dir: vec3<f32>;
    if (idx == 0u) {
        dir = vec3<f32>(1.0, 0.0, 0.0);
    } else if (idx == 1u) {
        dir = vec3<f32>(-1.0, 0.0, 0.0);
    } else if (idx == 2u) {
        dir = vec3<f32>(0.0, 1.0, 0.0);
    } else if (idx == 3u) {
        dir = vec3<f32>(0.0, -1.0, 0.0);
    } else if (idx == 4u) {
        dir = vec3<f32>(0.0, 0.0, 1.0);
    } else {
        dir = vec3<f32>(0.0, 0.0, -1.0);
    }

    return textureSample(t, s, dir);
}
"#;

#[wasm_bindgen_test(async)]
async fn d3d9_cube_map_upload_and_sample_faces() {
    let mut rm = util::init_manager().await;
    rm.begin_frame();

    rm.create_sampler(
        1,
        SamplerDesc {
            filter: FilterMode::Point,
            address_u: AddressMode::Clamp,
            address_v: AddressMode::Clamp,
            address_w: AddressMode::Clamp,
            max_anisotropy: 1,
        },
    )
    .unwrap();
    let sampler = rm.sampler(1).unwrap().sampler();

    let tex_id = 7;
    rm.create_texture(
        tex_id,
        TextureDesc {
            kind: TextureKind::Cube { size: 4, levels: 1 },
            format: D3DFormat::A8R8G8B8,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        },
    )
    .unwrap();

    // Face order in both D3D9 and WebGPU cube textures is +X, -X, +Y, -Y, +Z, -Z.
    // Values here are BGRA8 for upload, with expected RGBA8 listed below.
    let faces_bgra: [[u8; 4]; 6] = [
        [0, 0, 255, 255],   // red
        [0, 255, 0, 255],   // green
        [255, 0, 0, 255],   // blue
        [0, 255, 255, 255], // yellow
        [255, 0, 255, 255], // magenta
        [255, 255, 0, 255], // cyan
    ];
    let expected_rgba: [[u8; 4]; 6] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [255, 0, 255, 255],
        [0, 255, 255, 255],
    ];

    for (face, color) in faces_bgra.iter().enumerate() {
        let mut locked = rm
            .lock_texture_rect(tex_id, 0, face as u32, LockFlags::empty())
            .unwrap();
        for px in locked.data.chunks_exact_mut(4) {
            px.copy_from_slice(color);
        }
        drop(locked);
        rm.unlock_texture_rect(tex_id).unwrap();
    }

    let view = rm.texture_view(tex_id).unwrap();

    let device = rm.device();
    let out_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("out_cube"),
        size: wgpu::Extent3d {
            width: 6,
            height: 1,
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
        label: Some("cube shader"),
        source: wgpu::ShaderSource::Wgsl(CUBE_SHADER.into()),
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[],
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

    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    rm.encode_uploads(&mut encoder);

    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass"),
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
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }

    rm.submit(encoder);

    let data = util::read_texture_rgba8(rm.device(), rm.queue(), &out_tex, 6, 1).await;
    for i in 0..6usize {
        let off = i * 4;
        let px = [data[off], data[off + 1], data[off + 2], data[off + 3]];
        let expected = expected_rgba[i];
        for (a, e) in px.iter().zip(expected.iter()) {
            assert!(
                (*a as i16 - *e as i16).abs() <= 2,
                "face {} got {:?} expected {:?}",
                i,
                px,
                expected
            );
        }
    }

    rm.destroy_texture(tex_id);
}

