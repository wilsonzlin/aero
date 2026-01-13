mod common;

use aero_dxbc::test_utils as dxbc_test_utils;
use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::{
    parse_signatures, translate_sm4_module_to_wgsl, BufferKind, DxbcFile, FourCC, OperandModifier,
    ShaderModel, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, UavRef,
    WriteMask,
};

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn src_imm_bits(bits: [u32; 4]) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32(bits),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

#[test]
fn compute_store_raw_writes_u32_word() {
    pollster::block_on(async {
        let test_name = concat!(module_path!(), "::compute_store_raw_writes_u32_word");

        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("compute_store_raw test device").await {
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

        let dxbc_bytes = build_dxbc(&[(FOURCC_SHEX, Vec::new())]);
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let signatures = parse_signatures(&dxbc).expect("parse signatures");

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
                    addr: src_imm_bits([0; 4]),
                    value: src_imm_bits([0xdead_beefu32, 0, 0, 0]),
                    mask: WriteMask::X,
                },
                // Use a float immediate (`16.0`) for the byte address to exercise the
                // float-to-u32 conversion heuristic in the translator.
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_imm_bits([16.0f32.to_bits(); 4]),
                    value: src_imm_bits([0xcafe_babeu32, 0, 0, 0]),
                    mask: WriteMask::X,
                },
                Sm4Inst::Ret,
            ],
        };

        let translated =
            translate_sm4_module_to_wgsl(&dxbc, &module, &signatures).expect("translate");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("compute_store_raw shader"),
            source: wgpu::ShaderSource::Wgsl(translated.wgsl.into()),
        });

        // Need at least 16 bytes to write word index 4 (byte offset 16).
        const BUF_SIZE: u64 = 32;
        let uav = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_store_raw uav buffer"),
            size: BUF_SIZE,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Ensure deterministic initial contents.
        queue.write_buffer(&uav, 0, &[0u8; BUF_SIZE as usize]);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute_store_raw staging buffer"),
            size: BUF_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_store_raw empty bgl"),
            entries: &[],
        });
        let uav_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute_store_raw uav bgl"),
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
            label: Some("compute_store_raw pipeline layout"),
            // The shader uses @group(2) to match the AeroGPU binding model.
            bind_group_layouts: &[&empty_layout, &empty_layout, &uav_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("compute_store_raw pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "cs_main",
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        });

        let uav_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute_store_raw bind group"),
            layout: &uav_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: BINDING_BASE_UAV,
                resource: uav.as_entire_binding(),
            }],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("compute_store_raw encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute_store_raw pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(2, &uav_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&uav, 0, &staging, 0, BUF_SIZE);
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
            .ok_or_else(|| anyhow::anyhow!("wgpu: map_async dropped"))
            .and_then(|r| r.map_err(|e| anyhow::anyhow!("wgpu: map_async failed: {e:?}")))
            .unwrap();

        let mapped = slice.get_mapped_range();
        let got0 = u32::from_le_bytes(mapped[0..4].try_into().unwrap());
        let got4 = u32::from_le_bytes(mapped[16..20].try_into().unwrap());
        drop(mapped);
        staging.unmap();

        assert_eq!(got0, 0xdead_beefu32);
        assert_eq!(got4, 0xcafe_babeu32);
    });
}
