#![forbid(unsafe_code)]
#![cfg_attr(all(target_arch = "wasm32", feature = "wasm-threaded"), feature(thread_local))]

// The full implementation is only meaningful on wasm32.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::RefCell;

    use aero_gpu::shader_lib::{BuiltinShader, wgsl as builtin_wgsl};
    use aero_gpu::{
        AeroGpuCommandProcessor, AeroGpuEvent, FrameTimingsReport, GpuBackendKind, GpuProfiler,
    };
    use futures_intrusive::channel::shared::oneshot_channel;
    use js_sys::{Array, BigInt, Object, Reflect, Uint8Array};
    use wasm_bindgen::prelude::*;
    use web_sys::OffscreenCanvas;

    // wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
    // `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
    // by the linker when there is at least one TLS variable. We keep a tiny TLS
    // slot behind a cargo feature enabled only for the threaded build.
    #[cfg(feature = "wasm-threaded")]
    #[thread_local]
    static TLS_DUMMY: u8 = 0;

    #[wasm_bindgen(start)]
    pub fn wasm_start() {
        #[cfg(feature = "wasm-threaded")]
        {
            // Ensure the TLS dummy is not optimized away.
            let _ = &TLS_DUMMY as *const u8;
        }
    }

    thread_local! {
        static PROCESSOR: RefCell<AeroGpuCommandProcessor> =
            RefCell::new(AeroGpuCommandProcessor::new());
    }

    #[wasm_bindgen]
    pub fn submit_aerogpu(
        cmd_stream: Uint8Array,
        signal_fence: u64,
        alloc_table: Option<Uint8Array>,
    ) -> Result<JsValue, JsValue> {
        // `alloc_table` is reserved for future guest-memory backing support.
        // Keep the parameter so the JS/IPC surface remains stable.
        drop(alloc_table);

        let mut bytes = vec![0u8; cmd_stream.length() as usize];
        cmd_stream.copy_to(&mut bytes);

        let present_count = PROCESSOR.with(|processor| {
            let mut processor = processor.borrow_mut();
            let events = processor
                .process_submission(&bytes, signal_fence)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;

            let mut present_count = 0u64;
            for event in events {
                if matches!(event, AeroGpuEvent::PresentCompleted { .. }) {
                    present_count = present_count.saturating_add(1);
                }
            }

            Ok::<u64, JsValue>(present_count)
        })?;

        let out = Object::new();
        Reflect::set(
            &out,
            &JsValue::from_str("completedFence"),
            &BigInt::from(signal_fence).into(),
        )?;
        Reflect::set(
            &out,
            &JsValue::from_str("presentCount"),
            &BigInt::from(present_count).into(),
        )?;

        Ok(out.into())
    }

    const FLAG_APPLY_SRGB_ENCODE: u32 = 1;
    const FLAG_PREMULTIPLY_ALPHA: u32 = 2;
    const FLAG_FORCE_OPAQUE_ALPHA: u32 = 4;
    const FLAG_FLIP_Y: u32 = 8;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct ViewportTransform {
        scale: [f32; 2],
        offset: [f32; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct BlitParams {
        flags: u32,
        _pad: [u32; 3],
    }

    struct AdapterInfo {
        vendor: Option<String>,
        renderer: Option<String>,
        description: Option<String>,
    }

    struct Presenter {
        backend_kind: GpuBackendKind,
        adapter_info: AdapterInfo,

        canvas: OffscreenCanvas,

        // Keep the `wgpu::Instance` alive for the lifetime of the surface/device.
        #[allow(dead_code)]
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        device: wgpu::Device,
        queue: wgpu::Queue,

        surface_format: wgpu::TextureFormat,
        alpha_mode: wgpu::CompositeAlphaMode,
        config: wgpu::SurfaceConfiguration,

        pipeline: wgpu::RenderPipeline,
        bind_group_layout: wgpu::BindGroupLayout,

        sampler: wgpu::Sampler,
        viewport_buffer: wgpu::Buffer,
        params_buffer: wgpu::Buffer,

        // Framebuffer texture (RGBA8, linear).
        framebuffer_size: (u32, u32),
        framebuffer_texture: wgpu::Texture,
        framebuffer_view: wgpu::TextureView,
        bind_group: wgpu::BindGroup,

        // Screenshot path: render the same blit into a copy-src texture.
        capture_size: (u32, u32),
        capture_texture: Option<wgpu::Texture>,
        capture_view: Option<wgpu::TextureView>,

        // Best-effort timing report (CPU only for now).
        profiler: GpuProfiler,
    }

    impl Presenter {
        async fn new(
            canvas: OffscreenCanvas,
            backend_kind: GpuBackendKind,
            required_features: wgpu::Features,
        ) -> Result<Self, JsValue> {
            let backends = match backend_kind {
                GpuBackendKind::WebGpu => wgpu::Backends::BROWSER_WEBGPU,
                // On wasm32, `wgpu`'s GL backend maps to WebGL2 when the `webgl`
                // feature is enabled.
                GpuBackendKind::WebGl2 => wgpu::Backends::GL,
            };

            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..Default::default()
            });

            let surface = instance
                .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas.clone()))
                .map_err(|err| {
                    JsValue::from_str(&format!("Failed to create wgpu surface: {err:?}"))
                })?;

            let adapter = request_adapter_robust(&instance, &surface)
                .await
                .ok_or_else(|| JsValue::from_str("No suitable GPU adapter found"))?;

            let supported = adapter.features();
            if !supported.contains(required_features) {
                return Err(JsValue::from_str(&format!(
                    "Adapter does not support required features: {required_features:?}"
                )));
            }

            // Keep limits conservative to ensure WebGL2 fallback compatibility.
            let limits = wgpu::Limits::downlevel_webgl2_defaults();

            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("aero-gpu-wasm device"),
                        required_features,
                        required_limits: limits,
                    },
                    None,
                )
                .await
                .map_err(|err| JsValue::from_str(&format!("Failed to request device: {err}")))?;

            let info = adapter.get_info();
            let adapter_info = AdapterInfo {
                // WebGPU doesn't expose stable vendor strings; surface best-effort info.
                vendor: Some(format!("0x{:04x}", info.vendor)),
                renderer: Some(info.name.clone()),
                description: if info.driver_info.is_empty() {
                    None
                } else {
                    Some(info.driver_info.clone())
                },
            };

            let surface_caps = surface.get_capabilities(&adapter);
            let surface_format = choose_surface_format(&surface_caps.formats);
            let alpha_mode = choose_alpha_mode(&surface_caps.alpha_modes);
            let present_mode = choose_present_mode(&surface_caps.present_modes);

            // Initial surface size is taken from the canvas (physical pixels).
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: canvas.width().max(1),
                height: canvas.height().max(1),
                present_mode,
                alpha_mode,
                desired_maximum_frame_latency: 2,
                view_formats: vec![],
            };
            surface.configure(&device, &config);

            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("aero-gpu-wasm.blit.bind_group_layout"),
                    entries: &[
                        // viewport
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        // input texture
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
                        // sampler
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        // params
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("aero-gpu-wasm.blit.pipeline_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aero-gpu-wasm.blit.shader"),
                source: wgpu::ShaderSource::Wgsl(builtin_wgsl(BuiltinShader::Blit).into()),
            });

            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aero-gpu-wasm.blit.pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("aero-gpu-wasm.blit.sampler"),
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });

            let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.blit.viewport_uniform"),
                size: std::mem::size_of::<ViewportTransform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.blit.params_uniform"),
                size: std::mem::size_of::<BlitParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            // Create initial framebuffer texture at canvas size.
            let fb_w = canvas.width().max(1);
            let fb_h = canvas.height().max(1);
            let (framebuffer_texture, framebuffer_view) =
                create_framebuffer_texture(&device, fb_w, fb_h);

            // Default present policy (docs/04-graphics-subsystem.md):
            // - input framebuffer is linear RGBA8 (rgba8unorm)
            // - output is sRGB
            // - alpha is forced opaque
            let mut flags = FLAG_FORCE_OPAQUE_ALPHA;
            if needs_srgb_encode_in_shader(surface_format) {
                flags |= FLAG_APPLY_SRGB_ENCODE;
            }
            // Top-left UV origin is the default for the shared blit shader.
            flags &= !FLAG_FLIP_Y;
            flags &= !FLAG_PREMULTIPLY_ALPHA;

            queue.write_buffer(
                &viewport_buffer,
                0,
                bytemuck::bytes_of(&ViewportTransform {
                    scale: [1.0, 1.0],
                    offset: [0.0, 0.0],
                }),
            );
            queue.write_buffer(
                &params_buffer,
                0,
                bytemuck::bytes_of(&BlitParams {
                    flags,
                    _pad: [0; 3],
                }),
            );

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aero-gpu-wasm.blit.bind_group"),
                layout: &bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: viewport_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&framebuffer_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: params_buffer.as_entire_binding(),
                    },
                ],
            });

            Ok(Self {
                backend_kind,
                adapter_info,
                canvas,
                instance,
                surface,
                device,
                queue,
                surface_format,
                alpha_mode,
                config,
                pipeline,
                bind_group_layout,
                sampler,
                viewport_buffer,
                params_buffer,
                framebuffer_size: (fb_w, fb_h),
                framebuffer_texture,
                framebuffer_view,
                bind_group,
                capture_size: (0, 0),
                capture_texture: None,
                capture_view: None,
                profiler: GpuProfiler::new_cpu_only(backend_kind),
            })
        }

        fn backend_kind_string(&self) -> &'static str {
            match self.backend_kind {
                GpuBackendKind::WebGpu => "webgpu",
                GpuBackendKind::WebGl2 => "webgl2",
            }
        }

        fn set_canvas_size(&mut self, pixel_width: u32, pixel_height: u32) {
            self.canvas.set_width(pixel_width.max(1));
            self.canvas.set_height(pixel_height.max(1));
        }

        fn resize(&mut self, pixel_width: u32, pixel_height: u32) {
            let pixel_width = pixel_width.max(1);
            let pixel_height = pixel_height.max(1);

            self.set_canvas_size(pixel_width, pixel_height);

            self.config.width = pixel_width;
            self.config.height = pixel_height;
            self.surface.configure(&self.device, &self.config);

            if self.framebuffer_size != (pixel_width, pixel_height) {
                let (tex, view) =
                    create_framebuffer_texture(&self.device, pixel_width, pixel_height);
                self.framebuffer_texture = tex;
                self.framebuffer_view = view;
                self.framebuffer_size = (pixel_width, pixel_height);

                self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aero-gpu-wasm.blit.bind_group"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.viewport_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&self.framebuffer_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: self.params_buffer.as_entire_binding(),
                        },
                    ],
                });
            }

            if self.capture_size != (pixel_width, pixel_height) {
                self.capture_size = (pixel_width, pixel_height);
                self.capture_texture = None;
                self.capture_view = None;
            }
        }

        fn upload_rgba8(&self, rgba8: &[u8], width: u32, height: u32) -> Result<(), JsValue> {
            if width == 0 || height == 0 {
                return Ok(());
            }
            let expected_len = width as usize * height as usize * 4;
            if rgba8.len() != expected_len {
                return Err(JsValue::from_str(&format!(
                    "Invalid RGBA8 buffer size: got {}, expected {}",
                    rgba8.len(),
                    expected_len
                )));
            }
            if self.framebuffer_size != (width, height) {
                return Err(JsValue::from_str(
                    "Framebuffer size mismatch; call resize() first",
                ));
            }

            let bytes_per_row = width * 4;
            let padded_bpr = padded_bytes_per_row(bytes_per_row);
            let mut upload = Vec::new();
            if padded_bpr == bytes_per_row {
                upload.extend_from_slice(rgba8);
            } else {
                upload.resize(padded_bpr as usize * height as usize, 0);
                for y in 0..height as usize {
                    let src_off = y * bytes_per_row as usize;
                    let dst_off = y * padded_bpr as usize;
                    upload[dst_off..dst_off + bytes_per_row as usize]
                        .copy_from_slice(&rgba8[src_off..src_off + bytes_per_row as usize]);
                }
            }

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.framebuffer_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &upload,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
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

        fn present(&mut self) -> Result<(), JsValue> {
            self.profiler.begin_frame(None, None);

            let device = &self.device;
            let frame = acquire_surface_frame(&mut self.surface, device, &mut self.config)?;
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu-wasm.present.encoder"),
            });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aero-gpu-wasm.present.pass"),
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
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }

            self.profiler.end_encode(&mut encoder);
            let command_buffer = encoder.finish();
            self.profiler.submit(&self.queue, command_buffer);
            frame.present();
            Ok(())
        }

        async fn screenshot(&mut self) -> Result<Vec<u8>, JsValue> {
            let (width, height) = self.framebuffer_size;
            if width == 0 || height == 0 {
                return Ok(Vec::new());
            }

            if self.capture_texture.is_none() {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("aero-gpu-wasm.screenshot.capture_texture"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.surface_format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.capture_texture = Some(texture);
                self.capture_view = Some(view);
            }

            let capture_view = self
                .capture_view
                .as_ref()
                .expect("capture_view should exist");
            let capture_texture = self
                .capture_texture
                .as_ref()
                .expect("capture_texture should exist");

            let bytes_per_row = width * 4;
            let padded_bpr = padded_bytes_per_row(bytes_per_row);
            let buffer_size = padded_bpr as u64 * height as u64;

            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.screenshot.readback"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aero-gpu-wasm.screenshot.encoder"),
                });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aero-gpu-wasm.screenshot.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: capture_view,
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
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }

            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: capture_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &readback,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bpr),
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

            let slice = readback.slice(..);
            let (sender, receiver) = oneshot_channel();
            slice.map_async(wgpu::MapMode::Read, move |res| {
                sender.send(res).ok();
            });

            // `poll(Maintain::Wait)` is a no-op on WebGPU, but required on native. Calling it is fine.
            self.device.poll(wgpu::Maintain::Wait);

            match receiver.receive().await {
                Some(Ok(())) => {}
                Some(Err(err)) => {
                    return Err(JsValue::from_str(&format!(
                        "Failed to map screenshot buffer: {err}"
                    )));
                }
                None => {
                    return Err(JsValue::from_str(
                        "Screenshot map callback dropped unexpectedly",
                    ));
                }
            }

            let mapped = slice.get_mapped_range();
            let mut out = vec![0u8; (bytes_per_row * height) as usize];
            for y in 0..height as usize {
                let src_off = y * padded_bpr as usize;
                let dst_off = y * bytes_per_row as usize;
                out[dst_off..dst_off + bytes_per_row as usize]
                    .copy_from_slice(&mapped[src_off..src_off + bytes_per_row as usize]);
            }
            drop(mapped);
            readback.unmap();

            if is_bgra(self.surface_format) {
                // Convert BGRA -> RGBA.
                for px in out.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }

            Ok(out)
        }

        fn latest_timings(&self) -> Option<FrameTimingsReport> {
            self.profiler.get_frame_timings()
        }

        fn adapter_info_js(&self) -> JsValue {
            let obj = Object::new();
            if let Some(vendor) = &self.adapter_info.vendor {
                let _ = Reflect::set(
                    &obj,
                    &JsValue::from_str("vendor"),
                    &JsValue::from_str(vendor),
                );
            }
            if let Some(renderer) = &self.adapter_info.renderer {
                let _ = Reflect::set(
                    &obj,
                    &JsValue::from_str("renderer"),
                    &JsValue::from_str(renderer),
                );
            }
            if let Some(description) = &self.adapter_info.description {
                let _ = Reflect::set(
                    &obj,
                    &JsValue::from_str("description"),
                    &JsValue::from_str(description),
                );
            }
            obj.into()
        }

        fn capabilities_js(&self, css_width: u32, css_height: u32, dpr: f64) -> JsValue {
            let pixel_width = self.canvas.width();
            let pixel_height = self.canvas.height();

            let obj = Object::new();
            let _ = Reflect::set(&obj, &JsValue::from_str("initialized"), &JsValue::TRUE);
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("backend"),
                &JsValue::from_str(self.backend_kind_string()),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("surfaceFormat"),
                &JsValue::from_str(&format!("{:?}", self.surface_format)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("alphaMode"),
                &JsValue::from_str(&format!("{:?}", self.alpha_mode)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("cssSize"),
                &size_obj(css_width, css_height),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("devicePixelRatio"),
                &JsValue::from_f64(dpr),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("pixelSize"),
                &size_obj(pixel_width, pixel_height),
            );
            obj.into()
        }
    }

    fn create_framebuffer_texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-gpu-wasm.framebuffer_rgba8"),
            size: wgpu::Extent3d {
                width,
                height,
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
        (texture, view)
    }

    fn padded_bytes_per_row(bytes_per_row: u32) -> u32 {
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        ((bytes_per_row + align - 1) / align) * align
    }

    fn is_bgra(format: wgpu::TextureFormat) -> bool {
        matches!(
            format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        )
    }

    fn needs_srgb_encode_in_shader(format: wgpu::TextureFormat) -> bool {
        // If the surface format is already sRGB, the GPU will encode automatically.
        !matches!(
            format,
            wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb
        )
    }

    fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
        // Prefer an sRGB surface format (docs/04-graphics-subsystem.md).
        for &fmt in formats {
            if matches!(
                fmt,
                wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb
            ) {
                return fmt;
            }
        }
        formats
            .first()
            .copied()
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm)
    }

    fn choose_alpha_mode(modes: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
        // Default to opaque output.
        if modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            return wgpu::CompositeAlphaMode::Opaque;
        }
        modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Opaque)
    }

    fn choose_present_mode(modes: &[wgpu::PresentMode]) -> wgpu::PresentMode {
        if modes.contains(&wgpu::PresentMode::Fifo) {
            return wgpu::PresentMode::Fifo;
        }
        modes.first().copied().unwrap_or(wgpu::PresentMode::Fifo)
    }

    async fn request_adapter_robust(
        instance: &wgpu::Instance,
        surface: &wgpu::Surface<'_>,
    ) -> Option<wgpu::Adapter> {
        for (power, fallback) in [
            (wgpu::PowerPreference::HighPerformance, false),
            (wgpu::PowerPreference::LowPower, false),
            (wgpu::PowerPreference::LowPower, true),
        ] {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: power,
                    compatible_surface: Some(surface),
                    force_fallback_adapter: fallback,
                })
                .await;
            if adapter.is_some() {
                return adapter;
            }
        }
        None
    }

    fn acquire_surface_frame(
        surface: &mut wgpu::Surface<'static>,
        device: &wgpu::Device,
        config: &mut wgpu::SurfaceConfiguration,
    ) -> Result<wgpu::SurfaceTexture, JsValue> {
        match surface.get_current_texture() {
            Ok(frame) => Ok(frame),
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                // Reconfigure and retry once (docs/04-graphics-subsystem.md).
                surface.configure(device, config);
                surface.get_current_texture().map_err(|err| {
                    JsValue::from_str(&format!(
                        "Surface acquire failed after reconfigure: {err:?}"
                    ))
                })
            }
            Err(wgpu::SurfaceError::Timeout) => Err(JsValue::from_str("Surface acquire timeout")),
            Err(wgpu::SurfaceError::OutOfMemory) => Err(JsValue::from_str("Surface out of memory")),
        }
    }

    fn size_obj(width: u32, height: u32) -> JsValue {
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("width"),
            &JsValue::from_f64(width as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("height"),
            &JsValue::from_f64(height as f64),
        );
        obj.into()
    }

    struct GpuState {
        css_width: u32,
        css_height: u32,
        device_pixel_ratio: f64,
        presenter: Presenter,
    }

    thread_local! {
        static STATE: RefCell<Option<GpuState>> = RefCell::new(None);
    }

    fn with_state<T>(f: impl FnOnce(&GpuState) -> Result<T, JsValue>) -> Result<T, JsValue> {
        STATE.with(|state| match state.borrow().as_ref() {
            Some(s) => f(s),
            None => Err(JsValue::from_str("GPU backend not initialized.")),
        })
    }

    fn with_state_mut<T>(
        f: impl FnOnce(&mut GpuState) -> Result<T, JsValue>,
    ) -> Result<T, JsValue> {
        STATE.with(|state| match state.borrow_mut().as_mut() {
            Some(s) => f(s),
            None => Err(JsValue::from_str("GPU backend not initialized.")),
        })
    }

    fn parse_bool(obj: &JsValue, key: &str) -> Option<bool> {
        if obj.is_undefined() || obj.is_null() {
            return None;
        }
        let value = Reflect::get(obj, &JsValue::from_str(key)).ok()?;
        if value.is_undefined() || value.is_null() {
            return None;
        }
        value.as_bool()
    }

    fn parse_required_features(obj: &JsValue) -> Result<wgpu::Features, JsValue> {
        if obj.is_undefined() || obj.is_null() {
            return Ok(wgpu::Features::empty());
        }
        let value =
            Reflect::get(obj, &JsValue::from_str("requiredFeatures")).unwrap_or(JsValue::UNDEFINED);
        if value.is_undefined() || value.is_null() {
            return Ok(wgpu::Features::empty());
        }
        if !Array::is_array(&value) {
            return Err(JsValue::from_str(
                "GpuWorkerInitOptions.requiredFeatures must be an array of strings",
            ));
        }
        let arr: Array = value.unchecked_into();
        let mut out = wgpu::Features::empty();
        for entry in arr.iter() {
            let Some(name) = entry.as_string() else {
                return Err(JsValue::from_str(
                    "GpuWorkerInitOptions.requiredFeatures must contain only strings",
                ));
            };
            out |= map_webgpu_feature(&name)?;
        }
        Ok(out)
    }

    fn map_webgpu_feature(name: &str) -> Result<wgpu::Features, JsValue> {
        match name {
            "texture-compression-bc" => Ok(wgpu::Features::TEXTURE_COMPRESSION_BC),
            "texture-compression-etc2" => Ok(wgpu::Features::TEXTURE_COMPRESSION_ETC2),
            "timestamp-query" => {
                Ok(wgpu::Features::TIMESTAMP_QUERY
                    | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
            }
            other => Err(JsValue::from_str(&format!(
                "Unsupported WebGPU feature: {other}"
            ))),
        }
    }

    fn clamp_pixel_size(css: u32, dpr: f64) -> u32 {
        let ratio = if dpr.is_finite() && dpr > 0.0 {
            dpr
        } else {
            1.0
        };
        ((css as f64) * ratio).round().max(1.0) as u32
    }

    fn make_test_pattern(width: u32, height: u32) -> Vec<u8> {
        let half_w = width / 2;
        let half_h = height / 2;
        let mut rgba = vec![0u8; width as usize * height as usize * 4];

        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                let left = x < half_w;
                let top = y < half_h;

                // Top-left origin:
                // - top-left: red
                // - top-right: green
                // - bottom-left: blue
                // - bottom-right: white
                let (r, g, b) = match (top, left) {
                    (true, true) => (255, 0, 0),
                    (true, false) => (0, 255, 0),
                    (false, true) => (0, 0, 255),
                    (false, false) => (255, 255, 255),
                };

                rgba[i] = r;
                rgba[i + 1] = g;
                rgba[i + 2] = b;
                rgba[i + 3] = 255;
            }
        }
        rgba
    }

    fn timings_to_js(report: &FrameTimingsReport) -> JsValue {
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("frame_index"),
            &JsValue::from_f64(report.frame_index as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("backend"),
            &JsValue::from_str(match report.backend {
                GpuBackendKind::WebGpu => "webgpu",
                GpuBackendKind::WebGl2 => "webgl2",
            }),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("cpu_encode_us"),
            &JsValue::from_f64(report.cpu_encode_us as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("cpu_submit_us"),
            &JsValue::from_f64(report.cpu_submit_us as f64),
        );
        if let Some(gpu_us) = report.gpu_us {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("gpu_us"),
                &JsValue::from_f64(gpu_us as f64),
            );
        }
        obj.into()
    }

    #[wasm_bindgen]
    pub async fn init_gpu(
        offscreen_canvas: OffscreenCanvas,
        width: u32,
        height: u32,
        dpr: f64,
        options: Option<JsValue>,
    ) -> Result<(), JsValue> {
        let options = options.unwrap_or(JsValue::UNDEFINED);

        // Align default behavior with the TS runtime worker: try WebGPU unless explicitly
        // opted out (preferWebGpu === false) or disableWebGpu === true.
        let prefer_webgpu = parse_bool(&options, "preferWebGpu").unwrap_or(true);
        let disable_webgpu = parse_bool(&options, "disableWebGpu").unwrap_or(false);

        let css_width = width.max(1);
        let css_height = height.max(1);
        let device_pixel_ratio = if dpr.is_finite() && dpr > 0.0 {
            dpr
        } else {
            1.0
        };

        let pixel_width = clamp_pixel_size(css_width, device_pixel_ratio);
        let pixel_height = clamp_pixel_size(css_height, device_pixel_ratio);
        offscreen_canvas.set_width(pixel_width);
        offscreen_canvas.set_height(pixel_height);

        let backends = if disable_webgpu {
            vec![GpuBackendKind::WebGl2]
        } else if prefer_webgpu {
            vec![GpuBackendKind::WebGpu, GpuBackendKind::WebGl2]
        } else {
            vec![GpuBackendKind::WebGl2, GpuBackendKind::WebGpu]
        };

        let mut last_err: Option<JsValue> = None;
        for backend_kind in backends {
            // Required WebGPU features are only meaningful for the WebGPU path. When
            // falling back to WebGL2, ignore them.
            let required_features = match backend_kind {
                GpuBackendKind::WebGpu => parse_required_features(&options)?,
                GpuBackendKind::WebGl2 => wgpu::Features::empty(),
            };

            match Presenter::new(offscreen_canvas.clone(), backend_kind, required_features).await {
                Ok(mut presenter) => {
                    presenter.resize(pixel_width, pixel_height);

                    let state = GpuState {
                        css_width,
                        css_height,
                        device_pixel_ratio,
                        presenter,
                    };

                    STATE.with(|slot| {
                        *slot.borrow_mut() = Some(state);
                    });

                    return Ok(());
                }
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            JsValue::from_str("No supported GPU backend could be initialized.")
        }))
    }

    #[wasm_bindgen]
    pub fn resize(width: u32, height: u32, dpr: f64) -> Result<(), JsValue> {
        with_state_mut(|state| {
            state.css_width = width.max(1);
            state.css_height = height.max(1);
            state.device_pixel_ratio = if dpr.is_finite() && dpr > 0.0 {
                dpr
            } else {
                1.0
            };

            let pixel_width = clamp_pixel_size(state.css_width, state.device_pixel_ratio);
            let pixel_height = clamp_pixel_size(state.css_height, state.device_pixel_ratio);
            state.presenter.resize(pixel_width, pixel_height);
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn backend_kind() -> Result<String, JsValue> {
        with_state(|state| Ok(state.presenter.backend_kind_string().to_string()))
    }

    #[wasm_bindgen]
    pub fn adapter_info() -> Result<JsValue, JsValue> {
        with_state(|state| Ok(state.presenter.adapter_info_js()))
    }

    #[wasm_bindgen]
    pub fn capabilities() -> Result<JsValue, JsValue> {
        with_state(|state| {
            Ok(state.presenter.capabilities_js(
                state.css_width,
                state.css_height,
                state.device_pixel_ratio,
            ))
        })
    }

    #[wasm_bindgen]
    pub fn present_test_pattern() -> Result<(), JsValue> {
        with_state_mut(|state| {
            let (w, h) = state.presenter.framebuffer_size;
            let rgba = make_test_pattern(w, h);
            state.presenter.upload_rgba8(&rgba, w, h)?;
            state.presenter.present()
        })
    }

    #[wasm_bindgen]
    pub async fn request_screenshot() -> Result<Uint8Array, JsValue> {
        let mut state = STATE
            .with(|slot| slot.borrow_mut().take())
            .ok_or_else(|| JsValue::from_str("GPU backend not initialized."))?;

        let result = state.presenter.screenshot().await;

        // Restore state regardless of whether screenshot succeeds.
        STATE.with(|slot| {
            *slot.borrow_mut() = Some(state);
        });

        let bytes = result?;
        Ok(Uint8Array::from(bytes.as_slice()))
    }

    #[wasm_bindgen]
    pub fn get_frame_timings() -> Result<JsValue, JsValue> {
        with_state(|state| match state.presenter.latest_timings() {
            Some(report) => Ok(timings_to_js(&report)),
            None => Ok(JsValue::NULL),
        })
    }
}

// Re-export wasm bindings so the crate's public surface is identical across
// `crate::` and `crate::wasm::` paths.
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
