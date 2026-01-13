mod common;

use aero_d3d11::runtime::index_pulling::{
    wgsl_index_pulling_lib, IndexPullingParams, INDEX_FORMAT_U16, INDEX_FORMAT_U32,
};
use anyhow::{anyhow, Context, Result};

fn pack_u16_indices_to_words(indices: &[u16]) -> Vec<u32> {
    let mut words = vec![0u32; (indices.len() + 1) / 2];
    for (i, &idx) in indices.iter().enumerate() {
        let word_idx = i / 2;
        let shift = (i % 2) * 16;
        words[word_idx] |= (idx as u32) << shift;
    }
    words
}

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue, bool)> {
    common::wgpu::create_device_queue("aero-d3d11 index_pulling test device").await
}

async fn run_index_pulling_case(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    index_words: &[u32],
    params: IndexPullingParams,
    index_count: u32,
    instance_count: u32,
) -> Result<Vec<i32>> {
    let total_invocations_u64 = (index_count as u64)
        .checked_mul(instance_count as u64)
        .ok_or_else(|| anyhow!("total invocations overflow"))?;
    let total_invocations: u32 = total_invocations_u64
        .try_into()
        .map_err(|_| anyhow!("total invocations out of u32 range"))?;

    let index_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("index_pulling index buffer"),
        size: (index_words.len() * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&index_buf, 0, bytemuck::cast_slice(index_words));

    let out_bytes = total_invocations_u64
        .checked_mul(4)
        .ok_or_else(|| anyhow!("output size overflow"))?;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("index_pulling output buffer"),
        size: out_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("index_pulling params buffer"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&params_buf, 0, &params.to_le_bytes());

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("index_pulling bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
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

    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("index_pulling bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: index_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("index_pulling pipeline layout"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });

    let lib = wgsl_index_pulling_lib(/*group=*/ 0, /*params_binding=*/ 0, /*index_binding=*/ 1);
    let wgsl = format!(
        r#"
{lib}

@group(0) @binding(2)
var<storage, read_write> out_ids: array<i32>;

const INDEX_COUNT: u32 = {index_count}u;
const TOTAL: u32 = {total_invocations}u;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let id = gid.x;
    if (id >= TOTAL) {{
        return;
    }}
    let index_in_draw = id % INDEX_COUNT;
    out_ids[id] = index_pulling_resolve_vertex_id(index_in_draw);
}}
"#
    );

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("index_pulling shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("index_pulling pipeline"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: "cs_main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("index_pulling readback"),
        size: out_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("index_pulling encoder"),
    });

    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("index_pulling pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(total_invocations, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &staging, 0, out_bytes);
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

    let mapped = slice.get_mapped_range();
    let values: Vec<i32> = bytemuck::cast_slice::<u8, i32>(&mapped).to_vec();
    drop(mapped);
    staging.unmap();
    Ok(values)
}

#[test]
fn index_pulling_reads_u16_and_u32_and_applies_first_index_and_base_vertex() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::index_pulling_reads_u16_and_u32_and_applies_first_index_and_base_vertex"
        );
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("{err:#}"));
                return;
            }
        };
        if !supports_compute {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }

        // --- u16 indices (with instancing) ---
        let indices_u16: Vec<u16> = vec![0, 1, 2, 3, 4, 5];
        let words_u16 = pack_u16_indices_to_words(&indices_u16);
        let params = IndexPullingParams {
            first_index: 1,
            base_vertex: -3,
            index_format: INDEX_FORMAT_U16,
            _pad0: 0,
        };
        let got = run_index_pulling_case(&device, &queue, &words_u16, params, 4, 2)
            .await
            .unwrap();
        let expected: Vec<i32> = vec![-2, -1, 0, 1, -2, -1, 0, 1];
        assert_eq!(got, expected, "u16 index pulling mismatch");

        // --- u32 indices ---
        let indices_u32: Vec<u32> = vec![70_000, 70_001, 70_002, 70_003, 70_004];
        let params = IndexPullingParams {
            first_index: 1,
            base_vertex: -70_002,
            index_format: INDEX_FORMAT_U32,
            _pad0: 0,
        };
        let got = run_index_pulling_case(&device, &queue, &indices_u32, params, 3, 1)
            .await
            .unwrap();
        let expected: Vec<i32> = vec![-1, 0, 1];
        assert_eq!(got, expected, "u32 index pulling mismatch");
    });
}
