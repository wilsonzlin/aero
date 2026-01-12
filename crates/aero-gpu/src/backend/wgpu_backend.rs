use std::borrow::Cow;
use std::num::NonZeroUsize;

use crate::hal::*;
use crate::{GpuCapabilities, GpuError};

use lru::LruCache;

enum Pipeline {
    Render(wgpu::RenderPipeline),
    Compute(wgpu::ComputePipeline),
}

const PIPELINE_LAYOUT_CACHE_CAPACITY: usize = 128;

/// `wgpu` implementation of the backend-agnostic HAL.
///
/// This backend is used for both native GPU access and browser backends (WebGPU and wgpu-GL).
pub struct WgpuBackend {
    kind: BackendKind,
    capabilities: GpuCapabilities,
    device: wgpu::Device,
    queue: wgpu::Queue,

    buffers: ResourceRegistry<BufferTag, wgpu::Buffer>,
    textures: ResourceRegistry<TextureTag, wgpu::Texture>,
    texture_views: ResourceRegistry<TextureViewTag, wgpu::TextureView>,
    samplers: ResourceRegistry<SamplerTag, wgpu::Sampler>,
    bind_group_layouts: ResourceRegistry<BindGroupLayoutTag, wgpu::BindGroupLayout>,
    bind_groups: ResourceRegistry<BindGroupTag, wgpu::BindGroup>,
    pipelines: ResourceRegistry<PipelineTag, Pipeline>,
    command_buffers: ResourceRegistry<CommandBufferTag, wgpu::CommandBuffer>,

    /// Cache `wgpu::PipelineLayout` objects keyed by bind-group layout IDs.
    ///
    /// Creating pipeline layouts can show up in profiles when pipelines are rebuilt frequently but
    /// share identical bind-group layouts.
    pipeline_layout_cache: LruCache<Vec<BindGroupLayoutId>, wgpu::PipelineLayout>,
}

impl WgpuBackend {
    /// Creates a backend without a presentation surface.
    ///
    /// This is primarily intended for tests and offscreen rendering.
    pub async fn new_headless(kind: BackendKind) -> Result<Self, GpuError> {
        // When using the GL backend on Linux, wgpu can emit noisy warnings if `XDG_RUNTIME_DIR` is
        // unset or points at a directory with unsafe permissions (e.g. `/tmp` is typically `1777`).
        // Create a per-process temp dir so headless callers don't need to care about display-server
        // environment details.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
                Ok(dir) if !dir.is_empty() => match std::fs::metadata(&dir) {
                    Ok(meta) => !meta.is_dir() || (meta.permissions().mode() & 0o077) != 0,
                    Err(_) => true,
                },
                _ => true,
            };
            if needs_runtime_dir {
                let dir = std::env::temp_dir().join(format!(
                    "aero-wgpu-xdg-runtime-{}-wgpu-backend",
                    std::process::id()
                ));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        // On Linux CI we prefer the GL backend first to avoid crashes seen with some Vulkan
        // software adapters (lavapipe/llvmpipe). If no GL adapter is available, fall back to
        // the native backends.
        let adapter = if cfg!(target_os = "linux") {
            let gl_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::GL,
                ..Default::default()
            });
            let adapter = gl_instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await;
            if adapter.is_some() {
                adapter
            } else {
                let primary_instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::PRIMARY,
                    ..Default::default()
                });
                primary_instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        } else {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                // Prefer "native" backends; this avoids initializing GL stacks on platforms where
                // they're more likely to require a windowing system.
                backends: wgpu::Backends::PRIMARY,
                ..Default::default()
            });
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
        }
        .ok_or_else(|| GpuError::Backend("no suitable wgpu adapter found".into()))?;

        let downlevel = adapter.get_downlevel_capabilities();
        let supports_compute = downlevel
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);

        let requested_features = crate::wgpu_features::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero wgpu backend"),
                    required_features: requested_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|err| GpuError::Backend(err.to_string()))?;

        let mut capabilities = GpuCapabilities::from_device(&device);
        capabilities.supports_compute = supports_compute;

        Ok(Self {
            kind,
            capabilities,
            device,
            queue,
            buffers: ResourceRegistry::new("buffer"),
            textures: ResourceRegistry::new("texture"),
            texture_views: ResourceRegistry::new("texture_view"),
            samplers: ResourceRegistry::new("sampler"),
            bind_group_layouts: ResourceRegistry::new("bind_group_layout"),
            bind_groups: ResourceRegistry::new("bind_group"),
            pipelines: ResourceRegistry::new("pipeline"),
            command_buffers: ResourceRegistry::new("command_buffer"),
            pipeline_layout_cache: LruCache::new(
                NonZeroUsize::new(PIPELINE_LAYOUT_CACHE_CAPACITY)
                    .expect("PIPELINE_LAYOUT_CACHE_CAPACITY must be non-zero"),
            ),
        })
    }
}

impl WgpuBackend {
    fn map_buffer_usages(usages: BufferUsages) -> wgpu::BufferUsages {
        let mut out = wgpu::BufferUsages::empty();
        if usages.contains(BufferUsages::MAP_READ) {
            out |= wgpu::BufferUsages::MAP_READ;
        }
        if usages.contains(BufferUsages::MAP_WRITE) {
            out |= wgpu::BufferUsages::MAP_WRITE;
        }
        if usages.contains(BufferUsages::COPY_SRC) {
            out |= wgpu::BufferUsages::COPY_SRC;
        }
        if usages.contains(BufferUsages::COPY_DST) {
            out |= wgpu::BufferUsages::COPY_DST;
        }
        if usages.contains(BufferUsages::INDEX) {
            out |= wgpu::BufferUsages::INDEX;
        }
        if usages.contains(BufferUsages::VERTEX) {
            out |= wgpu::BufferUsages::VERTEX;
        }
        if usages.contains(BufferUsages::UNIFORM) {
            out |= wgpu::BufferUsages::UNIFORM;
        }
        if usages.contains(BufferUsages::STORAGE) {
            out |= wgpu::BufferUsages::STORAGE;
        }
        if usages.contains(BufferUsages::INDIRECT) {
            out |= wgpu::BufferUsages::INDIRECT;
        }
        out
    }

    fn map_texture_usages(usages: TextureUsages) -> wgpu::TextureUsages {
        let mut out = wgpu::TextureUsages::empty();
        if usages.contains(TextureUsages::COPY_SRC) {
            out |= wgpu::TextureUsages::COPY_SRC;
        }
        if usages.contains(TextureUsages::COPY_DST) {
            out |= wgpu::TextureUsages::COPY_DST;
        }
        if usages.contains(TextureUsages::TEXTURE_BINDING) {
            out |= wgpu::TextureUsages::TEXTURE_BINDING;
        }
        if usages.contains(TextureUsages::STORAGE_BINDING) {
            out |= wgpu::TextureUsages::STORAGE_BINDING;
        }
        if usages.contains(TextureUsages::RENDER_ATTACHMENT) {
            out |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        out
    }

    fn map_texture_format(format: TextureFormat) -> wgpu::TextureFormat {
        match format {
            TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            TextureFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
            TextureFormat::Depth24Plus => wgpu::TextureFormat::Depth24Plus,
        }
    }

    fn map_filter_mode(mode: FilterMode) -> wgpu::FilterMode {
        match mode {
            FilterMode::Nearest => wgpu::FilterMode::Nearest,
            FilterMode::Linear => wgpu::FilterMode::Linear,
        }
    }

    fn map_address_mode(mode: AddressMode) -> wgpu::AddressMode {
        match mode {
            AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
            AddressMode::Repeat => wgpu::AddressMode::Repeat,
            AddressMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
        }
    }

    fn map_shader_stages(stages: ShaderStages) -> wgpu::ShaderStages {
        let mut out = wgpu::ShaderStages::empty();
        if stages.contains(ShaderStages::VERTEX) {
            out |= wgpu::ShaderStages::VERTEX;
        }
        if stages.contains(ShaderStages::FRAGMENT) {
            out |= wgpu::ShaderStages::FRAGMENT;
        }
        if stages.contains(ShaderStages::COMPUTE) {
            out |= wgpu::ShaderStages::COMPUTE;
        }
        out
    }

    fn map_binding_type(ty: &BindingTypeDesc) -> wgpu::BindingType {
        match ty {
            BindingTypeDesc::UniformBuffer { dynamic, min_size } => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: *dynamic,
                min_binding_size: min_size.map(wgpu::BufferSize::new).flatten(),
            },
            BindingTypeDesc::SamplerFiltering => {
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
            }
            BindingTypeDesc::Texture2dFloat { filterable } => wgpu::BindingType::Texture {
                multisampled: false,
                view_dimension: wgpu::TextureViewDimension::D2,
                sample_type: wgpu::TextureSampleType::Float {
                    filterable: *filterable,
                },
            },
        }
    }

    fn map_primitive_topology(topology: PrimitiveTopology) -> wgpu::PrimitiveTopology {
        match topology {
            PrimitiveTopology::TriangleList => wgpu::PrimitiveTopology::TriangleList,
        }
    }

    fn map_load_op_color(load: LoadOp<Color>) -> wgpu::LoadOp<wgpu::Color> {
        match load {
            LoadOp::Load => wgpu::LoadOp::Load,
            LoadOp::Clear(color) => wgpu::LoadOp::Clear(wgpu::Color {
                r: color.r,
                g: color.g,
                b: color.b,
                a: color.a,
            }),
        }
    }

    fn map_store_op(store: StoreOp) -> wgpu::StoreOp {
        match store {
            StoreOp::Store => wgpu::StoreOp::Store,
            StoreOp::Discard => wgpu::StoreOp::Discard,
        }
    }

    fn map_index_format(format: IndexFormat) -> wgpu::IndexFormat {
        match format {
            IndexFormat::Uint16 => wgpu::IndexFormat::Uint16,
            IndexFormat::Uint32 => wgpu::IndexFormat::Uint32,
        }
    }
}

impl GpuBackend for WgpuBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn capabilities(&self) -> &GpuCapabilities {
        &self.capabilities
    }

    fn create_buffer(&mut self, desc: BufferDesc) -> Result<BufferId, GpuError> {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: desc.label.as_deref(),
            size: desc.size,
            usage: Self::map_buffer_usages(desc.usage),
            mapped_at_creation: false,
        });
        Ok(self.buffers.insert(buffer))
    }

    fn destroy_buffer(&mut self, id: BufferId) -> Result<(), GpuError> {
        let buffer = self.buffers.remove(id)?;
        buffer.destroy();
        Ok(())
    }

    fn write_buffer(&mut self, buffer: BufferId, offset: u64, data: &[u8]) -> Result<(), GpuError> {
        let buffer = self.buffers.get(buffer)?;
        let size_bytes = u64::try_from(data.len())
            .map_err(|_| GpuError::Backend("write_buffer payload too large".into()))?;
        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        if !offset.is_multiple_of(alignment) || !size_bytes.is_multiple_of(alignment) {
            return Err(GpuError::Backend(format!(
                "write_buffer offset/size must be {alignment}-byte aligned (offset={offset}, size_bytes={size_bytes})"
            )));
        }
        self.queue.write_buffer(buffer, offset, data);
        Ok(())
    }

    fn create_texture(&mut self, desc: TextureDesc) -> Result<TextureId, GpuError> {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: desc.label.as_deref(),
            size: wgpu::Extent3d {
                width: desc.size.width,
                height: desc.size.height,
                depth_or_array_layers: desc.size.depth_or_array_layers,
            },
            mip_level_count: desc.mip_level_count,
            sample_count: desc.sample_count,
            dimension: match desc.dimension {
                TextureDimension::D2 => wgpu::TextureDimension::D2,
            },
            format: Self::map_texture_format(desc.format),
            usage: Self::map_texture_usages(desc.usage),
            view_formats: &[],
        });
        Ok(self.textures.insert(texture))
    }

    fn destroy_texture(&mut self, id: TextureId) -> Result<(), GpuError> {
        let texture = self.textures.remove(id)?;
        texture.destroy();
        Ok(())
    }

    fn write_texture(&mut self, desc: TextureWriteDesc, data: &[u8]) -> Result<(), GpuError> {
        let texture = self.textures.get(desc.texture)?;
        if desc.layout.bytes_per_row == Some(0) {
            return Err(GpuError::Backend(
                "ImageDataLayout.bytes_per_row must be non-zero".into(),
            ));
        }
        if desc.layout.rows_per_image == Some(0) {
            return Err(GpuError::Backend(
                "ImageDataLayout.rows_per_image must be non-zero".into(),
            ));
        }

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: desc.mip_level,
                origin: wgpu::Origin3d {
                    x: desc.origin.x,
                    y: desc.origin.y,
                    z: desc.origin.z,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::ImageDataLayout {
                offset: desc.layout.offset,
                bytes_per_row: desc.layout.bytes_per_row,
                rows_per_image: desc.layout.rows_per_image,
            },
            wgpu::Extent3d {
                width: desc.size.width,
                height: desc.size.height,
                depth_or_array_layers: desc.size.depth_or_array_layers,
            },
        );
        Ok(())
    }

    fn create_texture_view(
        &mut self,
        texture: TextureId,
        _desc: TextureViewDesc,
    ) -> Result<TextureViewId, GpuError> {
        let texture = self.textures.get(texture)?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(self.texture_views.insert(view))
    }

    fn destroy_texture_view(&mut self, id: TextureViewId) -> Result<(), GpuError> {
        let _view = self.texture_views.remove(id)?;
        Ok(())
    }

    fn create_sampler(&mut self, desc: SamplerDesc) -> Result<SamplerId, GpuError> {
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: desc.label.as_deref(),
            address_mode_u: Self::map_address_mode(desc.address_mode_u),
            address_mode_v: Self::map_address_mode(desc.address_mode_v),
            address_mode_w: Self::map_address_mode(desc.address_mode_w),
            mag_filter: Self::map_filter_mode(desc.mag_filter),
            min_filter: Self::map_filter_mode(desc.min_filter),
            mipmap_filter: Self::map_filter_mode(desc.mipmap_filter),
            ..Default::default()
        });
        Ok(self.samplers.insert(sampler))
    }

    fn destroy_sampler(&mut self, id: SamplerId) -> Result<(), GpuError> {
        let _sampler = self.samplers.remove(id)?;
        Ok(())
    }

    fn create_bind_group_layout(
        &mut self,
        desc: BindGroupLayoutDesc,
    ) -> Result<BindGroupLayoutId, GpuError> {
        let entries: Vec<wgpu::BindGroupLayoutEntry> = desc
            .entries
            .iter()
            .map(|entry| wgpu::BindGroupLayoutEntry {
                binding: entry.binding,
                visibility: Self::map_shader_stages(entry.visibility),
                ty: Self::map_binding_type(&entry.ty),
                count: None,
            })
            .collect();

        let layout = self
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: desc.label.as_deref(),
                entries: &entries,
            });
        Ok(self.bind_group_layouts.insert(layout))
    }

    fn destroy_bind_group_layout(&mut self, id: BindGroupLayoutId) -> Result<(), GpuError> {
        let _layout = self.bind_group_layouts.remove(id)?;
        // Pipeline layouts are defined in terms of bind-group layouts; when one is destroyed,
        // drop cached pipeline layouts to avoid pinning old layouts indefinitely.
        self.pipeline_layout_cache.clear();
        Ok(())
    }

    fn create_bind_group(&mut self, desc: BindGroupDesc) -> Result<BindGroupId, GpuError> {
        let layout = self.bind_group_layouts.get(desc.layout)?;
        let mut entries = Vec::with_capacity(desc.entries.len());
        for entry in &desc.entries {
            let resource = match &entry.resource {
                BindingResourceDesc::Buffer {
                    buffer,
                    offset,
                    size,
                } => {
                    let buffer = self.buffers.get(*buffer)?;
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer,
                        offset: *offset,
                        size: size.map(wgpu::BufferSize::new).flatten(),
                    })
                }
                BindingResourceDesc::Sampler(id) => {
                    let sampler = self.samplers.get(*id)?;
                    wgpu::BindingResource::Sampler(sampler)
                }
                BindingResourceDesc::TextureView(id) => {
                    let view = self.texture_views.get(*id)?;
                    wgpu::BindingResource::TextureView(view)
                }
            };

            entries.push(wgpu::BindGroupEntry {
                binding: entry.binding,
                resource,
            });
        }

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: desc.label.as_deref(),
            layout,
            entries: &entries,
        });

        Ok(self.bind_groups.insert(bind_group))
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) -> Result<(), GpuError> {
        let _bind_group = self.bind_groups.remove(id)?;
        Ok(())
    }

    fn create_render_pipeline(&mut self, desc: RenderPipelineDesc) -> Result<PipelineId, GpuError> {
        let RenderPipelineDesc {
            label,
            shader_wgsl,
            vertex_entry,
            fragment_entry,
            bind_group_layouts: bind_group_layout_ids,
            color_format,
            depth_format,
            topology,
        } = desc;

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: label.as_deref(),
                source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader_wgsl)),
            });

        let pipeline_layout = if bind_group_layout_ids.is_empty() {
            None
        } else if let Some(layout) = self.pipeline_layout_cache.get(&bind_group_layout_ids) {
            Some(layout)
        } else {
            let bind_group_layouts: Vec<&wgpu::BindGroupLayout> = bind_group_layout_ids
                .iter()
                .map(|id| self.bind_group_layouts.get(*id))
                .collect::<Result<_, _>>()?;

            let layout = self
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    // Pipeline layout labels are only for debugging; cache hits may reuse a layout that
                    // was originally created with a different label.
                    label: label.as_deref(),
                    bind_group_layouts: &bind_group_layouts,
                    push_constant_ranges: &[],
                });

            self.pipeline_layout_cache
                .put(bind_group_layout_ids.clone(), layout);
            Some(
                self.pipeline_layout_cache
                    .get(&bind_group_layout_ids)
                    .expect("just inserted pipeline layout"),
            )
        };

        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: label.as_deref(),
                layout: pipeline_layout,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: vertex_entry.as_str(),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: fragment_entry.as_str(),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: Self::map_texture_format(color_format),
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: Self::map_primitive_topology(topology),
                    ..Default::default()
                },
                depth_stencil: depth_format.map(|format| wgpu::DepthStencilState {
                    format: Self::map_texture_format(format),
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview: None,
            });

        Ok(self.pipelines.insert(Pipeline::Render(pipeline)))
    }

    fn create_compute_pipeline(
        &mut self,
        desc: ComputePipelineDesc,
    ) -> Result<PipelineId, GpuError> {
        if !self.capabilities.supports_compute {
            return Err(GpuError::Unsupported("compute_pipelines"));
        }

        let ComputePipelineDesc {
            label,
            shader_wgsl,
            entry_point,
            bind_group_layouts: bind_group_layout_ids,
        } = desc;

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: label.as_deref(),
                source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader_wgsl)),
            });

        let pipeline_layout = if bind_group_layout_ids.is_empty() {
            None
        } else if let Some(layout) = self.pipeline_layout_cache.get(&bind_group_layout_ids) {
            Some(layout)
        } else {
            let bind_group_layouts: Vec<&wgpu::BindGroupLayout> = bind_group_layout_ids
                .iter()
                .map(|id| self.bind_group_layouts.get(*id))
                .collect::<Result<_, _>>()?;

            let layout = self
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: label.as_deref(),
                    bind_group_layouts: &bind_group_layouts,
                    push_constant_ranges: &[],
                });

            self.pipeline_layout_cache
                .put(bind_group_layout_ids.clone(), layout);
            Some(
                self.pipeline_layout_cache
                    .get(&bind_group_layout_ids)
                    .expect("just inserted pipeline layout"),
            )
        };

        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: label.as_deref(),
                layout: pipeline_layout,
                module: &shader,
                entry_point: entry_point.as_str(),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            });

        Ok(self.pipelines.insert(Pipeline::Compute(pipeline)))
    }

    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<(), GpuError> {
        let _pipeline = self.pipelines.remove(id)?;
        Ok(())
    }

    fn create_command_buffer(
        &mut self,
        commands: &[GpuCommand],
    ) -> Result<CommandBufferId, GpuError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu command encoder"),
            });

        let mut i = 0usize;
        while i < commands.len() {
            match &commands[i] {
                GpuCommand::BeginRenderPass(desc) => {
                    let mut color_attachments = Vec::with_capacity(desc.color_attachments.len());
                    for attachment in &desc.color_attachments {
                        let view = self.texture_views.get(attachment.view)?;
                        color_attachments.push(Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: Self::map_load_op_color(attachment.ops.load),
                                store: Self::map_store_op(attachment.ops.store),
                            },
                        }));
                    }

                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: desc.label.as_deref(),
                        color_attachments: &color_attachments,
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                    });

                    let mut ended = false;
                    i += 1;
                    while i < commands.len() {
                        match &commands[i] {
                            GpuCommand::EndRenderPass => {
                                ended = true;
                                i += 1;
                                break;
                            }
                            GpuCommand::SetPipeline(id) => match self.pipelines.get(*id)? {
                                Pipeline::Render(pipeline) => render_pass.set_pipeline(pipeline),
                                Pipeline::Compute(_) => {
                                    return Err(GpuError::Backend(
                                        "attempted to bind compute pipeline in render pass".into(),
                                    ))
                                }
                            },
                            GpuCommand::SetBindGroup { index, bind_group } => {
                                let group = self.bind_groups.get(*bind_group)?;
                                render_pass.set_bind_group(*index, group, &[]);
                            }
                            GpuCommand::SetVertexBuffer {
                                slot,
                                buffer,
                                offset,
                            } => {
                                let buffer = self.buffers.get(*buffer)?;
                                render_pass.set_vertex_buffer(*slot, buffer.slice(*offset..));
                            }
                            GpuCommand::SetIndexBuffer {
                                buffer,
                                offset,
                                format,
                            } => {
                                let buffer = self.buffers.get(*buffer)?;
                                render_pass.set_index_buffer(
                                    buffer.slice(*offset..),
                                    Self::map_index_format(*format),
                                );
                            }
                            GpuCommand::Draw {
                                vertices,
                                instances,
                            } => {
                                render_pass.draw(vertices.clone(), instances.clone());
                            }
                            GpuCommand::DrawIndexed {
                                indices,
                                base_vertex,
                                instances,
                            } => {
                                render_pass.draw_indexed(
                                    indices.clone(),
                                    *base_vertex,
                                    instances.clone(),
                                );
                            }
                            other => {
                                return Err(GpuError::Backend(format!(
                                    "invalid command inside render pass: {other:?}"
                                )))
                            }
                        }
                        i += 1;
                    }

                    if !ended {
                        return Err(GpuError::Backend(
                            "render pass did not terminate with EndRenderPass".into(),
                        ));
                    }
                }
                GpuCommand::BeginComputePass { label } => {
                    let mut compute_pass =
                        encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: label.as_deref(),
                            timestamp_writes: None,
                        });

                    let mut ended = false;
                    i += 1;
                    while i < commands.len() {
                        match &commands[i] {
                            GpuCommand::EndComputePass => {
                                ended = true;
                                i += 1;
                                break;
                            }
                            GpuCommand::SetPipeline(id) => match self.pipelines.get(*id)? {
                                Pipeline::Compute(pipeline) => compute_pass.set_pipeline(pipeline),
                                Pipeline::Render(_) => {
                                    return Err(GpuError::Backend(
                                        "attempted to bind render pipeline in compute pass".into(),
                                    ))
                                }
                            },
                            GpuCommand::SetBindGroup { index, bind_group } => {
                                let group = self.bind_groups.get(*bind_group)?;
                                compute_pass.set_bind_group(*index, group, &[]);
                            }
                            GpuCommand::DispatchWorkgroups { x, y, z } => {
                                compute_pass.dispatch_workgroups(*x, *y, *z);
                            }
                            other => {
                                return Err(GpuError::Backend(format!(
                                    "invalid command inside compute pass: {other:?}"
                                )))
                            }
                        }
                        i += 1;
                    }

                    if !ended {
                        return Err(GpuError::Backend(
                            "compute pass did not terminate with EndComputePass".into(),
                        ));
                    }
                }
                other => {
                    return Err(GpuError::Backend(format!(
                        "unexpected command outside pass: {other:?}"
                    )))
                }
            }
        }

        let command_buffer = encoder.finish();
        Ok(self.command_buffers.insert(command_buffer))
    }

    fn submit(&mut self, command_buffers: &[CommandBufferId]) -> Result<(), GpuError> {
        let mut buffers = Vec::with_capacity(command_buffers.len());
        for id in command_buffers {
            buffers.push(self.command_buffers.remove(*id)?);
        }

        self.queue.submit(buffers);
        Ok(())
    }

    fn present(&mut self) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("present"))
    }
}
