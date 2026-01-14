mod common;

use aero_d3d11::{DxbcFile, ShaderStage, Sm4Program};
use anyhow::{anyhow, Context, Result};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

// This fixture is also used by `sm4_geometry_decode.rs` to validate that the SM4 decoder
// recognizes `emit` + `cut` instructions.
const GS_CUT_DXBC: &[u8] = include_bytes!("fixtures/gs_emit_cut.dxbc");

fn pixel_rgba8(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * WIDTH + x) * 4) as usize;
    buf[idx..idx + 4].try_into().expect("pixel slice")
}

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir =
                std::env::temp_dir().join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            // Prefer "native" backends; this avoids noisy platform warnings from
            // initializing GL/WAYLAND stacks in headless CI environments.
            wgpu::Backends::PRIMARY
        },
        ..Default::default()
    });

    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })
        .await
    {
        Some(adapter) => Some(adapter),
        None => {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
    }
    .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

    let downlevel = adapter.get_downlevel_capabilities();
    if !downlevel
        .flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS | wgpu::DownlevelFlags::INDIRECT_EXECUTION)
    {
        return Err(anyhow!(
            "wgpu: adapter lacks required downlevel flags for GS emulation: {:?}",
            downlevel.flags
        ));
    }

    let info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 gs_cut test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue, info))
}

async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>> {
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = WIDTH * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = padded_bytes_per_row as u64 * HEIGHT as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gs_cut readback staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gs_cut readback encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });
    device.poll(wgpu::Maintain::Wait);
    receiver
        .receive()
        .await
        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
        .context("wgpu: map_async failed")?;

    let mapped = slice.get_mapped_range();
    let mut out = Vec::with_capacity((unpadded_bytes_per_row * HEIGHT) as usize);
    for row in 0..HEIGHT as usize {
        let start = row * padded_bytes_per_row as usize;
        out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    staging.unmap();
    Ok(out)
}

#[test]
fn gs_cut_restartstrip_resets_triangle_strip_assembly_semantics() -> Result<()> {
    pollster::block_on(async {
        // Ensure our checked-in fixture is at least a valid geometry shader DXBC container and
        // actually contains the `cut` opcode token. This helps catch accidental fixture
        // corruption, even though the test below uses WGSL to emulate the expected behavior.
        let dxbc = DxbcFile::parse(GS_CUT_DXBC).context("parse gs_emit_cut.dxbc as DXBC")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("parse gs_emit_cut.dxbc as SM4")?;
        assert_eq!(
            program.stage,
            ShaderStage::Geometry,
            "gs_emit_cut.dxbc must be a geometry shader"
        );
        assert!(
            program
                .tokens
                .iter()
                .any(|t| (*t & aero_d3d11::sm4::opcode::OPCODE_MASK) == aero_d3d11::sm4::opcode::OPCODE_CUT),
            "gs_emit_cut.dxbc must contain a cut opcode (RestartStrip)"
        );

        let test_name =
            concat!(module_path!(), "::gs_cut_restartstrip_resets_triangle_strip_assembly_semantics");
        let (device, queue, info) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("{err:#}"));
                return Ok(());
            }
        };

        eprintln!("running {test_name} on wgpu backend {:?}", info.backend);

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gs_cut vertices"),
            size: 6 * 16,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gs_cut indices"),
            size: 7 * 4,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let indirect_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gs_cut indirect args"),
            // wgpu requires 4-byte alignment; use a little extra to avoid edge cases.
            size: 64,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        let cs_wgsl = r#"
struct DrawIndexedIndirect {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

@group(0) @binding(0) var<storage, read_write> vertices: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> indices: array<u32>;
@group(0) @binding(2) var<storage, read_write> indirect: DrawIndexedIndirect;

// This compute shader stands in for the project’s GS emulation compute pass:
// it emits two triangle-strip segments and inserts a primitive-restart index
// between them (equivalent to HLSL `RestartStrip()` / DXBC `cut`).
@compute @workgroup_size(1)
fn cs_main() {
    // Left triangle.
    vertices[0] = vec4<f32>(-0.9, -0.5, 0.0, 1.0);
    vertices[1] = vec4<f32>(-0.1, -0.5, 0.0, 1.0);
    vertices[2] = vec4<f32>(-0.5,  0.5, 0.0, 1.0);

    // Right triangle.
    vertices[3] = vec4<f32>( 0.1, -0.5, 0.0, 1.0);
    vertices[4] = vec4<f32>( 0.9, -0.5, 0.0, 1.0);
    vertices[5] = vec4<f32>( 0.5,  0.5, 0.0, 1.0);

    // Triangle strip indices with a primitive-restart value between strips.
    indices[0] = 0u;
    indices[1] = 1u;
    indices[2] = 2u;
    indices[3] = 0xffff_ffffu; // primitive restart (Uint32)
    indices[4] = 3u;
    indices[5] = 4u;
    indices[6] = 5u;

    indirect.index_count = 7u;
    indirect.instance_count = 1u;
    indirect.first_index = 0u;
    indirect.base_vertex = 0;
    indirect.first_instance = 0u;
}
"#;
        let cs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gs_cut compute"),
            source: wgpu::ShaderSource::Wgsl(cs_wgsl.into()),
        });

        let cs_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gs_cut compute bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let cs_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gs_cut compute layout"),
            bind_group_layouts: &[&cs_bind_group_layout],
            push_constant_ranges: &[],
        });
        let cs_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("gs_cut compute pipeline"),
            layout: Some(&cs_pipeline_layout),
            module: &cs_module,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });
        let cs_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gs_cut compute bg"),
            layout: &cs_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vertex_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: index_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: indirect_buffer.as_entire_binding(),
                },
            ],
        });

        let rt = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gs_cut render target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let rt_view = rt.create_view(&wgpu::TextureViewDescriptor::default());

        let rs_wgsl = r#"
@vertex
fn vs_main(@location(0) pos: vec4<f32>) -> @builtin(position) vec4<f32> {
    return pos;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;
        let rs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gs_cut render shaders"),
            source: wgpu::ShaderSource::Wgsl(rs_wgsl.into()),
        });

        let rs_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gs_cut render layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let rs_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gs_cut render pipeline"),
            layout: Some(&rs_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &rs_module,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        shader_location: 0,
                        offset: 0,
                        format: wgpu::VertexFormat::Float32x4,
                    }],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &rs_module,
                entry_point: "fs_main",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: Some(wgpu::IndexFormat::Uint32),
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gs_cut encoder"),
        });

        // Compute pass: generate vertices/indices/indirect args.
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gs_cut compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&cs_pipeline);
            pass.set_bind_group(0, &cs_bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }

        // Render pass: draw the indexed triangle strip using indirect args.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("gs_cut render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&rs_pipeline);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed_indirect(&indirect_buffer, 0);
        }

        queue.submit([encoder.finish()]);
        device.poll(wgpu::Maintain::Wait);

        let pixels = read_texture_rgba8(&device, &queue, &rt).await?;
        assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);

        // Verify both triangles rendered (left/right), and the would-be “bridge” region between
        // strips remains background. Without a RestartStrip/cut, the triangle strip would include
        // additional connecting triangles that cover the center pixel.
        let bg = [0u8, 0u8, 0u8, 255u8];
        let fg = [255u8, 255u8, 255u8, 255u8];

        assert_eq!(pixel_rgba8(&pixels, 16, 32), fg, "left triangle should render");
        assert_eq!(pixel_rgba8(&pixels, 48, 32), fg, "right triangle should render");
        assert_eq!(
            pixel_rgba8(&pixels, 32, 32),
            bg,
            "gap pixel should remain background (RestartStrip/cut must reset strip assembly)"
        );

        Ok(())
    })
}
