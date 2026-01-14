use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use aero_gpu::bindings::bind_group_cache::{
    BindGroupCache, BindGroupCacheEntry, BindGroupCacheResource, BufferId, TextureViewId,
};
use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
use aero_gpu::bindings::samplers::SamplerCache;
use aero_gpu::bindings::CacheStats;
use aero_gpu::pipeline_cache::{PipelineCache, PipelineCacheConfig};
use aero_gpu::pipeline_key::{PipelineLayoutKey, ShaderStage};
use aero_gpu::protocol_d3d11::{
    BindingType, BufferUsage, CmdPacket, CmdStream, D3D11Opcode, DxgiFormat, IndexFormat,
    PipelineKind, PrimitiveTopology, ShaderStageFlags, TextureUsage, VertexFormat, VertexStepMode,
};
use aero_gpu::{GpuCapabilities, GpuError};
use anyhow::{anyhow, bail, Context, Result};

use super::pipeline_layout_cache::PipelineLayoutCache;
use super::resources::{
    BindingDef, BindingKind, BufferResource, ComputePipelineResource, D3D11Resources,
    RenderPipelineResource, RenderPipelineVariants, SamplerResource, ShaderModuleResource,
    Texture2dDesc, TextureResource, TextureViewResource,
};
use super::state::{
    BoundIndexBuffer, BoundResource, BoundVertexBuffer, D3D11State, PipelineBinding,
};

const DEFAULT_BIND_GROUP_CACHE_CAPACITY: usize = 4096;
// The shared AeroGPU D3D11 binding model reserves `@group(2)` for compute-stage resources (see
// `binding_model.rs`). The `protocol_d3d11` command stream only describes a single bind group, so
// the runtime places that bind group at index 2 and fills groups 0/1 with empty layouts.
const COMPUTE_BIND_GROUP_INDEX: u32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct D3D11CacheStats {
    pub samplers: CacheStats,
    pub bind_group_layouts: CacheStats,
    pub bind_groups: CacheStats,
}

pub struct D3D11Runtime {
    device: wgpu::Device,
    queue: wgpu::Queue,
    supports_compute: bool,
    /// Whether the wgpu backend requires emulation for indexed strip primitive restart.
    ///
    /// wgpu's GL backend has historically had correctness issues with native primitive restart for
    /// indexed strip topologies. When this is true, the runtime falls back to CPU-side strip
    /// restart handling by splitting indexed strip draws into multiple segments that omit restart
    /// indices entirely.
    emulate_strip_restart: bool,
    pipelines: PipelineCache,
    pub resources: D3D11Resources,
    pub state: D3D11State,
    sampler_cache: SamplerCache,
    bind_group_layout_cache: BindGroupLayoutCache,
    bind_group_cache: BindGroupCache<Arc<wgpu::BindGroup>>,
    pipeline_layout_cache: PipelineLayoutCache<Arc<wgpu::PipelineLayout>>,
    /// Tracks whether the current command encoder has recorded any GPU work.
    ///
    /// `wgpu::Queue::write_buffer` / `write_texture` enqueue work immediately, so they can reorder
    /// ahead of earlier encoded work if we defer `queue.submit()` until the end of the command
    /// stream. When an update opcode needs to call `queue.write_*`, we flush the current encoder
    /// first (only if it has recorded work) to preserve strict stream ordering.
    encoder_has_commands: bool,
}

impl D3D11Runtime {
    pub async fn new_for_tests() -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir = std::env::temp_dir()
                    .join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
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

        let downlevel = adapter.get_downlevel_capabilities();
        let supports_compute =
            GpuCapabilities::supports_compute_from_downlevel_flags(downlevel.flags);
        let emulate_strip_restart = adapter.get_info().backend == wgpu::Backend::Gl;

        let requested_features = super::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 test device"),
                    required_features: requested_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        let caps = GpuCapabilities::from_device(&device).with_downlevel_flags(downlevel.flags);
        let pipelines = PipelineCache::new(PipelineCacheConfig::default(), caps);

        Ok(Self {
            device,
            queue,
            supports_compute,
            emulate_strip_restart,
            pipelines,
            resources: D3D11Resources::default(),
            state: D3D11State::new(),
            sampler_cache: SamplerCache::new(),
            bind_group_layout_cache: BindGroupLayoutCache::new(),
            bind_group_cache: BindGroupCache::new(DEFAULT_BIND_GROUP_CACHE_CAPACITY),
            pipeline_layout_cache: PipelineLayoutCache::new(),
            encoder_has_commands: false,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn supports_compute(&self) -> bool {
        self.supports_compute
    }

    pub fn cache_stats(&self) -> D3D11CacheStats {
        D3D11CacheStats {
            samplers: self.sampler_cache.stats(),
            bind_group_layouts: self.bind_group_layout_cache.stats(),
            bind_groups: self.bind_group_cache.stats(),
        }
    }

    pub fn execute(&mut self, words: &[u32]) -> Result<()> {
        self.encoder_has_commands = false;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 execute"),
            });

        let result: Result<()> = (|| {
            let mut stream = CmdStream::new(words);
            while let Some(packet) = stream.next() {
                let packet = packet.map_err(|e| anyhow!("{e}"))?;
                self.exec_packet(&mut encoder, packet, &mut stream)?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.queue.submit([encoder.finish()]);
                self.encoder_has_commands = false;
                Ok(())
            }
            Err(err) => {
                // Drop partially-recorded work, but still flush `queue.write_*` uploads so they
                // don't stay queued indefinitely and reorder with later submissions.
                self.encoder_has_commands = false;
                self.queue.submit([]);
                Err(err)
            }
        }
    }

    fn submit_encoder(&mut self, encoder: &mut wgpu::CommandEncoder, label: &'static str) {
        let new_encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        let finished = std::mem::replace(encoder, new_encoder).finish();
        self.queue.submit([finished]);
        self.encoder_has_commands = false;
    }

    fn submit_encoder_if_has_commands(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        label: &'static str,
    ) {
        if !self.encoder_has_commands {
            return;
        }
        self.submit_encoder(encoder, label);
    }

    pub fn poll_wait(&self) {
        self.poll();
    }

    fn poll(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        self.device.poll(wgpu::Maintain::Poll);
    }

    pub fn buffer_size(&self, id: u32) -> Result<u64> {
        self.resources
            .buffers
            .get(&id)
            .map(|b| b.size)
            .ok_or_else(|| anyhow!("unknown buffer {id}"))
    }

    pub async fn read_buffer(&self, id: u32, offset: u64, size: u64) -> Result<Vec<u8>> {
        let buffer = self
            .resources
            .buffers
            .get(&id)
            .ok_or_else(|| anyhow!("unknown buffer {id}"))?;

        if size == 0 {
            return Ok(Vec::new());
        }

        let end = offset.checked_add(size).ok_or_else(|| {
            anyhow!("read_buffer range overflows u64 (offset={offset} size={size})")
        })?;
        if end > buffer.size {
            bail!(
                "read_buffer out of bounds: offset={offset} size={size} buffer_size={}",
                buffer.size
            );
        }

        // WebGPU buffers cannot be both `MAP_READ` and `STORAGE`/`VERTEX`/`INDEX`, but tests and
        // internal tooling frequently want to read back those GPU-only buffers.
        //
        // Perform a staging copy into a dedicated `MAP_READ | COPY_DST` buffer and map that.
        let align = wgpu::COPY_BUFFER_ALIGNMENT;
        let aligned_offset = offset / align * align;
        let aligned_end = end.div_ceil(align) * align;
        let aligned_size = aligned_end
            .checked_sub(aligned_offset)
            .ok_or_else(|| anyhow!("read_buffer aligned range underflow"))?;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 read_buffer staging"),
            size: aligned_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 read_buffer encoder"),
            });
        encoder.copy_buffer_to_buffer(&buffer.buffer, aligned_offset, &staging, 0, aligned_size);
        self.queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).ok();
        });
        self.poll();
        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
            .context("wgpu: map_async failed")?;

        let offset_in_staging = offset - aligned_offset;
        let start = usize::try_from(offset_in_staging)
            .context("read_buffer offset_in_staging overflows usize")?;
        let len = usize::try_from(size).context("read_buffer size overflows usize")?;
        let end = start
            .checked_add(len)
            .ok_or_else(|| anyhow!("read_buffer staging range overflows usize"))?;

        let mapped = slice.get_mapped_range();
        let data = mapped
            .get(start..end)
            .ok_or_else(|| anyhow!("read_buffer staging range out of bounds"))?
            .to_vec();
        drop(mapped);
        staging.unmap();
        Ok(data)
    }

    pub async fn read_texture_rgba8(&self, texture_id: u32) -> Result<Vec<u8>> {
        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;

        let needs_bgra_swizzle = match texture.desc.format {
            wgpu::TextureFormat::Rgba8Unorm => false,
            wgpu::TextureFormat::Bgra8Unorm => true,
            other => {
                bail!("read_texture_rgba8 only supports Rgba8Unorm/Bgra8Unorm (got {other:?})")
            }
        };

        let width = texture.desc.width;
        let height = texture.desc.height;

        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 read_texture staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-d3d11 read_texture encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture.texture,
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
        self.poll();
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

    fn exec_packet(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        packet: CmdPacket<'_>,
        stream: &mut CmdStream<'_>,
    ) -> Result<()> {
        match packet.header.opcode {
            D3D11Opcode::CreateBuffer => self.exec_create_buffer(packet.payload),
            D3D11Opcode::UpdateBuffer => self.exec_update_buffer(encoder, packet.payload),
            D3D11Opcode::CreateTexture2D => self.exec_create_texture2d(packet.payload),
            D3D11Opcode::UpdateTexture2D => self.exec_update_texture2d(encoder, packet.payload),
            D3D11Opcode::CreateTextureView => self.exec_create_texture_view(packet.payload),
            D3D11Opcode::CreateSampler => self.exec_create_sampler(packet.payload),
            D3D11Opcode::CreateShaderModuleWgsl => {
                self.exec_create_shader_module_wgsl(packet.payload)
            }
            D3D11Opcode::CreateRenderPipeline => self.exec_create_render_pipeline(packet.payload),
            D3D11Opcode::CreateComputePipeline => self.exec_create_compute_pipeline(packet.payload),
            D3D11Opcode::SetPipeline => state_set_pipeline(&mut self.state, packet.payload),
            D3D11Opcode::SetVertexBuffer => {
                state_set_vertex_buffer(&mut self.state, packet.payload)
            }
            D3D11Opcode::SetIndexBuffer => state_set_index_buffer(&mut self.state, packet.payload),
            D3D11Opcode::SetBindBuffer => state_set_bind_buffer(&mut self.state, packet.payload),
            D3D11Opcode::SetBindSampler => state_set_bind_sampler(&mut self.state, packet.payload),
            D3D11Opcode::SetBindTextureView => {
                state_set_bind_texture_view(&mut self.state, packet.payload)
            }
            D3D11Opcode::BeginRenderPass => self.exec_render_pass(encoder, packet.payload, stream),
            D3D11Opcode::EndRenderPass => bail!("unexpected EndRenderPass outside render pass"),
            D3D11Opcode::BeginComputePass => self.exec_compute_pass(encoder, stream),
            D3D11Opcode::EndComputePass => bail!("unexpected EndComputePass outside compute pass"),
            D3D11Opcode::Draw | D3D11Opcode::DrawIndexed => {
                bail!("draw commands must be inside BeginRenderPass/EndRenderPass")
            }
            D3D11Opcode::Dispatch => {
                bail!("dispatch must be inside BeginComputePass/EndComputePass")
            }
            D3D11Opcode::CopyBufferToBuffer => {
                self.exec_copy_buffer_to_buffer(encoder, packet.payload)
            }
        }
    }

    fn take_bytes(payload: &[u32], fixed_words: usize) -> Result<&[u8]> {
        let bytes_start = fixed_words
            .checked_add(1)
            .ok_or_else(|| anyhow!("fixed payload word count overflows usize"))?;
        if payload.len() < bytes_start {
            bail!(
                "expected at least {} words for fixed payload + byte len",
                fixed_words
            );
        }
        let byte_len = payload[fixed_words] as usize;
        let bytes_words = byte_len.div_ceil(4);
        let bytes_end = bytes_start
            .checked_add(bytes_words)
            .ok_or_else(|| anyhow!("byte payload length overflows usize"))?;
        if bytes_end > payload.len() {
            bail!(
                "truncated byte payload: need {} words, have {}",
                bytes_words,
                payload.len().saturating_sub(bytes_start)
            );
        }
        let byte_words = &payload[bytes_start..bytes_end];
        let bytes_ptr = byte_words.as_ptr() as *const u8;
        let bytes_len = bytes_words
            .checked_mul(4)
            .ok_or_else(|| anyhow!("byte payload size overflows usize"))?;
        // Safety: `u32` slice is properly aligned and we only read within it.
        let bytes = unsafe { std::slice::from_raw_parts(bytes_ptr, bytes_len) };
        Ok(&bytes[..byte_len])
    }

    fn exec_create_buffer(&mut self, payload: &[u32]) -> Result<()> {
        if payload.len() != 4 {
            bail!(
                "CreateBuffer payload words expected 4, got {}",
                payload.len()
            );
        }
        let id = payload[0];
        let size = (payload[1] as u64) | ((payload[2] as u64) << 32);
        let usage = BufferUsage::from_bits_truncate(payload[3]);

        let mut wgpu_usage = map_buffer_usage(usage, self.supports_compute);
        // `wgpu::Queue::write_buffer` / `copy_buffer_to_buffer` require COPY_* usages. To keep the
        // runtime robust against callers that omit copy flags, add them back where valid.
        //
        // Note: wgpu enforces that mappable buffers only combine MAP usage with the "opposite"
        // copy direction:
        // - MAP_READ  -> COPY_DST
        // - MAP_WRITE -> COPY_SRC
        let has_map_read = wgpu_usage.contains(wgpu::BufferUsages::MAP_READ);
        let has_map_write = wgpu_usage.contains(wgpu::BufferUsages::MAP_WRITE);
        if has_map_read && has_map_write {
            bail!("CreateBuffer: MAP_READ|MAP_WRITE is not supported by this runtime");
        }
        if has_map_read {
            wgpu_usage |= wgpu::BufferUsages::COPY_DST;
        } else if has_map_write {
            wgpu_usage |= wgpu::BufferUsages::COPY_SRC;
        } else {
            wgpu_usage |= wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        }

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 buffer"),
            size,
            usage: wgpu_usage,
            mapped_at_creation: false,
        });

        let shadow = if self.emulate_strip_restart {
            usize::try_from(size)
                .ok()
                .map(|shadow_len| vec![0u8; shadow_len])
        } else {
            None
        };
        self.resources.buffers.insert(
            id,
            BufferResource {
                buffer,
                size,
                shadow,
            },
        );
        Ok(())
    }

    fn exec_update_buffer(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        payload: &[u32],
    ) -> Result<()> {
        if payload.len() < 4 {
            bail!("UpdateBuffer payload too small");
        }
        let id = payload[0];
        let offset = (payload[1] as u64) | ((payload[2] as u64) << 32);
        let bytes = Self::take_bytes(payload, 3)?;
        let buffer_size = self
            .resources
            .buffers
            .get(&id)
            .ok_or_else(|| anyhow!("unknown buffer {id}"))?
            .size;
        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        let size_bytes = bytes.len() as u64;
        if !offset.is_multiple_of(alignment) || !size_bytes.is_multiple_of(alignment) {
            bail!(
                "UpdateBuffer offset/size must be {alignment}-byte aligned (offset={offset} size_bytes={size_bytes})"
            );
        }
        if offset.saturating_add(size_bytes) > buffer_size {
            bail!(
                "UpdateBuffer out of bounds: offset={offset} size_bytes={size_bytes} buffer_size={buffer_size}"
            );
        }

        // Preserve stream ordering relative to any previously encoded GPU work.
        self.submit_encoder_if_has_commands(encoder, "aero-d3d11 encoder after UpdateBuffer");

        let buffer = self
            .resources
            .buffers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("unknown buffer {id}"))?;
        self.queue.write_buffer(&buffer.buffer, offset, bytes);
        if let Some(shadow) = buffer.shadow.as_mut() {
            let shadow_start =
                usize::try_from(offset).context("UpdateBuffer offset overflows usize")?;
            let shadow_end = shadow_start
                .checked_add(bytes.len())
                .ok_or_else(|| anyhow!("UpdateBuffer shadow range overflows usize"))?;
            shadow
                .get_mut(shadow_start..shadow_end)
                .ok_or_else(|| anyhow!("UpdateBuffer shadow range out of bounds"))?
                .copy_from_slice(bytes);
        }
        Ok(())
    }

    fn exec_create_texture2d(&mut self, payload: &[u32]) -> Result<()> {
        if payload.len() != 7 {
            bail!(
                "CreateTexture2D payload words expected 7, got {}",
                payload.len()
            );
        }
        let id = payload[0];
        let width = payload[1];
        let height = payload[2];
        let array_layers = payload[3];
        let mip_level_count = payload[4];
        let format = DxgiFormat::from_word(payload[5]);
        let usage = TextureUsage::from_bits_truncate(payload[6]);

        if width == 0 || height == 0 {
            bail!("CreateTexture2D width/height must be non-zero");
        }
        if array_layers == 0 {
            bail!("CreateTexture2D array_layers must be >= 1");
        }
        if mip_level_count == 0 {
            bail!("CreateTexture2D mip_level_count must be >= 1");
        }
        let limits = self.device.limits();
        let max_texture_dim = limits.max_texture_dimension_2d;
        if width > max_texture_dim || height > max_texture_dim {
            bail!(
                "CreateTexture2D dimensions {width}x{height} exceed device limit {max_texture_dim}"
            );
        }
        let max_texture_layers = limits.max_texture_array_layers;
        if array_layers > max_texture_layers {
            bail!(
                "CreateTexture2D array_layers {array_layers} exceed device limit {max_texture_layers}"
            );
        }
        // WebGPU validation requires `mip_level_count` to be within the possible chain length for
        // the given dimensions.
        let max_dim = width.max(height);
        let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
        if mip_level_count > max_mip_levels {
            bail!(
                "CreateTexture2D mip_level_count {mip_level_count} exceeds maximum {max_mip_levels} for {width}x{height} texture"
            );
        }

        if usage.contains(TextureUsage::STORAGE_BINDING)
            && self.device.limits().max_storage_textures_per_shader_stage == 0
        {
            bail!(
                "CreateTexture2D requested STORAGE_BINDING, but this device reports max_storage_textures_per_shader_stage=0"
            );
        }

        let format = map_texture_format(format)?;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d11 texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: array_layers,
            },
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // `wgpu::Queue::write_texture` requires COPY_DST. Also include COPY_SRC so test helpers
            // like `read_texture_rgba8` can safely copy out of textures even if the protocol usage
            // omitted it.
            usage: map_texture_usage(usage, self.supports_compute)
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.resources.textures.insert(
            id,
            TextureResource {
                texture,
                desc: Texture2dDesc {
                    width,
                    height,
                    array_layers,
                    mip_level_count,
                    format,
                },
            },
        );
        Ok(())
    }

    fn exec_update_texture2d(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        payload: &[u32],
    ) -> Result<()> {
        if payload.len() < 7 {
            bail!("UpdateTexture2D payload too small");
        }
        let texture_id = payload[0];
        let mip_level = payload[1];
        let array_layer = payload[2];
        let width = payload[3];
        let height = payload[4];
        let bytes_per_row = payload[5];
        let bytes = Self::take_bytes(payload, 6)?;

        let texture_desc = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?
            .desc
            .clone();
        let texture_format = texture_desc.format;

        if width == 0 || height == 0 {
            bail!("UpdateTexture2D width/height must be non-zero");
        }
        if mip_level >= texture_desc.mip_level_count {
            bail!(
                "UpdateTexture2D mip_level {mip_level} out of range (mip_level_count={})",
                texture_desc.mip_level_count
            );
        }
        if array_layer >= texture_desc.array_layers {
            bail!(
                "UpdateTexture2D array_layer {array_layer} out of range (array_layers={})",
                texture_desc.array_layers
            );
        }
        let mip_width = texture_desc
            .width
            .checked_shr(mip_level)
            .unwrap_or(0)
            .max(1);
        let mip_height = texture_desc
            .height
            .checked_shr(mip_level)
            .unwrap_or(0)
            .max(1);
        if width > mip_width || height > mip_height {
            bail!(
                "UpdateTexture2D update extent {width}x{height} out of bounds for mip {mip_level} (mip_size={mip_width}x{mip_height})"
            );
        }

        let bytes_per_texel = match texture_format {
            wgpu::TextureFormat::Rgba8Unorm
            | wgpu::TextureFormat::Rgba8UnormSrgb
            | wgpu::TextureFormat::Bgra8Unorm
            | wgpu::TextureFormat::Bgra8UnormSrgb
            | wgpu::TextureFormat::R32Float
            | wgpu::TextureFormat::Depth32Float
            | wgpu::TextureFormat::Depth24PlusStencil8 => 4u32,
            wgpu::TextureFormat::Rgba16Float => 8u32,
            wgpu::TextureFormat::Rgba32Float => 16u32,
            other => bail!("UpdateTexture2D: unsupported texture format for CPU upload: {other:?}"),
        };

        let unpadded_bytes_per_row = width
            .checked_mul(bytes_per_texel)
            .ok_or_else(|| anyhow!("UpdateTexture2D: bytes_per_row overflow"))?;

        let src_bytes_per_row = if bytes_per_row == 0 {
            unpadded_bytes_per_row
        } else {
            bytes_per_row
        };

        if src_bytes_per_row < unpadded_bytes_per_row {
            bail!(
                "UpdateTexture2D: bytes_per_row {} is smaller than required row size {}",
                src_bytes_per_row,
                unpadded_bytes_per_row
            );
        }

        let required_src_len = (src_bytes_per_row as usize).saturating_mul(height as usize);
        if bytes.len() < required_src_len {
            bail!(
                "UpdateTexture2D: source data too small: need {} bytes for {} rows, got {}",
                required_src_len,
                height,
                bytes.len()
            );
        }

        // WebGPU requires `bytes_per_row` to be aligned to 256 bytes for multi-row uploads. D3D
        // workloads frequently use tightly-packed rows that are not 256-aligned, so we repack into
        // an aligned scratch buffer when needed.
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let needs_repack = height > 1 && !src_bytes_per_row.is_multiple_of(align);

        let (repacked, upload_bytes_per_row) = if needs_repack {
            let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
            let mut tmp = vec![0u8; padded_bytes_per_row as usize * height as usize];
            for row in 0..height as usize {
                let src_start = row * src_bytes_per_row as usize;
                let dst_start = row * padded_bytes_per_row as usize;
                tmp[dst_start..dst_start + unpadded_bytes_per_row as usize].copy_from_slice(
                    &bytes[src_start..src_start + unpadded_bytes_per_row as usize],
                );
            }
            (Some(tmp), padded_bytes_per_row)
        } else {
            (None, src_bytes_per_row)
        };
        let upload_bytes = repacked.as_deref().unwrap_or(bytes);

        // Preserve stream ordering relative to any previously encoded GPU work.
        self.submit_encoder_if_has_commands(encoder, "aero-d3d11 encoder after UpdateTexture2D");

        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture.texture,
                mip_level,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: array_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            upload_bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(upload_bytes_per_row),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    fn exec_create_texture_view(&mut self, payload: &[u32]) -> Result<()> {
        if payload.len() != 6 {
            bail!(
                "CreateTextureView payload words expected 6, got {}",
                payload.len()
            );
        }
        let view_id = payload[0];
        let texture_id = payload[1];
        let base_mip_level = payload[2];
        let mip_level_count = payload[3];
        let base_array_layer = payload[4];
        let array_layer_count = payload[5];

        let texture = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?;

        let mip_level_count = if mip_level_count == 0 {
            None
        } else {
            Some(mip_level_count)
        };
        let array_layer_count = if array_layer_count == 0 {
            None
        } else {
            Some(array_layer_count)
        };

        if base_mip_level >= texture.desc.mip_level_count {
            bail!(
                "CreateTextureView base_mip_level {base_mip_level} out of range (mip_level_count={})",
                texture.desc.mip_level_count
            );
        }
        let resolved_mip_level_count =
            mip_level_count.unwrap_or(texture.desc.mip_level_count - base_mip_level);
        if base_mip_level
            .checked_add(resolved_mip_level_count)
            .ok_or_else(|| anyhow!("CreateTextureView mip level overflow"))?
            > texture.desc.mip_level_count
        {
            bail!(
                "CreateTextureView mip range out of bounds (base_mip_level={base_mip_level} mip_level_count={resolved_mip_level_count} total_mips={})",
                texture.desc.mip_level_count
            );
        }

        if base_array_layer >= texture.desc.array_layers {
            bail!(
                "CreateTextureView base_array_layer {base_array_layer} out of range (array_layers={})",
                texture.desc.array_layers
            );
        }
        let resolved_array_layer_count =
            array_layer_count.unwrap_or(texture.desc.array_layers - base_array_layer);
        if base_array_layer
            .checked_add(resolved_array_layer_count)
            .ok_or_else(|| anyhow!("CreateTextureView array layer overflow"))?
            > texture.desc.array_layers
        {
            bail!(
                "CreateTextureView array range out of bounds (base_array_layer={base_array_layer} array_layer_count={resolved_array_layer_count} total_layers={})",
                texture.desc.array_layers
            );
        }

        let view_dimension = if resolved_array_layer_count == 1 {
            wgpu::TextureViewDimension::D2
        } else {
            wgpu::TextureViewDimension::D2Array
        };

        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("aero-d3d11 texture view"),
            format: None,
            dimension: Some(view_dimension),
            aspect: wgpu::TextureAspect::All,
            base_mip_level,
            mip_level_count,
            base_array_layer,
            array_layer_count,
        });
        self.resources
            .texture_views
            .insert(view_id, TextureViewResource { view });
        Ok(())
    }

    fn exec_create_sampler(&mut self, payload: &[u32]) -> Result<()> {
        if payload.len() != 2 {
            bail!(
                "CreateSampler payload words expected 2, got {}",
                payload.len()
            );
        }
        let sampler_id = payload[0];
        let filter_mode = payload[1];

        let cached = self.sampler_cache.get_or_create(
            &self.device,
            &wgpu::SamplerDescriptor {
                label: None,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: if filter_mode == 0 {
                    wgpu::FilterMode::Nearest
                } else {
                    wgpu::FilterMode::Linear
                },
                min_filter: if filter_mode == 0 {
                    wgpu::FilterMode::Nearest
                } else {
                    wgpu::FilterMode::Linear
                },
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            },
        );
        self.resources.samplers.insert(
            sampler_id,
            SamplerResource {
                id: cached.id,
                sampler: cached.sampler,
            },
        );
        Ok(())
    }

    fn exec_create_shader_module_wgsl(&mut self, payload: &[u32]) -> Result<()> {
        if payload.len() < 2 {
            bail!("CreateShaderModuleWgsl payload too small");
        }
        let shader_id = payload[0];
        let bytes = Self::take_bytes(payload, 1)?;
        let wgsl = std::str::from_utf8(bytes).context("shader WGSL not valid UTF-8")?;
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aero-d3d11 shader module"),
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
            });
        self.resources.shaders.insert(
            shader_id,
            ShaderModuleResource {
                module,
                wgsl: wgsl.to_owned(),
            },
        );
        Ok(())
    }

    fn exec_create_render_pipeline(&mut self, payload: &[u32]) -> Result<()> {
        let mut cursor = 0usize;
        let take = |payload: &[u32], cursor: &mut usize| -> Result<u32> {
            if *cursor >= payload.len() {
                bail!("unexpected end of payload while decoding render pipeline");
            }
            let v = payload[*cursor];
            *cursor += 1;
            Ok(v)
        };

        let pipeline_id = take(payload, &mut cursor)?;
        let vs_shader = take(payload, &mut cursor)?;
        let fs_shader = take(payload, &mut cursor)?;
        let color_format = DxgiFormat::from_word(take(payload, &mut cursor)?);
        let depth_format = DxgiFormat::from_word(take(payload, &mut cursor)?);
        let topology = take(payload, &mut cursor)?;

        let vs = self
            .resources
            .shaders
            .get(&vs_shader)
            .ok_or_else(|| anyhow!("unknown shader module {vs_shader}"))?;
        let fs = self
            .resources
            .shaders
            .get(&fs_shader)
            .ok_or_else(|| anyhow!("unknown shader module {fs_shader}"))?;

        // WebGPU requires the vertex output interface to exactly match the fragment input
        // interface. D3D shaders often export extra varyings (so a single VS can be reused with
        // multiple PS variants), and pixel shaders may declare inputs they never read.
        //
        // To preserve D3D behavior (and satisfy WebGPU validation), trim the stage interface:
        // - Drop unused PS inputs when the VS does not output them.
        // - Drop unused VS outputs that the PS does not declare.
        let ps_declared_inputs = super::wgsl_link::locations_in_struct(&fs.wgsl, "PsIn")?;
        let vs_outputs = super::wgsl_link::locations_in_struct(&vs.wgsl, "VsOut")?;
        let ps_can_trim_inputs = fs.wgsl.contains("struct PsIn {")
            && fs.wgsl.contains("fn fs_main(")
            && fs.wgsl.contains("input: PsIn");
        let vs_can_trim_outputs = vs.wgsl.contains("struct VsOut {")
            && vs.wgsl.contains("fn vs_main(")
            && vs.wgsl.contains("var out: VsOut");
        let mut ps_link_locations = ps_declared_inputs.clone();

        let ps_missing_locations: BTreeSet<u32> = ps_declared_inputs
            .difference(&vs_outputs)
            .copied()
            .collect();
        if ps_can_trim_inputs && !ps_missing_locations.is_empty() {
            let ps_used_locations = super::wgsl_link::referenced_ps_input_locations(&fs.wgsl);
            let used_missing: Vec<u32> = ps_missing_locations
                .intersection(&ps_used_locations)
                .copied()
                .collect();
            if let Some(&loc) = used_missing.first() {
                bail!("fragment shader reads @location({loc}), but VS does not output it");
            }
            ps_link_locations = ps_declared_inputs
                .intersection(&vs_outputs)
                .copied()
                .collect();
        }

        let mut linked_fs_wgsl = Cow::Borrowed(fs.wgsl.as_str());
        if ps_can_trim_inputs && ps_link_locations != ps_declared_inputs {
            linked_fs_wgsl = Cow::Owned(super::wgsl_link::trim_ps_inputs_to_locations(
                linked_fs_wgsl.as_ref(),
                &ps_link_locations,
            ));
        }

        // WebGPU requires every fragment `@location(N)` output to have a corresponding
        // `ColorTargetState` at index N. This runtime protocol only exposes a single color target
        // (RT0) in `CreateRenderPipeline`, but real D3D shaders may declare additional MRT outputs.
        // D3D discards writes to unbound RTV slots; emulate this by trimming outputs to location 0.
        let keep_output_locations = BTreeSet::from([0u32]);
        let declared_outputs =
            super::wgsl_link::declared_ps_output_locations(linked_fs_wgsl.as_ref())?;
        let missing_outputs: BTreeSet<u32> = declared_outputs
            .difference(&keep_output_locations)
            .copied()
            .collect();
        // Defer creation of the trimmed shader module until after we've decoded the rest of the
        // pipeline descriptor so we don't hold a borrow of `self.pipelines` across other `&mut self`
        // method calls.
        let trimmed_fs_wgsl: Option<String> =
            if linked_fs_wgsl.as_ref() == fs.wgsl.as_str() && missing_outputs.is_empty() {
                None
            } else if missing_outputs.is_empty() {
                Some(linked_fs_wgsl.into_owned())
            } else {
                // Apply MRT output trimming on top of any PS input trimming already done above.
                Some(super::wgsl_link::trim_ps_outputs_to_locations(
                    linked_fs_wgsl.as_ref(),
                    &keep_output_locations,
                ))
            };

        let trimmed_vs_module = if vs_can_trim_outputs
            && ps_link_locations.is_subset(&vs_outputs)
            && vs_outputs != ps_link_locations
        {
            let trimmed_vs_wgsl =
                super::wgsl_link::trim_vs_outputs_to_locations(&vs.wgsl, &ps_link_locations);
            Some(
                self.device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("aero-d3d11 trimmed vertex shader"),
                        source: wgpu::ShaderSource::Wgsl(trimmed_vs_wgsl.into()),
                    }),
            )
        } else {
            None
        };
        let vs_module_for_pipeline = trimmed_vs_module.as_ref().unwrap_or(&vs.module);

        let color_format = map_texture_format(color_format)?;
        let depth_stencil = if depth_format == DxgiFormat::Unknown {
            None
        } else {
            Some(wgpu::DepthStencilState {
                format: map_texture_format(depth_format)?,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: Default::default(),
            })
        };

        let topology = match topology {
            x if x == PrimitiveTopology::TriangleList as u32 => {
                wgpu::PrimitiveTopology::TriangleList
            }
            x if x == PrimitiveTopology::TriangleStrip as u32 => {
                wgpu::PrimitiveTopology::TriangleStrip
            }
            x if x == PrimitiveTopology::LineList as u32 => wgpu::PrimitiveTopology::LineList,
            x if x == PrimitiveTopology::LineStrip as u32 => wgpu::PrimitiveTopology::LineStrip,
            x if x == PrimitiveTopology::PointList as u32 => wgpu::PrimitiveTopology::PointList,
            _ => bail!("unknown primitive topology {topology}"),
        };

        struct ParsedVertexBuffer {
            array_stride: u64,
            step_mode: wgpu::VertexStepMode,
            attrs: Box<[wgpu::VertexAttribute]>,
        }

        let vb_count = take(payload, &mut cursor)? as usize;
        let mut parsed_vbs: Vec<ParsedVertexBuffer> = Vec::with_capacity(vb_count);
        for _ in 0..vb_count {
            let array_stride = take(payload, &mut cursor)?;
            let step_mode = take(payload, &mut cursor)?;
            let attr_count = take(payload, &mut cursor)? as usize;
            let mut attrs: Vec<wgpu::VertexAttribute> = Vec::with_capacity(attr_count);
            for _ in 0..attr_count {
                let shader_location = take(payload, &mut cursor)?;
                let offset = take(payload, &mut cursor)?;
                let format = take(payload, &mut cursor)?;
                attrs.push(wgpu::VertexAttribute {
                    shader_location,
                    offset: offset as u64,
                    format: map_vertex_format(format)?,
                });
            }
            parsed_vbs.push(ParsedVertexBuffer {
                array_stride: array_stride as u64,
                step_mode: if step_mode == VertexStepMode::Vertex as u32 {
                    wgpu::VertexStepMode::Vertex
                } else {
                    wgpu::VertexStepMode::Instance
                },
                attrs: attrs.into_boxed_slice(),
            });
        }

        let vertex_buffers: Vec<wgpu::VertexBufferLayout<'_>> = parsed_vbs
            .iter()
            .map(|vb| wgpu::VertexBufferLayout {
                array_stride: vb.array_stride,
                step_mode: vb.step_mode,
                attributes: &vb.attrs,
            })
            .collect();

        let binding_count = take(payload, &mut cursor)? as usize;
        let bindings = self.decode_binding_defs(payload, &mut cursor, binding_count)?;
        self.validate_storage_binding_capabilities("CreateRenderPipeline", &bindings)?;
        let bind_group_layout_entries: Vec<wgpu::BindGroupLayoutEntry> =
            bindings.iter().map(binding_def_to_layout_entry).collect();
        let bind_group_layout = self
            .bind_group_layout_cache
            .get_or_create(&self.device, &bind_group_layout_entries);

        let layout_key = PipelineLayoutKey {
            bind_group_layout_hashes: vec![bind_group_layout.hash],
        };
        let pipeline_layout = self.pipeline_layout_cache.get_or_create(
            &self.device,
            &layout_key,
            &[bind_group_layout.layout.as_ref()],
            Some("aero-d3d11 pipeline layout"),
        );

        let color_target_states = [Some(wgpu::ColorTargetState {
            format: color_format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        // WebGPU requires `PrimitiveState.strip_index_format` to be specified in the render
        // pipeline when using indexed strip topologies (`LineStrip` / `TriangleStrip`). Since the
        // protocol models D3D11's decoupled state (index buffer bound separately from pipeline
        // creation), we build pipeline variants for each possible index format and select the
        // correct variant at draw time.
        let is_strip_topology = matches!(
            topology,
            wgpu::PrimitiveTopology::LineStrip | wgpu::PrimitiveTopology::TriangleStrip
        );

        let fs_module_for_pipeline = if let Some(ref trimmed_wgsl) = trimmed_fs_wgsl {
            let (_hash, module) = self.pipelines.get_or_create_shader_module(
                &self.device,
                ShaderStage::Fragment,
                trimmed_wgsl,
                Some("aero-d3d11 trimmed fragment shader"),
            );
            module
        } else {
            &fs.module
        };

        let create_pipeline =
            |topology: wgpu::PrimitiveTopology, strip_index_format: Option<wgpu::IndexFormat>| {
                self.device
                    .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some("aero-d3d11 render pipeline"),
                        layout: Some(pipeline_layout.as_ref()),
                        vertex: wgpu::VertexState {
                            module: vs_module_for_pipeline,
                            entry_point: "vs_main",
                            compilation_options: wgpu::PipelineCompilationOptions::default(),
                            buffers: &vertex_buffers,
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: fs_module_for_pipeline,
                            entry_point: "fs_main",
                            compilation_options: wgpu::PipelineCompilationOptions::default(),
                            targets: &color_target_states,
                        }),
                        primitive: wgpu::PrimitiveState {
                            topology,
                            strip_index_format,
                            front_face: wgpu::FrontFace::Ccw,
                            cull_mode: None,
                            polygon_mode: wgpu::PolygonMode::Fill,
                            unclipped_depth: false,
                            conservative: false,
                        },
                        depth_stencil: depth_stencil.clone(),
                        multisample: wgpu::MultisampleState::default(),
                        multiview: None,
                    })
            };

        let pipelines = if is_strip_topology {
            RenderPipelineVariants::Strip {
                non_indexed: create_pipeline(topology, None),
                u16: create_pipeline(topology, Some(wgpu::IndexFormat::Uint16)),
                u32: create_pipeline(topology, Some(wgpu::IndexFormat::Uint32)),
            }
        } else {
            RenderPipelineVariants::NonStrip(create_pipeline(topology, None))
        };

        self.resources.render_pipelines.insert(
            pipeline_id,
            RenderPipelineResource {
                pipelines,
                topology,
                bind_group_layout,
                bindings,
            },
        );
        Ok(())
    }

    fn exec_create_compute_pipeline(&mut self, payload: &[u32]) -> Result<()> {
        if !self.supports_compute {
            bail!(GpuError::Unsupported("compute"));
        }

        let mut cursor = 0usize;
        let take = |payload: &[u32], cursor: &mut usize| -> Result<u32> {
            if *cursor >= payload.len() {
                bail!("unexpected end of payload while decoding compute pipeline");
            }
            let v = payload[*cursor];
            *cursor += 1;
            Ok(v)
        };

        let pipeline_id = take(payload, &mut cursor)?;
        let cs_shader = take(payload, &mut cursor)?;
        let cs = self
            .resources
            .shaders
            .get(&cs_shader)
            .ok_or_else(|| anyhow!("unknown shader module {cs_shader}"))?;

        let binding_count = take(payload, &mut cursor)? as usize;
        let bindings = self.decode_binding_defs(payload, &mut cursor, binding_count)?;
        let bind_group_layout_entries: Vec<wgpu::BindGroupLayoutEntry> =
            bindings.iter().map(binding_def_to_layout_entry).collect();
        let bind_group_layout = self
            .bind_group_layout_cache
            .get_or_create(&self.device, &bind_group_layout_entries);

        // Compute shaders produced by the signature-driven SM4/5 translator use the stage-scoped
        // AeroGPU binding model (`@group(2)` for CS). Some older protocol WGSL expected `@group(0)`.
        // Build a pipeline layout that exposes the same layout at both group 0 and group 2 (with
        // an empty group 1 in between) so both conventions can execute.
        let empty_bind_group_layout = self
            .bind_group_layout_cache
            .get_or_create(&self.device, &[]);

        let layout_key = PipelineLayoutKey {
            bind_group_layout_hashes: vec![
                bind_group_layout.hash,
                empty_bind_group_layout.hash,
                bind_group_layout.hash,
            ],
        };
        let pipeline_layout = self.pipeline_layout_cache.get_or_create(
            &self.device,
            &layout_key,
            &[
                bind_group_layout.layout.as_ref(),
                empty_bind_group_layout.layout.as_ref(),
                bind_group_layout.layout.as_ref(),
            ],
            Some("aero-d3d11 compute pipeline layout (groups 0..2)"),
        );

        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("aero-d3d11 compute pipeline"),
                layout: Some(pipeline_layout.as_ref()),
                module: &cs.module,
                entry_point: "cs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            });

        self.resources.compute_pipelines.insert(
            pipeline_id,
            ComputePipelineResource {
                pipeline,
                bind_group_layout,
                bindings,
            },
        );
        Ok(())
    }

    fn decode_binding_defs(
        &self,
        payload: &[u32],
        cursor: &mut usize,
        binding_count: usize,
    ) -> Result<Vec<BindingDef>> {
        let features = self.device.features();
        let max_storage_buffers_per_shader_stage =
            self.device.limits().max_storage_buffers_per_shader_stage;
        let max_storage_textures_per_shader_stage =
            self.device.limits().max_storage_textures_per_shader_stage;
        let max_uniform_buffers_per_shader_stage =
            self.device.limits().max_uniform_buffers_per_shader_stage;
        let max_sampled_textures_per_shader_stage =
            self.device.limits().max_sampled_textures_per_shader_stage;
        let max_samplers_per_shader_stage = self.device.limits().max_samplers_per_shader_stage;
        let max_bindings_per_bind_group = self.device.limits().max_bindings_per_bind_group as usize;

        // Keep bindings unique by `@binding(N)` while decoding. Duplicate bindings can arise from
        // buggy command writers and would otherwise trigger wgpu validation panics when creating
        // bind group layouts.
        let mut bindings: BTreeMap<u32, BindingDef> = BTreeMap::new();
        for _ in 0..binding_count {
            if *cursor + 4 > payload.len() {
                bail!("unexpected end of payload while decoding binding defs");
            }
            let binding = payload[*cursor];
            let ty = payload[*cursor + 1];
            let visibility_bits = payload[*cursor + 2];
            let storage_tex_format = DxgiFormat::from_word(payload[*cursor + 3]);
            *cursor += 4;

            let visibility =
                map_shader_stages(ShaderStageFlags::from_bits_truncate(visibility_bits))?;
            let kind = match ty {
                x if x == BindingType::UniformBuffer as u32 => BindingKind::UniformBuffer,
                x if x == BindingType::StorageBufferReadOnly as u32 => {
                    BindingKind::StorageBuffer { read_only: true }
                }
                x if x == BindingType::StorageBufferReadWrite as u32 => {
                    BindingKind::StorageBuffer { read_only: false }
                }
                x if x == BindingType::Sampler as u32 => BindingKind::Sampler,
                x if x == BindingType::Texture2D as u32 => BindingKind::Texture2D,
                x if x == BindingType::StorageTexture2DWriteOnly as u32 => {
                    BindingKind::StorageTexture2DWriteOnly {
                        format: map_texture_format(storage_tex_format)?,
                    }
                }
                _ => bail!("unknown binding type {ty}"),
            };

            if matches!(kind, BindingKind::StorageBuffer { .. })
                && max_storage_buffers_per_shader_stage == 0
            {
                bail!(
                    "binding @binding({binding}) requires storage buffers, but this device reports max_storage_buffers_per_shader_stage=0"
                );
            }

            if matches!(kind, BindingKind::StorageTexture2DWriteOnly { .. })
                && max_storage_textures_per_shader_stage == 0
            {
                bail!(
                    "binding @binding({binding}) requires storage textures, but this device reports max_storage_textures_per_shader_stage=0"
                );
            }

            // WebGPU only allows writable storage buffers/textures in the compute stage. wgpu
            // exposes optional native-only features to enable writable storage in vertex/fragment
            // stages. If those features are absent, fail fast with a clear diagnostic rather than
            // triggering a wgpu validation panic during pipeline creation.
            let writable_storage = matches!(kind, BindingKind::StorageBuffer { read_only: false })
                || matches!(kind, BindingKind::StorageTexture2DWriteOnly { .. });
            if writable_storage {
                if visibility.contains(wgpu::ShaderStages::VERTEX)
                    && !features.contains(wgpu::Features::VERTEX_WRITABLE_STORAGE)
                {
                    bail!(
                        "binding @binding({binding}) uses writable storage in vertex stage, but device does not support wgpu::Features::VERTEX_WRITABLE_STORAGE"
                    );
                }
                if visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    bail!(
                        "binding @binding({binding}) uses writable storage in fragment stage, which is not supported by this wgpu/WebGPU build"
                    );
                }
            }

            match bindings.entry(binding) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(BindingDef {
                        binding,
                        visibility,
                        kind,
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    if existing.kind != kind {
                        bail!(
                            "binding @binding({binding}) kind mismatch ({:?} vs {:?})",
                            existing.kind,
                            kind
                        );
                    }
                    existing.visibility |= visibility;
                }
            }
        }

        if bindings.len() > max_bindings_per_bind_group {
            bail!(
                "pipeline uses {} bindings in its bind group, but device limit max_bindings_per_bind_group={max_bindings_per_bind_group}",
                bindings.len()
            );
        }

        // wgpu enforces per-stage resource count limits. Validate early so callers get a clear
        // error rather than a backend validation panic.
        let mut uniform_buffers_vertex = 0u32;
        let mut uniform_buffers_fragment = 0u32;
        let mut uniform_buffers_compute = 0u32;
        let mut sampled_textures_vertex = 0u32;
        let mut sampled_textures_fragment = 0u32;
        let mut sampled_textures_compute = 0u32;
        let mut samplers_vertex = 0u32;
        let mut samplers_fragment = 0u32;
        let mut samplers_compute = 0u32;
        let mut storage_buffers_vertex = 0u32;
        let mut storage_buffers_fragment = 0u32;
        let mut storage_buffers_compute = 0u32;
        let mut storage_textures_vertex = 0u32;
        let mut storage_textures_fragment = 0u32;
        let mut storage_textures_compute = 0u32;

        for def in bindings.values() {
            let is_uniform_buffer = matches!(def.kind, BindingKind::UniformBuffer);
            let is_sampled_texture = matches!(def.kind, BindingKind::Texture2D);
            let is_sampler = matches!(def.kind, BindingKind::Sampler);
            let is_storage_buffer = matches!(def.kind, BindingKind::StorageBuffer { .. });
            let is_storage_texture =
                matches!(def.kind, BindingKind::StorageTexture2DWriteOnly { .. });

            if is_uniform_buffer {
                if def.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    uniform_buffers_vertex = uniform_buffers_vertex.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    uniform_buffers_fragment = uniform_buffers_fragment.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    uniform_buffers_compute = uniform_buffers_compute.saturating_add(1);
                }
            }
            if is_sampled_texture {
                if def.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    sampled_textures_vertex = sampled_textures_vertex.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    sampled_textures_fragment = sampled_textures_fragment.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    sampled_textures_compute = sampled_textures_compute.saturating_add(1);
                }
            }
            if is_sampler {
                if def.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    samplers_vertex = samplers_vertex.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    samplers_fragment = samplers_fragment.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    samplers_compute = samplers_compute.saturating_add(1);
                }
            }
            if is_storage_buffer {
                if def.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    storage_buffers_vertex = storage_buffers_vertex.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    storage_buffers_fragment = storage_buffers_fragment.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    storage_buffers_compute = storage_buffers_compute.saturating_add(1);
                }
            }
            if is_storage_texture {
                if def.visibility.contains(wgpu::ShaderStages::VERTEX) {
                    storage_textures_vertex = storage_textures_vertex.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::FRAGMENT) {
                    storage_textures_fragment = storage_textures_fragment.saturating_add(1);
                }
                if def.visibility.contains(wgpu::ShaderStages::COMPUTE) {
                    storage_textures_compute = storage_textures_compute.saturating_add(1);
                }
            }
        }

        if uniform_buffers_vertex > max_uniform_buffers_per_shader_stage {
            bail!(
                "pipeline uses {uniform_buffers_vertex} uniform buffers in vertex stage, but device limit max_uniform_buffers_per_shader_stage={max_uniform_buffers_per_shader_stage}"
            );
        }
        if uniform_buffers_fragment > max_uniform_buffers_per_shader_stage {
            bail!(
                "pipeline uses {uniform_buffers_fragment} uniform buffers in fragment stage, but device limit max_uniform_buffers_per_shader_stage={max_uniform_buffers_per_shader_stage}"
            );
        }
        if uniform_buffers_compute > max_uniform_buffers_per_shader_stage {
            bail!(
                "pipeline uses {uniform_buffers_compute} uniform buffers in compute stage, but device limit max_uniform_buffers_per_shader_stage={max_uniform_buffers_per_shader_stage}"
            );
        }

        if sampled_textures_vertex > max_sampled_textures_per_shader_stage {
            bail!(
                "pipeline uses {sampled_textures_vertex} sampled textures in vertex stage, but device limit max_sampled_textures_per_shader_stage={max_sampled_textures_per_shader_stage}"
            );
        }
        if sampled_textures_fragment > max_sampled_textures_per_shader_stage {
            bail!(
                "pipeline uses {sampled_textures_fragment} sampled textures in fragment stage, but device limit max_sampled_textures_per_shader_stage={max_sampled_textures_per_shader_stage}"
            );
        }
        if sampled_textures_compute > max_sampled_textures_per_shader_stage {
            bail!(
                "pipeline uses {sampled_textures_compute} sampled textures in compute stage, but device limit max_sampled_textures_per_shader_stage={max_sampled_textures_per_shader_stage}"
            );
        }

        if samplers_vertex > max_samplers_per_shader_stage {
            bail!(
                "pipeline uses {samplers_vertex} samplers in vertex stage, but device limit max_samplers_per_shader_stage={max_samplers_per_shader_stage}"
            );
        }
        if samplers_fragment > max_samplers_per_shader_stage {
            bail!(
                "pipeline uses {samplers_fragment} samplers in fragment stage, but device limit max_samplers_per_shader_stage={max_samplers_per_shader_stage}"
            );
        }
        if samplers_compute > max_samplers_per_shader_stage {
            bail!(
                "pipeline uses {samplers_compute} samplers in compute stage, but device limit max_samplers_per_shader_stage={max_samplers_per_shader_stage}"
            );
        }

        if storage_buffers_vertex > max_storage_buffers_per_shader_stage {
            bail!(
                "pipeline uses {storage_buffers_vertex} storage buffers in vertex stage, but device limit max_storage_buffers_per_shader_stage={max_storage_buffers_per_shader_stage}"
            );
        }
        if storage_buffers_fragment > max_storage_buffers_per_shader_stage {
            bail!(
                "pipeline uses {storage_buffers_fragment} storage buffers in fragment stage, but device limit max_storage_buffers_per_shader_stage={max_storage_buffers_per_shader_stage}"
            );
        }
        if storage_buffers_compute > max_storage_buffers_per_shader_stage {
            bail!(
                "pipeline uses {storage_buffers_compute} storage buffers in compute stage, but device limit max_storage_buffers_per_shader_stage={max_storage_buffers_per_shader_stage}"
            );
        }

        if storage_textures_vertex > max_storage_textures_per_shader_stage {
            bail!(
                "pipeline uses {storage_textures_vertex} storage textures in vertex stage, but device limit max_storage_textures_per_shader_stage={max_storage_textures_per_shader_stage}"
            );
        }
        if storage_textures_fragment > max_storage_textures_per_shader_stage {
            bail!(
                "pipeline uses {storage_textures_fragment} storage textures in fragment stage, but device limit max_storage_textures_per_shader_stage={max_storage_textures_per_shader_stage}"
            );
        }
        if storage_textures_compute > max_storage_textures_per_shader_stage {
            bail!(
                "pipeline uses {storage_textures_compute} storage textures in compute stage, but device limit max_storage_textures_per_shader_stage={max_storage_textures_per_shader_stage}"
            );
        }

        Ok(bindings.into_values().collect())
    }

    fn validate_storage_binding_capabilities(
        &self,
        op: &str,
        bindings: &[BindingDef],
    ) -> Result<()> {
        if self.supports_compute {
            return Ok(());
        }

        let uses_storage = bindings.iter().any(|binding| {
            matches!(
                binding.kind,
                BindingKind::StorageBuffer { .. } | BindingKind::StorageTexture2DWriteOnly { .. }
            )
        });
        if uses_storage {
            bail!(
                "{op} requires compute shaders (storage buffer/texture bindings), but this backend/device does not support compute"
            );
        }

        Ok(())
    }

    fn exec_copy_buffer_to_buffer(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        payload: &[u32],
    ) -> Result<()> {
        if payload.len() != 8 {
            bail!(
                "CopyBufferToBuffer payload words expected 8, got {}",
                payload.len()
            );
        }
        let src = payload[0];
        let src_offset = (payload[1] as u64) | ((payload[2] as u64) << 32);
        let dst = payload[3];
        let dst_offset = (payload[4] as u64) | ((payload[5] as u64) << 32);
        let size = (payload[6] as u64) | ((payload[7] as u64) << 32);

        if size == 0 {
            return Ok(());
        }

        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        if !src_offset.is_multiple_of(alignment)
            || !dst_offset.is_multiple_of(alignment)
            || !size.is_multiple_of(alignment)
        {
            bail!(
                "CopyBufferToBuffer offsets and size must be {alignment}-byte aligned (src_offset={src_offset} dst_offset={dst_offset} size={size})"
            );
        }

        let (src_buf_size, dst_buf_size) = {
            let src_buf = self
                .resources
                .buffers
                .get(&src)
                .ok_or_else(|| anyhow!("unknown buffer {src}"))?;
            let dst_buf = self
                .resources
                .buffers
                .get(&dst)
                .ok_or_else(|| anyhow!("unknown buffer {dst}"))?;
            (src_buf.size, dst_buf.size)
        };

        let src_end = src_offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("CopyBufferToBuffer src range overflows u64"))?;
        let dst_end = dst_offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("CopyBufferToBuffer dst range overflows u64"))?;
        if src_end > src_buf_size || dst_end > dst_buf_size {
            bail!(
                "CopyBufferToBuffer out of bounds: src_end={src_end} (size={}) dst_end={dst_end} (size={})",
                src_buf_size,
                dst_buf_size
            );
        }

        // Update the CPU shadow copies so later commands (e.g. strip primitive-restart emulation)
        // see the same buffer contents that the GPU will.
        let should_update_shadow = self
            .resources
            .buffers
            .get(&src)
            .is_some_and(|b| b.shadow.is_some())
            && self
                .resources
                .buffers
                .get(&dst)
                .is_some_and(|b| b.shadow.is_some());

        if should_update_shadow {
            if let (Ok(shadow_src_start), Ok(shadow_dst_start), Ok(shadow_size)) = (
                usize::try_from(src_offset),
                usize::try_from(dst_offset),
                usize::try_from(size),
            ) {
                let shadow_src_end =
                    shadow_src_start.checked_add(shadow_size).ok_or_else(|| {
                        anyhow!("CopyBufferToBuffer src shadow range overflows usize")
                    })?;
                let shadow_dst_end =
                    shadow_dst_start.checked_add(shadow_size).ok_or_else(|| {
                        anyhow!("CopyBufferToBuffer dst shadow range overflows usize")
                    })?;

                if src == dst {
                    let buf = self
                        .resources
                        .buffers
                        .get_mut(&src)
                        .ok_or_else(|| anyhow!("unknown buffer {src}"))?;
                    if let Some(shadow) = buf.shadow.as_mut() {
                        shadow.copy_within(shadow_src_start..shadow_src_end, shadow_dst_start);
                    }
                } else {
                    let tmp = {
                        let src_buf = self
                            .resources
                            .buffers
                            .get(&src)
                            .ok_or_else(|| anyhow!("unknown buffer {src}"))?;
                        let src_shadow = src_buf
                            .shadow
                            .as_deref()
                            .ok_or_else(|| anyhow!("CopyBufferToBuffer src shadow missing"))?;
                        src_shadow
                            .get(shadow_src_start..shadow_src_end)
                            .ok_or_else(|| {
                                anyhow!("CopyBufferToBuffer src shadow range out of bounds")
                            })?
                            .to_vec()
                    };
                    let dst_buf = self
                        .resources
                        .buffers
                        .get_mut(&dst)
                        .ok_or_else(|| anyhow!("unknown buffer {dst}"))?;
                    if let Some(dst_shadow) = dst_buf.shadow.as_mut() {
                        dst_shadow
                            .get_mut(shadow_dst_start..shadow_dst_end)
                            .ok_or_else(|| {
                                anyhow!("CopyBufferToBuffer dst shadow range out of bounds")
                            })?
                            .copy_from_slice(&tmp);
                    }
                }
            }
        }

        let src_buf = self
            .resources
            .buffers
            .get(&src)
            .ok_or_else(|| anyhow!("unknown buffer {src}"))?;
        let dst_buf = self
            .resources
            .buffers
            .get(&dst)
            .ok_or_else(|| anyhow!("unknown buffer {dst}"))?;
        encoder.copy_buffer_to_buffer(
            &src_buf.buffer,
            src_offset,
            &dst_buf.buffer,
            dst_offset,
            size,
        );

        self.encoder_has_commands = true;
        Ok(())
    }

    fn exec_render_pass(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        payload: &[u32],
        stream: &mut CmdStream<'_>,
    ) -> Result<()> {
        if payload.len() != 8 {
            bail!(
                "BeginRenderPass payload words expected 8, got {}",
                payload.len()
            );
        }

        self.encoder_has_commands = true;
        let device = &self.device;
        let resources = &self.resources;
        let emulate_strip_restart = self.emulate_strip_restart;
        let (state, bind_group_cache) = (&mut self.state, &mut self.bind_group_cache);

        let color_view_id = payload[0];
        let clear_color = wgpu::Color {
            r: f32::from_bits(payload[1]) as f64,
            g: f32::from_bits(payload[2]) as f64,
            b: f32::from_bits(payload[3]) as f64,
            a: f32::from_bits(payload[4]) as f64,
        };
        let depth_view_id = payload[5];
        let clear_depth = f32::from_bits(payload[6]);
        let clear_stencil = payload[7];

        let color_view = &resources
            .texture_views
            .get(&color_view_id)
            .ok_or_else(|| anyhow!("unknown texture view {color_view_id}"))?
            .view;

        let depth_stencil_attachment = if depth_view_id == 0 {
            None
        } else {
            let depth_view = &resources
                .texture_views
                .get(&depth_view_id)
                .ok_or_else(|| anyhow!("unknown depth texture view {depth_view_id}"))?
                .view;
            Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear_depth),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear_stencil),
                    store: wgpu::StoreOp::Store,
                }),
            })
        };

        // wgpu requires any `&BindGroup` passed to `set_bind_group` to remain alive for the entire
        // render pass lifetime. Since we may change bindings between draws, we keep every bind
        // group we create in an arena for the duration of the pass.
        let mut bind_group_arena: Vec<Arc<wgpu::BindGroup>> = Vec::new();
        let mut current_bind_group: Option<*const wgpu::BindGroup> = None;
        let mut bind_group_dirty = true;

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aero-d3d11 render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        let mut bound_pipeline: Option<BoundRenderPipeline> = None;
        let mut bound_bind_group: Option<*const wgpu::BindGroup> = None;
        let mut bound_vertex_buffers = vec![None; state.vertex_buffers.len()];
        let mut bound_index_buffer: Option<BoundIndexBuffer> = None;
        let mut vertex_buffers_synced = false;

        loop {
            let packet = stream
                .next()
                .ok_or_else(|| anyhow!("unexpected end of command stream inside render pass"))?
                .map_err(|e| anyhow!("{e}"))?;

            match packet.header.opcode {
                D3D11Opcode::EndRenderPass => break,
                D3D11Opcode::SetPipeline => {
                    state_set_pipeline(state, packet.payload)?;
                    let Some(PipelineBinding::Render(pipeline_id)) = state.current_pipeline else {
                        bail!("SetPipeline inside render pass did not select a render pipeline");
                    };
                    let pipeline_changed =
                        bound_pipeline.as_ref().map(|b| b.id) != Some(pipeline_id);
                    sync_render_pipeline(
                        &mut render_pass,
                        resources,
                        pipeline_id,
                        state.index_buffer.map(|ib| ib.format),
                        &mut bound_pipeline,
                    )?;
                    if pipeline_changed {
                        bind_group_dirty = true;
                        current_bind_group = None;
                        bound_bind_group = None;
                    }
                }
                D3D11Opcode::SetVertexBuffer => {
                    state_set_vertex_buffer(state, packet.payload)?;
                    let slot = packet.payload[0] as usize;
                    if let Some(vb) = state.vertex_buffers[slot] {
                        sync_vertex_buffer_slot(
                            &mut render_pass,
                            resources,
                            slot as u32,
                            vb,
                            &mut bound_vertex_buffers[slot],
                        )?;
                    }
                }
                D3D11Opcode::SetIndexBuffer => {
                    state_set_index_buffer(state, packet.payload)?;
                    if let Some(index) = state.index_buffer {
                        sync_index_buffer(
                            &mut render_pass,
                            resources,
                            index,
                            &mut bound_index_buffer,
                        )?;
                    }
                }
                D3D11Opcode::SetBindBuffer => {
                    state_set_bind_buffer(state, packet.payload)?;
                    bind_group_dirty = true;
                }
                D3D11Opcode::SetBindSampler => {
                    state_set_bind_sampler(state, packet.payload)?;
                    bind_group_dirty = true;
                }
                D3D11Opcode::SetBindTextureView => {
                    state_set_bind_texture_view(state, packet.payload)?;
                    bind_group_dirty = true;
                }
                D3D11Opcode::Draw => {
                    if packet.payload.len() != 4 {
                        bail!(
                            "Draw payload words expected 4, got {}",
                            packet.payload.len()
                        );
                    }

                    let vertex_count = packet.payload[0];
                    let instance_count = packet.payload[1];
                    let first_vertex = packet.payload[2];
                    let first_instance = packet.payload[3];

                    let Some(PipelineBinding::Render(pipeline_id)) = state.current_pipeline else {
                        bail!("Draw without a bound render pipeline");
                    };
                    sync_render_pipeline(
                        &mut render_pass,
                        resources,
                        pipeline_id,
                        state.index_buffer.map(|ib| ib.format),
                        &mut bound_pipeline,
                    )?;
                    let pipeline = resources
                        .render_pipelines
                        .get(&pipeline_id)
                        .ok_or_else(|| anyhow!("unknown render pipeline {pipeline_id}"))?;

                    if bind_group_dirty || current_bind_group.is_none() {
                        let bg = build_bind_group(
                            device,
                            bind_group_cache,
                            resources,
                            state,
                            &pipeline.bind_group_layout,
                            &pipeline.bindings,
                        )?;
                        let bg_ptr = Arc::as_ptr(&bg);
                        bind_group_arena.push(bg);
                        current_bind_group = Some(bg_ptr);
                        bind_group_dirty = false;
                    }

                    let bg_ptr = current_bind_group.expect("bind group must be built above");
                    if bound_bind_group != Some(bg_ptr) {
                        let bg_ref = unsafe { &*bg_ptr };
                        render_pass.set_bind_group(0, bg_ref, &[]);
                        bound_bind_group = Some(bg_ptr);
                    }

                    if !vertex_buffers_synced {
                        sync_vertex_buffers(
                            &mut render_pass,
                            resources,
                            state,
                            &mut bound_vertex_buffers,
                        )?;
                        vertex_buffers_synced = true;
                    }
                    render_pass.draw(
                        first_vertex..first_vertex + vertex_count,
                        first_instance..first_instance + instance_count,
                    );
                }
                D3D11Opcode::DrawIndexed => {
                    if packet.payload.len() != 5 {
                        bail!(
                            "DrawIndexed payload words expected 5, got {}",
                            packet.payload.len()
                        );
                    }

                    let index_count = packet.payload[0];
                    let instance_count = packet.payload[1];
                    let first_index = packet.payload[2];
                    let base_vertex = packet.payload[3] as i32;
                    let first_instance = packet.payload[4];

                    let Some(PipelineBinding::Render(pipeline_id)) = state.current_pipeline else {
                        bail!("DrawIndexed without a bound render pipeline");
                    };
                    let pipeline = resources
                        .render_pipelines
                        .get(&pipeline_id)
                        .ok_or_else(|| anyhow!("unknown render pipeline {pipeline_id}"))?;

                    let Some(index) = state.index_buffer else {
                        bail!("DrawIndexed without an index buffer bound");
                    };

                    sync_render_pipeline(
                        &mut render_pass,
                        resources,
                        pipeline_id,
                        Some(index.format),
                        &mut bound_pipeline,
                    )?;

                    if bind_group_dirty || current_bind_group.is_none() {
                        let bg = build_bind_group(
                            device,
                            bind_group_cache,
                            resources,
                            state,
                            &pipeline.bind_group_layout,
                            &pipeline.bindings,
                        )?;
                        let bg_ptr = Arc::as_ptr(&bg);
                        bind_group_arena.push(bg);
                        current_bind_group = Some(bg_ptr);
                        bind_group_dirty = false;
                    }

                    let bg_ptr = current_bind_group.expect("bind group must be built above");
                    if bound_bind_group != Some(bg_ptr) {
                        let bg_ref = unsafe { &*bg_ptr };
                        render_pass.set_bind_group(0, bg_ref, &[]);
                        bound_bind_group = Some(bg_ptr);
                    }

                    if !vertex_buffers_synced {
                        sync_vertex_buffers(
                            &mut render_pass,
                            resources,
                            state,
                            &mut bound_vertex_buffers,
                        )?;
                        vertex_buffers_synced = true;
                    }

                    sync_index_buffer(&mut render_pass, resources, index, &mut bound_index_buffer)?;

                    let instances = first_instance..first_instance + instance_count;

                    // Primitive restart (strip cuts) is required for D3D-style indexed strip
                    // topologies. wgpu's GL backend has historically had correctness issues with
                    // native primitive restart, so emulate restart by splitting the draw into
                    // multiple segments that omit restart indices entirely.
                    if emulate_strip_restart
                        && matches!(
                            pipeline.topology,
                            wgpu::PrimitiveTopology::LineStrip
                                | wgpu::PrimitiveTopology::TriangleStrip
                        )
                        && draw_indexed_strip_restart_emulated(
                            &mut render_pass,
                            resources,
                            index,
                            first_index,
                            index_count,
                            base_vertex,
                            instances.clone(),
                        )?
                    {
                        continue;
                    }

                    render_pass.draw_indexed(
                        first_index..first_index + index_count,
                        base_vertex,
                        instances,
                    );
                }
                _ => bail!(
                    "opcode {:?} not allowed inside render pass",
                    packet.header.opcode
                ),
            }
        }

        Ok(())
    }

    fn exec_compute_pass(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stream: &mut CmdStream<'_>,
    ) -> Result<()> {
        if !self.supports_compute {
            bail!(GpuError::Unsupported("compute"));
        }

        self.encoder_has_commands = true;
        let device = &self.device;
        let resources = &self.resources;

        // Compute-stage resources live in `@group(2)` in the AeroGPU D3D11 binding model. WebGPU
        // requires that bind groups below the highest used group index are bound too, so keep an
        // empty bind group ready for group(1).
        let empty_bind_group_layout = self.bind_group_layout_cache.get_or_create(device, &[]);
        let empty_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero-d3d11 empty bind group (compute)"),
            layout: empty_bind_group_layout.layout.as_ref(),
            entries: &[],
        });

        let (state, bind_group_cache) = (&mut self.state, &mut self.bind_group_cache);

        // wgpu requires any `&BindGroup` passed to `set_bind_group` to remain alive for the entire
        // compute pass lifetime. Since we may change bindings between dispatches, we keep every
        // bind group we create in an arena for the duration of the pass.
        let mut bind_group_arena: Vec<Arc<wgpu::BindGroup>> = Vec::new();
        let mut current_bind_group: Option<*const wgpu::BindGroup> = None;
        let mut bind_group_dirty = true;

        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("aero-d3d11 compute pass"),
            timestamp_writes: None,
        });
        // The compute pipeline layout reserves group(1). Bind an empty group once for the duration
        // of the pass.
        compute_pass.set_bind_group(1, &empty_bind_group, &[]);

        let mut bound_pipeline: Option<u32> = None;
        let mut bound_bind_group: Option<*const wgpu::BindGroup> = None;

        loop {
            let packet = stream
                .next()
                .ok_or_else(|| anyhow!("unexpected end of command stream inside compute pass"))?
                .map_err(|e| anyhow!("{e}"))?;

            match packet.header.opcode {
                D3D11Opcode::EndComputePass => break,
                D3D11Opcode::SetPipeline => {
                    state_set_pipeline(state, packet.payload)?;
                    let Some(PipelineBinding::Compute(pipeline_id)) = state.current_pipeline else {
                        bail!("SetPipeline inside compute pass did not select a compute pipeline");
                    };
                    let pipeline_changed = bound_pipeline != Some(pipeline_id);
                    sync_compute_pipeline(
                        &mut compute_pass,
                        resources,
                        pipeline_id,
                        &mut bound_pipeline,
                    )?;
                    if pipeline_changed {
                        bind_group_dirty = true;
                        current_bind_group = None;
                        bound_bind_group = None;
                    }
                }
                D3D11Opcode::SetBindBuffer => {
                    state_set_bind_buffer(state, packet.payload)?;
                    bind_group_dirty = true;
                }
                D3D11Opcode::SetBindSampler => {
                    state_set_bind_sampler(state, packet.payload)?;
                    bind_group_dirty = true;
                }
                D3D11Opcode::SetBindTextureView => {
                    state_set_bind_texture_view(state, packet.payload)?;
                    bind_group_dirty = true;
                }
                D3D11Opcode::Dispatch => {
                    if packet.payload.len() != 3 {
                        bail!(
                            "Dispatch payload words expected 3, got {}",
                            packet.payload.len()
                        );
                    }

                    let x = packet.payload[0];
                    let y = packet.payload[1];
                    let z = packet.payload[2];

                    let Some(PipelineBinding::Compute(pipeline_id)) = state.current_pipeline else {
                        bail!("Dispatch without a bound compute pipeline");
                    };
                    sync_compute_pipeline(
                        &mut compute_pass,
                        resources,
                        pipeline_id,
                        &mut bound_pipeline,
                    )?;
                    let pipeline = resources
                        .compute_pipelines
                        .get(&pipeline_id)
                        .ok_or_else(|| anyhow!("unknown compute pipeline {pipeline_id}"))?;

                    if bind_group_dirty || current_bind_group.is_none() {
                        let bg = build_bind_group(
                            device,
                            bind_group_cache,
                            resources,
                            state,
                            &pipeline.bind_group_layout,
                            &pipeline.bindings,
                        )?;
                        let bg_ptr = Arc::as_ptr(&bg);
                        bind_group_arena.push(bg);
                        current_bind_group = Some(bg_ptr);
                        bind_group_dirty = false;
                    }

                    let bg_ptr = current_bind_group.expect("bind group must be built above");
                    if bound_bind_group != Some(bg_ptr) {
                        let bg_ref = unsafe { &*bg_ptr };
                        // Bind at both group 0 and group 2 so the protocol runtime can execute
                        // compute shaders using either the legacy `@group(0)` convention or the
                        // stage-scoped AeroGPU model (`@group(2)`).
                        compute_pass.set_bind_group(0, bg_ref, &[]);
                        compute_pass.set_bind_group(COMPUTE_BIND_GROUP_INDEX, bg_ref, &[]);
                        bound_bind_group = Some(bg_ptr);
                    }
                    compute_pass.dispatch_workgroups(x, y, z);
                }
                _ => bail!(
                    "opcode {:?} not allowed inside compute pass",
                    packet.header.opcode
                ),
            }
        }

        Ok(())
    }
}

fn state_set_pipeline(state: &mut D3D11State, payload: &[u32]) -> Result<()> {
    if payload.len() != 2 {
        bail!(
            "SetPipeline payload words expected 2, got {}",
            payload.len()
        );
    }
    let kind = payload[0];
    let pipeline_id = payload[1];
    state.current_pipeline = Some(match kind {
        x if x == PipelineKind::Render as u32 => PipelineBinding::Render(pipeline_id),
        x if x == PipelineKind::Compute as u32 => PipelineBinding::Compute(pipeline_id),
        _ => bail!("unknown pipeline kind {kind}"),
    });
    Ok(())
}

fn state_set_vertex_buffer(state: &mut D3D11State, payload: &[u32]) -> Result<()> {
    if payload.len() != 4 {
        bail!(
            "SetVertexBuffer payload words expected 4, got {}",
            payload.len()
        );
    }
    let slot = payload[0] as usize;
    let buffer_id = payload[1];
    let offset = (payload[2] as u64) | ((payload[3] as u64) << 32);
    if slot >= state.vertex_buffers.len() {
        bail!("vertex buffer slot {slot} out of range");
    }
    state.vertex_buffers[slot] = Some(BoundVertexBuffer {
        buffer: buffer_id,
        offset,
    });
    Ok(())
}

fn state_set_index_buffer(state: &mut D3D11State, payload: &[u32]) -> Result<()> {
    if payload.len() != 4 {
        bail!(
            "SetIndexBuffer payload words expected 4, got {}",
            payload.len()
        );
    }
    let buffer_id = payload[0];
    let format = payload[1];
    let offset = (payload[2] as u64) | ((payload[3] as u64) << 32);
    let format = match format {
        x if x == IndexFormat::Uint16 as u32 => wgpu::IndexFormat::Uint16,
        x if x == IndexFormat::Uint32 as u32 => wgpu::IndexFormat::Uint32,
        _ => bail!("unknown index format {format}"),
    };
    state.index_buffer = Some(BoundIndexBuffer {
        buffer: buffer_id,
        format,
        offset,
    });
    Ok(())
}

fn state_set_bind_buffer(state: &mut D3D11State, payload: &[u32]) -> Result<()> {
    if payload.len() != 6 {
        bail!(
            "SetBindBuffer payload words expected 6, got {}",
            payload.len()
        );
    }
    let binding = payload[0];
    let buffer_id = payload[1];
    let offset = (payload[2] as u64) | ((payload[3] as u64) << 32);
    let size = (payload[4] as u64) | ((payload[5] as u64) << 32);
    state.bindings.insert(
        binding,
        BoundResource::Buffer {
            buffer: buffer_id,
            offset,
            size: if size == 0 { None } else { Some(size) },
        },
    );
    Ok(())
}

fn state_set_bind_sampler(state: &mut D3D11State, payload: &[u32]) -> Result<()> {
    if payload.len() != 2 {
        bail!(
            "SetBindSampler payload words expected 2, got {}",
            payload.len()
        );
    }
    let binding = payload[0];
    let sampler_id = payload[1];
    state.bindings.insert(
        binding,
        BoundResource::Sampler {
            sampler: sampler_id,
        },
    );
    Ok(())
}

fn state_set_bind_texture_view(state: &mut D3D11State, payload: &[u32]) -> Result<()> {
    if payload.len() != 2 {
        bail!(
            "SetBindTextureView payload words expected 2, got {}",
            payload.len()
        );
    }
    let binding = payload[0];
    let view_id = payload[1];
    state
        .bindings
        .insert(binding, BoundResource::TextureView { view: view_id });
    Ok(())
}

fn build_bind_group(
    device: &wgpu::Device,
    cache: &mut BindGroupCache<Arc<wgpu::BindGroup>>,
    resources: &D3D11Resources,
    state: &D3D11State,
    layout: &aero_gpu::bindings::layout_cache::CachedBindGroupLayout,
    bindings: &[BindingDef],
) -> Result<Arc<wgpu::BindGroup>> {
    let mut entries: Vec<BindGroupCacheEntry<'_>> = Vec::with_capacity(bindings.len());
    for def in bindings {
        let bound = state
            .bindings
            .get(&def.binding)
            .ok_or_else(|| anyhow!("binding {} is not set", def.binding))?;

        match (&def.kind, bound) {
            (
                BindingKind::UniformBuffer,
                BoundResource::Buffer {
                    buffer,
                    offset,
                    size,
                },
            )
            | (
                BindingKind::StorageBuffer { .. },
                BoundResource::Buffer {
                    buffer,
                    offset,
                    size,
                },
            ) => {
                let buf = resources
                    .buffers
                    .get(buffer)
                    .ok_or_else(|| anyhow!("unknown buffer {buffer}"))?;
                entries.push(BindGroupCacheEntry {
                    binding: def.binding,
                    resource: BindGroupCacheResource::Buffer {
                        id: BufferId((*buffer).into()),
                        buffer: &buf.buffer,
                        offset: *offset,
                        size: size.and_then(wgpu::BufferSize::new),
                    },
                });
            }
            (BindingKind::Sampler, BoundResource::Sampler { sampler }) => {
                let sampler = resources
                    .samplers
                    .get(sampler)
                    .ok_or_else(|| anyhow!("unknown sampler {sampler}"))?;
                entries.push(BindGroupCacheEntry {
                    binding: def.binding,
                    resource: BindGroupCacheResource::Sampler {
                        id: sampler.id,
                        sampler: sampler.sampler.as_ref(),
                    },
                });
            }
            (BindingKind::Texture2D, BoundResource::TextureView { view: view_id })
            | (
                BindingKind::StorageTexture2DWriteOnly { .. },
                BoundResource::TextureView { view: view_id },
            ) => {
                let view_res = resources
                    .texture_views
                    .get(view_id)
                    .ok_or_else(|| anyhow!("unknown texture view {view_id}"))?;
                entries.push(BindGroupCacheEntry {
                    binding: def.binding,
                    resource: BindGroupCacheResource::TextureView {
                        id: TextureViewId((*view_id).into()),
                        view: &view_res.view,
                    },
                });
            }
            _ => bail!(
                "binding {} kind mismatch between pipeline ({:?}) and bound resource ({:?})",
                def.binding,
                def.kind,
                bound
            ),
        }
    }

    Ok(cache.get_or_create(device, layout, &entries))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BoundRenderPipeline {
    id: u32,
    strip_index_format: Option<wgpu::IndexFormat>,
}

fn sync_render_pipeline<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a D3D11Resources,
    pipeline_id: u32,
    strip_index_format: Option<wgpu::IndexFormat>,
    bound: &mut Option<BoundRenderPipeline>,
) -> Result<()> {
    let pipeline = resources
        .render_pipelines
        .get(&pipeline_id)
        .ok_or_else(|| anyhow!("unknown render pipeline {pipeline_id}"))?;

    let strip_index_format = if pipeline.pipelines.uses_strip_index_format() {
        strip_index_format
    } else {
        None
    };

    let desired = BoundRenderPipeline {
        id: pipeline_id,
        strip_index_format,
    };
    if bound == &Some(desired) {
        return Ok(());
    }

    pass.set_pipeline(pipeline.pipelines.get(strip_index_format));
    *bound = Some(desired);
    Ok(())
}

fn sync_compute_pipeline<'a>(
    pass: &mut wgpu::ComputePass<'a>,
    resources: &'a D3D11Resources,
    pipeline_id: u32,
    bound: &mut Option<u32>,
) -> Result<()> {
    if bound == &Some(pipeline_id) {
        return Ok(());
    }
    let pipeline = resources
        .compute_pipelines
        .get(&pipeline_id)
        .ok_or_else(|| anyhow!("unknown compute pipeline {pipeline_id}"))?;
    pass.set_pipeline(&pipeline.pipeline);
    *bound = Some(pipeline_id);
    Ok(())
}

fn sync_vertex_buffers<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a D3D11Resources,
    state: &D3D11State,
    bound: &mut [Option<BoundVertexBuffer>],
) -> Result<()> {
    for (slot, vb) in state.vertex_buffers.iter().enumerate() {
        let Some(vb) = vb else { continue };
        sync_vertex_buffer_slot(pass, resources, slot as u32, *vb, &mut bound[slot])?;
    }
    Ok(())
}

fn sync_vertex_buffer_slot<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a D3D11Resources,
    slot: u32,
    vb: BoundVertexBuffer,
    bound: &mut Option<BoundVertexBuffer>,
) -> Result<()> {
    if bound.as_ref().is_some_and(|cur| cur == &vb) {
        return Ok(());
    }
    let buf = resources
        .buffers
        .get(&vb.buffer)
        .ok_or_else(|| anyhow!("unknown vertex buffer {}", vb.buffer))?;
    pass.set_vertex_buffer(slot, buf.buffer.slice(vb.offset..));
    *bound = Some(vb);
    Ok(())
}

fn sync_index_buffer<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a D3D11Resources,
    index: BoundIndexBuffer,
    bound: &mut Option<BoundIndexBuffer>,
) -> Result<()> {
    if bound.as_ref().is_some_and(|cur| cur == &index) {
        return Ok(());
    }
    let buf = resources
        .buffers
        .get(&index.buffer)
        .ok_or_else(|| anyhow!("unknown index buffer {}", index.buffer))?;
    pass.set_index_buffer(buf.buffer.slice(index.offset..), index.format);
    *bound = Some(index);
    Ok(())
}

fn draw_indexed_strip_restart_emulated<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a D3D11Resources,
    index: BoundIndexBuffer,
    first_index: u32,
    index_count: u32,
    base_vertex: i32,
    instances: std::ops::Range<u32>,
) -> Result<bool> {
    if index_count == 0 {
        return Ok(false);
    }

    let buf = resources
        .buffers
        .get(&index.buffer)
        .ok_or_else(|| anyhow!("unknown index buffer {}", index.buffer))?;
    let Some(shadow) = buf.shadow.as_deref() else {
        // Shadow is optional; fall back to wgpu's built-in behavior if we cannot inspect the index
        // data.
        return Ok(false);
    };

    let (stride_bytes, restart_value) = match index.format {
        wgpu::IndexFormat::Uint16 => (2u64, u16::MAX as u32),
        wgpu::IndexFormat::Uint32 => (4u64, u32::MAX),
    };

    let start_byte = index
        .offset
        .checked_add(u64::from(first_index) * stride_bytes)
        .ok_or_else(|| anyhow!("DrawIndexed index buffer slice start overflows u64"))?;
    let slice_len_bytes = u64::from(index_count)
        .checked_mul(stride_bytes)
        .ok_or_else(|| anyhow!("DrawIndexed index buffer slice length overflows u64"))?;
    let end_byte = start_byte
        .checked_add(slice_len_bytes)
        .ok_or_else(|| anyhow!("DrawIndexed index buffer slice end overflows u64"))?;

    if end_byte > buf.size {
        bail!(
            "DrawIndexed index buffer slice out of bounds: start={start_byte} end={end_byte} buffer_size={}",
            buf.size
        );
    }

    let start = usize::try_from(start_byte)
        .map_err(|_| anyhow!("DrawIndexed index buffer slice start does not fit in usize"))?;
    let end = usize::try_from(end_byte)
        .map_err(|_| anyhow!("DrawIndexed index buffer slice end does not fit in usize"))?;
    if end > shadow.len() {
        bail!(
            "DrawIndexed index buffer slice out of bounds for shadow copy: end={end} shadow_size={}",
            shadow.len()
        );
    }

    let bytes = &shadow[start..end];

    let mut seg_start = 0u32;
    let mut did_restart = false;

    match index.format {
        wgpu::IndexFormat::Uint16 => {
            for (i, chunk) in bytes.chunks_exact(2).enumerate() {
                let idx = u16::from_le_bytes([chunk[0], chunk[1]]) as u32;
                if idx == restart_value {
                    did_restart = true;
                    let i = i as u32;
                    if i > seg_start {
                        pass.draw_indexed(
                            first_index + seg_start..first_index + i,
                            base_vertex,
                            instances.clone(),
                        );
                    }
                    seg_start = i + 1;
                }
            }
        }
        wgpu::IndexFormat::Uint32 => {
            for (i, chunk) in bytes.chunks_exact(4).enumerate() {
                let idx = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if idx == restart_value {
                    did_restart = true;
                    let i = i as u32;
                    if i > seg_start {
                        pass.draw_indexed(
                            first_index + seg_start..first_index + i,
                            base_vertex,
                            instances.clone(),
                        );
                    }
                    seg_start = i + 1;
                }
            }
        }
    }

    if !did_restart {
        return Ok(false);
    }

    if seg_start < index_count {
        pass.draw_indexed(
            first_index + seg_start..first_index + index_count,
            base_vertex,
            instances,
        );
    }

    Ok(true)
}

fn map_buffer_usage(usage: BufferUsage, supports_compute: bool) -> wgpu::BufferUsages {
    let mut out = wgpu::BufferUsages::empty();
    let mut needs_storage = usage.contains(BufferUsage::STORAGE);
    // `MAP_READ`/`MAP_WRITE` are intentionally not mapped directly to WebGPU buffer map usages.
    //
    // WebGPU forbids combining `MAP_*` with most GPU usages (including `STORAGE`), but for D3D11
    // tests (and internal tooling) we often want "readback" buffers that are also written by GPU
    // passes. `D3D11Runtime::read_buffer` performs an explicit staging copy into a dedicated
    // `MAP_READ | COPY_DST` buffer to support that without requiring invalid usage combinations.
    if usage.contains(BufferUsage::COPY_SRC) {
        out |= wgpu::BufferUsages::COPY_SRC;
    }
    if usage.contains(BufferUsage::COPY_DST) {
        out |= wgpu::BufferUsages::COPY_DST;
    }
    if usage.contains(BufferUsage::INDEX) {
        out |= wgpu::BufferUsages::INDEX;
        needs_storage = true;
    }
    if usage.contains(BufferUsage::VERTEX) {
        out |= wgpu::BufferUsages::VERTEX;
        needs_storage = true;
    }
    if usage.contains(BufferUsage::UNIFORM) {
        out |= wgpu::BufferUsages::UNIFORM;
    }
    // D3D11 IA buffers may also be consumed by compute prepasses (vertex/index pulling) when
    // emulating GS/HS/DS. WebGPU requires such buffers to be created with `STORAGE` in order to
    // bind them as `var<storage>`. Gate this on compute support so downlevel backends (e.g. WebGL2)
    // don't hit validation errors.
    if supports_compute && needs_storage {
        out |= wgpu::BufferUsages::STORAGE;
    }
    if usage.contains(BufferUsage::INDIRECT) {
        out |= wgpu::BufferUsages::INDIRECT;
    }
    out
}

fn map_texture_usage(usage: TextureUsage, supports_compute: bool) -> wgpu::TextureUsages {
    let mut out = wgpu::TextureUsages::empty();
    if usage.contains(TextureUsage::COPY_SRC) {
        out |= wgpu::TextureUsages::COPY_SRC;
    }
    if usage.contains(TextureUsage::COPY_DST) {
        out |= wgpu::TextureUsages::COPY_DST;
    }
    if usage.contains(TextureUsage::TEXTURE_BINDING) {
        out |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    // Downlevel backends without compute support (e.g. WebGL2) do not support storage textures.
    // Gate this to avoid wgpu validation errors when creating textures.
    if supports_compute && usage.contains(TextureUsage::STORAGE_BINDING) {
        out |= wgpu::TextureUsages::STORAGE_BINDING;
    }
    if usage.contains(TextureUsage::RENDER_ATTACHMENT) {
        out |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    out
}

fn map_texture_format(format: DxgiFormat) -> Result<wgpu::TextureFormat> {
    Ok(match format {
        DxgiFormat::R8G8B8A8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        DxgiFormat::R8G8B8A8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        DxgiFormat::B8G8R8A8Unorm => wgpu::TextureFormat::Bgra8Unorm,
        DxgiFormat::B8G8R8A8UnormSrgb => wgpu::TextureFormat::Bgra8UnormSrgb,
        DxgiFormat::R16G16B16A16Float => wgpu::TextureFormat::Rgba16Float,
        DxgiFormat::R32G32B32A32Float => wgpu::TextureFormat::Rgba32Float,
        DxgiFormat::R32Float => wgpu::TextureFormat::R32Float,
        DxgiFormat::D32Float => wgpu::TextureFormat::Depth32Float,
        DxgiFormat::D24UnormS8Uint => wgpu::TextureFormat::Depth24PlusStencil8,
        DxgiFormat::Unknown => bail!("DXGI format UNKNOWN is not a valid WebGPU format here"),
    })
}

fn map_vertex_format(format: u32) -> Result<wgpu::VertexFormat> {
    Ok(match format {
        x if x == VertexFormat::Float32x2 as u32 => wgpu::VertexFormat::Float32x2,
        x if x == VertexFormat::Float32x3 as u32 => wgpu::VertexFormat::Float32x3,
        x if x == VertexFormat::Float32x4 as u32 => wgpu::VertexFormat::Float32x4,
        x if x == VertexFormat::Uint32 as u32 => wgpu::VertexFormat::Uint32,
        x if x == VertexFormat::Uint32x2 as u32 => wgpu::VertexFormat::Uint32x2,
        x if x == VertexFormat::Uint32x4 as u32 => wgpu::VertexFormat::Uint32x4,
        _ => bail!("unknown vertex format {format}"),
    })
}

fn map_shader_stages(stages: ShaderStageFlags) -> Result<wgpu::ShaderStages> {
    let mut out = wgpu::ShaderStages::empty();
    if stages.contains(ShaderStageFlags::VERTEX) {
        out |= wgpu::ShaderStages::VERTEX;
    }
    if stages.contains(ShaderStageFlags::FRAGMENT) {
        out |= wgpu::ShaderStages::FRAGMENT;
    }
    if stages.contains(ShaderStageFlags::COMPUTE) {
        out |= wgpu::ShaderStages::COMPUTE;
    }
    Ok(out)
}

fn binding_def_to_layout_entry(def: &BindingDef) -> wgpu::BindGroupLayoutEntry {
    let ty = match def.kind {
        BindingKind::UniformBuffer => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        BindingKind::StorageBuffer { read_only } => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        BindingKind::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        BindingKind::Texture2D => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        BindingKind::StorageTexture2DWriteOnly { format } => wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
    };

    wgpu::BindGroupLayoutEntry {
        binding: def.binding,
        visibility: def.visibility,
        ty,
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_gpu::protocol_d3d11::{BindingDesc, CmdWriter, RenderPipelineDesc};

    #[test]
    fn take_bytes_extracts_prefix() {
        let w0 = u32::from_ne_bytes([1, 2, 3, 4]);
        let w1 = u32::from_ne_bytes([5, 6, 7, 8]);
        // fixed_words=3 => payload[3] is byte_len and bytes start at index 4.
        let payload = [0u32, 0, 0, 5, w0, w1];
        let bytes = D3D11Runtime::take_bytes(&payload, 3).unwrap();
        assert_eq!(bytes, &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn take_bytes_rejects_truncated_byte_payload() {
        let w0 = u32::from_ne_bytes([1, 2, 3, 4]);
        let payload = [0u32, 0, 0, 5, w0];
        assert!(D3D11Runtime::take_bytes(&payload, 3).is_err());
    }

    #[test]
    fn create_render_pipeline_trims_unbound_fragment_outputs() {
        // Regression test: wgpu/WebGPU requires that every fragment `@location(N)` output has a
        // corresponding `ColorTargetState` at index N. D3D discards writes to unbound RTV slots.
        //
        // This runtime protocol only supports a single color target (RT0). Ensure pipeline
        // creation succeeds even when the fragment shader declares extra MRT outputs.
        pollster::block_on(async {
            let mut rt = match D3D11Runtime::new_for_tests().await {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("skipping {}: wgpu unavailable ({err:#})", module_path!());
                    return;
                }
            };

            let vs_wgsl = r#"
                @vertex
                fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
                    var pos = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -1.0),
                        vec2<f32>( 3.0, -1.0),
                        vec2<f32>(-1.0,  3.0),
                    );
                    return vec4<f32>(pos[idx], 0.0, 1.0);
                }
            "#;

            let fs_wgsl = r#"
                struct PsOut {
                    @location(0) o0: vec4<f32>,
                    @location(1) o1: vec4<f32>,
                };

                @fragment
                fn fs_main() -> PsOut {
                    var out: PsOut;
                    out.o0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);
                    out.o1 = vec4<f32>(0.0, 1.0, 0.0, 1.0);
                    return out;
                }
            "#;
            let keep_output_locations = BTreeSet::from([0u32]);
            let expected_trimmed_wgsl = super::super::wgsl_link::trim_ps_outputs_to_locations(
                fs_wgsl,
                &keep_output_locations,
            );
            assert!(
                !expected_trimmed_wgsl.contains("@location(1)"),
                "sanity check: trimmed WGSL should drop unbound outputs"
            );
            #[cfg(debug_assertions)]
            let expected_trimmed_hash = aero_gpu::pipeline_key::hash_wgsl(&expected_trimmed_wgsl);

            // Baseline: strict WebGPU implementations reject pipelines that write to unbound MRT
            // locations. Some wgpu backends are permissive and accept the untrimmed shader anyway,
            // so tolerate either outcome here.
            let vs = rt
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("d3d11 runtime mrt baseline vs"),
                    source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
                });
            let fs = rt
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("d3d11 runtime mrt baseline fs"),
                    source: wgpu::ShaderSource::Wgsl(fs_wgsl.into()),
                });
            let layout = rt
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("d3d11 runtime mrt baseline layout"),
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });

            rt.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let _ = rt
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("d3d11 runtime mrt baseline pipeline"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &vs,
                        entry_point: "vs_main",
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &fs,
                        entry_point: "fs_main",
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                });
            rt.device.poll(wgpu::Maintain::Wait);
            let err = rt.device.pop_error_scope().await;
            if err.is_none() {
                eprintln!(
                    "note: wgpu accepted an untrimmed MRT shader with a single color target; continuing"
                );
            }

            // Now go through the D3D11 runtime command path; the runtime should trim outputs and
            // pipeline creation should succeed without validation errors.
            let mut writer = CmdWriter::new();
            writer.create_shader_module_wgsl(1, vs_wgsl);
            writer.create_shader_module_wgsl(2, fs_wgsl);
            writer.create_render_pipeline(
                3,
                RenderPipelineDesc {
                    vs_shader: 1,
                    fs_shader: 2,
                    color_format: DxgiFormat::R8G8B8A8Unorm,
                    depth_format: DxgiFormat::Unknown,
                    topology: PrimitiveTopology::TriangleList,
                    vertex_buffers: &[],
                    bindings: &[],
                },
            );

            rt.device.push_error_scope(wgpu::ErrorFilter::Validation);
            rt.execute(&writer.finish())
                .expect("runtime should create pipeline with trimmed outputs");
            rt.device.poll(wgpu::Maintain::Wait);
            let err = rt.device.pop_error_scope().await;
            assert!(
                err.is_none(),
                "unexpected wgpu validation error while creating trimmed pipeline: {err:?}"
            );

            #[cfg(debug_assertions)]
            {
                let cached = rt
                    .pipelines
                    .debug_shader_source(ShaderStage::Fragment, expected_trimmed_hash);
                assert_eq!(
                    cached,
                    Some(expected_trimmed_wgsl.as_str()),
                    "expected trimmed fragment WGSL to be cached when creating the pipeline"
                );
            }
        });
    }

    #[test]
    fn create_render_pipeline_trims_all_fragment_outputs_when_rt0_not_written() {
        // Regression test: D3D allows pixel shaders that only write to a non-zero RTV slot (e.g.
        // SV_Target1) even when only RTV0 is bound. D3D discards writes to unbound RTVs, leaving
        // RTV0 unchanged.
        //
        // WebGPU requires that every fragment `@location(N)` output has a corresponding pipeline
        // target at index N. Ensure the runtime can trim away *all* fragment outputs when none map
        // to the single supported target (RT0).
        pollster::block_on(async {
            let mut rt = match D3D11Runtime::new_for_tests().await {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("skipping {}: wgpu unavailable ({err:#})", module_path!());
                    return;
                }
            };

            let vs_wgsl = r#"
                @vertex
                fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
                    var pos = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -1.0),
                        vec2<f32>( 3.0, -1.0),
                        vec2<f32>(-1.0,  3.0),
                    );
                    return vec4<f32>(pos[idx], 0.0, 1.0);
                }
            "#;

            let fs_wgsl = r#"
                struct PsOut {
                    @location(1) o1: vec4<f32>,
                };

                @fragment
                fn fs_main() -> PsOut {
                    var out: PsOut;
                    out.o1 = vec4<f32>(0.0, 1.0, 0.0, 1.0);
                    return out;
                }
            "#;

            let keep_output_locations = BTreeSet::from([0u32]);
            let expected_trimmed_wgsl = super::super::wgsl_link::trim_ps_outputs_to_locations(
                fs_wgsl,
                &keep_output_locations,
            );
            assert!(
                !expected_trimmed_wgsl.contains("@location(1)"),
                "sanity check: trimmed WGSL should drop unbound outputs"
            );
            #[cfg(debug_assertions)]
            let expected_trimmed_hash = aero_gpu::pipeline_key::hash_wgsl(&expected_trimmed_wgsl);

            // Now go through the D3D11 runtime command path; the runtime should trim outputs and
            // pipeline creation should succeed without validation errors.
            let mut writer = CmdWriter::new();
            writer.create_shader_module_wgsl(1, vs_wgsl);
            writer.create_shader_module_wgsl(2, fs_wgsl);
            writer.create_render_pipeline(
                3,
                RenderPipelineDesc {
                    vs_shader: 1,
                    fs_shader: 2,
                    color_format: DxgiFormat::R8G8B8A8Unorm,
                    depth_format: DxgiFormat::Unknown,
                    topology: PrimitiveTopology::TriangleList,
                    vertex_buffers: &[],
                    bindings: &[],
                },
            );

            rt.device.push_error_scope(wgpu::ErrorFilter::Validation);
            rt.execute(&writer.finish())
                .expect("runtime should create pipeline with trimmed outputs");
            rt.device.poll(wgpu::Maintain::Wait);
            let err = rt.device.pop_error_scope().await;
            assert!(
                err.is_none(),
                "unexpected wgpu validation error while creating trimmed pipeline: {err:?}"
            );

            #[cfg(debug_assertions)]
            {
                let cached = rt
                    .pipelines
                    .debug_shader_source(ShaderStage::Fragment, expected_trimmed_hash);
                assert_eq!(
                    cached,
                    Some(expected_trimmed_wgsl.as_str()),
                    "expected trimmed fragment WGSL to be cached when creating the pipeline"
                );
            }
        });
    }

    #[test]
    fn create_render_pipeline_trims_stage_interface_to_match() {
        // Regression test: WebGPU requires vertex outputs and fragment inputs to line up by
        // `@location`. D3D shaders may declare unused PS inputs or export extra VS varyings.
        //
        // The runtime should trim unused PS inputs and extra VS outputs to satisfy validation.
        pollster::block_on(async {
            let mut rt = match D3D11Runtime::new_for_tests().await {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("skipping {}: wgpu unavailable ({err:#})", module_path!());
                    return;
                }
            };

            let vs_wgsl = r#"
                struct VsOut {
                    @builtin(position) pos: vec4<f32>,
                    @location(0) o0: vec4<f32>,
                    @location(2) o2: vec4<f32>,
                };

                @vertex
                fn vs_main(@builtin(vertex_index) idx: u32) -> VsOut {
                    var out: VsOut;
                    var pos = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -1.0),
                        vec2<f32>( 3.0, -1.0),
                        vec2<f32>(-1.0,  3.0),
                    );
                    out.pos = vec4<f32>(pos[idx], 0.0, 1.0);
                    out.o0 = vec4<f32>(1.0, 0.0, 0.0, 1.0);
                    out.o2 = vec4<f32>(0.0, 1.0, 0.0, 1.0);
                    return out;
                }
            "#;

            let fs_wgsl = r#"
                struct PsIn {
                    @location(0) v0: vec4<f32>,
                    @location(1) v1: vec4<f32>,
                };

                @fragment
                fn fs_main(input: PsIn) -> @location(0) vec4<f32> {
                    // `v1` is declared but unused (mirrors D3D pixel shaders that declare
                    // interpolators they never read).
                    return input.v0;
                }
            "#;

            let keep_input_locations = BTreeSet::from([0u32]);
            let expected_trimmed_wgsl = super::super::wgsl_link::trim_ps_inputs_to_locations(
                fs_wgsl,
                &keep_input_locations,
            );
            assert!(
                !expected_trimmed_wgsl.contains("@location(1)"),
                "sanity check: trimmed WGSL should drop missing inputs"
            );
            #[cfg(debug_assertions)]
            let expected_trimmed_hash = aero_gpu::pipeline_key::hash_wgsl(&expected_trimmed_wgsl);

            // Now go through the D3D11 runtime command path; the runtime should trim the stage
            // interface and pipeline creation should succeed without validation errors.
            let mut writer = CmdWriter::new();
            writer.create_shader_module_wgsl(1, vs_wgsl);
            writer.create_shader_module_wgsl(2, fs_wgsl);
            writer.create_render_pipeline(
                3,
                RenderPipelineDesc {
                    vs_shader: 1,
                    fs_shader: 2,
                    color_format: DxgiFormat::R8G8B8A8Unorm,
                    depth_format: DxgiFormat::Unknown,
                    topology: PrimitiveTopology::TriangleList,
                    vertex_buffers: &[],
                    bindings: &[],
                },
            );

            rt.device.push_error_scope(wgpu::ErrorFilter::Validation);
            rt.execute(&writer.finish())
                .expect("runtime should create pipeline with trimmed interface");
            rt.device.poll(wgpu::Maintain::Wait);
            let err = rt.device.pop_error_scope().await;
            assert!(
                err.is_none(),
                "unexpected wgpu validation error while creating trimmed pipeline: {err:?}"
            );

            #[cfg(debug_assertions)]
            {
                let cached = rt
                    .pipelines
                    .debug_shader_source(ShaderStage::Fragment, expected_trimmed_hash);
                assert_eq!(
                    cached,
                    Some(expected_trimmed_wgsl.as_str()),
                    "expected trimmed fragment WGSL to be cached when creating the pipeline"
                );
            }
        });
    }

    #[test]
    fn dispatch_errors_cleanly_when_compute_is_unsupported() {
        pollster::block_on(async {
            let mut rt = match D3D11Runtime::new_for_tests().await {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("skipping {}: wgpu unavailable ({err:#})", module_path!());
                    return;
                }
            };

            // Force-disable compute so the error path is deterministic regardless of the host GPU.
            rt.supports_compute = false;

            let mut writer = CmdWriter::new();
            writer.begin_compute_pass();
            writer.dispatch(1, 1, 1);
            writer.end_compute_pass();

            let err = rt.execute(&writer.finish()).unwrap_err();
            assert_eq!(
                err.downcast_ref::<GpuError>(),
                Some(&GpuError::Unsupported("compute"))
            );
        });
    }

    #[test]
    fn create_render_pipeline_errors_cleanly_when_storage_bindings_used_and_compute_unsupported() {
        pollster::block_on(async {
            let mut rt = match D3D11Runtime::new_for_tests().await {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("skipping {}: wgpu unavailable ({err:#})", module_path!());
                    return;
                }
            };

            // Force-disable compute so the error path is deterministic regardless of the host GPU.
            rt.supports_compute = false;

            let vs_wgsl = r#"
                @vertex
                fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
                    var pos = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -1.0),
                        vec2<f32>( 3.0, -1.0),
                        vec2<f32>(-1.0,  3.0),
                    );
                    return vec4<f32>(pos[idx], 0.0, 1.0);
                }
            "#;

            let fs_wgsl = r#"
                @fragment
                fn fs_main() -> @location(0) vec4<f32> {
                    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
                }
            "#;

            let bindings = [
                BindingDesc {
                    binding: 0,
                    ty: BindingType::StorageBufferReadOnly,
                    visibility: ShaderStageFlags::FRAGMENT,
                    storage_texture_format: None,
                },
                BindingDesc {
                    binding: 1,
                    ty: BindingType::StorageTexture2DWriteOnly,
                    visibility: ShaderStageFlags::FRAGMENT,
                    storage_texture_format: Some(DxgiFormat::R8G8B8A8Unorm),
                },
            ];

            let mut writer = CmdWriter::new();
            writer.create_shader_module_wgsl(1, vs_wgsl);
            writer.create_shader_module_wgsl(2, fs_wgsl);
            writer.create_render_pipeline(
                3,
                RenderPipelineDesc {
                    vs_shader: 1,
                    fs_shader: 2,
                    color_format: DxgiFormat::R8G8B8A8Unorm,
                    depth_format: DxgiFormat::Unknown,
                    topology: PrimitiveTopology::TriangleList,
                    vertex_buffers: &[],
                    bindings: &bindings,
                },
            );

            let err = rt.execute(&writer.finish()).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("CreateRenderPipeline requires compute shaders"),
                "unexpected error: {msg:#}"
            );
            assert!(
                msg.contains("does not support compute"),
                "unexpected error: {msg:#}"
            );
        });
    }

    #[test]
    fn map_buffer_usage_gates_storage_on_compute_support() {
        let bu = map_buffer_usage(BufferUsage::VERTEX, false);
        assert!(bu.contains(wgpu::BufferUsages::VERTEX));
        assert!(!bu.contains(wgpu::BufferUsages::STORAGE));

        let storage = map_buffer_usage(BufferUsage::STORAGE, false);
        assert!(!storage.contains(wgpu::BufferUsages::STORAGE));

        let storage_compute = map_buffer_usage(BufferUsage::STORAGE, true);
        assert!(storage_compute.contains(wgpu::BufferUsages::STORAGE));
    }

    #[test]
    fn map_texture_usage_gates_storage_on_compute_support() {
        let usage = map_texture_usage(TextureUsage::STORAGE_BINDING, false);
        assert!(!usage.contains(wgpu::TextureUsages::STORAGE_BINDING));

        let usage = map_texture_usage(TextureUsage::STORAGE_BINDING, true);
        assert!(usage.contains(wgpu::TextureUsages::STORAGE_BINDING));
    }
}
