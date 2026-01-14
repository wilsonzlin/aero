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
            let dir = std::env::temp_dir().join(format!(
                "aero-d3d11-gs-vertex-pulling-xdg-runtime-{}",
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
        // Prefer a fallback adapter for CI stability, but if it doesn't support compute we still
        // want to try a non-fallback adapter so this test actually exercises the binding model on
        // real GPU backends.
        let mut saw_device = false;
        let mut device: Option<wgpu::Device> = None;
        let mut last_err: Option<anyhow::Error> = None;
        for force_fallback_adapter in [true, false] {
            match create_device(force_fallback_adapter).await {
                Ok((dev, supports_compute)) => {
                    saw_device = true;
                    if supports_compute {
                        device = Some(dev);
                        break;
                    }
                }
                Err(e) => last_err = Some(e),
            }
        }
        let Some(device) = device else {
            if saw_device {
                common::skip_or_panic(module_path!(), "compute unsupported");
            } else {
                common::skip_or_panic(
                    module_path!(),
                    &format!(
                        "wgpu unavailable ({})",
                        last_err
                            .as_ref()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "unknown error".to_owned())
                    ),
                );
            }
            return;
        };

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

        // Extended D3D11 stages (GS/HS/DS) and vertex pulling share bind group 3 in the binding
        // model. Vertex pulling uses a reserved high binding-number range so it doesn't collide
        // with the stage-ex D3D register mappings.
        let gs_group = RuntimeShaderStage::Geometry.as_bind_group_index();
        assert_eq!(gs_group, 3);
        assert_eq!(VERTEX_PULLING_GROUP, gs_group);

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

        // Bind group layouts for groups 0..2 are empty. Group 3 contains both:
        // - the stage-ex D3D bindings for GS, and
        // - the internal vertex pulling bindings (in the reserved `>= 256` range).
        let empty_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gs+vertex pulling empty bgl"),
            entries: &[],
        });

        let mut group3_entries = vec![
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
        ];
        group3_entries.extend(pulling.bind_group_layout_entries());
        let group3_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gs+vertex pulling bgl"),
            entries: &group3_entries,
        });

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gs+vertex pulling binding collision test shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        let layouts: [&wgpu::BindGroupLayout; 4] =
            [&empty_bgl, &empty_bgl, &empty_bgl, &group3_bgl];
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
