mod common;

use aero_d3d11::runtime::tessellation::tri_domain_integer::{
    tri_domain_integer_index_count, tri_domain_integer_triangle_count,
    tri_domain_integer_vertex_count, TriDomainIntegerIndexGen, TriDomainPatchMeta,
    TriIndexGenParams, TriangleWinding,
};
use aero_d3d11::runtime::tessellator;
use anyhow::{anyhow, Context, Result};

fn build_expected_indices(patch: &TriDomainPatchMeta, winding: TriangleWinding) -> Vec<u32> {
    let tri_count = tri_domain_integer_triangle_count(patch.tess_level);
    let mut out = Vec::with_capacity(tri_count as usize * 3);
    for tri_id in 0..tri_count {
        let tri = match winding {
            TriangleWinding::Ccw => {
                tessellator::tri_index_to_vertex_indices(patch.tess_level, tri_id)
            }
            TriangleWinding::Cw => {
                tessellator::tri_index_to_vertex_indices_cw(patch.tess_level, tri_id)
            }
        };
        let v0 = patch.vertex_base + tri[0];
        let v1 = patch.vertex_base + tri[1];
        let v2 = patch.vertex_base + tri[2];
        out.push(v0);
        out.push(v1);
        out.push(v2);
    }
    out
}

async fn run_index_gen(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    patches: &[TriDomainPatchMeta],
    winding: TriangleWinding,
) -> Result<Vec<u32>> {
    let patch_count_u32: u32 = patches
        .len()
        .try_into()
        .map_err(|_| anyhow!("patch_count out of range"))?;
    let max_index_count_per_patch = patches.iter().map(|p| p.index_count).max().unwrap_or(0);
    let total_index_count = patches
        .iter()
        .map(|p| p.index_base.saturating_add(p.index_count))
        .max()
        .unwrap_or(0);

    let mut patch_bytes = Vec::with_capacity(patches.len() * 20);
    for p in patches {
        patch_bytes.extend_from_slice(&p.to_le_bytes());
    }

    let patch_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess tri index gen patch meta"),
        size: patch_bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&patch_buf, 0, &patch_bytes);

    let out_bytes = (total_index_count as u64) * 4u64;
    let out_indices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess tri index gen out indices"),
        size: out_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let params = TriIndexGenParams::new(winding);
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess tri index gen params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&params_buf, 0, &params.to_le_bytes());

    let gen = TriDomainIntegerIndexGen::new(device);
    let bind_group = gen.create_bind_group(
        device,
        wgpu::BufferBinding {
            buffer: &patch_buf,
            offset: 0,
            size: None,
        },
        wgpu::BufferBinding {
            buffer: &out_indices,
            offset: 0,
            size: None,
        },
        wgpu::BufferBinding {
            buffer: &params_buf,
            offset: 0,
            size: None,
        },
    );

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tess tri index gen readback"),
        size: out_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tess tri index gen encoder"),
    });
    gen.dispatch(
        &mut encoder,
        &bind_group,
        patch_count_u32,
        max_index_count_per_patch,
    );
    encoder.copy_buffer_to_buffer(&out_indices, 0, &staging, 0, out_bytes);
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
    let indices: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&mapped).to_vec();
    drop(mapped);
    staging.unmap();
    Ok(indices)
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
                "aero-d3d11-tess-tri-index-gen-xdg-runtime-{}",
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
                label: Some("aero-d3d11 tessellation tri index gen test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, queue, supports_compute))
}

#[test]
fn tessellation_tri_domain_integer_index_gen_level2_produces_in_range_indices() {
    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("{err:#}"));
                return Ok(()) as Result<()>;
            }
        };

        if !supports_compute {
            common::skip_or_panic(
                module_path!(),
                "wgpu adapter does not support compute shaders",
            );
            return Ok(());
        }

        let tess_level = 2u32;
        let vertex_count_total = tri_domain_integer_vertex_count(tess_level);
        let index_count = tri_domain_integer_index_count(tess_level);
        let triangle_count = tri_domain_integer_triangle_count(tess_level);

        assert_eq!(
            index_count,
            triangle_count * 3,
            "index_count must be 3 * triangle_count"
        );

        let patch = TriDomainPatchMeta {
            tess_level,
            vertex_base: 0,
            index_base: 0,
            vertex_count: vertex_count_total,
            index_count,
        };
        let indices = run_index_gen(&device, &queue, &[patch], TriangleWinding::Ccw).await?;
        let expected = build_expected_indices(&patch, TriangleWinding::Ccw);
        assert_eq!(indices, expected, "CCW index buffer mismatch");

        assert_eq!(
            indices.len() as u32,
            index_count,
            "readback index count mismatch"
        );

        let tri_count_from_buffer = (indices.len() / 3) as u32;
        assert_eq!(
            tri_count_from_buffer, triangle_count,
            "triangle count mismatch"
        );

        assert!(vertex_count_total > 0, "vertex_count_total must be > 0");
        for &idx in &indices {
            assert!(
                idx < vertex_count_total,
                "index out of range: idx={idx} vertex_count_total={vertex_count_total}"
            );
        }

        // Sanity: ensure all vertices are referenced at least once for this small tess level.
        let mut seen = vec![false; vertex_count_total as usize];
        for &idx in &indices {
            seen[idx as usize] = true;
        }
        assert!(
            seen.into_iter().all(|v| v),
            "expected all vertices to be referenced at least once"
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn tessellation_tri_domain_integer_index_gen_multi_patch_chunking_and_winding() {
    pollster::block_on(async {
        let (device, queue, supports_compute) = match create_device_queue().await {
            Ok(v) => v,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("{err:#}"));
                return Ok(()) as Result<()>;
            }
        };

        if !supports_compute {
            common::skip_or_panic(
                module_path!(),
                "wgpu adapter does not support compute shaders",
            );
            return Ok(());
        }

        // Use levels that exceed WORKGROUP_SIZE_Y so the Y dimension must span multiple workgroups.
        let tess0 = 9u32; // index_count = 243
        let tess1 = 8u32; // index_count = 192

        let v0 = tri_domain_integer_vertex_count(tess0);
        let i0 = tri_domain_integer_index_count(tess0);
        let v1 = tri_domain_integer_vertex_count(tess1);
        let i1 = tri_domain_integer_index_count(tess1);

        let total_vertices = v0 + v1;
        let total_indices = i0 + i1;

        let patches = [
            TriDomainPatchMeta {
                tess_level: tess0,
                vertex_base: 0,
                index_base: 0,
                vertex_count: v0,
                index_count: i0,
            },
            TriDomainPatchMeta {
                tess_level: tess1,
                vertex_base: v0,
                index_base: i0,
                vertex_count: v1,
                index_count: i1,
            },
        ];

        let ccw = run_index_gen(&device, &queue, &patches, TriangleWinding::Ccw).await?;
        let cw = run_index_gen(&device, &queue, &patches, TriangleWinding::Cw).await?;

        assert_eq!(
            ccw.len() as u32, total_indices,
            "unexpected total index count for CCW run"
        );
        assert_eq!(
            cw.len() as u32, total_indices,
            "unexpected total index count for CW run"
        );

        for (tri_ccw, tri_cw) in ccw.chunks_exact(3).zip(cw.chunks_exact(3)) {
            assert_eq!(tri_ccw[0], tri_cw[0], "winding must keep v0 stable");
            assert_eq!(tri_ccw[1], tri_cw[2], "winding must swap v1/v2");
            assert_eq!(tri_ccw[2], tri_cw[1], "winding must swap v1/v2");
        }

        for patch in &patches {
            let tri_count = tri_domain_integer_triangle_count(patch.tess_level);
            assert_eq!(
                patch.index_count,
                tri_count * 3,
                "patch index_count mismatch for tess_level {}",
                patch.tess_level
            );

            let start = patch.index_base as usize;
            let end = start + patch.index_count as usize;
            let slice = ccw
                .get(start..end)
                .ok_or_else(|| anyhow!("patch indices out of range"))?;
            assert_eq!(
                slice.len() as u32,
                patch.index_count,
                "patch slice length mismatch"
            );

            for &idx in slice {
                assert!(
                    idx < total_vertices,
                    "index out of global range: idx={idx} total_vertices={total_vertices}"
                );
                assert!(
                    idx >= patch.vertex_base && idx < patch.vertex_base + patch.vertex_count,
                    "index out of patch range: idx={idx} patch_vertex_base={} patch_vertex_count={}",
                    patch.vertex_base,
                    patch.vertex_count
                );
            }
        }

        // Compare against CPU reference indexing to ensure connectivity matches `runtime::tessellator`.
        let expected0_ccw = build_expected_indices(&patches[0], TriangleWinding::Ccw);
        let expected1_ccw = build_expected_indices(&patches[1], TriangleWinding::Ccw);
        assert_eq!(
            ccw
                .get(0..i0 as usize)
                .ok_or_else(|| anyhow!("patch0 slice out of range"))?,
            expected0_ccw.as_slice(),
            "patch0 CCW indices mismatch"
        );
        assert_eq!(
            ccw.get(i0 as usize..(i0 + i1) as usize)
                .ok_or_else(|| anyhow!("patch1 slice out of range"))?,
            expected1_ccw.as_slice(),
            "patch1 CCW indices mismatch"
        );

        let expected0_cw = build_expected_indices(&patches[0], TriangleWinding::Cw);
        let expected1_cw = build_expected_indices(&patches[1], TriangleWinding::Cw);
        assert_eq!(
            cw.get(0..i0 as usize)
                .ok_or_else(|| anyhow!("patch0 slice out of range"))?,
            expected0_cw.as_slice(),
            "patch0 CW indices mismatch"
        );
        assert_eq!(
            cw.get(i0 as usize..(i0 + i1) as usize)
                .ok_or_else(|| anyhow!("patch1 slice out of range"))?,
            expected1_cw.as_slice(),
            "patch1 CW indices mismatch"
        );

        // Smoke-check that both patches contributed: indices in the second patch slice must be
        // offset by `vertex_base`.
        assert!(
            ccw[patches[1].index_base as usize] >= patches[1].vertex_base,
            "expected patch1 indices to include vertex_base offset"
        );

        Ok(())
    })
    .unwrap();
}
