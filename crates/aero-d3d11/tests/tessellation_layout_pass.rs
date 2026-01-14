mod common;

use aero_d3d11::runtime::indirect_args::DrawIndexedIndirectArgs;
use aero_d3d11::runtime::tessellation::layout_pass::wgsl_tessellation_layout_pass;
use aero_d3d11::runtime::tessellation::{TessellationLayoutParams, TessellationLayoutPatchMeta};
use anyhow::{anyhow, Context, Result};

async fn create_device_queue() -> Result<(wgpu::Device, wgpu::Queue, wgpu::DownlevelCapabilities)> {
    common::wgpu::create_device_queue_with_downlevel(
        "aero-d3d11 tessellation_layout_pass test device",
    )
    .await
}

async fn map_read_buffer(device: &wgpu::Device, buf: &wgpu::Buffer) -> Result<Vec<u8>> {
    let slice = buf.slice(..);
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
    buf.unmap();
    Ok(data)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| anyhow!("u32 offset overflow"))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| anyhow!("u32 read out of bounds"))?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| anyhow!("i32 offset overflow"))?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| anyhow!("i32 read out of bounds"))?;
    Ok(i32::from_le_bytes(slice.try_into().unwrap()))
}

async fn run_layout_pass_with_hs_factors_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    patch_count: u32,
    hs_factors_buf: &wgpu::Buffer,
    hs_factors_size: u64,
    max_vertices: u32,
    max_indices: u32,
) -> Result<(Vec<(u32, u32, u32, u32, u32)>, DrawIndexedIndirectArgs, u32)> {
    let (meta_stride, _) = TessellationLayoutPatchMeta::layout();
    let meta_total_size = (patch_count as u64)
        .checked_mul(meta_stride)
        .ok_or_else(|| anyhow!("meta_total_size overflow"))?;
    let out_meta_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout out_meta"),
        size: meta_total_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let args_size = DrawIndexedIndirectArgs::SIZE_BYTES;
    let out_args_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout out_args"),
        size: args_size,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::INDIRECT
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    // `wgsl_tessellation_layout_pass` optionally includes debug counters when the
    // `tessellation_debug_counters` feature is enabled. In the default configuration it writes
    // only a single `u32` flag.
    let out_debug_size: u64 = if cfg!(feature = "tessellation_debug_counters") {
        16
    } else {
        4
    };
    let out_debug_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout out_debug"),
        size: out_debug_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let params = TessellationLayoutParams {
        patch_count,
        max_vertices,
        max_indices,
        _pad0: 0,
    };
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout params"),
        size: TessellationLayoutParams::layout().0,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&params_buf, 0, &params.to_le_bytes());

    let layout_wgsl = wgsl_tessellation_layout_pass(
        /*group=*/ 0, /*params_binding=*/ 0, /*hs_tess_factors_binding=*/ 1,
        /*out_patch_meta_binding=*/ 2, /*out_indirect_args_binding=*/ 3,
        /*out_debug_binding=*/ 4,
    );
    let layout_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("tess_layout layout_pass shader"),
        source: wgpu::ShaderSource::Wgsl(layout_wgsl.into()),
    });

    let layout_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tess_layout layout_pass bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(TessellationLayoutParams::layout().0),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    // `wgsl_tessellation_layout_pass` declares this binding as `read_write` to avoid
                    // wgpu `ResourceUsageConflict` validation errors when the tessellation runtime
                    // allocates all scratch buffers from a single backing buffer. Keep this test's
                    // bind group layout in sync with the shader's declared access mode.
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(hs_factors_size),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(meta_total_size),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(args_size),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(out_debug_size),
                },
                count: None,
            },
        ],
    });

    let layout_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tess_layout layout_pass bg"),
        layout: &layout_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: hs_factors_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: out_meta_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: out_args_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: out_debug_buf.as_entire_binding(),
            },
        ],
    });

    let layout_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("tess_layout layout_pass pipeline layout"),
        bind_group_layouts: &[&layout_bgl],
        push_constant_ranges: &[],
    });

    let layout_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("tess_layout layout_pass pipeline"),
        layout: Some(&layout_pl),
        module: &layout_module,
        entry_point: "cs_main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });

    // Readback staging buffers.
    let meta_staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout meta readback"),
        size: meta_total_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let args_staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout args readback"),
        size: args_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let debug_staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout debug readback"),
        size: out_debug_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tess_layout encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("tess_layout layout_pass pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&layout_pipeline);
        pass.set_bind_group(0, &layout_bg, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_meta_buf, 0, &meta_staging, 0, meta_total_size);
    encoder.copy_buffer_to_buffer(&out_args_buf, 0, &args_staging, 0, args_size);
    encoder.copy_buffer_to_buffer(&out_debug_buf, 0, &debug_staging, 0, out_debug_size);

    queue.submit([encoder.finish()]);

    let meta_bytes = map_read_buffer(device, &meta_staging).await?;
    let args_bytes = map_read_buffer(device, &args_staging).await?;
    let debug_bytes = map_read_buffer(device, &debug_staging).await?;

    // Parse meta (5x u32).
    let stride: usize = meta_stride
        .try_into()
        .map_err(|_| anyhow!("meta_stride out of range"))?;
    let mut meta = Vec::with_capacity(patch_count as usize);
    for i in 0..patch_count as usize {
        let base = i
            .checked_mul(stride)
            .ok_or_else(|| anyhow!("meta offset overflow"))?;
        meta.push((
            read_u32(&meta_bytes, base)?,
            read_u32(&meta_bytes, base + 4)?,
            read_u32(&meta_bytes, base + 8)?,
            read_u32(&meta_bytes, base + 12)?,
            read_u32(&meta_bytes, base + 16)?,
        ));
    }

    // Parse DrawIndexedIndirectArgs.
    let (args_struct_size, args_align) = DrawIndexedIndirectArgs::layout();
    assert_eq!(args_align, 4);
    let expected_size: usize = args_struct_size
        .try_into()
        .map_err(|_| anyhow!("args_struct_size out of range"))?;
    if args_bytes.len() != expected_size {
        return Err(anyhow!(
            "indirect args readback size mismatch (got={} expected={})",
            args_bytes.len(),
            expected_size
        ));
    }
    let args = DrawIndexedIndirectArgs {
        index_count: read_u32(&args_bytes, 0)?,
        instance_count: read_u32(&args_bytes, 4)?,
        first_index: read_u32(&args_bytes, 8)?,
        base_vertex: read_i32(&args_bytes, 12)?,
        first_instance: read_u32(&args_bytes, 16)?,
    };

    let debug = read_u32(&debug_bytes, 0)?;
    Ok((meta, args, debug))
}

async fn run_layout_pass_case(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    patch_count: u32,
    tess_factor: f32,
    max_vertices: u32,
    max_indices: u32,
) -> Result<(Vec<(u32, u32, u32, u32, u32)>, DrawIndexedIndirectArgs, u32)> {
    // hs_tess_factors: array<vec4<f32>> (16 bytes per patch).
    let hs_factors_size = (patch_count as u64)
        .checked_mul(16)
        .ok_or_else(|| anyhow!("hs_factors_size overflow"))?;
    let hs_factors_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess_layout hs_factors"),
        size: hs_factors_size,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });

    // --- HS patch-constant test shader: writes fixed tess factors (GPU-only) ---
    let hs_pc_wgsl = format!(
        r#"
@group(0) @binding(0)
var<storage, read_write> hs_tess_factors: array<vec4<f32>>;

const PATCH_COUNT: u32 = {patch_count}u;
const FACTOR: f32 = {tess_factor};

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    if (gid.x != 0u) {{
        return;
    }}
    for (var i: u32 = 0u; i < PATCH_COUNT; i = i + 1u) {{
        hs_tess_factors[i] = vec4<f32>(FACTOR, FACTOR, FACTOR, FACTOR);
    }}
}}
"#
    );

    let hs_pc_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tess_layout hs_pc bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(hs_factors_size),
            },
            count: None,
        }],
    });

    let hs_pc_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tess_layout hs_pc bg"),
        layout: &hs_pc_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: hs_factors_buf.as_entire_binding(),
        }],
    });

    let hs_pc_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("tess_layout hs_pc shader"),
        source: wgpu::ShaderSource::Wgsl(hs_pc_wgsl.into()),
    });

    let hs_pc_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("tess_layout hs_pc pipeline layout"),
        bind_group_layouts: &[&hs_pc_bgl],
        push_constant_ranges: &[],
    });

    let hs_pc_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("tess_layout hs_pc pipeline"),
        layout: Some(&hs_pc_pl),
        module: &hs_pc_module,
        entry_point: "cs_main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tess_layout encoder"),
    });

    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("tess_layout hs_pc pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&hs_pc_pipeline);
        pass.set_bind_group(0, &hs_pc_bg, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }

    queue.submit([encoder.finish()]);

    run_layout_pass_with_hs_factors_buffer(
        device,
        queue,
        patch_count,
        &hs_factors_buf,
        hs_factors_size,
        max_vertices,
        max_indices,
    )
    .await
}

#[test]
fn tessellation_layout_pass_prefix_sums_and_indirect_args_match_expected() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::tessellation_layout_pass_prefix_sums_and_indirect_args_match_expected"
        );

        let (device, queue, downlevel) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        let supports_compute = downlevel
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);
        if !supports_compute {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }
        if !downlevel
            .flags
            .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION)
        {
            common::skip_or_panic(test_name, "indirect execution unsupported");
            return;
        }

        let patch_count = 3u32;
        let tess_factor = 4.0f32;

        // Triangle domain formulas (n=tess_level):
        // vertices=(n+1)(n+2)/2, indices=3*n^2.
        let expected_vertex_count = (tess_factor as u32 + 1) * (tess_factor as u32 + 2) / 2;
        let expected_index_count = 3 * (tess_factor as u32) * (tess_factor as u32);

        let max_vertices = expected_vertex_count * patch_count;
        let max_indices = expected_index_count * patch_count;

        let (meta, args, debug) = run_layout_pass_case(
            &device,
            &queue,
            patch_count,
            tess_factor,
            max_vertices,
            max_indices,
        )
        .await
        .unwrap();

        assert_eq!(debug, 0, "unexpected debug flag (should not clamp)");

        // Indirect args must cover the entire expanded index stream.
        assert_eq!(
            args.index_count,
            expected_index_count * patch_count,
            "indirect index_count mismatch"
        );
        assert_eq!(args.instance_count, 1);
        assert_eq!(args.first_index, 0);
        assert_eq!(args.base_vertex, 0);
        assert_eq!(args.first_instance, 0);

        // Per-patch offsets must be monotonic and contiguous.
        let mut running_v = 0u32;
        let mut running_i = 0u32;
        for (patch_id, (level, v_base, i_base, v_count, i_count)) in
            meta.iter().copied().enumerate()
        {
            assert_eq!(
                level, tess_factor as u32,
                "tess_level mismatch for patch {patch_id}"
            );
            assert_eq!(
                v_base, running_v,
                "vertex_base mismatch for patch {patch_id}"
            );
            assert_eq!(
                i_base, running_i,
                "index_base mismatch for patch {patch_id}"
            );
            assert_eq!(
                v_count, expected_vertex_count,
                "vertex_count mismatch for patch {patch_id}"
            );
            assert_eq!(
                i_count, expected_index_count,
                "index_count mismatch for patch {patch_id}"
            );

            assert!(
                v_base + v_count <= max_vertices,
                "vertex range out of bounds for patch {patch_id}"
            );
            assert!(
                i_base + i_count <= max_indices,
                "index range out of bounds for patch {patch_id}"
            );

            running_v += v_count;
            running_i += i_count;
        }
    });
}

#[test]
fn tessellation_layout_pass_clamps_to_capacity_and_sets_debug_flag() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::tessellation_layout_pass_clamps_to_capacity_and_sets_debug_flag"
        );

        let (device, queue, downlevel) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        let supports_compute = downlevel
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);
        if !supports_compute {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }
        if !downlevel
            .flags
            .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION)
        {
            common::skip_or_panic(test_name, "indirect execution unsupported");
            return;
        }

        // Intentionally choose capacities that force clamping:
        // - First patch must clamp from 64 to 8 to fit 200 indices (3*8^2=192).
        // - Second patch then clamps to 1 to fit the remaining indices.
        let patch_count = 2u32;
        let tess_factor = 64.0f32;
        let max_vertices = 48u32;
        let max_indices = 200u32;

        let (meta, args, debug) = run_layout_pass_case(
            &device,
            &queue,
            patch_count,
            tess_factor,
            max_vertices,
            max_indices,
        )
        .await
        .unwrap();

        assert_ne!(debug, 0, "expected debug flag when clamping");

        // Expected clamped layout:
        // patch0: level=8, vertices=45, indices=192
        // patch1: level=1, vertices=3, indices=3
        let expected = vec![
            (8u32, 0u32, 0u32, 45u32, 192u32),
            (1u32, 45u32, 192u32, 3u32, 3u32),
        ];
        assert_eq!(meta, expected, "clamped meta mismatch");

        assert_eq!(args.index_count, 195);
        assert_eq!(args.instance_count, 1);
        assert_eq!(args.first_index, 0);
        assert_eq!(args.base_vertex, 0);
        assert_eq!(args.first_instance, 0);

        // Ensure everything is within capacity.
        for (patch_id, (_level, v_base, i_base, v_count, i_count)) in
            meta.iter().copied().enumerate()
        {
            assert!(
                v_base + v_count <= max_vertices,
                "vertex range out of bounds for patch {patch_id}"
            );
            assert!(
                i_base + i_count <= max_indices,
                "index range out of bounds for patch {patch_id}"
            );
        }
    });
}

#[test]
fn tessellation_layout_pass_wgsl_avoids_isnan_isinf_builtins() {
    // wgpu 0.20's WGSL frontend (naga 0.20) rejects `isNan`/`isInf`.
    // Ensure we don't regress by reintroducing these as function calls.
    let wgsl = wgsl_tessellation_layout_pass(
        /*group=*/ 0, /*params_binding=*/ 0, /*hs_tess_factors_binding=*/ 1,
        /*out_patch_meta_binding=*/ 2, /*out_indirect_args_binding=*/ 3,
        /*out_debug_binding=*/ 4,
    );

    assert!(
        !wgsl.contains("isNan("),
        "generated WGSL unexpectedly calls isNan():\n{wgsl}"
    );
    assert!(
        !wgsl.contains("isInf("),
        "generated WGSL unexpectedly calls isInf():\n{wgsl}"
    );
}

#[test]
fn tessellation_layout_pass_handles_nan_and_inf_tess_factors() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::tessellation_layout_pass_handles_nan_and_inf_tess_factors"
        );

        let (device, queue, downlevel) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        let supports_compute = downlevel
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);
        if !supports_compute {
            common::skip_or_panic(test_name, "compute unsupported");
            return;
        }
        if !downlevel
            .flags
            .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION)
        {
            common::skip_or_panic(test_name, "indirect execution unsupported");
            return;
        }

        // Mix finite and non-finite tess factors. The layout pass should treat NaN/Inf as 0,
        // resulting in well-defined float->int conversions and a derived tess level.
        let patch_count = 3u32;
        let factors = [
            // Max edge factor is 4.
            [f32::NAN, 4.0, f32::NAN, f32::NAN],
            // All invalid => derived level clamps to 1.
            [f32::INFINITY, f32::INFINITY, f32::INFINITY, f32::INFINITY],
            // Contains -Inf + NaN but also a finite 2.
            [f32::NEG_INFINITY, 2.0, f32::NAN, 0.0],
        ];

        // hs_tess_factors: array<vec4<f32>> (16 bytes per patch).
        let hs_factors_size = (patch_count as u64)
            .checked_mul(16)
            .expect("hs_factors_size overflow");
        let hs_factors_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tess_layout hs_factors (nan/inf)"),
            size: hs_factors_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut hs_bytes = Vec::with_capacity(hs_factors_size as usize);
        for patch in factors {
            for f in patch {
                hs_bytes.extend_from_slice(&f.to_le_bytes());
            }
        }
        queue.write_buffer(&hs_factors_buf, 0, &hs_bytes);

        let max_vertices = 100u32;
        let max_indices = 100u32;
        let (meta, args, debug) = run_layout_pass_with_hs_factors_buffer(
            &device,
            &queue,
            patch_count,
            &hs_factors_buf,
            hs_factors_size,
            max_vertices,
            max_indices,
        )
        .await
        .unwrap();

        assert_eq!(debug, 0, "unexpected debug flag (no clamping expected)");

        let expected = vec![
            // patch0: level=4 => v=15, i=48
            (4u32, 0u32, 0u32, 15u32, 48u32),
            // patch1: all invalid => level=1 => v=3, i=3
            (1u32, 15u32, 48u32, 3u32, 3u32),
            // patch2: max finite factor is 2 => v=6, i=12
            (2u32, 18u32, 51u32, 6u32, 12u32),
        ];
        assert_eq!(meta, expected, "meta mismatch for nan/inf inputs");

        assert_eq!(args.index_count, 63, "indirect index_count mismatch");
        assert_eq!(args.instance_count, 1);
        assert_eq!(args.first_index, 0);
        assert_eq!(args.base_vertex, 0);
        assert_eq!(args.first_instance, 0);
    });
}
