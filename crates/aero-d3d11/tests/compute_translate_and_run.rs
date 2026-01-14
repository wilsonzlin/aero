mod common;

use aero_dxbc::test_utils as dxbc_test_utils;
use aero_d3d11::binding_model::{BINDING_BASE_TEXTURE, BINDING_BASE_UAV};
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, BufferRef, DstOperand, DxbcFile, OperandModifier,
    RegFile, RegisterRef, ShaderModel, ShaderStage, ShaderSignatures, Sm4Decl, Sm4Inst, Sm4Module,
    SrcKind, SrcOperand, Swizzle, UavRef, WriteMask,
};

// The Aero D3D11 translator emits compute-stage bindings in `@group(2)` (stage-scoped binding
// model). Validate that the generated WGSL can be executed on wgpu with a pipeline layout that
// includes empty bind groups 0/1 and the translated resources bound at group 2.

async fn read_mapped_buffer(device: &wgpu::Device, buffer: &wgpu::Buffer, size: u64) -> Vec<u8> {
    let slice = buffer.slice(0..size);
    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
    slice.map_async(wgpu::MapMode::Read, move |v| {
        sender.send(v).ok();
    });
    device.poll(wgpu::Maintain::Wait);

    receiver
        .receive()
        .await
        .expect("map_async dropped")
        .expect("map_async failed");
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    data
}

fn dummy_dxbc_bytes() -> Vec<u8> {
    // Minimal DXBC container with no chunks. The signature-driven SM4→WGSL translator only uses the
    // DXBC input for diagnostics, so this is sufficient for compute-stage tests.
    dxbc_test_utils::build_container(&[])
}

fn assert_wgsl_validates(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("generated WGSL failed to validate");
}

#[test]
fn compute_translate_and_run_store_raw_uav_buffer() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_store_raw_uav_buffer"
        );

        let expected: u32 = 0x1234_5678;

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    value: SrcOperand {
                        kind: SrcKind::ImmediateF32([expected, 0, 0, 0]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    mask: WriteMask::X,
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dummy_dxbc_bytes();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");

        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);

        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated
                .wgsl
                .contains(&format!("@group(2) @binding({binding_u0})")),
            "expected u0 storage buffer binding to use @group(2); wgsl={}",
            translated.wgsl
        );

        let rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(TEST_NAME, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        if !rt.supports_compute() {
            common::skip_or_panic(TEST_NAME, "compute unsupported");
            return;
        }

        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute_translate_and_run cs shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run empty bind group layout"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run bind group 2 layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: binding_u0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compute_translate_and_run pipeline layout"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute_translate_and_run compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run out buffer"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&out, 0, &[0u8; 16]);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run readback buffer"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_translate_and_run bind group 2"),
            layout: &group2_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: binding_u0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &out,
                    offset: 0,
                    size: None,
                }),
            }],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compute_translate_and_run encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute_translate_and_run compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, 4);
        queue.submit([encoder.finish()]);

        let data = read_mapped_buffer(device, &readback, 4).await;
        let got = u32::from_le_bytes(data[..4].try_into().expect("read 4 bytes"));
        assert_eq!(got, expected);
    });
}

#[test]
fn compute_translate_and_run_copy_raw_srv_to_uav() {
    pollster::block_on(async {
        const TEST_NAME: &str =
            concat!(module_path!(), "::compute_translate_and_run_copy_raw_srv_to_uav");

        let src_words: [u32; 4] = [0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444];

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::ResourceBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
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
                    addr: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    buffer: BufferRef { slot: 0 },
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
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

        let dxbc_bytes = dummy_dxbc_bytes();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");

        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);

        let rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(TEST_NAME, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        if !rt.supports_compute() {
            common::skip_or_panic(TEST_NAME, "compute unsupported");
            return;
        }

        let binding_t0 = BINDING_BASE_TEXTURE + 0;
        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated.wgsl.contains("@group(2)"),
            "translated compute WGSL must use @group(2):\n{}",
            translated.wgsl
        );

        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute_translate_and_run copy_raw cs shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run copy_raw empty bind group layout"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run copy_raw bind group 2 layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: binding_t0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: binding_u0,
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
            label: Some("compute_translate_and_run copy_raw pipeline layout"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute_translate_and_run copy_raw compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let srv = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run copy_raw srv buffer"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&srv, 0, bytemuck::cast_slice(&src_words));

        let uav = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run copy_raw uav buffer"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uav, 0, &[0u8; 16]);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run copy_raw readback buffer"),
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_translate_and_run copy_raw bind group 2"),
            layout: &group2_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: binding_t0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &srv,
                        offset: 0,
                        size: None,
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: binding_u0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uav,
                        offset: 0,
                        size: None,
                    }),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compute_translate_and_run copy_raw encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute_translate_and_run copy_raw compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&uav, 0, &readback, 0, 16);
        queue.submit([encoder.finish()]);

        let got = read_mapped_buffer(device, &readback, 16).await;
        assert_eq!(got.as_slice(), bytemuck::cast_slice(&src_words));
    });
}

#[test]
fn compute_translate_and_run_copy_structured_srv_to_uav() {
    pollster::block_on(async {
        const TEST_NAME: &str =
            concat!(module_path!(), "::compute_translate_and_run_copy_structured_srv_to_uav");

        // Two 16-byte elements (8 u32s). We'll read element 1 and write it into element 0.
        let src_words: [u32; 8] = [
            0, 1, 2, 3, // element 0
            0xaaaa_aaaa,
            0xbbbb_bbbb,
            0xcccc_cccc,
            0xdddd_dddd, // element 1
        ];
        let expected: [u32; 4] = [0xaaaa_aaaa, 0xbbbb_bbbb, 0xcccc_cccc, 0xdddd_dddd];

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::ResourceBuffer {
                    slot: 0,
                    stride: 16,
                    kind: BufferKind::Structured,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 16,
                    kind: BufferKind::Structured,
                },
            ],
            instructions: vec![
                Sm4Inst::LdStructured {
                    dst: DstOperand {
                        reg: RegisterRef {
                            file: RegFile::Temp,
                            index: 0,
                        },
                        mask: WriteMask::XYZW,
                        saturate: false,
                    },
                    index: SrcOperand {
                        kind: SrcKind::ImmediateF32([1, 1, 1, 1]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    offset: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    buffer: BufferRef { slot: 0 },
                },
                Sm4Inst::StoreStructured {
                    uav: UavRef { slot: 0 },
                    index: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    offset: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
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

        let dxbc_bytes = dummy_dxbc_bytes();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");

        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);

        let rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(TEST_NAME, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        if !rt.supports_compute() {
            common::skip_or_panic(TEST_NAME, "compute unsupported");
            return;
        }

        let binding_t0 = BINDING_BASE_TEXTURE + 0;
        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated.wgsl.contains("@group(2)"),
            "translated compute WGSL must use @group(2):\n{}",
            translated.wgsl
        );

        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute_translate_and_run copy_structured cs shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run copy_structured empty bind group layout"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run copy_structured bind group 2 layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: binding_t0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: binding_u0,
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
            label: Some("compute_translate_and_run copy_structured pipeline layout"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute_translate_and_run copy_structured compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let srv = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run copy_structured srv buffer"),
            size: 32,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&srv, 0, bytemuck::cast_slice(&src_words));

        let uav = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run copy_structured uav buffer"),
            size: 32,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uav, 0, &[0u8; 32]);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run copy_structured readback buffer"),
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_translate_and_run copy_structured bind group 2"),
            layout: &group2_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: binding_t0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &srv,
                        offset: 0,
                        size: None,
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: binding_u0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uav,
                        offset: 0,
                        size: None,
                    }),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compute_translate_and_run copy_structured encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute_translate_and_run copy_structured compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&uav, 0, &readback, 0, 16);
        queue.submit([encoder.finish()]);

        let got = read_mapped_buffer(device, &readback, 16).await;
        assert_eq!(got.as_slice(), bytemuck::cast_slice(&expected));
    });
}
