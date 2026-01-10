#![cfg(target_arch = "wasm32")]

mod util;

use aero_d3d9::resources::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

const SAMPLE_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
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
    out.uv = p * 0.5 + vec2<f32>(0.5);
    return out;
}

@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(t, s, in.uv);
}
"#;

async fn render_sampled_texture(
    rm: &mut ResourceManager,
    texture_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> [u8; 4] {
    let device = rm.device();

    // Render into a tiny RGBA8 render target and read back pixel (0,0).
    let out_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("out"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
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
        label: Some("sample shader"),
        source: wgpu::ShaderSource::Wgsl(SAMPLE_SHADER.into()),
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
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
        label: Some("sample pipeline"),
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
                resource: wgpu::BindingResource::TextureView(texture_view),
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
            label: None,
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

    let data = util::read_texture_rgba8(rm.device(), rm.queue(), &out_tex, 4, 4).await;
    [data[0], data[1], data[2], data[3]]
}

#[wasm_bindgen_test(async)]
async fn d3d9_texture_formats_upload_and_sample() {
    let mut rm = util::init_manager().await;
    rm.begin_frame();

    rm.create_sampler(
        1,
        SamplerDesc {
            filter: FilterMode::Linear,
            address_u: AddressMode::Clamp,
            address_v: AddressMode::Clamp,
            address_w: AddressMode::Clamp,
            max_anisotropy: 1,
        },
    )
    .unwrap();
    let sampler = rm.sampler(1).unwrap().sampler();

    // Build cases without closure capture.
    let cases: Vec<(D3DFormat, [u8; 4], Vec<u8>)> = vec![
        // A8R8G8B8: BGRA memory.
        (
            D3DFormat::A8R8G8B8,
            [255, 0, 0, 255],
            {
                let mut v = vec![0u8; 4 * 4 * 4];
                for px in v.chunks_exact_mut(4) {
                    px.copy_from_slice(&[0, 0, 255, 255]);
                }
                v
            },
        ),
        // X8R8G8B8: alpha must be forced to 255 during upload.
        (
            D3DFormat::X8R8G8B8,
            [0, 0, 255, 255],
            {
                let mut v = vec![0u8; 4 * 4 * 4];
                for px in v.chunks_exact_mut(4) {
                    px.copy_from_slice(&[255, 0, 0, 0]); // BGRA => blue with alpha=0 (ignored)
                }
                v
            },
        ),
        // DXT/BC: 4x4 texture == 1 block.
        (D3DFormat::Dxt1, [0, 255, 0, 255], util::bc1_solid_block([0, 255, 0]).to_vec()),
        (D3DFormat::Dxt3, [255, 0, 255, 255], util::bc2_solid_block([255, 0, 255]).to_vec()),
        (D3DFormat::Dxt5, [255, 255, 0, 255], util::bc3_solid_block([255, 255, 0]).to_vec()),
    ];

    for (i, (fmt, expected, data)) in cases.into_iter().enumerate() {
        let id = 100 + i as u32;
        rm.create_texture(
            id,
            TextureDesc {
                kind: TextureKind::Texture2D {
                    width: 4,
                    height: 4,
                    levels: 1,
                },
                format: fmt,
                pool: D3DPool::Default,
                usage: TextureUsageKind::Sampled,
            },
        )
        .unwrap();

        {
            let mut locked = rm.lock_texture_rect(id, 0, 0, LockFlags::empty()).unwrap();
            assert_eq!(locked.data.len(), data.len());
            locked.data.copy_from_slice(&data);
        }
        rm.unlock_texture_rect(id).unwrap();

        let view = rm.texture_view(id).unwrap();
        let sampled = render_sampled_texture(&mut rm, &view, sampler).await;

        // Allow a small tolerance for backend rounding.
        for (a, e) in sampled.iter().zip(expected.iter()) {
            assert!(
                (*a as i16 - *e as i16).abs() <= 2,
                "format {:?}: got {:?}, expected {:?}",
                fmt,
                sampled,
                expected
            );
        }

        rm.destroy_texture(id);
    }
}
