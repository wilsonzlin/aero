#![cfg(target_arch = "wasm32")]

mod util;

use aero_d3d9::resources::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn sample_level_shader(lod: u32) -> String {
    format!(
        r#"
struct VsOut {{
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {{
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
}}

@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{
    return textureSampleLevel(t, s, in.uv, {lod}.0);
}}
"#
    )
}

async fn render_sampled_texture_lod(
    rm: &mut ResourceManager,
    texture_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    lod: u32,
) -> [u8; 4] {
    let device = rm.device();

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

    let shader_src = sample_level_shader(lod);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sample mip shader"),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
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

    let data = util::read_texture_rgba8(rm.device(), rm.queue(), &out_tex, 4, 4).await;
    [data[0], data[1], data[2], data[3]]
}

#[wasm_bindgen_test(async)]
async fn d3d9_texture_mip_upload_and_sample_levels() {
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

    let tex_id = 42;
    rm.create_texture(
        tex_id,
        TextureDesc {
            kind: TextureKind::Texture2D {
                width: 4,
                height: 4,
                levels: 2,
            },
            format: D3DFormat::A8R8G8B8,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        },
    )
    .unwrap();

    // Mip 0: solid red (BGRA = 0,0,255,255).
    {
        let mut locked = rm.lock_texture_rect(tex_id, 0, 0, LockFlags::empty()).unwrap();
        for px in locked.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 255, 255]);
        }
    }
    rm.unlock_texture_rect(tex_id).unwrap();

    // Mip 1: solid green (BGRA = 0,255,0,255).
    {
        let mut locked = rm.lock_texture_rect(tex_id, 1, 0, LockFlags::empty()).unwrap();
        for px in locked.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 255, 0, 255]);
        }
    }
    rm.unlock_texture_rect(tex_id).unwrap();

    let view = rm.texture_view(tex_id).unwrap();

    let mip0 = render_sampled_texture_lod(&mut rm, &view, sampler, 0).await;
    let mip1 = render_sampled_texture_lod(&mut rm, &view, sampler, 1).await;

    assert!(mip0[0] <= 5 && mip0[1] <= 5 && mip0[2] >= 250, "mip0 {:?}", mip0);
    assert!(mip1[0] <= 5 && mip1[1] >= 250 && mip1[2] <= 5, "mip1 {:?}", mip1);

    rm.destroy_texture(tex_id);
}

