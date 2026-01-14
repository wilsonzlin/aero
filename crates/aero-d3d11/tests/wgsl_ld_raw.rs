mod common;

use aero_d3d11::binding_model::{BINDING_BASE_TEXTURE, BINDING_BASE_UAV};
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferRef, DstOperand, DxbcFile, OperandModifier, RegFile,
    RegisterRef, ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

fn build_minimal_dxbc() -> Vec<u8> {
    // Minimal DXBC container with zero chunks. The signature-driven translator uses DXBC only for
    // diagnostics today, but it requires a valid container reference.
    dxbc_test_utils::build_container(&[])
}

#[test]
fn compute_shader_ld_raw_reads_from_storage_buffer() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_shader_ld_raw_reads_from_storage_buffer"
        );

        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 ld_raw test device").await {
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

        // Input data: 8 words (32 bytes).
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

        // Output buffer: 12 words (48 bytes). Shader writes:
        // - words[4..8] = input_words[4..8] (byte offset = 16)
        // - words[8..12] = input_words[4..8] (byte offset = 32)
        let output_words_len: usize = 12;

        let input = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ld_raw input buffer"),
            size: (input_words.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&input, 0, bytemuck::cast_slice(&input_words));

        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ld_raw output buffer"),
            size: (output_words_len * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&output, 0, &vec![0u8; output_words_len * 4]);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ld_raw staging buffer"),
            size: (output_words_len * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Build a compute shader:
        // - ld_raw r0, l(bits=16), t0 (byte offset 16 -> word index 4)
        // - store_raw u0, l(bits=16), r0
        // - ld_raw r1, l(bits=16), t0
        // - store_raw u0, l(bits=32), r1 (byte offset 32 -> word index 8)
        let addr_bits_16 = SrcOperand {
            kind: SrcKind::ImmediateF32([16u32; 4]),
            swizzle: Swizzle::XXXX,
            modifier: OperandModifier::None,
        };
        let addr_bits_32 = SrcOperand {
            kind: SrcKind::ImmediateF32([32u32; 4]),
            swizzle: Swizzle::XXXX,
            modifier: OperandModifier::None,
        };

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 }],
            instructions: vec![
                Sm4Inst::LdRaw {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    addr: addr_bits_16.clone(),
                    buffer: BufferRef { slot: 0 },
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: addr_bits_16.clone(),
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
                Sm4Inst::LdRaw {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    addr: addr_bits_16,
                    buffer: BufferRef { slot: 0 },
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: addr_bits_32,
                    value: SrcOperand {
                        kind: SrcKind::Register(RegisterRef {
                            file: RegFile::Temp,
                            index: 1,
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
        assert!(
            translated.wgsl.contains("@compute"),
            "expected compute entry point:\n{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("var<storage, read> t0"),
            "expected raw SRV buffer declaration:\n{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("var<storage, read_write> u0"),
            "expected raw UAV buffer declaration:\n{}",
            translated.wgsl
        );

        let cs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ld_raw cs module"),
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
            label: Some("ld_raw bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_BASE_TEXTURE,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
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
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ld_raw pipeline layout"),
            bind_group_layouts: &[&empty_group0, &empty_group1, &group2],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ld_raw compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &cs,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ld_raw bind group"),
            layout: &group2,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BINDING_BASE_TEXTURE,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_BASE_UAV,
                    resource: output.as_entire_binding(),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ld_raw encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ld_raw pass"),
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

        // Words 0..4 remain untouched (zeros).
        assert!(words[..4].iter().all(|&w| w == 0));
        // The shader stores input_words[4..8] twice: at offsets 16B (word 4) and 32B (word 8).
        assert_eq!(&words[4..8], &input_words[4..8]);
        assert_eq!(&words[8..12], &input_words[4..8]);

        drop(bytes);
        staging.unmap();
    });
}
