mod common;

use std::collections::BTreeMap;

use aero_d3d11::binding_model::{BINDING_BASE_CBUFFER, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE};
use aero_d3d11::runtime::bindings::ShaderStage as RuntimeShaderStage;
use aero_d3d11::runtime::vertex_pulling::{VertexPullingLayout, VERTEX_PULLING_GROUP};
use anyhow::{anyhow, Result};

async fn create_device(bool_fallback: bool) -> Result<(wgpu::Device, bool)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir()
                .join(format!("aero-d3d11-gs-vertex-pulling-xdg-runtime-{}", std::process::id()));
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

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: bool_fallback,
        })
        .await
        .ok_or_else(|| anyhow!("wgpu: no suitable adapter found"))?;

    let supports_compute = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);

    let (device, _queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d11 gs+vertex pulling bindings test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

    Ok((device, supports_compute))
}

#[test]
fn gs_stage_bindings_do_not_conflict_with_vertex_pulling() {
    pollster::block_on(async {
        let (device, supports_compute) = match create_device(true).await {
            Ok(v) => v,
            Err(_) => match create_device(false).await {
                Ok(v) => v,
                Err(e) => {
                    common::skip_or_panic(
                        module_path!(),
                        &format!("wgpu unavailable ({e:#})"),
                    );
                    return;
                }
            },
        };
        if !supports_compute {
            common::skip_or_panic(module_path!(), "compute unsupported");
            return;
        }

        // Minimal vertex pulling layout: one compacted vertex buffer slot.
        let pulling = VertexPullingLayout {
            d3d_slot_to_pulling_slot: BTreeMap::from([(0u32, 0u32)]),
            pulling_slot_to_d3d_slot: vec![0u32],
            attributes: Vec::new(),
        };
        let prelude = pulling.wgsl_prelude();

        // Fake "GS stage" resources bound at the standard D3D11 register-space bindings.
        let gs_cb0_binding = BINDING_BASE_CBUFFER + 0;
        let gs_t0_binding = BINDING_BASE_TEXTURE + 0;
        let gs_s0_binding = BINDING_BASE_SAMPLER + 0;

        // Extended D3D11 stages (GS/HS/DS) share bind group 3 in the binding model, while vertex
        // pulling uses a dedicated internal emulation bind group.
        let gs_group = RuntimeShaderStage::Geometry.as_bind_group_index();
        assert_eq!(gs_group, 3);
        assert_eq!(VERTEX_PULLING_GROUP, 4);

        let wgsl = format!(
            r#"
{prelude}

struct GsCb0 {{
  v: vec4<u32>,
}};

@group({gs_group}) @binding({cb0}) var<uniform> gs_cb0: GsCb0;
@group({gs_group}) @binding({t0}) var gs_t0: texture_2d<f32>;
@group({gs_group}) @binding({s0}) var gs_s0: sampler;

@compute @workgroup_size(1)
fn main() {{
  // Touch everything so the bindings are kept by the compiler/validator.
  let x: u32 = gs_cb0.v.x;
  let y: u32 = aero_vp_vb0[0u];
  let z: u32 = aero_vp_ia.first_vertex;
  let c: vec4<f32> = textureSampleLevel(gs_t0, gs_s0, vec2<f32>(0.0, 0.0), 0.0);
  let _mix: f32 = c.x + f32(x + y + z);
}}
"#,
            gs_group = gs_group,
            cb0 = gs_cb0_binding,
            t0 = gs_t0_binding,
            s0 = gs_s0_binding,
        );

        // Bind group layouts for groups 0..2 are empty; group 3 carries the GS bindings; group 4
        // carries the vertex pulling bindings.
        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gs+vertex pulling empty bgl"),
            entries: &[],
        });

        // Group 3 layout contains the per-stage D3D bindings for GS.
        let group3_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gs binding bgl"),
            entries: &[
                // GS cbuffer b0.
                wgpu::BindGroupLayoutEntry {
                    binding: gs_cb0_binding,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // GS t0 + s0.
                wgpu::BindGroupLayoutEntry {
                    binding: gs_t0_binding,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: gs_s0_binding,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let vp_bgl = pulling.create_bind_group_layout(&device);

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gs+vertex pulling binding collision test shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let layouts: [&wgpu::BindGroupLayout; 5] =
            [&empty_bgl, &empty_bgl, &empty_bgl, &group3_bgl, &vp_bgl];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gs+vertex pulling pipeline layout"),
            bind_group_layouts: &layouts,
            push_constant_ranges: &[],
        });

        let _pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("gs+vertex pulling pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: "main",
            compilation_options: Default::default(),
        });
        device.poll(wgpu::Maintain::Wait);

        let err = device.pop_error_scope().await;
        assert!(
            err.is_none(),
            "expected pipeline creation to succeed with disjoint GS+vertex pulling bindings, got: {err:?}"
        );
    });
}
