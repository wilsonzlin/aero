mod common;

use aero_d3d11::runtime::tessellation::tri_domain_integer::{
    tri_domain_integer_index_count, tri_domain_integer_triangle_count,
    tri_domain_integer_vertex_count, TriDomainIntegerIndexGen, TriDomainPatchMeta,
    TriIndexGenParams, TriangleWinding,
};
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

        let patch_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tess tri index gen patch meta"),
            size: 20,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&patch_buf, 0, &patch.to_le_bytes());

        let out_bytes = (index_count as u64) * 4u64;
        let out_indices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tess tri index gen out indices"),
            size: out_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let params = TriIndexGenParams::new(TriangleWinding::Ccw);
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tess tri index gen params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&params_buf, 0, &params.to_le_bytes());

        let gen = TriDomainIntegerIndexGen::new(&device);
        let bind_group = gen.create_bind_group(&device, &patch_buf, &out_indices, &params_buf);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tess tri index gen readback"),
            size: out_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tess tri index gen encoder"),
        });
        gen.dispatch(&mut encoder, &bind_group, 1, index_count);
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
