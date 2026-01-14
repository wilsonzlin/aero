mod common;

use std::borrow::Cow;

use aero_d3d11::runtime::indirect_args::DrawIndexedIndirectArgs;

#[test]
fn wgpu_draw_indexed_indirect_uses_args_written_by_compute() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::wgpu_draw_indexed_indirect_uses_args_written_by_compute"
        );

        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 draw_indexed_indirect test device")
                .await
            {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                    return;
                }
            };
        if !supports_compute {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }

        let (args_size, args_align) = DrawIndexedIndirectArgs::layout();
        assert_eq!(args_size, 20);
        assert_eq!(args_align, 4);

        let args_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect args buffer"),
            size: args_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        // Indices: triangle list. Base vertex comes from the indirect args buffer.
        //
        // We deliberately use `base_vertex = 1` so a layout mismatch (e.g. reading a 0 from the
        // wrong offset) changes which vertices are referenced and therefore changes the rendered
        // color.
        let index_data: [u32; 3] = [0, 1, 2];
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect index buffer"),
            size: (index_data.len() * core::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&index_buffer, 0, bytemuck::cast_slice(&index_data));

        let cs_wgsl = r#"
            struct DrawIndexedArgs {
                index_count: u32,
                instance_count: u32,
                first_index: u32,
                base_vertex: i32,
                first_instance: u32,
            };

            @group(0) @binding(0)
            var<storage, read_write> args: DrawIndexedArgs;

            @compute @workgroup_size(1)
            fn main() {
                args.index_count = 3u;
                args.instance_count = 1u;
                args.first_index = 0u;
                args.base_vertex = 1;
                args.first_instance = 0u;
            }
        "#;

        let cs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect args compute"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(cs_wgsl)),
        });

        let cs_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect compute bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(args_size),
                },
                count: None,
            }],
        });

        let cs_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect compute bg"),
            layout: &cs_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: args_buffer.as_entire_binding(),
            }],
        });

        let cs_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect compute pl"),
            bind_group_layouts: &[&cs_bgl],
            push_constant_ranges: &[],
        });

        let cs_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect compute pipeline"),
            layout: Some(&cs_pl),
            module: &cs_module,
            entry_point: "main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        let rs_wgsl = r#"
            struct VsOut {
                @builtin(position) pos: vec4<f32>,
                @location(0) color: vec3<f32>,
            };

            fn pos_for_vid(vid: u32) -> vec2<f32> {
                switch (vid) {
                    // Note: vertex 0 and vertex 3 intentionally share a position so that changing
                    // `base_vertex` permutes which indices map to which triangle corners.
                    case 0u: { return vec2<f32>(-1.0, -1.0); }
                    case 1u: { return vec2<f32>(3.0, -1.0); }
                    case 2u: { return vec2<f32>(-1.0, 3.0); }
                    case 3u: { return vec2<f32>(-1.0, -1.0); }
                    default: { return vec2<f32>(0.0, 0.0); }
                }
            }

            fn color_for_vid(vid: u32) -> vec3<f32> {
                switch (vid) {
                    case 0u: { return vec3<f32>(1.0, 0.0, 0.0); } // red
                    case 1u: { return vec3<f32>(0.0, 1.0, 0.0); } // green
                    case 2u: { return vec3<f32>(0.0, 0.0, 1.0); } // blue
                    case 3u: { return vec3<f32>(1.0, 1.0, 0.0); } // yellow
                    default: { return vec3<f32>(0.0, 0.0, 0.0); }
                }
            }

            @vertex
            fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
                var out: VsOut;
                let p = pos_for_vid(vid);
                out.pos = vec4<f32>(p, 0.0, 1.0);
                out.color = color_for_vid(vid);
                return out;
            }

            @fragment
            fn fs_main(@location(0) color: vec3<f32>) -> @location(0) vec4<f32> {
                return vec4<f32>(color, 1.0);
            }
        "#;

        let rs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect render shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(rs_wgsl)),
        });

        let rt_format = wgpu::TextureFormat::Rgba8Unorm;
        let render_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect render pl"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect render pipeline"),
            layout: Some(&render_pl),
            vertex: wgpu::VertexState {
                module: &rs_module,
                entry_point: "vs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &rs_module,
                entry_point: "fs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: rt_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        // 1x1 render target: sample point is exactly NDC (0,0), making the expected color stable.
        let (width, height) = (1u32, 1u32);
        let rt = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect rt"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: rt_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let rt_view = rt.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero-d3d11 draw_indexed_indirect encoder"),
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("aero-d3d11 draw_indexed_indirect compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&cs_pipeline);
            pass.set_bind_group(0, &cs_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aero-d3d11 draw_indexed_indirect render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&render_pipeline);
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed_indirect(&args_buffer, 0);
        }

        queue.submit([encoder.finish()]);

        let pixels = common::wgpu::read_texture_rgba8(&device, &queue, &rt, width, height)
            .await
            .expect("read back render target");
        assert_eq!(pixels.len(), 4);

        // With `base_vertex = 1`, the triangle corners use vertex indices (1,2,3), which map to
        // per-vertex colors (green, blue, yellow). At NDC (0,0) the barycentric weights of our
        // full-screen triangle are (0.5, 0.25, 0.25), so the expected output is:
        //   0.5*yellow + 0.25*green + 0.25*blue = (0.5, 0.75, 0.25).
        let expected = [128u8, 191u8, 64u8, 255u8];
        for (i, (&got, &exp)) in pixels.iter().zip(expected.iter()).enumerate() {
            let delta = i16::from(got) - i16::from(exp);
            assert!(
                delta.abs() <= 1,
                "channel {i} mismatch: got={got} expected≈{exp}"
            );
        }
    });
}
