mod common;

use aero_d3d11::input_layout::{
    fnv1a_32, InputLayoutBinding, InputLayoutDesc, VsInputSignatureElement,
    AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
};
use aero_d3d11::runtime::expansion_scratch::{
    ExpansionScratchAllocator, ExpansionScratchDescriptor,
};
use aero_d3d11::runtime::index_pulling::{IndexPullingParams, INDEX_FORMAT_U16};
use aero_d3d11::runtime::tessellation::vs_as_compute::{
    alloc_vs_out_regs, VsAsComputeConfig, VsAsComputePipeline,
};
use aero_d3d11::runtime::vertex_pulling::{
    VertexPullingDrawParams, VertexPullingLayout, VertexPullingSlot,
};
use aero_d3d11::{parse_signatures, DxbcFile};
use anyhow::{anyhow, Context, Result};

fn load_vs_passthrough_signature() -> Result<(Vec<VsInputSignatureElement>, u32)> {
    let vs_dxbc = DxbcFile::parse(include_bytes!("fixtures/vs_passthrough.dxbc"))
        .context("parse vs_passthrough")?;
    let sigs = parse_signatures(&vs_dxbc).context("parse signatures")?;

    let isgn = sigs.isgn.context("vs_passthrough missing ISGN")?;
    let mut inputs = Vec::with_capacity(isgn.parameters.len());
    for p in &isgn.parameters {
        inputs.push(VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
            mask: p.mask,
            // Current Aero translation uses the D3D register as the WGSL @location.
            shader_location: p.register,
        });
    }

    let osgn = sigs.osgn.context("vs_passthrough missing OSGN")?;
    let mut max_reg = 0u32;
    for p in &osgn.parameters {
        max_reg = max_reg.max(p.register);
    }
    let out_reg_count = max_reg + 1;

    Ok((inputs, out_reg_count))
}

async fn read_back_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    src: &wgpu::Buffer,
    src_offset: u64,
    size: u64,
) -> Result<Vec<u8>> {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("VS-as-compute readback staging"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("VS-as-compute readback encoder"),
    });
    encoder.copy_buffer_to_buffer(src, src_offset, &staging, 0, size);
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
        .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
        .context("wgpu: map_async failed")?;

    let bytes = slice.get_mapped_range().to_vec();
    staging.unmap();
    Ok(bytes)
}

fn unpack_vec4_u32_as_f32(words: &[u32]) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    for chunk in words.chunks_exact(4) {
        out.push([
            f32::from_bits(chunk[0]),
            f32::from_bits(chunk[1]),
            f32::from_bits(chunk[2]),
            f32::from_bits(chunk[3]),
        ]);
    }
    out
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

#[test]
fn vs_as_compute_writes_vs_out_regs_non_indexed() {
    pollster::block_on(async {
        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 VS-as-compute test device").await {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(module_path!(), &format!("{err:#}"));
                    return;
                }
            };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }

        let (vs_signature, out_reg_count) = load_vs_passthrough_signature().unwrap();
        assert!(out_reg_count >= 2, "vs_passthrough should export >=2 regs");

        // ILAY fixture: POSITION0 (float3) + COLOR0 (float4).
        let layout = InputLayoutDesc::parse(include_bytes!("fixtures/ilay_pos3_color.bin"))
            .context("parse ILAY")
            .unwrap();
        let stride = 28u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &vs_signature)
            .context("pulling layout")
            .unwrap();

        // Two vertices: pos3 + color4.
        let vertices = [
            ([1.0f32, 2.0, 3.0], [0.25f32, 0.5, 0.75, 1.0]),
            ([4.0f32, 5.0, 6.0], [1.0f32, 0.0, 0.0, 1.0]),
        ];
        let mut vb_bytes = Vec::new();
        for (pos, col) in vertices {
            for f in pos {
                vb_bytes.extend_from_slice(&f.to_le_bytes());
            }
            for f in col {
                vb_bytes.extend_from_slice(&f.to_le_bytes());
            }
        }

        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let ia_uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute ia uniform"),
            size: ia_uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &ia_uniform_bytes);

        let vertex_count = 2u32;
        let instance_count = 2u32;
        let control_point_count = 1u32;

        let cfg = VsAsComputeConfig {
            control_point_count,
            out_reg_count,
            indexed: false,
        };
        let pipeline = VsAsComputePipeline::new(&device, &pulling, cfg).unwrap();

        let mut scratch = ExpansionScratchAllocator::new(ExpansionScratchDescriptor::default());
        let vs_out_regs = alloc_vs_out_regs(
            &mut scratch,
            &device,
            vertex_count,
            instance_count,
            out_reg_count,
        )
        .unwrap();

        let bg = pipeline
            .create_bind_group_group3(
                &device,
                &pulling,
                &[&vb],
                &ia_uniform,
                None,
                None,
                &vs_out_regs,
            )
            .unwrap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("VS-as-compute encoder"),
        });
        pipeline
            .dispatch(&mut encoder, vertex_count, instance_count, &bg)
            .unwrap();
        queue.submit([encoder.finish()]);

        let bytes = read_back_buffer(
            &device,
            &queue,
            vs_out_regs.buffer.as_ref(),
            vs_out_regs.offset,
            vs_out_regs.size,
        )
        .await
        .unwrap();
        let words: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&bytes).to_vec();
        let vecs = unpack_vec4_u32_as_f32(&words);

        // Layout: [patch_id_total][control_point_id=0][out_reg], flattened as:
        // patch0: o0,o1; patch1:o0,o1; ...
        let expected: Vec<[f32; 4]> = vec![
            // instance0, vertex0
            [1.0, 2.0, 3.0, 1.0],
            [0.25, 0.5, 0.75, 1.0],
            // instance0, vertex1
            [4.0, 5.0, 6.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            // instance1, vertex0
            [1.0, 2.0, 3.0, 1.0],
            [0.25, 0.5, 0.75, 1.0],
            // instance1, vertex1
            [4.0, 5.0, 6.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(vecs, expected);
    });
}

fn pack_u16_indices_to_words(indices: &[u16]) -> Vec<u32> {
    let mut words = vec![0u32; indices.len().div_ceil(2)];
    for (i, &idx) in indices.iter().enumerate() {
        let word_idx = i / 2;
        let shift = (i % 2) * 16;
        words[word_idx] |= (idx as u32) << shift;
    }
    words
}

#[test]
fn vs_as_compute_supports_index_pulling() {
    pollster::block_on(async {
        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 VS-as-compute test device").await {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(module_path!(), &format!("{err:#}"));
                    return;
                }
            };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }

        let (vs_signature, out_reg_count) = load_vs_passthrough_signature().unwrap();
        let layout = InputLayoutDesc::parse(include_bytes!("fixtures/ilay_pos3_color.bin"))
            .context("parse ILAY")
            .unwrap();
        let stride = 28u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &vs_signature)
            .context("pulling layout")
            .unwrap();

        // Three vertices.
        let vertices = [
            ([10.0f32, 0.0, 0.0], [1.0f32, 0.0, 0.0, 1.0]),
            ([0.0f32, 10.0, 0.0], [0.0f32, 1.0, 0.0, 1.0]),
            ([0.0f32, 0.0, 10.0], [0.0f32, 0.0, 1.0, 1.0]),
        ];
        let mut vb_bytes = Vec::new();
        for (pos, col) in vertices {
            for f in pos {
                vb_bytes.extend_from_slice(&f.to_le_bytes());
            }
            for f in col {
                vb_bytes.extend_from_slice(&f.to_le_bytes());
            }
        }
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute indexed vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        // Indices: [2, 0, 1]
        let indices = [2u16, 0, 1];
        let words = pack_u16_indices_to_words(&indices);
        let ib = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute index buffer words"),
            size: (words.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ib, 0, bytemuck::cast_slice(&words));

        let params = IndexPullingParams {
            first_index: 0,
            base_vertex: 0,
            index_format: INDEX_FORMAT_U16,
            _pad0: 0,
        };
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute index params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&params_buf, 0, &params.to_le_bytes());

        let ia_uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams {
                first_vertex: 0,
                first_instance: 0,
                base_vertex: 0,
                first_index: 0,
            },
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute indexed ia uniform"),
            size: ia_uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &ia_uniform_bytes);

        let index_count = indices.len() as u32;
        let instance_count = 1u32;
        let control_point_count = 1u32;

        let cfg = VsAsComputeConfig {
            control_point_count,
            out_reg_count,
            indexed: true,
        };
        let pipeline = VsAsComputePipeline::new(&device, &pulling, cfg).unwrap();

        let mut scratch = ExpansionScratchAllocator::new(ExpansionScratchDescriptor::default());
        let vs_out_regs = alloc_vs_out_regs(
            &mut scratch,
            &device,
            index_count,
            instance_count,
            out_reg_count,
        )
        .unwrap();

        let bg = pipeline
            .create_bind_group_group3(
                &device,
                &pulling,
                &[&vb],
                &ia_uniform,
                Some(&params_buf),
                Some(&ib),
                &vs_out_regs,
            )
            .unwrap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("VS-as-compute indexed encoder"),
        });
        pipeline
            .dispatch(&mut encoder, index_count, instance_count, &bg)
            .unwrap();
        queue.submit([encoder.finish()]);

        let bytes = read_back_buffer(
            &device,
            &queue,
            vs_out_regs.buffer.as_ref(),
            vs_out_regs.offset,
            vs_out_regs.size,
        )
        .await
        .unwrap();
        let words: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&bytes).to_vec();
        let vecs = unpack_vec4_u32_as_f32(&words);

        let expected: Vec<[f32; 4]> = vec![
            // idx0 = 2
            [0.0, 0.0, 10.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            // idx1 = 0
            [10.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            // idx2 = 1
            [0.0, 10.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
        ];
        assert_eq!(vecs, expected);
    });
}

#[test]
fn vs_as_compute_rejects_non_multiple_of_control_points() {
    pollster::block_on(async {
        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 VS-as-compute test device").await {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(module_path!(), &format!("{err:#}"));
                    return;
                }
            };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }

        let (vs_signature, out_reg_count) = load_vs_passthrough_signature().unwrap();
        let layout = InputLayoutDesc::parse(include_bytes!("fixtures/ilay_pos3_color.bin"))
            .context("parse ILAY")
            .unwrap();
        let stride = 28u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &vs_signature)
            .context("pulling layout")
            .unwrap();

        // Provide 4 vertices (the exact contents don't matter since dispatch should fail before execution).
        let mut vb_bytes = Vec::new();
        for i in 0..4u32 {
            vb_bytes.extend_from_slice(&(i as f32).to_le_bytes());
            vb_bytes.extend_from_slice(&(i as f32).to_le_bytes());
            vb_bytes.extend_from_slice(&(i as f32).to_le_bytes());
            vb_bytes.extend_from_slice(&0.0f32.to_le_bytes());
            vb_bytes.extend_from_slice(&0.0f32.to_le_bytes());
            vb_bytes.extend_from_slice(&0.0f32.to_le_bytes());
            vb_bytes.extend_from_slice(&1.0f32.to_le_bytes());
        }
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute invalid vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let ia_uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute invalid ia uniform"),
            size: ia_uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &ia_uniform_bytes);

        let invocations_per_instance = 4u32;
        let instance_count = 1u32;
        let control_point_count = 3u32; // does not divide invocations_per_instance

        let cfg = VsAsComputeConfig {
            control_point_count,
            out_reg_count,
            indexed: false,
        };
        let pipeline = VsAsComputePipeline::new(&device, &pulling, cfg).unwrap();

        let mut scratch = ExpansionScratchAllocator::new(ExpansionScratchDescriptor::default());
        let vs_out_regs = alloc_vs_out_regs(
            &mut scratch,
            &device,
            invocations_per_instance,
            instance_count,
            out_reg_count,
        )
        .unwrap();

        let bg = pipeline
            .create_bind_group_group3(
                &device,
                &pulling,
                &[&vb],
                &ia_uniform,
                None,
                None,
                &vs_out_regs,
            )
            .unwrap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("VS-as-compute invalid encoder"),
        });
        let err = pipeline
            .dispatch(&mut encoder, invocations_per_instance, instance_count, &bg)
            .expect_err("dispatch should reject non-multiple of control point count");
        assert!(
            err.to_string().contains("multiple of control_point_count"),
            "unexpected error: {err:#}"
        );
    });
}

#[test]
fn vs_as_compute_loads_f16x2_input() {
    pollster::block_on(async {
        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 VS-as-compute f16 test device")
                .await
            {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(module_path!(), &format!("{err:#}"));
                    return;
                }
            };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }

        // ILAY: one element at location 0, R16G16_FLOAT (F16x2).
        let mut ilay = Vec::new();
        push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut ilay, 1); // element_count
        push_u32(&mut ilay, 0); // reserved0
                                // Element: semantic hash + index are arbitrary as long as signature matches.
        push_u32(&mut ilay, 0xDEAD_BEEFu32);
        push_u32(&mut ilay, 0);
        push_u32(&mut ilay, 34); // DXGI_FORMAT_R16G16_FLOAT
        push_u32(&mut ilay, 0); // input_slot
        push_u32(&mut ilay, 0); // aligned_byte_offset
        push_u32(&mut ilay, 0); // per-vertex
        push_u32(&mut ilay, 0); // step rate
        let layout = InputLayoutDesc::parse(&ilay).unwrap();

        let signature = [VsInputSignatureElement {
            semantic_name_hash: 0xDEAD_BEEF,
            semantic_index: 0,
            input_register: 0,
            mask: 0x3,
            shader_location: 0,
        }];

        let stride = 4u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).unwrap();

        // One vertex: f16x2 = (1.0, 0.5)
        let mut vb_bytes = Vec::new();
        vb_bytes.extend_from_slice(&0x3c00u16.to_le_bytes()); // 1.0
        vb_bytes.extend_from_slice(&0x3800u16.to_le_bytes()); // 0.5
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute f16 vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let ia_uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute f16 ia uniform"),
            size: ia_uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &ia_uniform_bytes);

        let cfg = VsAsComputeConfig {
            control_point_count: 1,
            out_reg_count: 1,
            indexed: false,
        };
        let pipeline = VsAsComputePipeline::new(&device, &pulling, cfg).unwrap();

        let mut scratch = ExpansionScratchAllocator::new(ExpansionScratchDescriptor::default());
        let vs_out_regs =
            alloc_vs_out_regs(&mut scratch, &device, 1, 1, cfg.out_reg_count).unwrap();

        let bg = pipeline
            .create_bind_group_group3(
                &device,
                &pulling,
                &[&vb],
                &ia_uniform,
                None,
                None,
                &vs_out_regs,
            )
            .unwrap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("VS-as-compute f16 encoder"),
        });
        pipeline.dispatch(&mut encoder, 1, 1, &bg).unwrap();
        queue.submit([encoder.finish()]);

        let bytes = read_back_buffer(
            &device,
            &queue,
            vs_out_regs.buffer.as_ref(),
            vs_out_regs.offset,
            vs_out_regs.size,
        )
        .await
        .unwrap();
        let words: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&bytes).to_vec();
        let vecs = unpack_vec4_u32_as_f32(&words);
        assert_eq!(vecs, vec![[1.0, 0.5, 0.0, 1.0]]);
    });
}

#[test]
fn vs_as_compute_loads_u16x2_input() {
    pollster::block_on(async {
        let (device, queue, supports_compute) =
            match common::wgpu::create_device_queue("aero-d3d11 VS-as-compute u16 test device")
                .await
            {
                Ok(v) => v,
                Err(err) => {
                    common::skip_or_panic(module_path!(), &format!("{err:#}"));
                    return;
                }
            };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }

        // ILAY: one element at location 0, R16G16_UINT (U16x2).
        let mut ilay = Vec::new();
        push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        push_u32(&mut ilay, 1); // element_count
        push_u32(&mut ilay, 0); // reserved0
                                // Element: semantic hash + index are arbitrary as long as signature matches.
        push_u32(&mut ilay, 0xDEAD_BEEFu32);
        push_u32(&mut ilay, 0);
        push_u32(&mut ilay, 36); // DXGI_FORMAT_R16G16_UINT
        push_u32(&mut ilay, 0); // input_slot
        push_u32(&mut ilay, 0); // aligned_byte_offset
        push_u32(&mut ilay, 0); // per-vertex
        push_u32(&mut ilay, 0); // step rate
        let layout = InputLayoutDesc::parse(&ilay).unwrap();

        let signature = [VsInputSignatureElement {
            semantic_name_hash: 0xDEAD_BEEF,
            semantic_index: 0,
            input_register: 0,
            mask: 0x3,
            shader_location: 0,
        }];

        let stride = 4u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).unwrap();

        // One vertex: u16x2 = (123, 456).
        let mut vb_bytes = Vec::new();
        vb_bytes.extend_from_slice(&123u16.to_le_bytes());
        vb_bytes.extend_from_slice(&456u16.to_le_bytes());
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute u16 vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let ia_uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VS-as-compute u16 ia uniform"),
            size: ia_uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &ia_uniform_bytes);

        let cfg = VsAsComputeConfig {
            control_point_count: 1,
            out_reg_count: 1,
            indexed: false,
        };
        let pipeline = VsAsComputePipeline::new(&device, &pulling, cfg).unwrap();

        let mut scratch = ExpansionScratchAllocator::new(ExpansionScratchDescriptor::default());
        let vs_out_regs =
            alloc_vs_out_regs(&mut scratch, &device, 1, 1, cfg.out_reg_count).unwrap();

        let bg = pipeline
            .create_bind_group_group3(
                &device,
                &pulling,
                &[&vb],
                &ia_uniform,
                None,
                None,
                &vs_out_regs,
            )
            .unwrap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("VS-as-compute u16 encoder"),
        });
        pipeline.dispatch(&mut encoder, 1, 1, &bg).unwrap();
        queue.submit([encoder.finish()]);

        let bytes = read_back_buffer(
            &device,
            &queue,
            vs_out_regs.buffer.as_ref(),
            vs_out_regs.offset,
            vs_out_regs.size,
        )
        .await
        .unwrap();
        let words: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&bytes).to_vec();
        let vecs = unpack_vec4_u32_as_f32(&words);
        assert_eq!(vecs, vec![[123.0, 456.0, 0.0, 1.0]]);
    });
}

#[test]
fn vs_as_compute_loads_extended_formats() {
    fn assert_approx(a: f32, b: f32, eps: f32) {
        let d = (a - b).abs();
        assert!(d <= eps, "expected {a} ~= {b} (eps={eps}), abs diff {d}");
    }

    fn assert_vec4_approx(got: [f32; 4], expected: [f32; 4]) {
        for i in 0..4 {
            assert_approx(got[i], expected[i], 1e-6);
        }
    }

    pollster::block_on(async {
        let (device, queue, supports_compute) = match common::wgpu::create_device_queue(
            "aero-d3d11 VS-as-compute extended format test device",
        )
        .await
        {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("{err:#}"));
                return;
            }
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }

        async fn run_case(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            dxgi_format: u32,
            mask: u8,
            stride: u32,
            vb_bytes: &[u8],
            expected: [f32; 4],
            assert_vec4: fn([f32; 4], [f32; 4]),
        ) {
            // ILAY: one element at location 0.
            let mut ilay = Vec::new();
            push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
            push_u32(&mut ilay, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
            push_u32(&mut ilay, 1); // element_count
            push_u32(&mut ilay, 0); // reserved0
                                     // Element: semantic hash + index are arbitrary as long as signature matches.
            push_u32(&mut ilay, 0xDEAD_BEEFu32);
            push_u32(&mut ilay, 0);
            push_u32(&mut ilay, dxgi_format);
            push_u32(&mut ilay, 0); // input_slot
            push_u32(&mut ilay, 0); // aligned_byte_offset
            push_u32(&mut ilay, 0); // per-vertex
            push_u32(&mut ilay, 0); // step rate
            let layout = InputLayoutDesc::parse(&ilay).unwrap();

            let signature = [VsInputSignatureElement {
                semantic_name_hash: 0xDEAD_BEEF,
                semantic_index: 0,
                input_register: 0,
                mask,
                shader_location: 0,
            }];

            let slot_strides = [stride];
            let binding = InputLayoutBinding::new(&layout, &slot_strides);
            let pulling = VertexPullingLayout::new(&binding, &signature).unwrap();

            let vb = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("VS-as-compute extended vb"),
                size: vb_bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vb, 0, vb_bytes);

            let ia_uniform_bytes = pulling.pack_uniform_bytes(
                &[VertexPullingSlot {
                    base_offset_bytes: 0,
                    stride_bytes: stride,
                }],
                VertexPullingDrawParams::default(),
            );
            let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("VS-as-compute extended ia uniform"),
                size: ia_uniform_bytes.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&ia_uniform, 0, &ia_uniform_bytes);

            let cfg = VsAsComputeConfig {
                control_point_count: 1,
                out_reg_count: 1,
                indexed: false,
            };
            let pipeline = VsAsComputePipeline::new(device, &pulling, cfg).unwrap();

            let mut scratch =
                ExpansionScratchAllocator::new(ExpansionScratchDescriptor::default());
            let vs_out_regs =
                alloc_vs_out_regs(&mut scratch, device, 1, 1, cfg.out_reg_count).unwrap();

            let bg = pipeline
                .create_bind_group_group3(
                    device,
                    &pulling,
                    &[&vb],
                    &ia_uniform,
                    None,
                    None,
                    &vs_out_regs,
                )
                .unwrap();

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("VS-as-compute extended encoder"),
            });
            pipeline.dispatch(&mut encoder, 1, 1, &bg).unwrap();
            queue.submit([encoder.finish()]);

            let bytes = read_back_buffer(
                device,
                queue,
                vs_out_regs.buffer.as_ref(),
                vs_out_regs.offset,
                vs_out_regs.size,
            )
            .await
            .unwrap();
            let words: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&bytes).to_vec();
            let vecs = unpack_vec4_u32_as_f32(&words);
            assert_eq!(vecs.len(), 1);
            assert_vec4(vecs[0], expected);
        }

        // i16x2 (-1, -32768)
        let mut vb_i16 = Vec::new();
        vb_i16.extend_from_slice(&(-1i16).to_le_bytes());
        vb_i16.extend_from_slice(&(-32768i16).to_le_bytes());
        run_case(
            &device,
            &queue,
            38,  // DXGI_FORMAT_R16G16_SINT
            0x3, // xy
            4,
            &vb_i16,
            [-1.0, -32768.0, 0.0, 1.0],
            assert_vec4_approx,
        )
        .await;

        // i8x4 (-1, 1, -128, 127)
        let vb_i8 = [(-1i8) as u8, 1u8, (-128i8) as u8, 127u8];
        run_case(
            &device,
            &queue,
            32,  // DXGI_FORMAT_R8G8B8A8_SINT
            0xF, // xyzw
            4,
            &vb_i8,
            [-1.0, 1.0, -128.0, 127.0],
            assert_vec4_approx,
        )
        .await;

        // unorm16x2 (0.0, 1.0)
        let mut vb_un16 = Vec::new();
        vb_un16.extend_from_slice(&0u16.to_le_bytes());
        vb_un16.extend_from_slice(&0xffffu16.to_le_bytes());
        run_case(
            &device,
            &queue,
            35,  // DXGI_FORMAT_R16G16_UNORM
            0x3, // xy
            4,
            &vb_un16,
            [0.0, 1.0, 0.0, 1.0],
            assert_vec4_approx,
        )
        .await;

        // snorm16x2 (-1.0, 1.0)
        let mut vb_sn16 = Vec::new();
        vb_sn16.extend_from_slice(&(-32768i16).to_le_bytes());
        vb_sn16.extend_from_slice(&(32767i16).to_le_bytes());
        run_case(
            &device,
            &queue,
            37,  // DXGI_FORMAT_R16G16_SNORM
            0x3, // xy
            4,
            &vb_sn16,
            [-1.0, 1.0, 0.0, 1.0],
            assert_vec4_approx,
        )
        .await;

        // f16x4 (1.0, -2.0, 4.0, 0.0)
        let mut vb_f16x4 = Vec::new();
        vb_f16x4.extend_from_slice(&0x3c00u16.to_le_bytes()); // 1.0
        vb_f16x4.extend_from_slice(&0xc000u16.to_le_bytes()); // -2.0
        vb_f16x4.extend_from_slice(&0x4400u16.to_le_bytes()); // 4.0
        vb_f16x4.extend_from_slice(&0x0000u16.to_le_bytes()); // 0.0
        run_case(
            &device,
            &queue,
            10,  // DXGI_FORMAT_R16G16B16A16_FLOAT
            0xF, // xyzw
            8,
            &vb_f16x4,
            [1.0, -2.0, 4.0, 0.0],
            assert_vec4_approx,
        )
        .await;

        // snorm8x4 (-1.0, 1.0, 0.0, -1/127)
        let vb_sn8 = [(-128i8) as u8, 127u8, 0u8, (-1i8) as u8];
        run_case(
            &device,
            &queue,
            31,  // DXGI_FORMAT_R8G8B8A8_SNORM
            0xF, // xyzw
            4,
            &vb_sn8,
            [-1.0, 1.0, 0.0, -(1.0 / 127.0)],
            assert_vec4_approx,
        )
        .await;
    });
}
