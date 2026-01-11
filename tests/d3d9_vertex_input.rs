use aero_d3d9::vertex::{
    translate_vertex_declaration, DeclMethod, DeclType, DeclUsage, StreamsFreqState,
    VertexDeclaration, VertexElement, WebGpuVertexCaps,
};
use bytemuck::{Pod, Zeroable};
use std::borrow::Cow;
use std::collections::BTreeMap;
use wgpu::util::DeviceExt;

fn create_device() -> (wgpu::Device, wgpu::Queue) {
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        // Some WGPU backends complain loudly if this isn't set, even when we never create a surface.
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
    }

    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
    }))
    .or_else(|| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
    })
    .expect("no compatible wgpu adapter found");

    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("d3d9_vertex_input device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
        },
        None,
    ))
    .expect("failed to request wgpu device")
}

fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let bytes_per_pixel = 4;
    let bytes_per_row = width * bytes_per_pixel;
    assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback buffer"),
        size: (bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback encoder"),
    });

    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| tx.send(res).unwrap());
    device.poll(wgpu::Maintain::Wait);
    rx.recv().unwrap().unwrap();

    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    data
}

fn sample_rgba(data: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = (y * width * 4 + x * 4) as usize;
    [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]]
}

fn assert_channel_near(actual: u8, expected: u8, tolerance: u8, label: &str) {
    let diff = actual.abs_diff(expected);
    assert!(
        diff <= tolerance,
        "{label}: actual={actual} expected={expected} tolerance={tolerance}"
    );
}

fn unorm8(v: f32) -> u8 {
    ((v * 255.0).round() as i32).clamp(0, 255) as u8
}

const QUAD_SHADER: &str = r#"
struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) red: f32,
}

@vertex
fn vs_main(
  @location(0) pos: vec3<f32>,
  @location(6) color: vec4<f32>,
  @location(8) uv: vec2<f32>,
) -> VsOut {
  var out: VsOut;
  out.pos = vec4<f32>(pos, 1.0);
  out.uv = uv;
  out.red = color.x;
  return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  return vec4<f32>(in.uv, in.red, 1.0);
}
"#;

const INSTANCE_SHADER: &str = r#"
struct VsOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(
  @location(0) pos: vec2<f32>,
  @location(6) color: vec4<f32>,
  @builtin(instance_index) iid: u32,
) -> VsOut {
  var out: VsOut;
  let shift = select(-0.6, 0.6, iid == 1u);
  out.pos = vec4<f32>(pos.x + shift, pos.y, 0.0, 1.0);
  out.color = color;
  return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  return in.color;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VertexPosColorUv {
    pos: [f32; 3],
    color: u32,
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VertexPos {
    pos: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VertexUvColor {
    uv: [f32; 2],
    color: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VertexPos2 {
    pos: [f32; 2],
}

fn quad_positions() -> [[f32; 3]; 6] {
    [
        [-1.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 1.0, 0.0],
    ]
}

fn quad_uvs() -> [[f32; 2]; 6] {
    [
        [0.0, 1.0],
        [1.0, 1.0],
        [0.0, 0.0],
        [0.0, 0.0],
        [1.0, 1.0],
        [1.0, 0.0],
    ]
}

fn create_pipeline(
    device: &wgpu::Device,
    shader_wgsl: &str,
    layouts: &[wgpu::VertexBufferLayout<'_>],
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(shader_wgsl)),
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: layouts,
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    })
}

fn create_color_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

#[test]
fn render_single_stream_position_color_uv() {
    use aero_d3d9::vertex::StandardLocationMap;

    let decl = VertexDeclaration {
        elements: vec![
            VertexElement::new(
                0,
                0,
                DeclType::Float3,
                DeclMethod::Default,
                DeclUsage::Position,
                0,
            ),
            VertexElement::new(
                0,
                12,
                DeclType::D3dColor,
                DeclMethod::Default,
                DeclUsage::Color,
                0,
            ),
            VertexElement::new(
                0,
                16,
                DeclType::Float2,
                DeclMethod::Default,
                DeclUsage::TexCoord,
                0,
            ),
        ],
    };

    let mut strides = BTreeMap::new();
    strides.insert(0, std::mem::size_of::<VertexPosColorUv>() as u32);

    let translated = translate_vertex_declaration(
        &decl,
        &strides,
        &StreamsFreqState::default(),
        WebGpuVertexCaps {
            vertex_attribute_16bit: true,
        },
        &StandardLocationMap,
    )
    .unwrap();

    assert_eq!(translated.buffers.len(), 1);

    let layouts: Vec<_> = translated
        .buffers
        .iter()
        .map(|b| wgpu::VertexBufferLayout {
            array_stride: b.array_stride,
            step_mode: b.step_mode,
            attributes: b.attributes.as_slice(),
        })
        .collect();

    let (device, queue) = create_device();
    let pipeline = create_pipeline(&device, QUAD_SHADER, &layouts);

    let color_red: u32 = 0xffff_0000;
    let positions = quad_positions();
    let uvs = quad_uvs();
    let vertices: Vec<VertexPosColorUv> = positions
        .into_iter()
        .zip(uvs.into_iter())
        .map(|(pos, uv)| VertexPosColorUv {
            pos,
            color: color_red,
            uv,
        })
        .collect();

    let vb_bytes = if let Some(plan) = translated.conversions.get(&0) {
        plan.convert_vertices(bytemuck::cast_slice(&vertices), vertices.len())
            .unwrap()
    } else {
        bytemuck::cast_slice(&vertices).to_vec()
    };
    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb"),
        contents: &vb_bytes,
        usage: wgpu::BufferUsages::VERTEX,
    });

    let (target, target_view) = create_color_target(&device, 64, 64);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, vb.slice(..));
        pass.draw(0..6, 0..1);
    }
    queue.submit([encoder.finish()]);

    let pixels = read_texture_rgba8(&device, &queue, &target, 64, 64);
    let center = sample_rgba(&pixels, 64, 32, 32);

    // We output `vec4(uv.x, uv.y, color.r, 1.0)`. The center pixel is ~0.5, 0.5.
    let expected_x = unorm8((32.0 + 0.5) / 64.0);
    let expected_y = unorm8((32.0 + 0.5) / 64.0);
    assert_channel_near(center[0], expected_x, 10, "uv.x");
    assert_channel_near(center[1], expected_y, 10, "uv.y");
    assert_channel_near(center[2], 255, 0, "color.r");
    assert_channel_near(center[3], 255, 0, "alpha");
}

#[test]
fn render_two_stream_position_uv_color() {
    use aero_d3d9::vertex::StandardLocationMap;

    let decl = VertexDeclaration {
        elements: vec![
            VertexElement::new(
                0,
                0,
                DeclType::Float3,
                DeclMethod::Default,
                DeclUsage::Position,
                0,
            ),
            VertexElement::new(
                1,
                0,
                DeclType::Float2,
                DeclMethod::Default,
                DeclUsage::TexCoord,
                0,
            ),
            VertexElement::new(
                1,
                8,
                DeclType::D3dColor,
                DeclMethod::Default,
                DeclUsage::Color,
                0,
            ),
        ],
    };

    let mut strides = BTreeMap::new();
    strides.insert(0, std::mem::size_of::<VertexPos>() as u32);
    strides.insert(1, std::mem::size_of::<VertexUvColor>() as u32);

    let translated = translate_vertex_declaration(
        &decl,
        &strides,
        &StreamsFreqState::default(),
        WebGpuVertexCaps {
            vertex_attribute_16bit: true,
        },
        &StandardLocationMap,
    )
    .unwrap();

    assert_eq!(translated.buffers.len(), 2);

    let layouts: Vec<_> = translated
        .buffers
        .iter()
        .map(|b| wgpu::VertexBufferLayout {
            array_stride: b.array_stride,
            step_mode: b.step_mode,
            attributes: b.attributes.as_slice(),
        })
        .collect();

    let (device, queue) = create_device();
    let pipeline = create_pipeline(&device, QUAD_SHADER, &layouts);

    let positions = quad_positions();
    let uvs = quad_uvs();
    let vertices_pos: Vec<VertexPos> = positions.into_iter().map(|pos| VertexPos { pos }).collect();

    let color_red: u32 = 0xffff_0000;
    let vertices_extra: Vec<VertexUvColor> = uvs
        .into_iter()
        .map(|uv| VertexUvColor {
            uv,
            color: color_red,
        })
        .collect();

    let vb0_bytes = if let Some(plan) = translated.conversions.get(&0) {
        plan.convert_vertices(bytemuck::cast_slice(&vertices_pos), vertices_pos.len())
            .unwrap()
    } else {
        bytemuck::cast_slice(&vertices_pos).to_vec()
    };
    let vb0 = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb pos"),
        contents: &vb0_bytes,
        usage: wgpu::BufferUsages::VERTEX,
    });
    let vb1_bytes = if let Some(plan) = translated.conversions.get(&1) {
        plan.convert_vertices(bytemuck::cast_slice(&vertices_extra), vertices_extra.len())
            .unwrap()
    } else {
        bytemuck::cast_slice(&vertices_extra).to_vec()
    };
    let vb1 = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb uv+color"),
        contents: &vb1_bytes,
        usage: wgpu::BufferUsages::VERTEX,
    });

    let (target, target_view) = create_color_target(&device, 64, 64);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&pipeline);

        let slot0 = *translated.stream_to_buffer_slot.get(&0).unwrap();
        let slot1 = *translated.stream_to_buffer_slot.get(&1).unwrap();
        pass.set_vertex_buffer(slot0, vb0.slice(..));
        pass.set_vertex_buffer(slot1, vb1.slice(..));
        pass.draw(0..6, 0..1);
    }
    queue.submit([encoder.finish()]);

    let pixels = read_texture_rgba8(&device, &queue, &target, 64, 64);
    let center = sample_rgba(&pixels, 64, 32, 32);

    let expected_x = unorm8((32.0 + 0.5) / 64.0);
    let expected_y = unorm8((32.0 + 0.5) / 64.0);
    assert_channel_near(center[0], expected_x, 10, "uv.x");
    assert_channel_near(center[1], expected_y, 10, "uv.y");
    assert_channel_near(center[2], 255, 0, "color.r");
    assert_channel_near(center[3], 255, 0, "alpha");
}

#[test]
fn render_instanced_draw_per_instance_color() {
    use aero_d3d9::vertex::StandardLocationMap;

    let decl = VertexDeclaration {
        elements: vec![
            VertexElement::new(
                0,
                0,
                DeclType::Float2,
                DeclMethod::Default,
                DeclUsage::Position,
                0,
            ),
            VertexElement::new(
                1,
                0,
                DeclType::D3dColor,
                DeclMethod::Default,
                DeclUsage::Color,
                0,
            ),
        ],
    };

    let mut strides = BTreeMap::new();
    strides.insert(0, std::mem::size_of::<VertexPos2>() as u32);
    strides.insert(1, 4);

    let mut freq = StreamsFreqState::default();
    freq.set(0, 0x4000_0000 | 2).unwrap(); // INDEXEDDATA instances=2
    freq.set(1, 0x8000_0000 | 1).unwrap(); // INSTANCEDATA divisor=1

    let translated = translate_vertex_declaration(
        &decl,
        &strides,
        &freq,
        WebGpuVertexCaps {
            vertex_attribute_16bit: true,
        },
        &StandardLocationMap,
    )
    .unwrap();

    assert_eq!(translated.instancing.draw_instances(), 2);

    let layouts: Vec<_> = translated
        .buffers
        .iter()
        .map(|b| wgpu::VertexBufferLayout {
            array_stride: b.array_stride,
            step_mode: b.step_mode,
            attributes: b.attributes.as_slice(),
        })
        .collect();

    let (device, queue) = create_device();
    let pipeline = create_pipeline(&device, INSTANCE_SHADER, &layouts);

    let base_quad: [VertexPos2; 6] = [
        VertexPos2 { pos: [-0.3, -0.5] },
        VertexPos2 { pos: [0.3, -0.5] },
        VertexPos2 { pos: [-0.3, 0.5] },
        VertexPos2 { pos: [-0.3, 0.5] },
        VertexPos2 { pos: [0.3, -0.5] },
        VertexPos2 { pos: [0.3, 0.5] },
    ];

    // Use colors that catch D3DCOLOR byte-order mismatches (red/blue swapping).
    let instance_colors: [u32; 2] = [
        0xffff_0000, // red
        0xff00_00ff, // blue
    ];

    let vb0 = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb positions"),
        contents: bytemuck::cast_slice(&base_quad),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let vb1_bytes = if let Some(plan) = translated.conversions.get(&1) {
        plan.convert_vertices(
            bytemuck::cast_slice(&instance_colors),
            instance_colors.len(),
        )
        .unwrap()
    } else {
        bytemuck::cast_slice(&instance_colors).to_vec()
    };
    let vb1 = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("vb instance colors"),
        contents: &vb1_bytes,
        usage: wgpu::BufferUsages::VERTEX,
    });

    let (target, target_view) = create_color_target(&device, 64, 64);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&pipeline);

        let slot0 = *translated.stream_to_buffer_slot.get(&0).unwrap();
        let slot1 = *translated.stream_to_buffer_slot.get(&1).unwrap();
        pass.set_vertex_buffer(slot0, vb0.slice(..));
        pass.set_vertex_buffer(slot1, vb1.slice(..));
        pass.draw(0..6, 0..translated.instancing.draw_instances());
    }
    queue.submit([encoder.finish()]);

    let pixels = read_texture_rgba8(&device, &queue, &target, 64, 64);
    let left = sample_rgba(&pixels, 64, 16, 32);
    let right = sample_rgba(&pixels, 64, 48, 32);

    assert_channel_near(left[0], 255, 2, "left.r");
    assert_channel_near(left[1], 0, 2, "left.g");
    assert_channel_near(left[2], 0, 2, "left.b");
    assert_channel_near(left[3], 255, 2, "left.a");

    assert_channel_near(right[0], 0, 2, "right.r");
    assert_channel_near(right[1], 0, 2, "right.g");
    assert_channel_near(right[2], 255, 2, "right.b");
    assert_channel_near(right[3], 255, 2, "right.a");
}
