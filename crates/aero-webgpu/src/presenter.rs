use crate::{
    BackendCaps, BackendError, BackendKind, PresentError, WebGl2Stub, WebGpuContext,
    WebGpuInitError, WebGpuInitOptions,
};

#[cfg(target_arch = "wasm32")]
use crate::RequestedBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramebufferSize {
    pub width: u32,
    pub height: u32,
}

impl FramebufferSize {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    fn clamped_for_surface(self) -> Self {
        Self {
            width: self.width.max(1),
            height: self.height.max(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspectMode {
    /// Stretch to fill the canvas/surface.
    Stretch,
    /// Preserve aspect ratio (letterboxing/pillarboxing).
    Fit,
}

impl AspectMode {
    fn as_u32(self) -> u32 {
        match self {
            AspectMode::Stretch => 0,
            AspectMode::Fit => 1,
        }
    }
}

/// Presentation abstraction: a `wgpu` presenter (WebGPU/WebGL2 backends) or a stub backend.
pub enum FramebufferPresenter<'a> {
    Wgpu(WebGpuFramebufferPresenter<'a>),
    WebGl2Stub(WebGl2Stub),
}

impl<'a> FramebufferPresenter<'a> {
    pub fn kind(&self) -> BackendKind {
        match self {
            FramebufferPresenter::Wgpu(p) => p.context.kind(),
            FramebufferPresenter::WebGl2Stub(_) => BackendKind::WebGl2,
        }
    }

    pub fn caps(&self) -> &BackendCaps {
        match self {
            FramebufferPresenter::Wgpu(p) => p.context.caps(),
            FramebufferPresenter::WebGl2Stub(stub) => stub.caps(),
        }
    }

    pub fn resize(&mut self, new_size: FramebufferSize) -> Result<(), PresentError> {
        match self {
            FramebufferPresenter::Wgpu(p) => p.resize(new_size),
            FramebufferPresenter::WebGl2Stub(_) => Ok(()),
        }
    }

    pub fn set_aspect_mode(&mut self, mode: AspectMode) {
        match self {
            FramebufferPresenter::Wgpu(p) => p.set_aspect_mode(mode),
            FramebufferPresenter::WebGl2Stub(_) => {}
        }
    }

    pub fn present_rgba8(
        &mut self,
        pixels: &[u8],
        size: FramebufferSize,
    ) -> Result<(), PresentError> {
        match self {
            FramebufferPresenter::Wgpu(p) => p.present_rgba8(pixels, size),
            FramebufferPresenter::WebGl2Stub(_) => Err(PresentError::WebGl2NotImplemented),
        }
    }

    /// Create a presenter from an existing `wgpu::Surface`.
    pub async fn new_with_surface(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'a>,
        surface_size: FramebufferSize,
        options: crate::BackendOptions,
    ) -> Result<Self, BackendError> {
        Self::new_with_surface_backend(
            instance,
            surface,
            surface_size,
            BackendKind::WebGpu,
            options,
        )
        .await
    }

    async fn new_with_surface_backend(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'a>,
        surface_size: FramebufferSize,
        backend_kind: BackendKind,
        options: crate::BackendOptions,
    ) -> Result<Self, BackendError> {
        let presenter = WebGpuFramebufferPresenter::new(
            instance,
            surface,
            surface_size,
            backend_kind,
            options.webgpu.clone(),
        )
        .await?;
        Ok(FramebufferPresenter::Wgpu(presenter))
    }

    /// Create a presenter for a browser `<canvas>` using WebGPU, with a WebGL2 fallback.
    ///
    /// This is only available on `wasm32`, since it depends on `web-sys`.
    #[cfg(target_arch = "wasm32")]
    pub async fn new_for_canvas(
        canvas: web_sys::HtmlCanvasElement,
        options: crate::BackendOptions,
    ) -> Result<FramebufferPresenter<'static>, BackendError> {
        let size = FramebufferSize::new(canvas.width(), canvas.height()).clamped_for_surface();
        let requested = options.requested_backend;
        let allow_fallback =
            options.allow_webgl2_fallback && matches!(requested, RequestedBackend::Auto);

        match requested {
            RequestedBackend::WebGpu => {
                try_presenter_for_html_canvas(
                    canvas,
                    size,
                    BackendKind::WebGpu,
                    wgpu::Backends::BROWSER_WEBGPU,
                    options,
                )
                .await
            }
            RequestedBackend::WebGl2 => {
                try_presenter_for_html_canvas(
                    canvas,
                    size,
                    BackendKind::WebGl2,
                    wgpu::Backends::BROWSER_WEBGL,
                    options,
                )
                .await
            }
            RequestedBackend::Auto => {
                match try_presenter_for_html_canvas(
                    canvas.clone(),
                    size,
                    BackendKind::WebGpu,
                    wgpu::Backends::BROWSER_WEBGPU,
                    options.clone(),
                )
                .await
                {
                    Ok(p) => Ok(p),
                    Err(webgpu_err) if allow_fallback => match try_presenter_for_html_canvas(
                        canvas,
                        size,
                        BackendKind::WebGl2,
                        wgpu::Backends::BROWSER_WEBGL,
                        options,
                    )
                    .await
                    {
                        Ok(p) => Ok(p),
                        Err(webgl_err) => Err(BackendError::NoUsableBackend {
                            webgpu: webgpu_err.to_string(),
                            webgl2: webgl_err.to_string(),
                        }),
                    },
                    Err(err) => Err(err),
                }
            }
        }
    }

    /// Create a presenter for a browser `OffscreenCanvas`, with optional WebGL2 fallback.
    ///
    /// This is intended for GPU worker usage where the main thread transfers an
    /// `HTMLCanvasElement` to a worker via `transferControlToOffscreen()`.
    #[cfg(target_arch = "wasm32")]
    pub async fn new_for_offscreen_canvas(
        canvas: web_sys::OffscreenCanvas,
        options: crate::BackendOptions,
    ) -> Result<FramebufferPresenter<'static>, BackendError> {
        let size = FramebufferSize::new(canvas.width(), canvas.height()).clamped_for_surface();
        let requested = options.requested_backend;
        let allow_fallback =
            options.allow_webgl2_fallback && matches!(requested, RequestedBackend::Auto);

        match requested {
            RequestedBackend::WebGpu => {
                try_presenter_for_offscreen_canvas(
                    canvas,
                    size,
                    BackendKind::WebGpu,
                    wgpu::Backends::BROWSER_WEBGPU,
                    options,
                )
                .await
            }
            RequestedBackend::WebGl2 => {
                try_presenter_for_offscreen_canvas(
                    canvas,
                    size,
                    BackendKind::WebGl2,
                    wgpu::Backends::BROWSER_WEBGL,
                    options,
                )
                .await
            }
            RequestedBackend::Auto => {
                match try_presenter_for_offscreen_canvas(
                    canvas.clone(),
                    size,
                    BackendKind::WebGpu,
                    wgpu::Backends::BROWSER_WEBGPU,
                    options.clone(),
                )
                .await
                {
                    Ok(p) => Ok(p),
                    Err(webgpu_err) if allow_fallback => match try_presenter_for_offscreen_canvas(
                        canvas,
                        size,
                        BackendKind::WebGl2,
                        wgpu::Backends::BROWSER_WEBGL,
                        options,
                    )
                    .await
                    {
                        Ok(p) => Ok(p),
                        Err(webgl_err) => Err(BackendError::NoUsableBackend {
                            webgpu: webgpu_err.to_string(),
                            webgl2: webgl_err.to_string(),
                        }),
                    },
                    Err(err) => Err(err),
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
async fn try_presenter_for_html_canvas(
    canvas: web_sys::HtmlCanvasElement,
    surface_size: FramebufferSize,
    backend_kind: BackendKind,
    backends: wgpu::Backends,
    options: crate::BackendOptions,
) -> Result<FramebufferPresenter<'static>, BackendError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| BackendError::WebGpu(WebGpuInitError::CreateSurface(format!("{e:?}"))))?;
    FramebufferPresenter::new_with_surface_backend(
        instance,
        surface,
        surface_size,
        backend_kind,
        options,
    )
    .await
}

#[cfg(target_arch = "wasm32")]
async fn try_presenter_for_offscreen_canvas(
    canvas: web_sys::OffscreenCanvas,
    surface_size: FramebufferSize,
    backend_kind: BackendKind,
    backends: wgpu::Backends,
    options: crate::BackendOptions,
) -> Result<FramebufferPresenter<'static>, BackendError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas))
        .map_err(|e| BackendError::WebGpu(WebGpuInitError::CreateSurface(format!("{e:?}"))))?;
    FramebufferPresenter::new_with_surface_backend(
        instance,
        surface,
        surface_size,
        backend_kind,
        options,
    )
    .await
}

struct SourceTexture {
    size: FramebufferSize,
    texture: wgpu::Texture,
    // Keep the view alive for the lifetime of the bind group.
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

pub struct WebGpuFramebufferPresenter<'a> {
    pub(crate) context: WebGpuContext,
    surface: wgpu::Surface<'a>,
    _surface_format: wgpu::TextureFormat,
    config: wgpu::SurfaceConfiguration,

    surface_size: FramebufferSize,
    aspect_mode: AspectMode,

    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,

    source: Option<SourceTexture>,
    staging_rgba: Vec<u8>,
}

impl<'a> WebGpuFramebufferPresenter<'a> {
    pub async fn new(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'a>,
        surface_size: FramebufferSize,
        backend_kind: BackendKind,
        options: WebGpuInitOptions,
    ) -> Result<Self, WebGpuInitError> {
        let surface_size = surface_size.clamped_for_surface();

        let context =
            WebGpuContext::request_with_surface(instance, backend_kind, options, &surface).await?;
        let device = context.device();
        let adapter = context.adapter();

        let surface_caps = surface.get_capabilities(adapter);
        let surface_format = preferred_surface_format(&surface_caps.formats);
        let alpha_mode = surface_caps.alpha_modes[0];
        let present_mode = preferred_present_mode(&surface_caps.present_modes);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: surface_size.width,
            height: surface_size.height,
            present_mode,
            alpha_mode,
            desired_maximum_frame_latency: 2,
            view_formats: vec![],
        };

        surface.configure(device, &config);

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero framebuffer presenter bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aero framebuffer presenter pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero framebuffer presenter shader"),
            source: wgpu::ShaderSource::Wgsl(PRESENT_WGSL.into()),
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aero framebuffer presenter pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aero framebuffer presenter sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero framebuffer presenter uniforms"),
            size: std::mem::size_of::<PresentUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            context,
            surface,
            _surface_format: surface_format,
            config,
            surface_size,
            aspect_mode: AspectMode::Fit,
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            source: None,
            staging_rgba: Vec::new(),
        })
    }

    pub fn resize(&mut self, new_size: FramebufferSize) -> Result<(), PresentError> {
        let new_size = new_size.clamped_for_surface();
        self.surface_size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(self.context.device(), &self.config);
        Ok(())
    }

    pub fn set_aspect_mode(&mut self, mode: AspectMode) {
        self.aspect_mode = mode;
    }

    pub fn present_rgba8(
        &mut self,
        pixels: &[u8],
        size: FramebufferSize,
    ) -> Result<(), PresentError> {
        if size.width == 0 || size.height == 0 {
            return Err(PresentError::InvalidFramebufferSize);
        }

        let expected_len = size.width as usize * size.height as usize * 4;
        if pixels.len() != expected_len {
            return Err(PresentError::InvalidFramebufferLength {
                expected: expected_len,
                actual: pixels.len(),
            });
        }

        self.ensure_source_texture(size)?;
        self.upload_rgba8(pixels, size);
        self.draw()?;
        Ok(())
    }

    fn ensure_source_texture(&mut self, size: FramebufferSize) -> Result<(), PresentError> {
        let needs_recreate = match &self.source {
            Some(src) => src.size != size,
            None => true,
        };

        if !needs_recreate {
            return Ok(());
        }

        let device = self.context.device();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero framebuffer presenter source texture"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aero framebuffer presenter bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        self.source = Some(SourceTexture {
            size,
            texture,
            _view: view,
            bind_group,
        });
        Ok(())
    }

    fn upload_rgba8(&mut self, pixels: &[u8], size: FramebufferSize) {
        let queue = self.context.queue();
        let src = self.source.as_ref().expect("source texture must exist");

        let unpadded_bpr = size.width * 4;
        let needs_padding = (unpadded_bpr % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) != 0;

        let (src_bytes, bytes_per_row) = if !needs_padding {
            (pixels, unpadded_bpr)
        } else {
            let padded_bpr = padded_bytes_per_row(unpadded_bpr);
            let padded_len = padded_bpr as usize * size.height as usize;
            self.staging_rgba.resize(padded_len, 0);

            for y in 0..size.height as usize {
                let src_row = &pixels[y * unpadded_bpr as usize..(y + 1) * unpadded_bpr as usize];
                let dst_row = &mut self.staging_rgba
                    [y * padded_bpr as usize..y * padded_bpr as usize + unpadded_bpr as usize];
                dst_row.copy_from_slice(src_row);
            }

            (&self.staging_rgba[..], padded_bpr)
        };

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &src.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            src_bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(size.height),
            },
            wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
        );

        let uniforms = PresentUniforms {
            output_size: [
                self.surface_size.width as f32,
                self.surface_size.height as f32,
            ],
            input_size: [size.width as f32, size.height as f32],
            mode: self.aspect_mode.as_u32(),
            _pad: 0,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    fn draw(&mut self) -> Result<(), PresentError> {
        let device = self.context.device();
        let queue = self.context.queue();

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                // Window resize / swap chain invalidation; reconfigure and retry once.
                self.surface.configure(device, &self.config);
                self.surface.get_current_texture()?
            }
            Err(e) => return Err(e.into()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero framebuffer presenter encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aero framebuffer presenter pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(
                0,
                &self.source.as_ref().expect("source must exist").bind_group,
                &[],
            );
            pass.draw(0..3, 0..1);
        }

        queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

fn preferred_surface_format(formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    for &format in formats {
        if matches!(
            format,
            wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb
        ) {
            return format;
        }
    }
    formats
        .first()
        .copied()
        .unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb)
}

fn preferred_present_mode(modes: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    // Fifo is universally supported and matches browser vsync semantics.
    if modes.contains(&wgpu::PresentMode::Fifo) {
        return wgpu::PresentMode::Fifo;
    }
    modes.first().copied().unwrap_or(wgpu::PresentMode::Fifo)
}

fn padded_bytes_per_row(unpadded_bytes_per_row: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    ((unpadded_bytes_per_row + align - 1) / align) * align
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PresentUniforms {
    output_size: [f32; 2],
    input_size: [f32; 2],
    mode: u32,
    _pad: u32,
}

const PRESENT_WGSL: &str = r#"
struct Uniforms {
    output_size: vec2<f32>,
    input_size: vec2<f32>,
    mode: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(pos[idx], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var uv = pos.xy / u.output_size;

    if (u.mode == 1u) {
        let src_aspect = u.input_size.x / u.input_size.y;
        let dst_aspect = u.output_size.x / u.output_size.y;

        if (dst_aspect > src_aspect) {
            let ratio = src_aspect / dst_aspect;
            let x = (uv.x - 0.5) / ratio + 0.5;
            if (x < 0.0 || x > 1.0) {
                return vec4<f32>(0.0, 0.0, 0.0, 1.0);
            }
            uv = vec2<f32>(x, uv.y);
        } else {
            let ratio = dst_aspect / src_aspect;
            let y = (uv.y - 0.5) / ratio + 0.5;
            if (y < 0.0 || y > 1.0) {
                return vec4<f32>(0.0, 0.0, 0.0, 1.0);
            }
            uv = vec2<f32>(uv.x, y);
        }
    }

    return textureSample(src_tex, src_samp, uv);
}
"#;
