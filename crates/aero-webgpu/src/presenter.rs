use crate::{
    BackendCaps, BackendError, BackendKind, PresentError, WebGl2Stub, WebGpuContext,
    WebGpuInitError, WebGpuInitOptions,
};

#[cfg(target_arch = "wasm32")]
use crate::RequestedBackend;

use crate::upload::Rgba8TextureUploader;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AspectMode {
    /// Stretch to fill the canvas/surface.
    Stretch,
    /// Preserve aspect ratio (letterboxing/pillarboxing).
    #[default]
    FitKeepAspect,
    /// Preserve aspect ratio using an integer scale factor when possible.
    ///
    /// If the surface is smaller than the framebuffer, this falls back to `FitKeepAspect`.
    IntegerScale,
}

impl AspectMode {
    fn as_u32(self) -> u32 {
        match self {
            AspectMode::Stretch => 0,
            AspectMode::FitKeepAspect => 1,
            AspectMode::IntegerScale => 2,
        }
    }
}

/// Presentation abstraction: a `wgpu` presenter (WebGPU/WebGL2 backends) or a stub backend.
pub enum FramebufferPresenter<'a> {
    Wgpu(Box<WebGpuFramebufferPresenter<'a>>),
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
        stride_bytes: u32,
    ) -> Result<(), PresentError> {
        match self {
            FramebufferPresenter::Wgpu(p) => p.present_rgba8(pixels, size, stride_bytes),
            FramebufferPresenter::WebGl2Stub(_) => Err(PresentError::WebGl2NotImplemented),
        }
    }

    pub async fn screenshot_high_level(&self) -> Result<Vec<u8>, PresentError> {
        match self {
            FramebufferPresenter::Wgpu(p) => p.screenshot_high_level().await,
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
        Ok(FramebufferPresenter::Wgpu(Box::new(presenter)))
    }

    /// Create a presenter for a browser `<canvas>` using WebGPU, with a WebGL2 fallback.
    ///
    /// This is only available on `wasm32`, since it depends on `web-sys`.
    #[cfg(target_arch = "wasm32")]
    pub async fn new_for_canvas(
        canvas: web_sys::HtmlCanvasElement,
        options: crate::BackendOptions,
    ) -> Result<FramebufferPresenter<'static>, BackendError> {
        let size = FramebufferSize::new(canvas.width(), canvas.height());
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
                    wgpu::Backends::GL,
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
                        wgpu::Backends::GL,
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
        let size = FramebufferSize::new(canvas.width(), canvas.height());
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
                    wgpu::Backends::GL,
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
                        wgpu::Backends::GL,
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
    surface_view_format: wgpu::TextureFormat,
    srgb_encode: bool,
    config: wgpu::SurfaceConfiguration,

    surface_size: FramebufferSize,
    aspect_mode: AspectMode,

    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,

    source: Option<SourceTexture>,
    uploader: Rgba8TextureUploader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceAcquireErrorAction {
    /// Drop the frame and continue rendering.
    DropFrame,
    /// Reconfigure the surface and retry once.
    ReconfigureAndRetry,
    /// Treat the error as fatal.
    Fatal,
}

fn surface_acquire_error_action(err: &wgpu::SurfaceError) -> SurfaceAcquireErrorAction {
    match err {
        wgpu::SurfaceError::Timeout => SurfaceAcquireErrorAction::DropFrame,
        wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
            SurfaceAcquireErrorAction::ReconfigureAndRetry
        }
        wgpu::SurfaceError::OutOfMemory => SurfaceAcquireErrorAction::Fatal,
    }
}

impl<'a> WebGpuFramebufferPresenter<'a> {
    pub async fn new(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'a>,
        surface_size: FramebufferSize,
        backend_kind: BackendKind,
        options: WebGpuInitOptions,
    ) -> Result<Self, WebGpuInitError> {
        let config_size = surface_size.clamped_for_surface();

        let context =
            WebGpuContext::request_with_surface(instance, backend_kind, options, &surface).await?;
        let device = context.device();
        let adapter = context.adapter();

        let surface_caps = surface.get_capabilities(adapter);
        let supports_view_formats = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS);
        let prefer_srgb_view =
            matches!(context.kind(), BackendKind::WebGpu) && supports_view_formats;
        let (surface_format, mut surface_view_format, view_formats) =
            preferred_surface_config(&surface_caps.formats, prefer_srgb_view);
        let alpha_mode = preferred_composite_alpha_mode(&surface_caps.alpha_modes);
        let present_mode = preferred_present_mode(&surface_caps.present_modes);

        let mut config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: config_size.width,
            height: config_size.height,
            present_mode,
            alpha_mode,
            desired_maximum_frame_latency: 2,
            view_formats,
        };

        // Some WebGPU implementations may reject `view_formats` (or specific view formats) for
        // canvas/surface configuration. Prefer an sRGB view when requested, but fall back to manual
        // shader encoding if configuration fails.
        if !config.view_formats.is_empty() {
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            surface.configure(device, &config);
            #[cfg(not(target_arch = "wasm32"))]
            device.poll(wgpu::Maintain::Wait);
            #[cfg(target_arch = "wasm32")]
            device.poll(wgpu::Maintain::Poll);
            let err = device.pop_error_scope().await;
            if let Some(err) = err {
                tracing::warn!(
                    "wgpu surface rejected sRGB view format configuration; falling back to manual encoding: {err}"
                );
                config.view_formats.clear();
                surface_view_format = surface_format;
                surface.configure(device, &config);
            }
        } else {
            surface.configure(device, &config);
        }

        let srgb_encode = surface_format_requires_manual_srgb_encoding(surface_view_format);

        let uniform_min_binding_size =
            wgpu::BufferSize::new(std::mem::size_of::<PresentUniforms>() as u64);
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aero framebuffer presenter bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: uniform_min_binding_size,
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aero framebuffer presenter shader"),
            source: wgpu::ShaderSource::Wgsl(PRESENT_WGSL.into()),
        });

        let pipeline = crate::pipeline::create_fullscreen_triangle_pipeline(
            device,
            &bind_group_layout,
            &shader,
            "fs_main",
            surface_view_format,
            Some("aero framebuffer presenter pipeline"),
        );

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
            surface_view_format,
            srgb_encode,
            config,
            surface_size,
            aspect_mode: AspectMode::default(),
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            source: None,
            uploader: Rgba8TextureUploader::new(),
        })
    }

    pub fn resize(&mut self, new_size: FramebufferSize) -> Result<(), PresentError> {
        self.surface_size = new_size;
        let config_size = new_size.clamped_for_surface();
        self.config.width = config_size.width;
        self.config.height = config_size.height;
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
        stride_bytes: u32,
    ) -> Result<(), PresentError> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        let min_stride = size.width.saturating_mul(4);
        if stride_bytes < min_stride {
            return Err(PresentError::InvalidFramebufferStride {
                stride: stride_bytes,
                min: min_stride,
            });
        }

        let required_len = (stride_bytes as u64) * (size.height as u64);
        if (pixels.len() as u64) < required_len {
            return Err(PresentError::InvalidFramebufferLength {
                expected: required_len as usize,
                actual: pixels.len(),
            });
        }

        self.ensure_source_texture(size)?;
        self.upload_rgba8(pixels, size, stride_bytes);
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
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
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

    fn upload_rgba8(&mut self, pixels: &[u8], size: FramebufferSize, stride_bytes: u32) {
        let queue = self.context.queue();
        let src = self.source.as_ref().expect("source texture must exist");
        self.uploader.write_texture_with_stride(
            queue,
            &src.texture,
            size.width,
            size.height,
            pixels,
            stride_bytes,
        );

        let uniforms = PresentUniforms {
            output_size: [
                self.surface_size.width as f32,
                self.surface_size.height as f32,
            ],
            input_size: [size.width as f32, size.height as f32],
            mode: self.aspect_mode.as_u32(),
            srgb_encode: if self.srgb_encode { 1 } else { 0 },
            _pad: [0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    fn draw(&mut self) -> Result<(), PresentError> {
        if self.surface_size.width == 0 || self.surface_size.height == 0 {
            return Ok(());
        }

        let device = self.context.device();
        let queue = self.context.queue();

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(err) => match surface_acquire_error_action(&err) {
                SurfaceAcquireErrorAction::DropFrame => {
                    tracing::warn!("wgpu surface timeout during present; dropping frame");
                    return Ok(());
                }
                SurfaceAcquireErrorAction::ReconfigureAndRetry => {
                    // Window resize / swap chain invalidation; reconfigure and retry once.
                    self.surface.configure(device, &self.config);
                    match self.surface.get_current_texture() {
                        Ok(frame) => frame,
                        Err(err) => match surface_acquire_error_action(&err) {
                            SurfaceAcquireErrorAction::Fatal => return Err(err.into()),
                            SurfaceAcquireErrorAction::DropFrame
                            | SurfaceAcquireErrorAction::ReconfigureAndRetry => {
                                tracing::warn!(
                                    "wgpu surface acquire failed after reconfigure; dropping frame: {err:?}"
                                );
                                return Ok(());
                            }
                        },
                    }
                }
                SurfaceAcquireErrorAction::Fatal => return Err(err.into()),
            },
        };
        let view_desc = wgpu::TextureViewDescriptor {
            format: if self.surface_view_format == self.config.format {
                None
            } else {
                Some(self.surface_view_format)
            },
            ..Default::default()
        };
        let view = frame.texture.create_view(&view_desc);

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

    pub async fn screenshot_high_level(&self) -> Result<Vec<u8>, PresentError> {
        let Some(src) = self.source.as_ref() else {
            return Ok(Vec::new());
        };

        if src.size.width == 0 || src.size.height == 0 {
            return Ok(Vec::new());
        }

        let device = self.context.device();
        let queue = self.context.queue();

        let bytes_per_pixel = 4u32;
        let unpadded_bpr = src.size.width * bytes_per_pixel;
        let padded_bpr = padded_bytes_per_row(unpadded_bpr);
        let buffer_size = padded_bpr as u64 * src.size.height as u64;

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero presenter screenshot buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero presenter screenshot encoder"),
        });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &src.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(src.size.height),
                },
            },
            wgpu::Extent3d {
                width: src.size.width,
                height: src.size.height,
                depth_or_array_layers: 1,
            },
        );

        queue.submit(Some(encoder.finish()));

        let slice = readback.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            sender.send(res).ok();
        });
        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        device.poll(wgpu::Maintain::Poll);

        match receiver.receive().await {
            Some(Ok(())) => {}
            Some(Err(err)) => return Err(err.into()),
            None => return Err(PresentError::ScreenshotChannelClosed),
        }

        let mapped = slice.get_mapped_range();
        let mut rgba = vec![0u8; (unpadded_bpr as usize) * src.size.height as usize];
        for y in 0..src.size.height as usize {
            let src_off = y * padded_bpr as usize;
            let dst_off = y * unpadded_bpr as usize;
            rgba[dst_off..dst_off + unpadded_bpr as usize]
                .copy_from_slice(&mapped[src_off..src_off + unpadded_bpr as usize]);
        }
        drop(mapped);
        readback.unmap();

        Ok(rgba)
    }
}

fn preferred_surface_format(formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    // Use an explicit preference order so behavior is deterministic even if the
    // backend enumerates formats in different orders.
    for &preferred in [
        wgpu::TextureFormat::Bgra8UnormSrgb,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
    ]
    .iter()
    {
        if formats.contains(&preferred) {
            return preferred;
        }
    }
    formats
        .first()
        .copied()
        .unwrap_or(wgpu::TextureFormat::Bgra8Unorm)
}

fn preferred_surface_config(
    formats: &[wgpu::TextureFormat],
    prefer_srgb_view: bool,
) -> (
    wgpu::TextureFormat,
    wgpu::TextureFormat,
    Vec<wgpu::TextureFormat>,
) {
    let surface_format = preferred_surface_format(formats);

    if prefer_srgb_view && !surface_format.is_srgb() {
        if let Some(srgb_view) = srgb_view_format_for_surface_format(surface_format) {
            return (surface_format, srgb_view, vec![srgb_view]);
        }
    }

    (surface_format, surface_format, Vec::new())
}

fn srgb_view_format_for_surface_format(format: wgpu::TextureFormat) -> Option<wgpu::TextureFormat> {
    match format {
        wgpu::TextureFormat::Bgra8Unorm => Some(wgpu::TextureFormat::Bgra8UnormSrgb),
        wgpu::TextureFormat::Rgba8Unorm => Some(wgpu::TextureFormat::Rgba8UnormSrgb),
        _ => None,
    }
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
    unpadded_bytes_per_row.div_ceil(align) * align
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PresentUniforms {
    output_size: [f32; 2],
    input_size: [f32; 2],
    mode: u32,
    srgb_encode: u32,
    _pad: [u32; 2],
}

const PRESENT_WGSL: &str = r#"
struct Uniforms {
    output_size: vec2<f32>,
    input_size: vec2<f32>,
    mode: u32,
    srgb_encode: u32,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

fn linear_to_srgb_channel(x: f32) -> f32 {
    let xc = max(x, 0.0);
    if (xc <= 0.0031308) {
        return xc * 12.92;
    }
    return 1.055 * pow(xc, 1.0 / 2.4) - 0.055;
}

fn linear_to_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        linear_to_srgb_channel(rgb.r),
        linear_to_srgb_channel(rgb.g),
        linear_to_srgb_channel(rgb.b),
    );
}

fn encode_output(color: vec4<f32>) -> vec4<f32> {
    // Presentation is effectively scanout; keep the output opaque even if the source framebuffer
    // contains alpha.
    let a = 1.0;
    if (u.srgb_encode == 0u) {
        return vec4<f32>(color.rgb, a);
    }
    return vec4<f32>(linear_to_srgb(color.rgb), a);
}

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
    // Stretch: map directly.
    if (u.mode == 0u) {
        let uv = pos.xy / u.output_size;
        return encode_output(textureSample(src_tex, src_samp, uv));
    }

    let dst = u.output_size;
    let src = u.input_size;

    // FitKeepAspect / IntegerScale: compute a centered destination rect.
    var scale = min(dst.x / src.x, dst.y / src.y);
    if (u.mode == 2u) {
        let int_scale = floor(scale);
        if (int_scale >= 1.0) {
            scale = int_scale;
        }
    }

    let scaled = src * scale;
    let offset = (dst - scaled) * 0.5;
    let p = pos.xy - offset;

    if (p.x < 0.0 || p.y < 0.0 || p.x >= scaled.x || p.y >= scaled.y) {
        return encode_output(vec4<f32>(0.0, 0.0, 0.0, 1.0));
    }

    let uv = p / scaled;
    return encode_output(textureSample(src_tex, src_samp, uv));
}
"#;

fn preferred_composite_alpha_mode(modes: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
    if modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        return wgpu::CompositeAlphaMode::Opaque;
    }
    modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Opaque)
}

fn surface_format_requires_manual_srgb_encoding(format: wgpu::TextureFormat) -> bool {
    !format.is_srgb()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn require_webgpu() -> bool {
        let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
            return false;
        };
        let v = raw.trim();
        v == "1"
            || v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
    }

    fn skip_or_panic(test_name: &str, reason: &str) {
        if require_webgpu() {
            panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
        }
        eprintln!("skipping {test_name}: {reason}");
    }

    #[test]
    fn composite_alpha_mode_prefers_opaque() {
        let modes = [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Opaque,
        ];
        assert_eq!(
            preferred_composite_alpha_mode(&modes),
            wgpu::CompositeAlphaMode::Opaque
        );

        let modes = [wgpu::CompositeAlphaMode::PostMultiplied];
        assert_eq!(
            preferred_composite_alpha_mode(&modes),
            wgpu::CompositeAlphaMode::PostMultiplied
        );

        let modes: [wgpu::CompositeAlphaMode; 0] = [];
        assert_eq!(
            preferred_composite_alpha_mode(&modes),
            wgpu::CompositeAlphaMode::Opaque
        );
    }

    #[test]
    fn surface_config_prefers_srgb_format_when_available() {
        let formats = [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ];
        let (surface_format, view_format, view_formats) = preferred_surface_config(&formats, true);
        assert_eq!(surface_format, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(view_format, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert!(view_formats.is_empty());
        assert!(!surface_format_requires_manual_srgb_encoding(view_format));
    }

    #[test]
    fn surface_format_preference_is_deterministic() {
        // BGRA sRGB should win over RGBA sRGB regardless of enumeration order.
        let formats = [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ];
        let chosen = preferred_surface_format(&formats);
        assert_eq!(chosen, wgpu::TextureFormat::Bgra8UnormSrgb);

        // Linear BGRA should win over linear RGBA.
        let formats = [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ];
        let chosen = preferred_surface_format(&formats);
        assert_eq!(chosen, wgpu::TextureFormat::Bgra8Unorm);

        // Empty format list should use a conservative, widely supported default.
        let formats: [wgpu::TextureFormat; 0] = [];
        let chosen = preferred_surface_format(&formats);
        assert_eq!(chosen, wgpu::TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn surface_config_linear_fallback_encodes_when_srgb_view_not_requested() {
        let formats = [wgpu::TextureFormat::Bgra8Unorm];
        let (surface_format, view_format, view_formats) = preferred_surface_config(&formats, false);
        assert_eq!(surface_format, wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(view_format, wgpu::TextureFormat::Bgra8Unorm);
        assert!(view_formats.is_empty());
        assert!(surface_format_requires_manual_srgb_encoding(view_format));
    }

    #[test]
    fn surface_config_uses_srgb_view_on_webgpu_when_available() {
        let formats = [wgpu::TextureFormat::Bgra8Unorm];
        let (surface_format, view_format, view_formats) = preferred_surface_config(&formats, true);
        assert_eq!(surface_format, wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(view_format, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(view_formats, vec![wgpu::TextureFormat::Bgra8UnormSrgb]);
        assert!(!surface_format_requires_manual_srgb_encoding(view_format));

        let formats = [wgpu::TextureFormat::Rgba8Unorm];
        let (surface_format, view_format, view_formats) = preferred_surface_config(&formats, true);
        assert_eq!(surface_format, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(view_format, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert_eq!(view_formats, vec![wgpu::TextureFormat::Rgba8UnormSrgb]);
        assert!(!surface_format_requires_manual_srgb_encoding(view_format));
    }

    #[test]
    fn surface_error_policy_matches_docs() {
        assert_eq!(
            surface_acquire_error_action(&wgpu::SurfaceError::Timeout),
            SurfaceAcquireErrorAction::DropFrame
        );
        assert_eq!(
            surface_acquire_error_action(&wgpu::SurfaceError::Lost),
            SurfaceAcquireErrorAction::ReconfigureAndRetry
        );
        assert_eq!(
            surface_acquire_error_action(&wgpu::SurfaceError::Outdated),
            SurfaceAcquireErrorAction::ReconfigureAndRetry
        );
        assert_eq!(
            surface_acquire_error_action(&wgpu::SurfaceError::OutOfMemory),
            SurfaceAcquireErrorAction::Fatal
        );
    }

    #[test]
    fn present_shader_srgb_encode_flag_controls_output_gamma() {
        pollster::block_on(async {
            let ctx = match crate::WebGpuContext::request_headless(Default::default()).await {
                Ok(ctx) => ctx,
                Err(err) => {
                    skip_or_panic(
                        "present_shader_srgb_encode_flag_controls_output_gamma",
                        &err.to_string(),
                    );
                    return;
                }
            };

            let device = ctx.device();
            let queue = ctx.queue();

            let uniform_min_binding_size =
                wgpu::BufferSize::new(std::mem::size_of::<PresentUniforms>() as u64);
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("present shader test bgl"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: uniform_min_binding_size,
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

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("present shader test shader"),
                source: wgpu::ShaderSource::Wgsl(PRESENT_WGSL.into()),
            });

            let pipeline = crate::pipeline::create_fullscreen_triangle_pipeline(
                device,
                &bind_group_layout,
                &shader,
                "fs_main",
                wgpu::TextureFormat::Rgba8Unorm,
                Some("present shader test pipeline"),
            );

            let pipeline_srgb = crate::pipeline::create_fullscreen_triangle_pipeline(
                device,
                &bind_group_layout,
                &shader,
                "fs_main",
                wgpu::TextureFormat::Rgba8UnormSrgb,
                Some("present shader test pipeline (srgb)"),
            );

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("present shader test sampler"),
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });

            let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("present shader test uniforms"),
                size: std::mem::size_of::<PresentUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let input = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("present shader test input"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
            // `queue.write_texture` requires `bytes_per_row` alignment, so pad the single pixel
            // row out to 256 bytes.
            let mut input_row = [0u8; wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize];
            input_row[..4].copy_from_slice(&[128u8, 0, 0, 255]);
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &input,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                // Mid-gray in linear space.
                &input_row,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );

            let output = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("present shader test output"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

            let output_srgb = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("present shader test output (srgb)"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let output_srgb_view = output_srgb.create_view(&wgpu::TextureViewDescriptor::default());

            let bytes_per_row = padded_bytes_per_row(4);
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("present shader test readback"),
                size: bytes_per_row as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("present shader test bind group"),
                layout: &bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&input_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });

            struct RenderAndReadback<'a> {
                device: &'a wgpu::Device,
                queue: &'a wgpu::Queue,
                bind_group: &'a wgpu::BindGroup,
                uniform_buffer: &'a wgpu::Buffer,
                readback: &'a wgpu::Buffer,
                bytes_per_row: u32,
            }

            impl<'a> RenderAndReadback<'a> {
                async fn render_and_readback(
                    &self,
                    pipeline: &wgpu::RenderPipeline,
                    output: &wgpu::Texture,
                    output_view: &wgpu::TextureView,
                    srgb_encode: bool,
                ) -> [u8; 4] {
                    let uniforms = PresentUniforms {
                        output_size: [1.0, 1.0],
                        input_size: [1.0, 1.0],
                        mode: 0,
                        srgb_encode: if srgb_encode { 1 } else { 0 },
                        _pad: [0; 2],
                    };
                    self.queue
                        .write_buffer(self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

                    let mut encoder =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("present shader test encoder"),
                            });
                    {
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("present shader test pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: output_view,
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
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(0, self.bind_group, &[]);
                        pass.draw(0..3, 0..1);
                    }

                    encoder.copy_texture_to_buffer(
                        wgpu::ImageCopyTexture {
                            texture: output,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::ImageCopyBuffer {
                            buffer: self.readback,
                            layout: wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(self.bytes_per_row),
                                rows_per_image: Some(1),
                            },
                        },
                        wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.queue.submit(Some(encoder.finish()));

                    let slice = self.readback.slice(..);
                    let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
                    slice.map_async(wgpu::MapMode::Read, move |res| {
                        sender.send(res).ok();
                    });
                    #[cfg(not(target_arch = "wasm32"))]
                    self.device.poll(wgpu::Maintain::Wait);
                    #[cfg(target_arch = "wasm32")]
                    self.device.poll(wgpu::Maintain::Poll);
                    receiver.receive().await.unwrap().unwrap();

                    let mapped = slice.get_mapped_range();
                    let out = [mapped[0], mapped[1], mapped[2], mapped[3]];
                    drop(mapped);
                    self.readback.unmap();
                    out
                }
            }

            let renderer = RenderAndReadback {
                device,
                queue,
                bind_group: &bind_group,
                uniform_buffer: &uniform_buffer,
                readback: &readback,
                bytes_per_row,
            };

            let linear = renderer
                .render_and_readback(&pipeline, &output, &output_view, false)
                .await;
            assert_eq!(linear[0], 128, "linear path should preserve input channel");
            assert_eq!(linear[1], 0);
            assert_eq!(linear[2], 0);
            assert_eq!(linear[3], 255, "present shader must force opaque alpha");

            let encoded = renderer
                .render_and_readback(&pipeline, &output, &output_view, true)
                .await;
            assert!(
                (186..=189).contains(&encoded[0]),
                "manual sRGB encode produced unexpected value: {}",
                encoded[0]
            );
            assert_eq!(encoded[1], 0);
            assert_eq!(encoded[2], 0);
            assert_eq!(encoded[3], 255, "present shader must force opaque alpha");
            assert!(
                encoded[0] > linear[0],
                "manual sRGB encoding should increase mid-range values"
            );

            // Rendering to an sRGB render target should encode automatically when the shader does
            // *not* apply manual gamma.
            let srgb_auto = renderer
                .render_and_readback(&pipeline_srgb, &output_srgb, &output_srgb_view, false)
                .await;
            assert!(
                (186..=189).contains(&srgb_auto[0]),
                "sRGB render target encode produced unexpected value: {}",
                srgb_auto[0]
            );
            assert_eq!(srgb_auto[3], 255);

            // And if manual sRGB encoding is (incorrectly) enabled on top of an sRGB target, we
            // should observe double-gamma.
            let srgb_double = renderer
                .render_and_readback(&pipeline_srgb, &output_srgb, &output_srgb_view, true)
                .await;
            assert!(
                (220..=225).contains(&srgb_double[0]),
                "double sRGB encode produced unexpected value: {}",
                srgb_double[0]
            );
            assert_eq!(srgb_double[3], 255);
            assert!(
                srgb_double[0] > srgb_auto[0],
                "double gamma should be brighter than single gamma"
            );
        });
    }
}
