mod common;

use aero_d3d11::input_layout::{fnv1a_32, InputLayoutBinding, InputLayoutDesc, VsInputSignatureElement};
use aero_d3d11::runtime::expansion_scratch::{ExpansionScratchAllocator, ExpansionScratchDescriptor};
use aero_d3d11::runtime::index_pulling::{IndexPullingParams, INDEX_FORMAT_U16};
use aero_d3d11::runtime::tessellation::vs_as_compute::{alloc_vs_out_regs, VsAsComputeConfig, VsAsComputePipeline};
use aero_d3d11::runtime::vertex_pulling::{VertexPullingDrawParams, VertexPullingLayout, VertexPullingSlot};
use aero_d3d11::{parse_signatures, DxbcFile};
use anyhow::{anyhow, Context, Result};

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue, bool)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir()
                .join(format!("aero-d3d11-vs-as-compute-xdg-runtime-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::PRIMARY
        },
        ..Default::default()
    });

    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })
        .await
    {
        Some(adapter) => Some(adapter),
        None => {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
    }
    .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

    let supports_compute = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 VS-as-compute test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue, supports_compute))
}

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

#[test]
fn vs_as_compute_writes_vs_out_regs_non_indexed() {
    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
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
        let pulling = VertexPullingLayout::new(&binding, &vs_signature).context("pulling layout").unwrap();

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
        let vs_out_regs = alloc_vs_out_regs(&mut scratch, &device, vertex_count, instance_count, out_reg_count).unwrap();

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
        pipeline.dispatch(&mut encoder, vertex_count, instance_count, &bg);
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
    let mut words = vec![0u32; (indices.len() + 1) / 2];
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
        let (device, queue, supports_compute) = match create_device_queue().await {
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
        let pulling = VertexPullingLayout::new(&binding, &vs_signature).context("pulling layout").unwrap();

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
        let vs_out_regs = alloc_vs_out_regs(&mut scratch, &device, index_count, instance_count, out_reg_count).unwrap();

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
        pipeline.dispatch(&mut encoder, index_count, instance_count, &bg);
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
