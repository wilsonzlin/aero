mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, DstOperand, DxbcFile, OperandModifier, RegFile, RegisterRef,
    ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand,
    Swizzle, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

fn build_minimal_dxbc() -> Vec<u8> {
    // Minimal DXBC container with zero chunks. The signature-driven translator uses DXBC only for
    // diagnostics today, but it requires a valid container reference.
    dxbc_test_utils::build_container(&[])
}

#[test]
fn compute_shader_ld_uav_raw_accepts_float_byte_address() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_shader_ld_uav_raw_accepts_float_byte_address"
        );

        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 ld_uav_raw test device").await {
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

        // Input UAV data: 8 words (32 bytes).
        let input_words: [u32; 8] = [
            0x0001_0203,
            0x0405_0607,
            0x0809_0A0B,
            0x0C0D_0E0F,
            0x1020_3040,
            0x5566_7788,
            0x99AA_BBCC,
            0xDDEE_FF00,
        ];

        // Output UAV: 4 words (16 bytes).
        let output_words_len: usize = 4;

        let input = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ld_uav_raw input buffer"),
            size: (input_words.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&input, 0, bytemuck::cast_slice(&input_words));

        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ld_uav_raw output buffer"),
            size: (output_words_len * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&output, 0, &vec![0u8; output_words_len * 4]);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ld_uav_raw staging buffer"),
            size: (output_words_len * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Build a compute shader:
        // - ld_uav_raw r0, l(16.0), u0
        // - store_raw u1, l(0), r0
        //
        // The `16.0` address is encoded as a float immediate. The translator should apply the
        // float-to-u32 heuristic and treat it as byte offset 16 (word index 4).
        let addr_f32_16 = SrcOperand {
            kind: SrcKind::ImmediateF32([16.0f32.to_bits(); 4]),
            swizzle: Swizzle::XXXX,
            modifier: OperandModifier::None,
        };
        let addr_bits_0 = SrcOperand {
            kind: SrcKind::ImmediateF32([0; 4]),
            swizzle: Swizzle::XXXX,
            modifier: OperandModifier::None,
        };

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: aero_d3d11::BufferKind::Raw,
                },
                Sm4Decl::UavBuffer {
                    slot: 1,
                    stride: 0,
                    kind: aero_d3d11::BufferKind::Raw,
                },
            ],
            instructions: vec![
                Sm4Inst::LdUavRaw {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    addr: addr_f32_16,
                    uav: UavRef { slot: 0 },
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 1 },
                    addr: addr_bits_0,
                    value: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        }),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = build_minimal_dxbc();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = ShaderSignatures {
            isgn: None,
            osgn: None,
            psgn: None,
            pcsg: None,
        };
        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &signatures)
            .expect("compute translation should succeed");

        let cs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ld_uav_raw cs module"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_group0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("empty bind group layout 0"),
            entries: &[],
        });
        let empty_group1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("empty bind group layout 1"),
            entries: &[],
        });

        let group2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ld_uav_raw bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_BASE_UAV,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_BASE_UAV + 1,
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ld_uav_raw pipeline layout"),
            bind_group_layouts: &[&empty_group0, &empty_group1, &group2],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ld_uav_raw compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &cs,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ld_uav_raw bind group"),
            layout: &group2,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BINDING_BASE_UAV,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_BASE_UAV + 1,
                    resource: output.as_entire_binding(),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ld_uav_raw encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ld_uav_raw pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, (output_words_len * 4) as u64);
        queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(
            wgpu::MapMode::Read,
            move |v: Result<(), wgpu::BufferAsyncError>| {
                sender.send(v).ok();
            },
        );
        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);
        #[cfg(target_arch = "wasm32")]
        device.poll(wgpu::Maintain::Poll);
        receiver
            .receive()
            .await
            .expect("map_async dropped")
            .expect("map_async failed");

        let bytes = slice.get_mapped_range();
        let words: &[u32] = bytemuck::cast_slice(&bytes);
        assert_eq!(words.len(), output_words_len);

        // The shader loads u0.words[4..8] (byte offset 16) and stores them into u1.words[0..4].
        assert_eq!(words, &input_words[4..8]);

        drop(bytes);
        staging.unmap();
    });
}
