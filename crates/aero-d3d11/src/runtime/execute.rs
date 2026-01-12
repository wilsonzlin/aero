use std::sync::Arc;

use aero_gpu::bindings::bind_group_cache::{
    BindGroupCache, BindGroupCacheEntry, BindGroupCacheResource, BufferId, TextureViewId,
};
use aero_gpu::bindings::layout_cache::BindGroupLayoutCache;
use aero_gpu::bindings::samplers::SamplerCache;
use aero_gpu::bindings::CacheStats;
use aero_gpu::pipeline_key::PipelineLayoutKey;
use aero_gpu::protocol_d3d11::{
    BindingType, BufferUsage, CmdPacket, CmdStream, D3D11Opcode, DxgiFormat, IndexFormat,
    PipelineKind, PrimitiveTopology, ShaderStageFlags, TextureUsage, VertexFormat, VertexStepMode,
};
use anyhow::{anyhow, bail, Context, Result};

use super::pipeline_layout_cache::PipelineLayoutCache;
use super::resources::{
    BindingDef, BindingKind, BufferResource, ComputePipelineResource, D3D11Resources,
    RenderPipelineResource, SamplerResource, ShaderModuleResource, Texture2dDesc, TextureResource,
    TextureViewResource,
};
use super::state::{
    BoundIndexBuffer, BoundResource, BoundVertexBuffer, D3D11State, PipelineBinding,
};

const DEFAULT_BIND_GROUP_CACHE_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct D3D11CacheStats {
    pub samplers: CacheStats,
    pub bind_group_layouts: CacheStats,
    pub bind_groups: CacheStats,
}

pub struct D3D11Runtime {
    device: wgpu::Device,
    queue: wgpu::Queue,
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

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        Ok(Self {
            device,
            queue,
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

        let slice = buffer.buffer.slice(offset..offset.saturating_add(size));

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

        let data = slice.get_mapped_range().to_vec();
        buffer.buffer.unmap();
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
        if payload.len() < fixed_words + 1 {
            bail!(
                "expected at least {} words for fixed payload + byte len",
                fixed_words
            );
        }
        let byte_len = payload[fixed_words] as usize;
        let bytes_start = fixed_words + 1;
        let bytes_words = byte_len.div_ceil(4);
        if payload.len() < bytes_start + bytes_words {
            bail!(
                "truncated byte payload: need {} words, have {}",
                bytes_words,
                payload.len() - bytes_start
            );
        }
        let byte_words = &payload[bytes_start..bytes_start + bytes_words];
        let bytes_ptr = byte_words.as_ptr() as *const u8;
        // Safety: `u32` slice is properly aligned and we only read within it.
        let bytes = unsafe { std::slice::from_raw_parts(bytes_ptr, bytes_words * 4) };
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

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d11 buffer"),
            size,
            usage: map_buffer_usage(usage),
            mapped_at_creation: false,
        });
        self.resources
            .buffers
            .insert(id, BufferResource { buffer, size });
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
            .get(&id)
            .ok_or_else(|| anyhow!("unknown buffer {id}"))?;
        self.queue.write_buffer(&buffer.buffer, offset, bytes);
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
            usage: map_texture_usage(usage),
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

        let texture_format = self
            .resources
            .textures
            .get(&texture_id)
            .ok_or_else(|| anyhow!("unknown texture {texture_id}"))?
            .desc
            .format;

        if width == 0 || height == 0 {
            bail!("UpdateTexture2D width/height must be non-zero");
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

        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("aero-d3d11 texture view"),
            format: None,
            dimension: Some(if texture.desc.array_layers > 1 {
                wgpu::TextureViewDimension::D2Array
            } else {
                wgpu::TextureViewDimension::D2
            }),
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
        self.resources
            .shaders
            .insert(shader_id, ShaderModuleResource { module });
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
            layout_key,
            &[bind_group_layout.layout.as_ref()],
            Some("aero-d3d11 pipeline layout"),
        );

        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aero-d3d11 render pipeline"),
                layout: Some(pipeline_layout.as_ref()),
                vertex: wgpu::VertexState {
                    module: &vs.module,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &vertex_buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &fs.module,
                    entry_point: "fs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: color_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        self.resources.render_pipelines.insert(
            pipeline_id,
            RenderPipelineResource {
                pipeline,
                bind_group_layout,
                bindings,
            },
        );
        Ok(())
    }

    fn exec_create_compute_pipeline(&mut self, payload: &[u32]) -> Result<()> {
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

        let layout_key = PipelineLayoutKey {
            bind_group_layout_hashes: vec![bind_group_layout.hash],
        };
        let pipeline_layout = self.pipeline_layout_cache.get_or_create(
            &self.device,
            layout_key,
            &[bind_group_layout.layout.as_ref()],
            Some("aero-d3d11 pipeline layout"),
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
        let mut bindings = Vec::with_capacity(binding_count);
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
            bindings.push(BindingDef {
                binding,
                visibility,
                kind,
            });
        }
        Ok(bindings)
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

        let src_end = src_offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("CopyBufferToBuffer src range overflows u64"))?;
        let dst_end = dst_offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("CopyBufferToBuffer dst range overflows u64"))?;
        if src_end > src_buf.size || dst_end > dst_buf.size {
            bail!(
                "CopyBufferToBuffer out of bounds: src_end={src_end} (size={}) dst_end={dst_end} (size={})",
                src_buf.size,
                dst_buf.size
            );
        }

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

        let mut bound_pipeline: Option<u32> = None;
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
                    let pipeline_changed = bound_pipeline != Some(pipeline_id);
                    sync_render_pipeline(
                        &mut render_pass,
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
                    sync_render_pipeline(
                        &mut render_pass,
                        resources,
                        pipeline_id,
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
                    let Some(index) = state.index_buffer else {
                        bail!("DrawIndexed without an index buffer bound");
                    };
                    sync_index_buffer(&mut render_pass, resources, index, &mut bound_index_buffer)?;
                    render_pass.draw_indexed(
                        first_index..first_index + index_count,
                        base_vertex,
                        first_instance..first_instance + instance_count,
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
        self.encoder_has_commands = true;
        let device = &self.device;
        let resources = &self.resources;
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
                        compute_pass.set_bind_group(0, bg_ref, &[]);
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

fn sync_render_pipeline<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a D3D11Resources,
    pipeline_id: u32,
    bound: &mut Option<u32>,
) -> Result<()> {
    if bound == &Some(pipeline_id) {
        return Ok(());
    }
    let pipeline = resources
        .render_pipelines
        .get(&pipeline_id)
        .ok_or_else(|| anyhow!("unknown render pipeline {pipeline_id}"))?;
    pass.set_pipeline(&pipeline.pipeline);
    *bound = Some(pipeline_id);
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

fn map_buffer_usage(usage: BufferUsage) -> wgpu::BufferUsages {
    let mut out = wgpu::BufferUsages::empty();
    if usage.contains(BufferUsage::MAP_READ) {
        out |= wgpu::BufferUsages::MAP_READ;
    }
    if usage.contains(BufferUsage::MAP_WRITE) {
        out |= wgpu::BufferUsages::MAP_WRITE;
    }
    if usage.contains(BufferUsage::COPY_SRC) {
        out |= wgpu::BufferUsages::COPY_SRC;
    }
    if usage.contains(BufferUsage::COPY_DST) {
        out |= wgpu::BufferUsages::COPY_DST;
    }
    if usage.contains(BufferUsage::INDEX) {
        out |= wgpu::BufferUsages::INDEX;
    }
    if usage.contains(BufferUsage::VERTEX) {
        out |= wgpu::BufferUsages::VERTEX;
    }
    if usage.contains(BufferUsage::UNIFORM) {
        out |= wgpu::BufferUsages::UNIFORM;
    }
    if usage.contains(BufferUsage::STORAGE) {
        out |= wgpu::BufferUsages::STORAGE;
    }
    if usage.contains(BufferUsage::INDIRECT) {
        out |= wgpu::BufferUsages::INDIRECT;
    }
    out
}

fn map_texture_usage(usage: TextureUsage) -> wgpu::TextureUsages {
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
    if usage.contains(TextureUsage::STORAGE_BINDING) {
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
