mod common;

use aero_d3d11::input_layout::{
    fnv1a_32, InputLayoutDesc, VsInputSignatureElement, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
    AEROGPU_INPUT_LAYOUT_BLOB_VERSION, D3D11_APPEND_ALIGNED_ELEMENT,
};
use aero_d3d11::runtime::aerogpu_resources::AerogpuResourceManager;
use aero_d3d11::vertex_pulling::{build_vertex_pull_plan, emit_wgsl_pull_table, WGSL_VERTEX_PULLING_HELPERS};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
use anyhow::{anyhow, Context, Result};

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct OutVertex {
    pos: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    base_offset_bytes: u32,
    stride_bytes: u32,
    vertex_count: u32,
    _pad: u32,
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn build_pos2_color4_ilay_blob() -> Vec<u8> {
    let mut blob = Vec::new();
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut blob, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut blob, 2); // element_count
    push_u32(&mut blob, 0); // reserved0

    // POSITION0: R32G32_FLOAT, slot 0, offset 0.
    push_u32(&mut blob, fnv1a_32(b"POSITION"));
    push_u32(&mut blob, 0);
    push_u32(&mut blob, 16); // DXGI_FORMAT_R32G32_FLOAT
    push_u32(&mut blob, 0); // slot
    push_u32(&mut blob, 0); // aligned_byte_offset
    push_u32(&mut blob, 0); // per-vertex
    push_u32(&mut blob, 0); // instance step rate

    // COLOR0: R32G32B32A32_FLOAT, slot 0, append-aligned.
    push_u32(&mut blob, fnv1a_32(b"COLOR"));
    push_u32(&mut blob, 0);
    push_u32(&mut blob, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
    push_u32(&mut blob, 0); // slot
    push_u32(&mut blob, D3D11_APPEND_ALIGNED_ELEMENT);
    push_u32(&mut blob, 0); // per-vertex
    push_u32(&mut blob, 0); // instance step rate

    blob
}

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
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

    let downlevel = adapter.get_downlevel_capabilities();
    if !downlevel.flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS) {
        return Err(anyhow!(
            "wgpu: adapter does not support compute shaders (DownlevelFlags::COMPUTE_SHADERS missing)"
        ));
    }

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 vertex pulling test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue))
}

async fn read_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    size: u64,
) -> Result<Vec<u8>> {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vertex pulling readback staging"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("vertex pulling readback encoder"),
    });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
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

    let data = slice.get_mapped_range().to_vec();
    staging.unmap();
    Ok(data)
}

fn write_vertex(dst: &mut [u8], pos: [f32; 2], color: [f32; 4]) {
    dst[..8].copy_from_slice(bytemuck::bytes_of(&pos));
    dst[8..24].copy_from_slice(bytemuck::bytes_of(&color));
}

#[test]
fn gs_emulation_vertex_pulling_smoke() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return Ok::<(), anyhow::Error>(());
            }
        };

        let mut resources = AerogpuResourceManager::new(device, queue);

        // Create vertex buffers via the AeroGPU resource manager. This exercises the host-side
        // requirement that `AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER` implies `wgpu::BufferUsages::STORAGE`.
        resources
            .create_buffer(1, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 256, 0, 0)
            .context("create input vertex buffer")?;
        resources
            .create_buffer(2, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, 256, 0, 0)
            .context("create output vertex buffer")?;

        // Upload one input vertex: float2 POSITION + float4 COLOR.
        let mut vb_data = vec![0u8; 256];
        write_vertex(
            &mut vb_data,
            [0.0, 0.0],
            [1.0, 0.0, 0.0, 1.0], // red
        );
        resources
            .queue()
            .write_buffer(&resources.buffer(1)?.buffer, 0, &vb_data);

        let params = Params {
            base_offset_bytes: 0,
            stride_bytes: 24,
            vertex_count: 1,
            _pad: 0,
        };
        let params_buf = resources.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling params"),
            size: core::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        resources
            .queue()
            .write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));

        let ilay_blob = build_pos2_color4_ilay_blob();
        let layout = InputLayoutDesc::parse(&ilay_blob).expect("ILAY blob must parse");
        let vs_signature = vec![
            VsInputSignatureElement {
                semantic_name_hash: fnv1a_32(b"POSITION"),
                semantic_index: 0,
                input_register: 3,
                mask: 0x3, // xy
                shader_location: 0,
            },
            VsInputSignatureElement {
                semantic_name_hash: fnv1a_32(b"COLOR"),
                semantic_index: 0,
                input_register: 7,
                mask: 0xF,
                shader_location: 1,
            },
        ];
        let plan = build_vertex_pull_plan(&layout, &vs_signature, 0, params.stride_bytes, 0)
            .expect("vertex pull plan must build");

        let wgsl = format!(
            r#"
{helpers}

{pulls}

const VS_IN_REG_COUNT: u32 = 16u;
const POS_REG: u32 = {pos_reg}u;
const COLOR_REG: u32 = {color_reg}u;

struct Params {{
  base_offset_bytes: u32,
  stride_bytes: u32,
  vertex_count: u32,
  _pad: u32,
}}

struct OutVertex {{
  pos: vec4<f32>,
  color: vec4<f32>,
}}

@group(0) @binding(0) var<storage, read> vb_words: array<u32>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read_write> out_vertices: array<OutVertex>;

fn aero_expand_to_vec4(fmt: u32, vtx_byte_offset: u32) -> vec4<f32> {{
  if (fmt == 16u) {{
    let v = aero_load_vec2_f32(&vb_words, vtx_byte_offset);
    return vec4<f32>(v.x, v.y, 0.0, 1.0);
  }}
  if (fmt == 6u) {{
    let v = aero_load_vec3_f32(&vb_words, vtx_byte_offset);
    return vec4<f32>(v.x, v.y, v.z, 1.0);
  }}
  if (fmt == 2u) {{
    return aero_load_vec4_f32(&vb_words, vtx_byte_offset);
  }}
  return vec4<f32>(0.0);
}}

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  let vertex_id = gid.x;
  if (vertex_id >= params.vertex_count) {{
    return;
  }}

  var vs_in: array<vec4<f32>, VS_IN_REG_COUNT>;
  for (var i: u32 = 0u; i < VS_IN_REG_COUNT; i = i + 1u) {{
    vs_in[i] = vec4<f32>(0.0);
  }}

  for (var i: u32 = 0u; i < AERO_VERTEX_PULL_COUNT; i = i + 1u) {{
    let p = AERO_VERTEX_PULLS[i];
    let base = params.base_offset_bytes + vertex_id * params.stride_bytes + p.offset;
    let v = aero_expand_to_vec4(p.fmt, base);
    vs_in[p.reg] = aero_apply_mask(vs_in[p.reg], p.mask, v);
  }}

  let center = vs_in[POS_REG].xy;
  let color = vs_in[COLOR_REG];
  let s = 0.25;

  out_vertices[0] = OutVertex(pos: vec4<f32>(center.x, center.y + s, 0.0, 1.0), color: color);
  out_vertices[1] = OutVertex(pos: vec4<f32>(center.x - s, center.y - s, 0.0, 1.0), color: color);
  out_vertices[2] = OutVertex(pos: vec4<f32>(center.x + s, center.y - s, 0.0, 1.0), color: color);
}}
"#,
            helpers = WGSL_VERTEX_PULLING_HELPERS,
            pulls = emit_wgsl_pull_table(&plan.pulls),
            pos_reg = vs_signature[0].input_register,
            color_reg = vs_signature[1].input_register,
        );

        let shader = resources
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("vertex pulling cs"),
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
            });

        let bind_group_layout = resources
            .device()
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("vertex pulling bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
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

        let pipeline_layout = resources
            .device()
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vertex pulling pl"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline = resources
            .device()
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("vertex pulling pipeline"),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: "cs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            });

        let bind_group = resources.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertex pulling bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: resources.buffer(1)?.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: resources.buffer(2)?.buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = resources
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vertex pulling encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vertex pulling pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(params.vertex_count, 1, 1);
        }
        resources.queue().submit([encoder.finish()]);

        let out_bytes_0 = read_buffer(
            resources.device(),
            resources.queue(),
            &resources.buffer(2)?.buffer,
            256,
        )
        .await?;
        let out_0: &[OutVertex] = bytemuck::cast_slice(&out_bytes_0);
        assert_eq!(out_0[0].color, [1.0, 0.0, 0.0, 1.0]);

        // Now update vertex data and ensure output changes (proves vertex pulling works).
        write_vertex(
            &mut vb_data,
            [0.5, -0.5],
            [0.0, 1.0, 0.0, 1.0], // green
        );
        resources
            .queue()
            .write_buffer(&resources.buffer(1)?.buffer, 0, &vb_data);

        let mut encoder = resources
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vertex pulling encoder (2)"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vertex pulling pass (2)"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(params.vertex_count, 1, 1);
        }
        resources.queue().submit([encoder.finish()]);

        let out_bytes_1 = read_buffer(
            resources.device(),
            resources.queue(),
            &resources.buffer(2)?.buffer,
            256,
        )
        .await?;
        assert_ne!(out_bytes_0, out_bytes_1, "output buffer must change when vertex data changes");

        let out_1: &[OutVertex] = bytemuck::cast_slice(&out_bytes_1);
        assert_eq!(out_1[0].color, [0.0, 1.0, 0.0, 1.0]);

        Ok(())
    })
    .unwrap();
}
