mod common;

use aero_dxbc::test_utils as dxbc_test_utils;
use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, DxbcFile, OperandModifier, RegFile, RegisterRef,
    ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand,
    Swizzle, UavRef, WriteMask,
};

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

fn imm(bits: [u32; 4]) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32(bits),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

fn src_temp0() -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef {
            file: RegFile::Temp,
            index: 0,
        }),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

fn dst_temp0() -> aero_d3d11::DstOperand {
    aero_d3d11::DstOperand {
        reg: RegisterRef {
            file: RegFile::Temp,
            index: 0,
        },
        mask: WriteMask::XYZW,
        saturate: false,
    }
}

fn firstbit_hi_ref(x: u32) -> u32 {
    if x == 0 {
        0xffff_ffff
    } else {
        31 - x.leading_zeros()
    }
}

fn firstbit_lo_ref(x: u32) -> u32 {
    if x == 0 {
        0xffff_ffff
    } else {
        x.trailing_zeros()
    }
}

fn firstbit_shi_ref(x: i32) -> u32 {
    if x == 0 || x == -1 {
        0xffff_ffff
    } else if x > 0 {
        31 - (x as u32).leading_zeros()
    } else {
        let inv = !(x as u32);
        31 - inv.leading_zeros()
    }
}

#[test]
fn compute_bit_utils_produce_expected_results() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(module_path!(), "::compute_bit_utils_produce_expected_results");

        let bfrev_in = [1u32, 0x8000_0000, 0x0123_4567, 0];
        let countbits_in = [0u32, 1, 0xffff_ffff, 0x0123_4567];
        let firstbit_in = [0x8000_0000u32, 0x0000_0010, 0, 1];
        let firstbit_shi_in = [0xffff_ffffu32, 0x8000_0000, 0, 1];

        let expected_bfrev = bfrev_in.map(|x| x.reverse_bits());
        let expected_countbits = countbits_in.map(|x| x.count_ones());
        let expected_firstbit_hi = firstbit_in.map(firstbit_hi_ref);
        let expected_firstbit_lo = firstbit_in.map(firstbit_lo_ref);
        let expected_firstbit_shi = firstbit_shi_in.map(|x| firstbit_shi_ref(x as i32));

        // Compute shader writes 5 vec4<u32> blocks (one per opcode) into a raw UAV buffer.
        const STRIDE_BYTES: u32 = 16;
        const OUT_SIZE: u64 = (STRIDE_BYTES as u64) * 5;

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
                Sm4Inst::Bfrev {
                    dst: dst_temp0(),
                    src: imm(bfrev_in),
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([0; 4]),
                    value: src_temp0(),
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::CountBits {
                    dst: dst_temp0(),
                    src: imm(countbits_in),
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([STRIDE_BYTES; 4]),
                    value: src_temp0(),
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::FirstbitHi {
                    dst: dst_temp0(),
                    src: imm(firstbit_in),
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([STRIDE_BYTES * 2; 4]),
                    value: src_temp0(),
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::FirstbitLo {
                    dst: dst_temp0(),
                    src: imm(firstbit_in),
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([STRIDE_BYTES * 3; 4]),
                    value: src_temp0(),
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::FirstbitShi {
                    dst: dst_temp0(),
                    src: imm(firstbit_shi_in),
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([STRIDE_BYTES * 4; 4]),
                    value: src_temp0(),
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

        let binding_u0 = BINDING_BASE_UAV + 0;
        let device = rt.device();
        let queue = rt.queue();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sm4_bit_utils_semantics cs shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sm4_bit_utils_semantics empty bind group layout"),
            entries: &[],
        });
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sm4_bit_utils_semantics bind group 2 layout"),
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
            label: Some("sm4_bit_utils_semantics pipeline layout"),
            bind_group_layouts: &[&empty_layout, &empty_layout, &group2_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sm4_bit_utils_semantics compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: Default::default(),
        });

        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sm4_bit_utils_semantics out buffer"),
            size: OUT_SIZE,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&out, 0, &vec![0u8; OUT_SIZE as usize]);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sm4_bit_utils_semantics readback buffer"),
            size: OUT_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sm4_bit_utils_semantics bind group 2"),
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
            label: Some("sm4_bit_utils_semantics encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sm4_bit_utils_semantics compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, OUT_SIZE);
        queue.submit([encoder.finish()]);

        let data = read_mapped_buffer(device, &readback, OUT_SIZE).await;
        assert_eq!(data.len(), OUT_SIZE as usize);

        let mut got = [0u32; 20];
        for (i, slot) in got.iter_mut().enumerate() {
            let start = i * 4;
            *slot = u32::from_le_bytes(
                data[start..start + 4]
                    .try_into()
                    .expect("read 4 bytes"),
            );
        }

        let expect_block = |block_index: usize, expected: [u32; 4]| {
            let start = block_index * 4;
            assert_eq!(
                got[start..start + 4],
                expected,
                "mismatch in block {block_index} (u32 indices {start}..{})",
                start + 4
            );
        };

        expect_block(0, expected_bfrev);
        expect_block(1, expected_countbits);
        expect_block(2, expected_firstbit_hi);
        expect_block(3, expected_firstbit_lo);
        expect_block(4, expected_firstbit_shi);
    });
}
