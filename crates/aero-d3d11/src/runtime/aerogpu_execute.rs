use std::collections::HashMap;

use aero_gpu::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use aero_gpu::pipeline_key::{
    ColorTargetKey, PipelineLayoutKey, RenderPipelineKey, ShaderHash, ShaderStage,
    VertexAttributeKey, VertexBufferLayoutKey,
};
use aero_gpu::stats::PipelineCacheStats;
use aero_gpu::GpuCapabilities;
use anyhow::{anyhow, bail, Context, Result};

use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    VertexBufferLayoutOwned, VsInputSignatureElement, MAX_INPUT_SLOTS,
};
use crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap;
use crate::{parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, Sm4Program};

use super::aerogpu_state::{
    AerogpuHandle, BlendState, D3D11ShadowState, DepthStencilState, IndexBufferBinding,
    PrimitiveTopology, RasterizerState, ScissorRect, VertexBufferBinding, Viewport,
};
use super::pipeline_layout_cache::PipelineLayoutCache;

#[derive(Debug)]
pub struct BufferResource {
    pub buffer: wgpu::Buffer,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

#[derive(Debug)]
pub struct TextureResource {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub desc: Texture2dDesc,
}

#[derive(Debug, Clone)]
pub struct ShaderResource {
    pub stage: ShaderStage,
    pub wgsl: String,
    pub hash: ShaderHash,
    pub vs_input_signature: Vec<VsInputSignatureElement>,
}

#[derive(Debug, Clone)]
pub struct InputLayoutResource {
    layout: InputLayoutDesc,
}

#[derive(Debug, Default)]
pub struct AerogpuResources {
    buffers: HashMap<AerogpuHandle, BufferResource>,
    textures: HashMap<AerogpuHandle, TextureResource>,
    shaders: HashMap<AerogpuHandle, ShaderResource>,
    input_layouts: HashMap<AerogpuHandle, InputLayoutResource>,
}

/// A minimal `aerogpu_cmd`-style executor focused on D3D10/11 rendering.
///
/// The guest streams D3D11-style incremental state updates; when a draw is
/// issued we derive a [`RenderPipelineKey`] from the current [`D3D11ShadowState`]
/// and use [`PipelineCache`] to materialize `wgpu` pipelines on demand.
pub struct AerogpuCmdRuntime {
    device: wgpu::Device,
    queue: wgpu::Queue,

    pub state: D3D11ShadowState,
    pub resources: AerogpuResources,
    pipelines: PipelineCache,
    pipeline_layout_cache: PipelineLayoutCache,
}

impl AerogpuCmdRuntime {
    pub async fn new_for_tests() -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);
            if needs_runtime_dir {
                let dir = std::env::temp_dir().join(format!(
                    "aero-d3d11-aerogpu-xdg-runtime-{}",
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
                // Prefer "native" backends; this avoids noisy platform warnings from
                // initializing GL/WAYLAND stacks in headless CI environments.
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

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 aerogpu test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        let caps = GpuCapabilities::from_device(&device);
        let pipelines = PipelineCache::new(PipelineCacheConfig::default(), caps);

        Ok(Self {
            device,
            queue,
            state: D3D11ShadowState::default(),
            resources: AerogpuResources::default(),
            pipelines,
            pipeline_layout_cache: PipelineLayoutCache::new(),
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn poll_wait(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);
    }

    pub fn pipeline_cache_stats(&self) -> PipelineCacheStats {
        self.pipelines.stats()
    }

    pub fn create_buffer(&mut self, handle: AerogpuHandle, size: u64, usage: wgpu::BufferUsages) {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu buffer"),
            size,
            usage,
            mapped_at_creation: false,
        });
        self.resources
            .buffers
            .insert(handle, BufferResource { buffer, size });
    }

    pub fn write_buffer(&self, handle: AerogpuHandle, offset: u64, data: &[u8]) -> Result<()> {
        let buf = self
            .resources
            .buffers
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown buffer handle {handle}"))?;
        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        let size_bytes = data.len() as u64;
        if !offset.is_multiple_of(alignment) || !size_bytes.is_multiple_of(alignment) {
            bail!(
                "write_buffer: offset and size must be {alignment}-byte aligned (offset={offset} size_bytes={size_bytes})"
            );
        }
        if offset.saturating_add(data.len() as u64) > buf.size {
            bail!(
                "write_buffer out of bounds: offset={} len={} size={}",
                offset,
                data.len(),
                buf.size
            );
        }
        self.queue.write_buffer(&buf.buffer, offset, data);
        Ok(())
    }

    pub fn create_texture2d(
        &mut self,
        handle: AerogpuHandle,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d11 aerogpu texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.resources.textures.insert(
            handle,
            TextureResource {
                texture,
                view,
                desc: Texture2dDesc {
                    width,
                    height,
                    format,
                },
            },
        );
    }

    pub fn create_shader_dxbc(&mut self, handle: AerogpuHandle, dxbc_bytes: &[u8]) -> Result<()> {
        let dxbc = DxbcFile::parse(dxbc_bytes).context("parse DXBC")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("parse DXBC shader chunk")?;
        let stage = match program.stage {
            crate::ShaderStage::Vertex => ShaderStage::Vertex,
            crate::ShaderStage::Pixel => ShaderStage::Fragment,
            // Geometry/hull/domain stages are not supported by the AeroGPU/WebGPU pipeline. Accept
            // the create call (so guests can compile/pass these shaders around) but ignore the
            // resulting shader since there is no way to bind it.
            crate::ShaderStage::Geometry
            | crate::ShaderStage::Hull
            | crate::ShaderStage::Domain => {
                return Ok(());
            }
            other => bail!("unsupported shader stage for aerogpu_cmd executor: {other:?}"),
        };

        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;
        let vs_input_signature = if stage == ShaderStage::Vertex {
            extract_vs_input_signature(&signatures).context("extract VS input signature")?
        } else {
            Vec::new()
        };

        let wgsl = if signatures.isgn.is_some() && signatures.osgn.is_some() {
            try_translate_sm4_signature_driven(&dxbc, &program, &signatures)?
        } else {
            translate_sm4_to_wgsl_bootstrap(&program)
                .context("translate SM4/5 to WGSL")?
                .wgsl
        };

        // Register into the shared PipelineCache shader-module cache.
        let (hash, _module) = self.pipelines.get_or_create_shader_module(
            &self.device,
            stage,
            &wgsl,
            Some("aerogpu shader"),
        );

        self.resources.shaders.insert(
            handle,
            ShaderResource {
                stage,
                wgsl,
                hash,
                vs_input_signature,
            },
        );
        Ok(())
    }

    pub fn create_input_layout(&mut self, handle: AerogpuHandle, blob: &[u8]) -> Result<()> {
        let layout = InputLayoutDesc::parse(blob)
            .map_err(|e| anyhow!("failed to parse ILAY input layout blob: {e}"))?;
        self.resources
            .input_layouts
            .insert(handle, InputLayoutResource { layout });
        Ok(())
    }

    pub fn bind_shaders(&mut self, vs: Option<AerogpuHandle>, ps: Option<AerogpuHandle>) {
        self.state.vs = vs;
        self.state.ps = ps;
    }

    pub fn set_input_layout(&mut self, layout: Option<AerogpuHandle>) {
        self.state.input_layout = layout;
    }

    pub fn set_vertex_buffers(&mut self, start_slot: usize, bindings: &[VertexBufferBinding]) {
        let end = start_slot.saturating_add(bindings.len());
        if end > self.state.vertex_buffers.len() {
            self.state.vertex_buffers.resize(end, None);
        }
        for (i, binding) in bindings.iter().enumerate() {
            self.state.vertex_buffers[start_slot + i] = Some(*binding);
        }
    }

    pub fn set_index_buffer(&mut self, binding: Option<IndexBufferBinding>) {
        self.state.index_buffer = binding;
    }

    pub fn set_primitive_topology(&mut self, topology: PrimitiveTopology) {
        self.state.primitive_topology = topology;
    }

    pub fn set_render_targets(
        &mut self,
        colors: &[Option<AerogpuHandle>; 8],
        depth_stencil: Option<AerogpuHandle>,
    ) {
        self.state.render_targets.colors = *colors;
        self.state.render_targets.depth_stencil = depth_stencil;
    }

    pub fn set_viewport(&mut self, viewport: Option<Viewport>) {
        self.state.viewport = viewport;
    }

    pub fn set_scissor(&mut self, scissor: Option<ScissorRect>) {
        self.state.scissor = scissor;
    }

    pub fn set_blend_state(&mut self, state: BlendState) {
        self.state.blend_state = state;
    }

    pub fn set_depth_stencil_state(&mut self, state: DepthStencilState) {
        self.state.depth_stencil_state = state;
    }

    pub fn set_rasterizer_state(&mut self, state: RasterizerState) {
        self.state.rasterizer_state = state;
    }

    pub fn draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) -> Result<()> {
        self.draw_internal(DrawKind::NonIndexed {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        })
    }

    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    ) -> Result<()> {
        self.draw_internal(DrawKind::Indexed {
            index_count,
            instance_count,
            first_index,
            base_vertex,
            first_instance,
        })
    }

    fn draw_internal(&mut self, kind: DrawKind) -> Result<()> {
        let vs_handle = self
            .state
            .vs
            .ok_or_else(|| anyhow!("draw without a bound VS"))?;
        let ps_handle = self
            .state
            .ps
            .ok_or_else(|| anyhow!("draw without a bound PS"))?;

        let vs = self
            .resources
            .shaders
            .get(&vs_handle)
            .ok_or_else(|| anyhow!("unknown VS handle {vs_handle}"))?;
        let ps = self
            .resources
            .shaders
            .get(&ps_handle)
            .ok_or_else(|| anyhow!("unknown PS handle {ps_handle}"))?;
        if vs.stage != ShaderStage::Vertex {
            bail!("shader {vs_handle} is not a vertex shader");
        }
        if ps.stage != ShaderStage::Fragment {
            bail!("shader {ps_handle} is not a pixel/fragment shader");
        }

        let (color_attachments, color_target_keys, target_size) =
            build_color_attachments(&self.resources, &self.state)?;

        let (depth_attachment, depth_target_key, depth_state) =
            build_depth_attachment(&self.resources, &self.state)?;

        let primitive_topology = map_topology(self.state.primitive_topology)?;
        let cull_mode = self.state.rasterizer_state.cull_mode;
        let front_face = self.state.rasterizer_state.front_face;
        let scissor_enabled = self.state.rasterizer_state.scissor_enable;

        let BuiltVertexState {
            layouts: owned_vertex_layouts,
            keys: vertex_buffer_keys,
            wgpu_slot_to_d3d_slot,
        } = build_vertex_state(&self.resources, &self.state, &vs.vs_input_signature)?;

        let layout_key = PipelineLayoutKey::empty();
        let pipeline_layout =
            self.pipeline_layout_cache
                .get_or_create(&self.device, &layout_key, &[]);

        let key = RenderPipelineKey {
            vertex_shader: vs.hash,
            fragment_shader: ps.hash,
            color_targets: color_target_keys,
            depth_stencil: depth_target_key,
            primitive_topology,
            cull_mode,
            front_face,
            vertex_buffers: vertex_buffer_keys,
            sample_count: 1,
            layout: layout_key,
        };

        let blend = self.state.blend_state;
        let mut color_target_states: Vec<Option<wgpu::ColorTargetState>> = Vec::new();
        for ct in &key.color_targets {
            color_target_states.push(Some(wgpu::ColorTargetState {
                format: ct.format,
                blend: blend.blend,
                write_mask: blend.write_mask,
            }));
        }

        let depth_stencil_state = depth_state.clone();

        // Fetch or create pipeline.
        let pipeline = self.pipelines.get_or_create_render_pipeline(
            &self.device,
            key,
            move |device, vs_module, fs_module| {
                let pipeline_layout = pipeline_layout.as_ref();

                let vertex_buffers: Vec<wgpu::VertexBufferLayout<'_>> = owned_vertex_layouts
                    .iter()
                    .map(|l| wgpu::VertexBufferLayout {
                        array_stride: l.array_stride,
                        step_mode: l.step_mode,
                        attributes: &l.attributes,
                    })
                    .collect();

                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("aero-d3d11 aerogpu render pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: vs_module,
                        entry_point: "vs_main",
                        buffers: &vertex_buffers,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: fs_module,
                        entry_point: "fs_main",
                        targets: &color_target_states,
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: primitive_topology,
                        front_face,
                        cull_mode,
                        ..Default::default()
                    },
                    depth_stencil: depth_stencil_state,
                    multisample: wgpu::MultisampleState {
                        count: 1,
                        ..Default::default()
                    },
                    multiview: None,
                })
            },
        )?;

        // Encode the draw.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 aerogpu draw encoder"),
            });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aero-d3d11 aerogpu draw pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        pass.set_pipeline(pipeline);

        // Viewport/scissor are dynamic state; apply on every draw.
        let default_viewport = Viewport {
            x: 0.0,
            y: 0.0,
            width: target_size.0 as f32,
            height: target_size.1 as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let mut viewport = self.state.viewport.unwrap_or(default_viewport);
        if !viewport.x.is_finite()
            || !viewport.y.is_finite()
            || !viewport.width.is_finite()
            || !viewport.height.is_finite()
            || !viewport.min_depth.is_finite()
            || !viewport.max_depth.is_finite()
        {
            viewport = default_viewport;
        }

        let max_w = target_size.0 as f32;
        let max_h = target_size.1 as f32;
        let left = viewport.x.max(0.0);
        let top = viewport.y.max(0.0);
        let right = (viewport.x + viewport.width).max(0.0).min(max_w);
        let bottom = (viewport.y + viewport.height).max(0.0).min(max_h);
        let width = (right - left).max(0.0);
        let height = (bottom - top).max(0.0);

        if width > 0.0 && height > 0.0 {
            let mut min_depth = viewport.min_depth.clamp(0.0, 1.0);
            let mut max_depth = viewport.max_depth.clamp(0.0, 1.0);
            if min_depth > max_depth {
                std::mem::swap(&mut min_depth, &mut max_depth);
            }
            pass.set_viewport(left, top, width, height, min_depth, max_depth);
        }

        if scissor_enabled {
            let scissor = self.state.scissor.unwrap_or(ScissorRect {
                x: 0,
                y: 0,
                width: target_size.0,
                height: target_size.1,
            });
            let x = scissor.x.min(target_size.0);
            let y = scissor.y.min(target_size.1);
            let width = scissor.width.min(target_size.0.saturating_sub(x));
            let height = scissor.height.min(target_size.1.saturating_sub(y));
            if width > 0 && height > 0 {
                pass.set_scissor_rect(x, y, width, height);
            } else {
                pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
            }
        } else {
            pass.set_scissor_rect(0, 0, target_size.0, target_size.1);
        }

        for (wgpu_slot, d3d_slot) in wgpu_slot_to_d3d_slot.iter().copied().enumerate() {
            let slot = d3d_slot as usize;
            let Some(binding) = self.state.vertex_buffers.get(slot).and_then(|b| *b) else {
                bail!("vertex buffer slot {d3d_slot} is required by input layout but not bound");
            };
            let buf = self
                .resources
                .buffers
                .get(&binding.buffer)
                .ok_or_else(|| anyhow!("unknown vertex buffer {}", binding.buffer))?;
            pass.set_vertex_buffer(wgpu_slot as u32, buf.buffer.slice(binding.offset..));
        }

        match kind {
            DrawKind::NonIndexed {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => {
                pass.draw(
                    first_vertex..first_vertex + vertex_count,
                    first_instance..first_instance + instance_count,
                );
            }
            DrawKind::Indexed {
                index_count,
                instance_count,
                first_index,
                base_vertex,
                first_instance,
            } => {
                let index = self
                    .state
                    .index_buffer
                    .ok_or_else(|| anyhow!("DrawIndexed without a bound index buffer"))?;
                let buf = self
                    .resources
                    .buffers
                    .get(&index.buffer)
                    .ok_or_else(|| anyhow!("unknown index buffer {}", index.buffer))?;
                pass.set_index_buffer(buf.buffer.slice(index.offset..), index.format);
                pass.draw_indexed(
                    first_index..first_index + index_count,
                    base_vertex,
                    first_instance..first_instance + instance_count,
                );
            }
        }

        drop(pass);
        self.queue.submit([encoder.finish()]);

        Ok(())
    }

    pub async fn read_texture_rgba8(&self, handle: AerogpuHandle) -> Result<Vec<u8>> {
        let tex = self
            .resources
            .textures
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown texture {handle}"))?;
        let needs_bgra_swizzle = match tex.desc.format {
            wgpu::TextureFormat::Rgba8Unorm => false,
            wgpu::TextureFormat::Bgra8Unorm => true,
            other => {
                bail!("read_texture_rgba8 only supports Rgba8Unorm/Bgra8Unorm (got {other:?})")
            }
        };

        let width = tex.desc.width;
        let height = tex.desc.height;

        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 aerogpu readback staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 aerogpu readback encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).ok();
        });
        self.poll_wait();
        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
            .context("wgpu: map_async failed")?;

        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in 0..height as usize {
            let start = row * padded_bytes_per_row as usize;
            out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        staging.unmap();

        if needs_bgra_swizzle {
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }

        Ok(out)
    }
}

#[derive(Debug, Copy, Clone)]
enum DrawKind {
    NonIndexed {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    Indexed {
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    },
}

#[derive(Debug)]
struct BuiltVertexState {
    layouts: Vec<VertexBufferLayoutOwned>,
    keys: Vec<VertexBufferLayoutKey>,
    /// WebGPU vertex buffer slot → D3D11 input slot.
    wgpu_slot_to_d3d_slot: Vec<u32>,
}

fn build_vertex_state(
    resources: &AerogpuResources,
    state: &D3D11ShadowState,
    vs_signature: &[VsInputSignatureElement],
) -> Result<BuiltVertexState> {
    let Some(layout_handle) = state.input_layout else {
        return Ok(BuiltVertexState {
            layouts: Vec::new(),
            keys: Vec::new(),
            wgpu_slot_to_d3d_slot: Vec::new(),
        });
    };

    let layout = resources
        .input_layouts
        .get(&layout_handle)
        .ok_or_else(|| anyhow!("unknown input layout handle {layout_handle}"))?;

    let mut slot_strides = vec![0u32; MAX_INPUT_SLOTS as usize];
    for (slot, binding) in state
        .vertex_buffers
        .iter()
        .enumerate()
        .take(slot_strides.len())
    {
        if let Some(binding) = binding {
            slot_strides[slot] = binding.stride;
        }
    }

    let fallback_signature;
    let sig = if vs_signature.is_empty() {
        fallback_signature = build_fallback_vs_signature(&layout.layout);
        fallback_signature.as_slice()
    } else {
        vs_signature
    };

    let binding = InputLayoutBinding::new(&layout.layout, &slot_strides);
    let mapped = map_layout_to_shader_locations_compact(&binding, sig)
        .map_err(|e| anyhow!("failed to map input layout to shader locations: {e}"))?;

    let keys = mapped
        .buffers
        .iter()
        .map(|l| VertexBufferLayoutKey {
            array_stride: l.array_stride,
            step_mode: l.step_mode,
            attributes: l
                .attributes
                .iter()
                .copied()
                .map(VertexAttributeKey::from)
                .collect(),
        })
        .collect();

    let mut wgpu_slot_to_d3d_slot = vec![0u32; mapped.buffers.len()];
    for (d3d_slot, wgpu_slot) in &mapped.d3d_slot_to_wgpu_slot {
        wgpu_slot_to_d3d_slot[*wgpu_slot as usize] = *d3d_slot;
    }

    Ok(BuiltVertexState {
        layouts: mapped.buffers,
        keys,
        wgpu_slot_to_d3d_slot,
    })
}

type ColorAttachments<'a> = (
    Vec<Option<wgpu::RenderPassColorAttachment<'a>>>,
    Vec<ColorTargetKey>,
    (u32, u32),
);

fn build_color_attachments<'a>(
    resources: &'a AerogpuResources,
    state: &D3D11ShadowState,
) -> Result<ColorAttachments<'a>> {
    let mut attachments = Vec::new();
    let mut keys = Vec::new();

    let mut size: Option<(u32, u32)> = None;
    let mut seen_gap = false;
    for (slot, handle) in state.render_targets.colors.iter().enumerate() {
        let Some(handle) = handle else {
            seen_gap = true;
            continue;
        };
        if seen_gap {
            bail!("render target slot {slot} is set after an earlier slot was unbound (gaps are not supported yet)");
        }

        let tex = resources
            .textures
            .get(handle)
            .ok_or_else(|| anyhow!("unknown render target texture {handle}"))?;

        let this_size = (tex.desc.width, tex.desc.height);
        if let Some(expected) = size {
            if expected != this_size {
                bail!(
                    "mismatched render target sizes: {:?} vs {:?}",
                    expected,
                    this_size
                );
            }
        } else {
            size = Some(this_size);
        }

        attachments.push(Some(wgpu::RenderPassColorAttachment {
            view: &tex.view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        }));

        keys.push(ColorTargetKey {
            format: tex.desc.format,
            blend: state.blend_state.blend.map(Into::into),
            write_mask: state.blend_state.write_mask,
        });
    }

    if keys.is_empty() {
        bail!("draw without bound render targets");
    }

    Ok((attachments, keys, size.unwrap_or((1, 1))))
}

fn build_depth_attachment<'a>(
    resources: &'a AerogpuResources,
    state: &D3D11ShadowState,
) -> Result<(
    Option<wgpu::RenderPassDepthStencilAttachment<'a>>,
    Option<aero_gpu::pipeline_key::DepthStencilKey>,
    Option<wgpu::DepthStencilState>,
)> {
    let Some(depth_handle) = state.render_targets.depth_stencil else {
        return Ok((None, None, None));
    };

    let tex = resources
        .textures
        .get(&depth_handle)
        .ok_or_else(|| anyhow!("unknown depth-stencil texture {depth_handle}"))?;

    let DepthStencilState {
        depth_enable,
        depth_write_enable,
        depth_compare,
        stencil_enable,
        stencil_read_mask,
        stencil_write_mask,
        depth_bias,
    } = state.depth_stencil_state;

    let depth_compare = if depth_enable {
        depth_compare
    } else {
        wgpu::CompareFunction::Always
    };
    let depth_write_enabled = depth_enable && depth_write_enable;

    let _ = stencil_enable;
    // P0: the protocol doesn't carry full stencil ops/functions yet.
    let stencil_state = wgpu::StencilState {
        front: wgpu::StencilFaceState::IGNORE,
        back: wgpu::StencilFaceState::IGNORE,
        read_mask: stencil_read_mask as u32,
        write_mask: stencil_write_mask as u32,
    };

    let depth_stencil_state = wgpu::DepthStencilState {
        format: tex.desc.format,
        depth_write_enabled,
        depth_compare,
        stencil: stencil_state,
        bias: wgpu::DepthBiasState {
            constant: depth_bias,
            slope_scale: 0.0,
            clamp: 0.0,
        },
    };

    let attachment = wgpu::RenderPassDepthStencilAttachment {
        view: &tex.view,
        depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0),
            store: wgpu::StoreOp::Store,
        }),
    };

    Ok((
        Some(attachment),
        Some(depth_stencil_state.clone().into()),
        Some(depth_stencil_state),
    ))
}

fn map_topology(topology: PrimitiveTopology) -> Result<wgpu::PrimitiveTopology> {
    Ok(match topology {
        PrimitiveTopology::PointList => wgpu::PrimitiveTopology::PointList,
        PrimitiveTopology::LineList => wgpu::PrimitiveTopology::LineList,
        PrimitiveTopology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
        PrimitiveTopology::TriangleList => wgpu::PrimitiveTopology::TriangleList,
        PrimitiveTopology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        PrimitiveTopology::TriangleFan => bail!("TriangleFan is not supported by WebGPU"),
    })
}

fn extract_vs_input_signature(
    signatures: &crate::ShaderSignatures,
) -> Result<Vec<VsInputSignatureElement>> {
    let Some(isgn) = signatures.isgn.as_ref() else {
        return Ok(Vec::new());
    };

    // D3D semantics are case-insensitive, but the signature chunk stores the original string. The
    // aerogpu ILAY protocol only preserves a hash, so we canonicalize to ASCII uppercase to match
    // how the guest typically hashes semantic names.
    Ok(isgn
        .parameters
        .iter()
        .map(|p| VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
        })
        .collect())
}

fn build_fallback_vs_signature(layout: &InputLayoutDesc) -> Vec<VsInputSignatureElement> {
    let mut seen: HashMap<(u32, u32), u32> = HashMap::new();
    let mut out: Vec<VsInputSignatureElement> = Vec::new();

    for elem in &layout.elements {
        let key = (elem.semantic_name_hash, elem.semantic_index);
        if seen.contains_key(&key) {
            continue;
        }
        let reg = out.len() as u32;
        seen.insert(key, reg);
        out.push(VsInputSignatureElement {
            semantic_name_hash: key.0,
            semantic_index: key.1,
            input_register: reg,
        });
    }

    out
}

fn try_translate_sm4_signature_driven(
    dxbc: &DxbcFile<'_>,
    program: &Sm4Program,
    signatures: &crate::ShaderSignatures,
) -> Result<String> {
    let module = program.decode().context("decode SM4/5 token stream")?;
    let translated = translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")?;

    // NOTE: `AerogpuCmdRuntime` does not yet build bind groups for translated resources. Only
    // accept the signature-driven path when the shader has no declared bindings.
    if !translated.reflection.bindings.is_empty() {
        bail!("shader requires resource bindings (not supported yet)");
    }

    Ok(translated.wgsl)
}
