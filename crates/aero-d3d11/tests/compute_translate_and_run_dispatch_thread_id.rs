mod common;

use aero_dxbc::test_utils as dxbc_test_utils;
use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, DstOperand, DxbcFile, OperandModifier, RegFile,
    RegisterRef, ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, UavRef, WriteMask,
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

fn dst(file: RegFile, index: u32, mask: WriteMask) -> DstOperand {
    DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_reg(file: RegFile, index: u32, swizzle: Swizzle) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef { file, index }),
        swizzle,
        modifier: OperandModifier::None,
    }
}

fn src_imm_u32(value: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32([value, 0, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

#[test]
fn compute_translate_and_run_dispatch_thread_id_writes_indexed_uav_buffer() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_dispatch_thread_id_writes_indexed_uav_buffer"
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

        const ELEMENTS: u32 = 16;
        let size_bytes = (ELEMENTS as u64) * 4;

        // D3D10_SB_NAME_DISPATCH_THREAD_ID.
        const D3D_NAME_DISPATCH_THREAD_ID: u32 = 20;

        // Build an SM4 IR module that writes `SV_DispatchThreadID.x` into a UAV buffer at index
        // `SV_DispatchThreadID.x`.
        //
        // Since `store_raw` takes a byte offset, compute `addr = id.x << 2` using the `bfi`
        // (bitfield insert) instruction:
        //   addr = insertBits(0, id.x, 2, 30)
        //
        // This test is specifically sensitive to whether the builtin is expanded into the untyped
        // `vec4<f32>` register model as *raw integer bits* (via bitcast) rather than float numeric
        // values; numeric conversion would produce float bit patterns and cause out-of-bounds UAV
        // writes.
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_DISPATCH_THREAD_ID,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                // r0.x = v0.x << 2
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 0, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Input, 0, Swizzle::XXXX),
                    base: src_imm_u32(0),
                },
                // store_raw u0.x, r0.x, v0.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    value: src_reg(RegFile::Input, 0, Swizzle::XXXX),
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
        assert!(
            translated.wgsl.contains("@builtin(global_invocation_id)"),
            "expected compute builtin in WGSL:\n{}",
            translated.wgsl
        );
        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated.wgsl.contains("@group(2)"),
            "translated compute WGSL must use @group(2):\n{}",
            translated.wgsl
        );

        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute_translate_and_run_dispatch_thread_id cs shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run_dispatch_thread_id empty bgl"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run_dispatch_thread_id group2 bgl"),
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
            label: Some("compute_translate_and_run_dispatch_thread_id pipeline layout"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute_translate_and_run_dispatch_thread_id compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run_dispatch_thread_id out buffer"),
            size: size_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&out, 0, vec![0u8; size_bytes as usize].as_slice());

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run_dispatch_thread_id readback buffer"),
            size: size_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_translate_and_run_dispatch_thread_id bind group 2"),
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
            label: Some("compute_translate_and_run_dispatch_thread_id encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute_translate_and_run_dispatch_thread_id compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            pass.dispatch_workgroups(ELEMENTS, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, size_bytes);
        queue.submit([encoder.finish()]);

        let data = read_mapped_buffer(device, &readback, size_bytes).await;
        let mut got = Vec::<u32>::with_capacity(ELEMENTS as usize);
        for i in 0..ELEMENTS as usize {
            let at = i * 4;
            got.push(u32::from_le_bytes(
                data[at..at + 4].try_into().expect("read 4 bytes"),
            ));
        }
        let expected: Vec<u32> = (0..ELEMENTS).collect();
        assert_eq!(got, expected);
    });
}

#[test]
fn compute_translate_and_run_group_id_and_group_index_write_linear_id() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_group_id_and_group_index_write_linear_id"
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

        // D3D10_SB_NAME_GROUP_ID / D3D10_SB_NAME_GROUP_INDEX.
        const D3D_NAME_GROUP_ID: u32 = 21;
        const D3D_NAME_GROUP_INDEX: u32 = 22;

        // Use a 4-thread workgroup and dispatch 4 workgroups: total invocations = 16.
        //
        // Compute `global = (group_id.x << 2) | group_index` using `bfi` (no integer add opcode in
        // our minimal IR) and write it to `u0[global]`.
        const ELEMENTS: u32 = 16;
        let size_bytes = (ELEMENTS as u64) * 4;

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 4, y: 1, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 1,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_INDEX,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                // r0.x = (group_id.x << 2) | group_index
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 0, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Input, 0, Swizzle::XXXX),
                    base: src_reg(RegFile::Input, 1, Swizzle::XXXX),
                },
                // r1.x = global << 2 (byte address)
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 1, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    base: src_imm_u32(0),
                },
                // store_raw u0.x, r1.x, r0.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_reg(RegFile::Temp, 1, Swizzle::XXXX),
                    value: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
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
        assert!(
            translated.wgsl.contains("@builtin(workgroup_id)"),
            "expected workgroup_id builtin in WGSL:\n{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("@builtin(local_invocation_index)"),
            "expected local_invocation_index builtin in WGSL:\n{}",
            translated.wgsl
        );
        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated.wgsl.contains("@group(2)"),
            "translated compute WGSL must use @group(2):\n{}",
            translated.wgsl
        );

        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute_translate_and_run_group_id_group_index cs shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run_group_id_group_index empty bgl"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_translate_and_run_group_id_group_index group2 bgl"),
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
            label: Some("compute_translate_and_run_group_id_group_index pipeline layout"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute_translate_and_run_group_id_group_index compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run_group_id_group_index out buffer"),
            size: size_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&out, 0, vec![0u8; size_bytes as usize].as_slice());

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_translate_and_run_group_id_group_index readback buffer"),
            size: size_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_translate_and_run_group_id_group_index bind group 2"),
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
            label: Some("compute_translate_and_run_group_id_group_index encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute_translate_and_run_group_id_group_index compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            // 4 workgroups, each of size 4 => 16 invocations total.
            pass.dispatch_workgroups(4, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, size_bytes);
        queue.submit([encoder.finish()]);

        let data = read_mapped_buffer(device, &readback, size_bytes).await;

        let mut got = Vec::<u32>::with_capacity(ELEMENTS as usize);
        for i in 0..ELEMENTS as usize {
            let at = i * 4;
            got.push(u32::from_le_bytes(
                data[at..at + 4].try_into().expect("read 4 bytes"),
            ));
        }
        let expected: Vec<u32> = (0..ELEMENTS).collect();
        assert_eq!(got, expected);
    });
}
