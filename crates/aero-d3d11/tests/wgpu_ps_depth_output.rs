mod common;

use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, DxbcSignatureParameter, FourCC,
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;
use anyhow::{anyhow, Context, Result};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");
const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn sig_param(name: &str, index: u32, register: u32, mask: u8) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type: 0,
        component_type: 0,
        register,
        mask,
        read_write_mask: mask,
        stream: 0,
        min_precision: 0,
    }
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
            min_precision: u32::from(p.min_precision),
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn dst(file: RegFile, index: u32, mask: WriteMask) -> aero_d3d11::DstOperand {
    aero_d3d11::DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_imm(vals: [f32; 4]) -> SrcOperand {
    let bits = vals.map(f32::to_bits);
    SrcOperand {
        kind: SrcKind::ImmediateF32(bits),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue)> {
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

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 wgpu_ps_depth_output test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue))
}

async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = padded_bytes_per_row as u64 * height as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("wgpu_ps_depth_output read_texture staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("wgpu_ps_depth_output read_texture encoder"),
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
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);

    #[cfg(target_arch = "wasm32")]
    device.poll(wgpu::Maintain::Poll);

    receiver
        .receive()
        .await
        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
        .context("wgpu: map_async failed")?;

    let data = slice.get_mapped_range().to_vec();
    staging.unmap();
    Ok(data)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PosVertex {
    pos: [f32; 3],
}

#[test]
fn wgpu_pixel_shader_depth_output_affects_depth_test() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Translate a fragment shader that writes both SV_Target0 and SV_Depth.
        //
        // The vertex shader draws two fullscreen triangles:
        // - First draw is "far" (z=0.9) but fragment shader overrides depth to 0.0 and outputs red.
        // - Second draw is "mid" (z=0.5) with a regular fragment shader that outputs green.
        //
        // With depth override working, the first draw writes depth=0.0, so the second draw fails
        // the LESS depth test and the output stays red.
        let osgn_params = vec![
            sig_param("SV_Target", 0, 0, 0b1111),
            // Depth signatures frequently reuse register 0 even when `SV_Target0` also uses 0.
            sig_param("SV_Depth", 0, 0, 0b0001),
        ];
        let dxbc_bytes = build_dxbc(&[
            (FOURCC_SHEX, Vec::new()),
            (FOURCC_ISGN, build_signature_chunk(&[])),
            (FOURCC_OSGN, build_signature_chunk(&osgn_params)),
        ]);
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = parse_signatures(&dxbc).expect("parse signatures");

        let fs_module_ir = Sm4Module {
            stage: ShaderStage::Pixel,
            model: ShaderModel { major: 5, minor: 0 },
            decls: Vec::new(),
            instructions: vec![
                // o0 = red
                Sm4Inst::Mov {
                    dst: dst(RegFile::Output, 0, WriteMask::XYZW),
                    src: src_imm([1.0, 0.0, 0.0, 1.0]),
                },
                // oDepth.x = 0.0 (override depth to near)
                Sm4Inst::Mov {
                    dst: dst(RegFile::OutputDepth, 0, WriteMask::X),
                    src: src_imm([0.0, 0.0, 0.0, 0.0]),
                },
                Sm4Inst::Ret,
            ],
        };

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &fs_module_ir, &signatures).expect("translate");
        let fs_depth_wgsl = translated.wgsl;
        assert!(
            fs_depth_wgsl.contains("@builtin(frag_depth)"),
            "expected frag_depth in translated WGSL:\n{fs_depth_wgsl}"
        );

        let vs_wgsl = r#"
            struct VsOut {
                @builtin(position) pos: vec4<f32>,
            };

            @vertex
            fn vs_main(@location(0) a_pos: vec3<f32>) -> VsOut {
                var out: VsOut;
                out.pos = vec4<f32>(a_pos, 1.0);
                return out;
            }
        "#;

        let fs_green_wgsl = r#"
            @fragment
            fn fs_main() -> @location(0) vec4<f32> {
                return vec4<f32>(0.0, 1.0, 0.0, 1.0);
            }
        "#;

        let vs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgpu_ps_depth_output vs"),
            source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
        });
        let fs_depth_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgpu_ps_depth_output fs_depth"),
            source: wgpu::ShaderSource::Wgsl(fs_depth_wgsl.into()),
        });
        let fs_green_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgpu_ps_depth_output fs_green"),
            source: wgpu::ShaderSource::Wgsl(fs_green_wgsl.into()),
        });

        let width = 4u32;
        let height = 4u32;

        let color_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgpu_ps_depth_output color"),
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
        let color_view = color_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgpu_ps_depth_output depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let vertices = [
            // First draw: far (z=0.9)
            PosVertex {
                pos: [-1.0, -1.0, 0.9],
            },
            PosVertex {
                pos: [-1.0, 3.0, 0.9],
            },
            PosVertex {
                pos: [3.0, -1.0, 0.9],
            },
            // Second draw: mid (z=0.5)
            PosVertex {
                pos: [-1.0, -1.0, 0.5],
            },
            PosVertex {
                pos: [-1.0, 3.0, 0.5],
            },
            PosVertex {
                pos: [3.0, -1.0, 0.5],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgpu_ps_depth_output vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX,
            mapped_at_creation: true,
        });
        vb.slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(vb_bytes);
        vb.unmap();

        let vb_layout = wgpu::VertexBufferLayout {
            array_stride: core::mem::size_of::<PosVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            }],
        };
        let vb_layouts = std::slice::from_ref(&vb_layout);

        let depth_state = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };

        let pipeline_depth = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgpu_ps_depth_output pipeline_depth"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &vs_module,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: vb_layouts,
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs_depth_module,
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
            depth_stencil: Some(depth_state.clone()),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let pipeline_green = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgpu_ps_depth_output pipeline_green"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &vs_module,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: vb_layouts,
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs_green_module,
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
            depth_stencil: Some(depth_state),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("wgpu_ps_depth_output encoder"),
        });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgpu_ps_depth_output pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            rp.set_vertex_buffer(0, vb.slice(..));

            rp.set_pipeline(&pipeline_depth);
            rp.draw(0..3, 0..1);

            rp.set_pipeline(&pipeline_green);
            rp.draw(3..6, 0..1);
        }
        queue.submit([encoder.finish()]);
        device.poll(wgpu::Maintain::Wait);

        let data = read_texture_rgba8(&device, &queue, &color_tex, width, height)
            .await
            .expect("readback");
        let bytes_per_pixel = 4usize;
        let first_px = &data[..bytes_per_pixel];
        assert_eq!(
            first_px,
            &[255, 0, 0, 255],
            "expected depth override to keep the first draw visible (red) but got {first_px:?}"
        );
    });
}
