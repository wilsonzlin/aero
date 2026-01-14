mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::sm4::decode_program;
use aero_d3d11::{translate_sm4_module_to_wgsl, DxbcFile, ShaderSignatures, Sm4Program};

const CS_LD_UAV_RAW_FLOAT_ADDR_DXBC: &[u8] =
    include_bytes!("fixtures/cs_ld_uav_raw_float_addr.dxbc");

#[test]
fn compute_shader_ld_uav_raw_uses_raw_bit_addresses() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::compute_shader_ld_uav_raw_uses_raw_bit_addresses"
        );

        // Compute shader under test (see `fixtures/cs_ld_uav_raw_float_addr.dxbc`):
        // - ld_uav_raw r0, l(16), u0
        // - store_raw u1, l(0), r0
        // - ld_uav_raw r1, l(16.0), u0
        // - store_raw u1, l(16), r1
        //
        // DXBC register lanes are untyped 32-bit values. Integer-like ops (including buffer
        // addresses) must consume raw lane bits, not attempt to reinterpret float-typed sources as
        // numeric integers.
        //
        // This fixture uses both:
        // - an integer immediate `16` (raw bits 0x00000010) which should load u0.words[4..8], and
        // - a float immediate `16.0` (raw bits 0x41800000) which is a *huge* integer address and
        //   should therefore read out-of-bounds and produce zeros (robust buffer access).
        let dxbc = DxbcFile::parse(CS_LD_UAV_RAW_FLOAT_ADDR_DXBC).expect("DXBC parse");
        let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse");
        let module = decode_program(&program).expect("SM5 decode");

        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        let wgsl = &translated.wgsl;
        assert!(
            wgsl.contains("0x41800000u"),
            "expected raw float bit-pattern 0x41800000 to be used as a byte address:\n{wgsl}"
        );
        assert!(
            !wgsl.contains("floor("),
            "expected strict raw-bit address handling (no float->u32 heuristics) in WGSL:\n{wgsl}"
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

        // Output UAV: 8 words (32 bytes).
        //
        // Layout:
        // - output[0..4] = load via integer immediate 16
        // - output[4..8] = load via float immediate 16.0 (expected zeros)
        let output_words_len: usize = 8;

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
        encoder.copy_buffer_to_buffer(
            &output,
            0,
            &staging,
            0,
            (output_words_len * 4) as u64,
        );
        queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v: Result<(), wgpu::BufferAsyncError>| {
            sender.send(v).ok();
        });
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

        assert_eq!(
            &words[0..4],
            &input_words[4..8],
            "expected ld_uav_raw with integer immediate 16 (0x10 bits) to load byte offset 16"
        );
        assert_eq!(
            &words[4..8],
            &[0u32; 4],
            "expected ld_uav_raw with float immediate 16.0 (0x41800000 bits) to read out-of-bounds and produce zeros"
        );

        drop(bytes);
        staging.unmap();
    });
}
