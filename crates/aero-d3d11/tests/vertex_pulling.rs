mod common;

use aero_d3d11::input_layout::{InputLayoutBinding, InputLayoutDesc, VsInputSignatureElement};
use aero_d3d11::runtime::vertex_pulling::{
    VertexPullingDrawParams, VertexPullingLayout, VertexPullingSlot, VERTEX_PULLING_GROUP,
    VERTEX_PULLING_UNIFORM_BINDING, VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE,
};
use anyhow::{anyhow, Context, Result};

#[test]
fn vertex_pulling_wgsl_uses_reserved_binding_range() -> Result<()> {
    // ILAY_POS3_COLOR fixture: POSITION0 (float3) + COLOR0 (float4).
    let layout = InputLayoutDesc::parse(include_bytes!("fixtures/ilay_pos3_color.bin"))
        .context("parse ILAY")?;

    // Signature locations: POSITION0 -> location0, COLOR0 -> location1.
    let signature = [
        VsInputSignatureElement {
            semantic_name_hash: layout.elements[0].semantic_name_hash,
            semantic_index: layout.elements[0].semantic_index,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        },
        VsInputSignatureElement {
            semantic_name_hash: layout.elements[1].semantic_name_hash,
            semantic_index: layout.elements[1].semantic_index,
            input_register: 1,
            mask: 0xF,
            shader_location: 1,
        },
    ];

    let stride = 28u32; // float3 (12) + float4 (16)
    let slot_strides = [stride];
    let binding = InputLayoutBinding::new(&layout, &slot_strides);
    let pulling = VertexPullingLayout::new(&binding, &signature).context("build pulling")?;
    assert_eq!(pulling.slot_count(), 1);

    let wgsl = pulling.wgsl_prelude();

    // Vertex pulling bindings must live in the reserved internal/emulation range so they can be
    // combined with other emulation helpers without colliding with the D3D11 register-space
    // bindings (`b#`/`t#`/`s#`/`u#`).
    assert!(
        wgsl.contains(&format!(
            "@group({}) @binding({}) var<uniform> aero_vp_ia",
            VERTEX_PULLING_GROUP, VERTEX_PULLING_UNIFORM_BINDING
        )),
        "missing expected vertex pulling uniform binding in WGSL prelude:\n{wgsl}"
    );
    assert!(
        wgsl.contains(&format!(
            "@group({}) @binding({}) var<storage, read> aero_vp_vb0",
            VERTEX_PULLING_GROUP, VERTEX_PULLING_VERTEX_BUFFER_BINDING_BASE
        )),
        "missing expected vertex pulling vertex-buffer binding in WGSL prelude:\n{wgsl}"
    );

    Ok(())
}

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue, bool)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-d3d11-vertex-pulling-xdg-runtime-{}",
                std::process::id()
            ));
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
                label: Some("aero-d3d11 vertex pulling test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue, supports_compute))
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

fn create_vertex_pulling_pipeline_layout(
    device: &wgpu::Device,
    label: &str,
    out_bgl: &wgpu::BindGroupLayout,
    ia_bgl: &wgpu::BindGroupLayout,
    empty_bgl: &wgpu::BindGroupLayout,
) -> wgpu::PipelineLayout {
    // The vertex pulling bind group lives at `VERTEX_PULLING_GROUP`; pad intermediate groups with
    // empty layouts so the pipeline layout is valid regardless of which group index we choose for
    // the pulling bindings.
    let mut layouts = vec![empty_bgl; (VERTEX_PULLING_GROUP as usize).saturating_add(1).max(1)];
    layouts[0] = out_bgl;
    layouts[VERTEX_PULLING_GROUP as usize] = ia_bgl;

    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &layouts,
        push_constant_ranges: &[],
    })
}

#[test]
fn compute_vertex_pulling_reads_pos3_color4() {
    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return Ok(());
        }

        // ILAY_POS3_COLOR fixture: POSITION0 (float3) + COLOR0 (float4).
        let layout = InputLayoutDesc::parse(include_bytes!("fixtures/ilay_pos3_color.bin"))
            .context("parse ILAY")?;

        // Signature locations: POSITION0 -> location0, COLOR0 -> location1.
        let signature = [
            VsInputSignatureElement {
                semantic_name_hash: layout.elements[0].semantic_name_hash,
                semantic_index: layout.elements[0].semantic_index,
                input_register: 0,
                mask: 0xF,
                shader_location: 0,
            },
            VsInputSignatureElement {
                semantic_name_hash: layout.elements[1].semantic_name_hash,
                semantic_index: layout.elements[1].semantic_index,
                input_register: 1,
                mask: 0xF,
                shader_location: 1,
            },
        ];

        let stride = 28u32; // float3 (12) + float4 (16)
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).context("build pulling")?;
        assert_eq!(pulling.slot_count(), 1);
        assert_eq!(pulling.attributes.len(), 2);

        // Build a single vertex worth of data.
        let pos = [1.0f32, 2.0, 3.0];
        let col = [4.0f32, 5.0, 6.0, 7.0];
        let mut vb_bytes = Vec::with_capacity(stride as usize);
        for f in pos {
            vb_bytes.extend_from_slice(&f.to_le_bytes());
        }
        for f in col {
            vb_bytes.extend_from_slice(&f.to_le_bytes());
        }
        assert_eq!(vb_bytes.len(), stride as usize);

        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling test vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        assert_eq!(uniform_bytes.len() as u64, pulling.uniform_size_bytes());

        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling test uniform"),
            size: uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &uniform_bytes);

        // Output buffer: 7 f32s (pos.xyz + color.xyzw).
        let out_f32_count = 7u64;
        let out_size = out_f32_count * 4;
        let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling out"),
            size: out_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let pos_off = pulling
            .attributes
            .iter()
            .find(|a| a.shader_location == 0)
            .context("missing attribute at location 0")?
            .offset_bytes;
        let col_off = pulling
            .attributes
            .iter()
            .find(|a| a.shader_location == 1)
            .context("missing attribute at location 1")?
            .offset_bytes;

        let prelude = pulling.wgsl_prelude();
        let wgsl = format!(
            r#"
{prelude}

@group(0) @binding(0) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  // One invocation == one vertex.
  let vertex: u32 = aero_vp_ia.first_vertex + gid.x;
  let base: u32 = aero_vp_ia.slots[0].base_offset_bytes + vertex * aero_vp_ia.slots[0].stride_bytes;

  let pos: vec3<f32> = load_attr_f32x3(0u, base + {pos_off}u);
  let col: vec4<f32> = load_attr_f32x4(0u, base + {col_off}u);

  out[0] = pos.x;
  out[1] = pos.y;
  out[2] = pos.z;
  out[3] = col.x;
  out[4] = col.y;
  out[5] = col.z;
  out[6] = col.w;
}}
"#,
            pos_off = pos_off,
            col_off = col_off,
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vertex pulling shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let out_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling out bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let out_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertex pulling out bg"),
            layout: &out_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: out_buf.as_entire_binding(),
            }],
        });

        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling empty bgl"),
            entries: &[],
        });

        let ia_bgl = pulling.create_bind_group_layout(&device);
        let ia_bg = pulling.create_bind_group(
            &device,
            &ia_bgl,
            &[&vb],
            wgpu::BufferBinding {
                buffer: &ia_uniform,
                offset: 0,
                size: None,
            },
        );
        let pipeline_layout = create_vertex_pulling_pipeline_layout(
            &device,
            "vertex pulling pipeline layout",
            &out_bgl,
            &ia_bgl,
            &empty_bgl,
        );

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vertex pulling pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "main",
            compilation_options: Default::default(),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vertex pulling encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vertex pulling pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &out_bg, &[]);
            pass.set_bind_group(VERTEX_PULLING_GROUP, &ia_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);

        let bytes = read_buffer(&device, &queue, &out_buf, out_size).await?;
        let mut got = Vec::new();
        for chunk in bytes.chunks_exact(4) {
            got.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        assert_eq!(got.len(), out_f32_count as usize);

        let expected = [
            1.0f32, 2.0, 3.0, // pos
            4.0, 5.0, 6.0, 7.0, // color
        ];
        assert_eq!(got, expected);

        Ok::<_, anyhow::Error>(())
    })
    .unwrap();
}

#[test]
fn compute_vertex_pulling_reads_unorm8x4() {
    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn assert_approx(a: f32, b: f32, eps: f32) {
        let d = (a - b).abs();
        assert!(d <= eps, "expected {a} ~= {b} (eps={eps}), abs diff {d}");
    }

    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return Ok(());
        }

        // Build a tiny ILAY: COLOR0 as R8G8B8A8_UNORM at offset 0 in slot 0.
        let color_hash = aero_d3d11::input_layout::fnv1a_32(b"COLOR");
        let mut blob = Vec::new();
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
        );
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
        );
        push_u32(&mut blob, 1); // element_count
        push_u32(&mut blob, 0); // reserved0

        push_u32(&mut blob, color_hash);
        push_u32(&mut blob, 0); // semantic index
        push_u32(&mut blob, 28); // DXGI_FORMAT_R8G8B8A8_UNORM
        push_u32(&mut blob, 0); // input_slot
        push_u32(&mut blob, 0); // offset
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).context("parse ILAY")?;

        let signature = [VsInputSignatureElement {
            semantic_name_hash: color_hash,
            semantic_index: 0,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        }];

        let stride = 4u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).context("build pulling")?;

        let vb_bytes = [0u8, 127u8, 255u8, 1u8]; // RGBA
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unorm vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unorm uniform"),
            size: uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &uniform_bytes);

        // Output buffer: 4 f32s (RGBA).
        let out_size = 16u64;
        let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unorm out"),
            size: out_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let prelude = pulling.wgsl_prelude();
        let wgsl = format!(
            r#"
{prelude}

@group(0) @binding(0) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  let vertex: u32 = aero_vp_ia.first_vertex + gid.x;
  let base: u32 = aero_vp_ia.slots[0].base_offset_bytes + vertex * aero_vp_ia.slots[0].stride_bytes;
  let col: vec4<f32> = load_attr_unorm8x4(0u, base);
  out[0] = col.x;
  out[1] = col.y;
  out[2] = col.z;
  out[3] = col.w;
}}
"#
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vertex pulling unorm shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let out_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling unorm out bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let out_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertex pulling unorm out bg"),
            layout: &out_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: out_buf.as_entire_binding(),
            }],
        });

        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling unorm empty bgl"),
            entries: &[],
        });

        let ia_bgl = pulling.create_bind_group_layout(&device);
        let ia_bg = pulling.create_bind_group(
            &device,
            &ia_bgl,
            &[&vb],
            wgpu::BufferBinding {
                buffer: &ia_uniform,
                offset: 0,
                size: None,
            },
        );
        let pipeline_layout = create_vertex_pulling_pipeline_layout(
            &device,
            "vertex pulling unorm pipeline layout",
            &out_bgl,
            &ia_bgl,
            &empty_bgl,
        );

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vertex pulling unorm pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "main",
            compilation_options: Default::default(),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vertex pulling unorm encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vertex pulling unorm pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &out_bg, &[]);
            pass.set_bind_group(VERTEX_PULLING_GROUP, &ia_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);

        let bytes = read_buffer(&device, &queue, &out_buf, out_size).await?;
        let mut got = Vec::new();
        for chunk in bytes.chunks_exact(4) {
            got.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        assert_eq!(got.len(), 4);

        // Compare with a small epsilon: the only nontrivial values come from division by 255.
        let expected = [0.0f32, 127.0f32 / 255.0, 1.0f32, 1.0f32 / 255.0];
        for (a, b) in got.iter().copied().zip(expected) {
            assert_approx(a, b, 1e-6);
        }

        Ok::<_, anyhow::Error>(())
    })
    .unwrap();
}

#[test]
fn compute_vertex_pulling_reads_unorm10_10_10_2() {
    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn assert_approx(a: f32, b: f32, eps: f32) {
        let d = (a - b).abs();
        assert!(d <= eps, "expected {a} ~= {b} (eps={eps}), abs diff {d}");
    }

    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return Ok(());
        }

        // Build a tiny ILAY: COLOR0 as R10G10B10A2_UNORM at offset 0 in slot 0.
        let color_hash = aero_d3d11::input_layout::fnv1a_32(b"COLOR");
        let mut blob = Vec::new();
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
        );
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
        );
        push_u32(&mut blob, 1); // element_count
        push_u32(&mut blob, 0); // reserved0

        push_u32(&mut blob, color_hash);
        push_u32(&mut blob, 0); // semantic index
        push_u32(&mut blob, 24); // DXGI_FORMAT_R10G10B10A2_UNORM
        push_u32(&mut blob, 0); // input_slot
        push_u32(&mut blob, 0); // offset
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).context("parse ILAY")?;
        let signature = [VsInputSignatureElement {
            semantic_name_hash: color_hash,
            semantic_index: 0,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        }];

        let stride = 4u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).context("build pulling")?;

        // Pack (R,G,B,A) as (0, 512/1023, 1, 1/3).
        let r: u32 = 0;
        let g: u32 = 512;
        let b: u32 = 1023;
        let a: u32 = 1;
        let packed: u32 =
            (r & 0x3ff) | ((g & 0x3ff) << 10) | ((b & 0x3ff) << 20) | ((a & 0x3) << 30);
        let vb_bytes = packed.to_le_bytes();

        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unorm10 vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unorm10 uniform"),
            size: uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &uniform_bytes);

        // Output buffer: 4 f32s (RGBA).
        let out_size = 16u64;
        let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unorm10 out"),
            size: out_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let prelude = pulling.wgsl_prelude();
        let wgsl = format!(
            r#"
{prelude}

@group(0) @binding(0) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  let vertex: u32 = aero_vp_ia.first_vertex + gid.x;
  let base: u32 = aero_vp_ia.slots[0].base_offset_bytes + vertex * aero_vp_ia.slots[0].stride_bytes;
  let col: vec4<f32> = load_attr_unorm10_10_10_2(0u, base);
  out[0] = col.x;
  out[1] = col.y;
  out[2] = col.z;
  out[3] = col.w;
}}
"#
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vertex pulling unorm10 shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let out_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling unorm10 out bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let out_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertex pulling unorm10 out bg"),
            layout: &out_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: out_buf.as_entire_binding(),
            }],
        });

        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling unorm10 empty bgl"),
            entries: &[],
        });

        let ia_bgl = pulling.create_bind_group_layout(&device);
        let ia_bg = pulling.create_bind_group(
            &device,
            &ia_bgl,
            &[&vb],
            wgpu::BufferBinding {
                buffer: &ia_uniform,
                offset: 0,
                size: None,
            },
        );
        let pipeline_layout = create_vertex_pulling_pipeline_layout(
            &device,
            "vertex pulling unorm10 pipeline layout",
            &out_bgl,
            &ia_bgl,
            &empty_bgl,
        );

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vertex pulling unorm10 pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "main",
            compilation_options: Default::default(),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vertex pulling unorm10 encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vertex pulling unorm10 pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &out_bg, &[]);
            pass.set_bind_group(VERTEX_PULLING_GROUP, &ia_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);

        let bytes = read_buffer(&device, &queue, &out_buf, out_size).await?;
        let mut got = Vec::new();
        for chunk in bytes.chunks_exact(4) {
            got.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        assert_eq!(got.len(), 4);

        let expected = [0.0f32, 512.0f32 / 1023.0, 1.0f32, 1.0f32 / 3.0];
        for (a, b) in got.iter().copied().zip(expected) {
            assert_approx(a, b, 1e-6);
        }

        Ok::<_, anyhow::Error>(())
    })
    .unwrap();
}

#[test]
fn compute_vertex_pulling_handles_unaligned_base_offset() {
    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return Ok(());
        }

        // ILAY: VALUE0 as R32_FLOAT at offset 0 in slot 0.
        let value_hash = aero_d3d11::input_layout::fnv1a_32(b"VALUE");
        let mut blob = Vec::new();
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
        );
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
        );
        push_u32(&mut blob, 1); // element_count
        push_u32(&mut blob, 0); // reserved0

        push_u32(&mut blob, value_hash);
        push_u32(&mut blob, 0); // semantic index
        push_u32(&mut blob, 41); // DXGI_FORMAT_R32_FLOAT
        push_u32(&mut blob, 0); // input_slot
        push_u32(&mut blob, 0); // offset
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).context("parse ILAY")?;
        let signature = [VsInputSignatureElement {
            semantic_name_hash: value_hash,
            semantic_index: 0,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        }];

        let stride = 4u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).context("build pulling")?;

        // Vertex buffer bytes:
        // - First byte is padding (0xAA).
        // - Next 4 bytes encode f32=1.0 in little endian.
        // - Remaining bytes are unused.
        let mut vb_bytes = vec![0u8; 8];
        vb_bytes[0] = 0xAA;
        vb_bytes[1..5].copy_from_slice(&1.0f32.to_le_bytes());

        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unaligned vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 1,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unaligned uniform"),
            size: uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &uniform_bytes);

        let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling unaligned out"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let prelude = pulling.wgsl_prelude();
        let wgsl = format!(
            r#"
{prelude}

@group(0) @binding(0) var<storage, read_write> out: array<u32>;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  let vertex: u32 = aero_vp_ia.first_vertex + gid.x;
  let base: u32 = aero_vp_ia.slots[0].base_offset_bytes + vertex * aero_vp_ia.slots[0].stride_bytes;
  let v: f32 = load_attr_f32(0u, base);
  out[0] = bitcast<u32>(v);
}}
"#
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vertex pulling unaligned shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let out_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling unaligned out bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let out_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertex pulling unaligned out bg"),
            layout: &out_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: out_buf.as_entire_binding(),
            }],
        });

        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling unaligned empty bgl"),
            entries: &[],
        });
        let ia_bgl = pulling.create_bind_group_layout(&device);
        let ia_bg = pulling.create_bind_group(
            &device,
            &ia_bgl,
            &[&vb],
            wgpu::BufferBinding {
                buffer: &ia_uniform,
                offset: 0,
                size: None,
            },
        );

        let pipeline_layout = create_vertex_pulling_pipeline_layout(
            &device,
            "vertex pulling unaligned pipeline layout",
            &out_bgl,
            &ia_bgl,
            &empty_bgl,
        );

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vertex pulling unaligned pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "main",
            compilation_options: Default::default(),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vertex pulling unaligned encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vertex pulling unaligned pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &out_bg, &[]);
            pass.set_bind_group(VERTEX_PULLING_GROUP, &ia_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);

        let bytes = read_buffer(&device, &queue, &out_buf, 4).await?;
        assert_eq!(bytes.len(), 4);
        let got = f32::from_le_bytes(bytes.try_into().unwrap());
        assert_eq!(got, 1.0);

        Ok::<_, anyhow::Error>(())
    })
    .unwrap();
}

#[test]
fn compute_vertex_pulling_oob_reads_return_zero() {
    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return Ok(());
            }
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return Ok(());
        }

        // ILAY: VALUE0 as R32G32_FLOAT at offset 0 in slot 0.
        let value_hash = aero_d3d11::input_layout::fnv1a_32(b"VALUE");
        let mut blob = Vec::new();
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC,
        );
        push_u32(
            &mut blob,
            aero_d3d11::input_layout::AEROGPU_INPUT_LAYOUT_BLOB_VERSION,
        );
        push_u32(&mut blob, 1); // element_count
        push_u32(&mut blob, 0); // reserved0

        push_u32(&mut blob, value_hash);
        push_u32(&mut blob, 0); // semantic index
        push_u32(&mut blob, 16); // DXGI_FORMAT_R32G32_FLOAT
        push_u32(&mut blob, 0); // input_slot
        push_u32(&mut blob, 0); // offset
        push_u32(&mut blob, 0); // per-vertex
        push_u32(&mut blob, 0); // step rate

        let layout = InputLayoutDesc::parse(&blob).context("parse ILAY")?;
        let signature = [VsInputSignatureElement {
            semantic_name_hash: value_hash,
            semantic_index: 0,
            input_register: 0,
            mask: 0xF,
            shader_location: 0,
        }];

        let stride = 8u32;
        let slot_strides = [stride];
        let binding = InputLayoutBinding::new(&layout, &slot_strides);
        let pulling = VertexPullingLayout::new(&binding, &signature).context("build pulling")?;

        // Only provide one f32 (4 bytes). The second f32 read should be out-of-bounds and return 0.
        let vb_bytes = 123.0f32.to_le_bytes();
        let vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling oob vb"),
            size: vb_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vb, 0, &vb_bytes);

        let uniform_bytes = pulling.pack_uniform_bytes(
            &[VertexPullingSlot {
                base_offset_bytes: 0,
                stride_bytes: stride,
            }],
            VertexPullingDrawParams::default(),
        );
        let ia_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling oob uniform"),
            size: uniform_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ia_uniform, 0, &uniform_bytes);

        let out_size = 8u64;
        let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex pulling oob out"),
            size: out_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let prelude = pulling.wgsl_prelude();
        let wgsl = format!(
            r#"
{prelude}

@group(0) @binding(0) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
  let vertex: u32 = aero_vp_ia.first_vertex + gid.x;
  let base: u32 = aero_vp_ia.slots[0].base_offset_bytes + vertex * aero_vp_ia.slots[0].stride_bytes;
  let v: vec2<f32> = load_attr_f32x2(0u, base);
  out[0] = v.x;
  out[1] = v.y;
}}
"#
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vertex pulling oob shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let out_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling oob out bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let out_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertex pulling oob out bg"),
            layout: &out_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: out_buf.as_entire_binding(),
            }],
        });

        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vertex pulling oob empty bgl"),
            entries: &[],
        });
        let ia_bgl = pulling.create_bind_group_layout(&device);
        let ia_bg = pulling.create_bind_group(
            &device,
            &ia_bgl,
            &[&vb],
            wgpu::BufferBinding {
                buffer: &ia_uniform,
                offset: 0,
                size: None,
            },
        );

        let pipeline_layout = create_vertex_pulling_pipeline_layout(
            &device,
            "vertex pulling oob pipeline layout",
            &out_bgl,
            &ia_bgl,
            &empty_bgl,
        );

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vertex pulling oob pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "main",
            compilation_options: Default::default(),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vertex pulling oob encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vertex pulling oob pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &out_bg, &[]);
            pass.set_bind_group(VERTEX_PULLING_GROUP, &ia_bg, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);

        let bytes = read_buffer(&device, &queue, &out_buf, out_size).await?;
        let got: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(got, vec![123.0, 0.0]);

        Ok::<_, anyhow::Error>(())
    })
    .unwrap();
}
