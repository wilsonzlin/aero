mod common;

use std::borrow::Cow;

use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, Builtin, DxbcFile, DxbcSignatureParameter,
    FourCC, OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Inst, Sm4Module,
    SrcKind, SrcOperand, Swizzle, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

// `D3D_NAME` system-value IDs (subset).
const D3D_NAME_PRIMITIVE_ID: u32 = 7;

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn build_signature_chunk(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name.as_str(),
            semantic_index: p.semantic_index,
            system_value_type: p.system_value_type,
            component_type: p.component_type,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.read_write_mask,
            stream: u32::from(p.stream),
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn sig_param(
    name: &str,
    index: u32,
    register: u32,
    mask: u8,
    sys_value: u32,
) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type: sys_value,
        component_type: 0,
        register,
        mask,
        read_write_mask: mask,
        stream: 0,
        min_precision: 0,
    }
}

fn dst(file: RegFile, index: u32, mask: WriteMask) -> aero_d3d11::DstOperand {
    aero_d3d11::DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_reg(file: RegFile, index: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef { file, index }),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Vertex {
    pos: [f32; 2],
}

fn setup_wgpu() -> anyhow::Result<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-d3d11-test-xdg-runtime-{}",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
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
        .ok_or_else(|| anyhow::anyhow!("wgpu: no suitable adapter found"))?;

        adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 primitive_id test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("wgpu: request_device failed: {e:?}"))
    })
}

async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> anyhow::Result<Vec<u8>> {
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = padded_bytes_per_row as u64 * height as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("primitive_id test readback staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("primitive_id test readback encoder"),
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

    let slice = staging.slice(..);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });

    device.poll(wgpu::Maintain::Wait);
    receiver
        .receive()
        .await
        .ok_or_else(|| anyhow::anyhow!("wgpu: map_async dropped"))?
        .map_err(|e| anyhow::anyhow!("wgpu: map_async failed: {e:?}"))?;

    let mapped = slice.get_mapped_range();
    let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    staging.unmap();

    Ok(out)
}

#[test]
fn primitive_id_pixel_shader_renders_different_colors_per_triangle() {
    pollster::block_on(async {
        let (device, queue) = match setup_wgpu() {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Build a pixel shader that writes `SV_PrimitiveID` into the red channel.
        let isgn = build_signature_chunk(&[sig_param(
            "SV_PrimitiveID",
            0,
            0,
            0b0001,
            D3D_NAME_PRIMITIVE_ID,
        )]);
        let osgn = build_signature_chunk(&[sig_param("SV_Target", 0, 0, 0b1111, 0)]);
        let dxbc_bytes = build_dxbc(&[
            (FOURCC_SHEX, Vec::new()),
            (FOURCC_ISGN, isgn),
            (FOURCC_OSGN, osgn),
        ]);
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = parse_signatures(&dxbc).expect("parse signatures");

        let ps_module = Sm4Module {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                // Convert the integer primitive ID bits into numeric float, then write to SV_Target.
                //
                // In real DXBC this comes from the `utof` opcode; our internal register model
                // stores integer system values as raw bits in `vec4<f32>` lanes.
                Sm4Inst::Utof {
                    dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                    src: src_reg(RegFile::Input, 0),
                },
                Sm4Inst::Ret,
            ],
        };

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &ps_module, &signatures).expect("translate");
        assert!(
            translated
                .reflection
                .inputs
                .iter()
                .any(|p| p.builtin == Some(Builtin::PrimitiveIndex)),
            "expected PrimitiveIndex builtin in reflection"
        );

        let vs_wgsl = r#"
            struct VsOut {
                @builtin(position) pos: vec4<f32>,
            };

            @vertex
            fn vs_main(@location(0) pos: vec2<f32>) -> VsOut {
                var out: VsOut;
                out.pos = vec4<f32>(pos, 0.0, 1.0);
                return out;
            }
        "#;

        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("primitive_id test VS"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(vs_wgsl)),
        });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("primitive_id test PS (translated)"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(translated.wgsl)),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("primitive_id test pipeline layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                shader_location: 0,
                offset: 0,
                format: wgpu::VertexFormat::Float32x2,
            }],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("primitive_id test pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[vertex_layout],
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: "fs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
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

        // Two disjoint triangles: one on the left (primitive_id=0) and one on the right
        // (primitive_id=1).
        let vertices: [Vertex; 6] = [
            // Triangle 0 (left)
            Vertex { pos: [-0.9, -0.9] },
            Vertex { pos: [-0.1, -0.9] },
            Vertex { pos: [-0.9, 0.9] },
            // Triangle 1 (right)
            Vertex { pos: [0.1, -0.9] },
            Vertex { pos: [0.9, -0.9] },
            Vertex { pos: [0.9, 0.9] },
        ];

        let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("primitive_id test vertex buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let width = 64u32;
        let height = 64u32;
        let rt = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("primitive_id test render target"),
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
        let rt_view = rt.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("primitive_id test encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("primitive_id test render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_vertex_buffer(0, vb.slice(..));
            pass.draw(0..6, 0..1);
        }
        queue.submit([encoder.finish()]);

        let pixels = read_texture_rgba8(&device, &queue, &rt, width, height)
            .await
            .expect("readback should succeed");

        let w = width as usize;
        let px = |x: usize, y: usize| -> &[u8] {
            let idx = (y * w + x) * 4;
            &pixels[idx..idx + 4]
        };

        // A pixel in triangle 0 should be black (primitive_id == 0).
        assert_eq!(px(8, 32), &[0, 0, 0, 255]);
        // A pixel in triangle 1 should be red (primitive_id == 1).
        assert_eq!(px(56, 32), &[255, 0, 0, 255]);
        // A pixel between triangles should be the clear color (blue).
        assert_eq!(px(32, 32), &[0, 0, 255, 255]);
    });
}
