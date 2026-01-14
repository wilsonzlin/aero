mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, DstOperand, DxbcFile, OperandModifier, RegFile,
    RegisterRef, ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;

fn build_minimal_dxbc() -> Vec<u8> {
    // Minimal DXBC container with zero chunks. The signature-driven translator uses DXBC only for
    // diagnostics today, but it requires a valid container reference.
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

fn dst_temp0() -> DstOperand {
    DstOperand {
        reg: RegisterRef {
            file: RegFile::Temp,
            index: 0,
        },
        mask: WriteMask::XYZW,
        saturate: false,
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

async fn read_mapped_buffer(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
    let slice = buffer.slice(..);
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

#[test]
fn compute_bitfield_ops_produce_expected_results() {
    pollster::block_on(async {
        let test_name = concat!(module_path!(), "::compute_bitfield_ops_produce_expected_results");

        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 bitfield semantics test device")
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

        // Test lane-wise behavior by extracting/inserting at different offsets per component.
        let width = imm([8, 8, 8, 8]);
        let offset = imm([0, 8, 16, 24]);

        // ubfe: extract bytes from 0x11223344 -> [0x44, 0x33, 0x22, 0x11]
        let ubfe_src = imm([0x1122_3344; 4]);
        let expected_ubfe = [0x44u32, 0x33, 0x22, 0x11];

        // ibfe: extract bytes from 0xFF01807F -> [0x7F, -128, 1, -1] (sign-extended)
        let ibfe_src = imm([0xFF01_807F; 4]);
        let expected_ibfe = [0x7Fu32, 0xFFFF_FF80, 0x01, 0xFFFF_FFFF];

        // bfi: insert 0xAB (low 8 bits) into base=0 at different offsets.
        let bfi_base = imm([0; 4]);
        let bfi_insert = imm([0xAB; 4]);
        let expected_bfi = [0x0000_00ABu32, 0x0000_AB00, 0x00AB_0000, 0xAB00_0000];

        // Compute shader writes 3 vec4<u32> blocks into a raw UAV buffer:
        // 0: ubfe result, 1: ibfe result, 2: bfi result.
        const STRIDE_BYTES: u32 = 16;
        const OUT_SIZE: u64 = (STRIDE_BYTES as u64) * 3;

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
                Sm4Inst::Ubfe {
                    dst: dst_temp0(),
                    width: width.clone(),
                    offset: offset.clone(),
                    src: ubfe_src,
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([0; 4]),
                    value: src_temp0(),
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::Ibfe {
                    dst: dst_temp0(),
                    width: width.clone(),
                    offset: offset.clone(),
                    src: ibfe_src,
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([STRIDE_BYTES; 4]),
                    value: src_temp0(),
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::Bfi {
                    dst: dst_temp0(),
                    width,
                    offset,
                    insert: bfi_insert,
                    base: bfi_base,
                },
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: imm([STRIDE_BYTES * 2; 4]),
                    value: src_temp0(),
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
        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated.wgsl.contains("insertBits"),
            "expected bfi translation to use WGSL insertBits:\n{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("extractBits"),
            "expected bfe translation to use WGSL extractBits:\n{}",
            translated.wgsl
        );

        let cs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sm4 bitfield semantics cs module"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        let empty_group0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sm4 bitfield semantics empty bind group layout 0"),
            entries: &[],
        });
        let empty_group1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sm4 bitfield semantics empty bind group layout 1"),
            entries: &[],
        });
        let group2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sm4 bitfield semantics bind group layout"),
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
            label: Some("sm4 bitfield semantics pipeline layout"),
            bind_group_layouts: &[&empty_group0, &empty_group1, &group2],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sm4 bitfield semantics compute pipeline"),
            layout: Some(&pipeline_layout),
            module: &cs,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sm4 bitfield semantics out buffer"),
            size: OUT_SIZE,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&out, 0, &vec![0u8; OUT_SIZE as usize]);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sm4 bitfield semantics staging buffer"),
            size: OUT_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sm4 bitfield semantics bind group"),
            layout: &group2,
            entries: &[wgpu::BindGroupEntry {
                binding: BINDING_BASE_UAV,
                resource: out.as_entire_binding(),
            }],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sm4 bitfield semantics encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sm4 bitfield semantics pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &staging, 0, OUT_SIZE);
        queue.submit([encoder.finish()]);

        let bytes = read_mapped_buffer(&device, &staging).await;
        assert_eq!(bytes.len(), OUT_SIZE as usize);

        let mut words = [0u32; 12];
        for (i, out) in words.iter_mut().enumerate() {
            let start = i * 4;
            *out = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
        }

        assert_eq!(&words[0..4], &expected_ubfe);
        assert_eq!(&words[4..8], &expected_ibfe);
        assert_eq!(&words[8..12], &expected_bfi);
    });
}
