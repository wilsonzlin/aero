mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::sm4_ir::ComputeBuiltin;
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BufferKind, DstOperand, DxbcFile, FourCC,
    OperandModifier, RegFile, RegisterRef, ShaderModel, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module,
    SrcKind, SrcOperand, Swizzle, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
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

fn dst_temp(index: u32) -> DstOperand {
    DstOperand {
        reg: RegisterRef {
            file: RegFile::Temp,
            index,
        },
        mask: WriteMask::XYZW,
        saturate: false,
    }
}

fn src_temp(index: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef {
            file: RegFile::Temp,
            index,
        }),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    }
}

fn src_imm_u32(value: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32([value, value, value, value]),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    }
}

fn src_dispatch_thread_id_x() -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ComputeBuiltin(ComputeBuiltin::DispatchThreadId),
        swizzle: Swizzle::XXXX,
        modifier: OperandModifier::None,
    }
}

#[test]
fn compute_store_raw_with_integer_alu_addressing_writes_expected_words() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_store_raw_with_integer_alu_addressing_writes_expected_words"
        );

        let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = parse_signatures(&dxbc).expect("parse signatures");

        // Build a compute module that uses the SM5 integer ALU ops to:
        // - compute a byte address from SV_DispatchThreadID.x (ishl),
        // - compute a value using add/mul/xor/and/or,
        // - and store the results into a raw UAV buffer.
        //
        // Each invocation writes three words:
        // - out[tid + 0] = value (tests iadd/umul/xor/and/or)
        // - out[tid + 4] = -8 >> 1 (tests ishr sign-extension)
        // - out[tid + 8] = 0xfffffff8u >> 1 (tests ushr zero-extension)
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
                // r0 = tid.x
                Sm4Inst::Mov {
                    dst: dst_temp(0),
                    src: src_dispatch_thread_id_x(),
                },
                // r1 = tid + 1
                Sm4Inst::IAdd {
                    dst: dst_temp(1),
                    a: src_temp(0),
                    b: src_imm_u32(1),
                },
                // r2 = (tid + 1) * 3
                Sm4Inst::UMul {
                    dst_lo: dst_temp(2),
                    dst_hi: None,
                    a: src_temp(1),
                    b: src_imm_u32(3),
                },
                // r3 = r2 ^ 0x55
                Sm4Inst::Xor {
                    dst: dst_temp(3),
                    a: src_temp(2),
                    b: src_imm_u32(0x55),
                },
                // r4 = r3 & 0xff
                Sm4Inst::And {
                    dst: dst_temp(4),
                    a: src_temp(3),
                    b: src_imm_u32(0xff),
                },
                // r5 = r4 | 0x100
                Sm4Inst::Or {
                    dst: dst_temp(5),
                    a: src_temp(4),
                    b: src_imm_u32(0x100),
                },
                // r6 = addr0 = tid << 2 (byte address)
                Sm4Inst::IShl {
                    dst: dst_temp(6),
                    a: src_temp(0),
                    b: src_imm_u32(2),
                },
                // store_raw u0.x, r6.x, r5.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_temp(6),
                    value: src_temp(5),
                    mask: WriteMask::X,
                },
                // r7 = -8 (as raw bits)
                Sm4Inst::Mov {
                    dst: dst_temp(7),
                    src: src_imm_u32(0xffff_fff8),
                },
                // r8 = r7 >> 1 (signed)
                Sm4Inst::IShr {
                    dst: dst_temp(8),
                    a: src_temp(7),
                    b: src_imm_u32(1),
                },
                // r9 = tid + 4
                Sm4Inst::IAdd {
                    dst: dst_temp(9),
                    a: src_temp(0),
                    b: src_imm_u32(4),
                },
                // r10 = addr1 = (tid + 4) << 2
                Sm4Inst::IShl {
                    dst: dst_temp(10),
                    a: src_temp(9),
                    b: src_imm_u32(2),
                },
                // store_raw u0.x, r10.x, r8.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_temp(10),
                    value: src_temp(8),
                    mask: WriteMask::X,
                },
                // r11 = r7 >> 1 (unsigned)
                Sm4Inst::UShr {
                    dst: dst_temp(11),
                    a: src_temp(7),
                    b: src_imm_u32(1),
                },
                // r12 = tid + 8
                Sm4Inst::IAdd {
                    dst: dst_temp(12),
                    a: src_temp(0),
                    b: src_imm_u32(8),
                },
                // r13 = addr2 = (tid + 8) << 2
                Sm4Inst::IShl {
                    dst: dst_temp(13),
                    a: src_temp(12),
                    b: src_imm_u32(2),
                },
                // store_raw u0.x, r13.x, r11.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_temp(13),
                    value: src_temp(11),
                    mask: WriteMask::X,
                },
                Sm4Inst::Ret,
            ],
        };

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");
        assert_wgsl_validates(&translated.wgsl);

        let (device, queue, supports_compute) = match common::wgpu::create_device_queue(
            "compute_store_raw_uav_int_ops test device",
        )
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute_store_raw_uav_int_ops shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        // Need 12 u32 words = 48 bytes.
        const BUF_SIZE: u64 = 64;
        let uav = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_store_raw_uav_int_ops uav buffer"),
            size: BUF_SIZE,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uav, 0, &[0u8; BUF_SIZE as usize]);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_store_raw_uav_int_ops staging buffer"),
            size: BUF_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_store_raw_uav_int_ops empty bgl"),
            entries: &[],
        });
        let empty_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_store_raw_uav_int_ops empty bg"),
            layout: &empty_layout,
            entries: &[],
        });

        let uav_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_store_raw_uav_int_ops uav bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: BINDING_BASE_UAV,
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
            label: Some("compute_store_raw_uav_int_ops pipeline layout"),
            // The shader uses @group(2) to match the AeroGPU binding model.
            bind_group_layouts: &[&empty_layout, &empty_layout, &uav_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute_store_raw_uav_int_ops pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        let uav_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_store_raw_uav_int_ops bind group"),
            layout: &uav_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: BINDING_BASE_UAV,
                resource: uav.as_entire_binding(),
            }],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compute_store_raw_uav_int_ops encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute_store_raw_uav_int_ops pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            // wgpu 0.20 requires intermediate bind groups to be bound even when they are empty.
            pass.set_bind_group(0, &empty_bg, &[]);
            pass.set_bind_group(1, &empty_bg, &[]);
            pass.set_bind_group(2, &uav_bg, &[]);
            pass.dispatch_workgroups(4, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&uav, 0, &staging, 0, BUF_SIZE);
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
        receiver.receive().await.unwrap().unwrap();

        let mapped = slice.get_mapped_range();
        let mut got_words = Vec::new();
        for chunk in mapped[..48].chunks_exact(4) {
            got_words.push(u32::from_le_bytes(chunk.try_into().unwrap()));
        }
        drop(mapped);
        staging.unmap();

        let expected_words: [u32; 12] = [
            0x156,
            0x153,
            0x15c,
            0x159,
            0xffff_fffc,
            0xffff_fffc,
            0xffff_fffc,
            0xffff_fffc,
            0x7fff_fffc,
            0x7fff_fffc,
            0x7fff_fffc,
            0x7fff_fffc,
        ];
        assert_eq!(
            got_words.as_slice(),
            &expected_words,
            "unexpected UAV contents: got={got_words:x?} expected={expected_words:x?}",
        );
    });
}
