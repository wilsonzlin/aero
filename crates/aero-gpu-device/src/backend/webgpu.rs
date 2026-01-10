use std::collections::HashMap;
use std::sync::mpsc;

use aero_webgpu::{WebGpuContext, WebGpuInitError, WebGpuInitOptions};

use crate::abi::{buffer_usage, pipeline, texture_usage, TextureFormat};
use crate::backend::{BackendError, GpuBackend, PresentedFrame, Viewport};

#[derive(Debug)]
struct Buffer {
    size_bytes: u64,
    usage: u32,
    buf: wgpu::Buffer,
}

#[derive(Debug)]
struct Texture2d {
    width: u32,
    height: u32,
    format: TextureFormat,
    usage: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Clone, Copy, Debug, Default)]
struct VertexBufferBinding {
    buffer_id: u32,
    offset: u64,
    stride: u32,
}

#[derive(Clone, Copy, Debug)]
struct PendingDraw {
    vertex_count: u32,
    first_vertex: u32,
    pipeline_id: u32,
    vertex: VertexBufferBinding,
    viewport: Option<Viewport>,
}

/// WebGPU (`wgpu`) backend implementation.
///
/// This is primarily intended for host-side tests and tooling. The interface is synchronous
/// because the ABI completion mechanism expects commands to be fully processed before a
/// completion entry is posted.
pub struct WebGpuBackend {
    ctx: WebGpuContext,

    buffers: HashMap<u32, Buffer>,
    textures: HashMap<u32, Texture2d>,

    render_target: Option<u32>,
    viewport: Option<Viewport>,
    pipeline_id: u32,
    pipeline_basic: Option<wgpu::RenderPipeline>,
    vertex_buffer: Option<VertexBufferBinding>,

    pending_clear: Option<[f32; 4]>,
    pending_draws: Vec<PendingDraw>,

    presented: Option<PresentedFrame>,
}

impl WebGpuBackend {
    pub async fn request_headless(options: WebGpuInitOptions) -> Result<Self, WebGpuInitError> {
        let ctx = WebGpuContext::request_headless(options).await?;
        Ok(Self::from_context(ctx))
    }

    pub fn from_context(ctx: WebGpuContext) -> Self {
        Self {
            ctx,
            buffers: HashMap::new(),
            textures: HashMap::new(),
            render_target: None,
            viewport: None,
            pipeline_id: 0,
            pipeline_basic: None,
            vertex_buffer: None,
            pending_clear: None,
            pending_draws: Vec::new(),
            presented: None,
        }
    }

    fn device(&self) -> &wgpu::Device {
        self.ctx.device()
    }

    fn queue(&self) -> &wgpu::Queue {
        self.ctx.queue()
    }

    fn ensure_basic_pipeline(&mut self, format: wgpu::TextureFormat) -> Result<(), BackendError> {
        if self.pipeline_basic.is_some() {
            return Ok(());
        }

        let shader = self
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aero-gpu-device basic vertex-color shader"),
                source: wgpu::ShaderSource::Wgsl(BASIC_VERTEX_COLOR_WGSL.into()),
            });

        let layout = self
            .device()
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("aero-gpu-device basic pipeline layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        const ATTRS: [wgpu::VertexAttribute; 2] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 8,
                shader_location: 1,
            },
        ];

        let pipeline = self
            .device()
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aero-gpu-device basic vertex-color pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: 24,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &ATTRS,
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        self.pipeline_basic = Some(pipeline);
        Ok(())
    }

    fn flush_pending(&mut self, texture_id: u32) -> Result<PresentedFrame, BackendError> {
        let (width, height, format, usage) = {
            let texture = self
                .textures
                .get(&texture_id)
                .ok_or(BackendError::InvalidResource)?;
            (texture.width, texture.height, texture.format, texture.usage)
        };

        if (usage & texture_usage::RENDER_ATTACHMENT) == 0 {
            return Err(BackendError::InvalidState(
                "present texture missing RENDER_ATTACHMENT usage",
            ));
        }
        if (usage & texture_usage::TRANSFER_SRC) == 0 {
            return Err(BackendError::InvalidState(
                "present texture missing TRANSFER_SRC usage",
            ));
        }

        let format = match format {
            TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        };

        let pending_draws = std::mem::take(&mut self.pending_draws);
        let clear = self.pending_clear.take().unwrap_or([0.0, 0.0, 0.0, 0.0]);

        // Encode render commands + copy-to-buffer in one submission.
        let bytes_per_row = align_up(
            width as usize * 4,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize,
        ) as u32;
        let readback_size = (bytes_per_row as u64)
            .checked_mul(height as u64)
            .ok_or(BackendError::OutOfBounds)?;

        let readback = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-gpu-device present readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu-device present encoder"),
            });

        {
            let load = if pending_draws.is_empty() && clear == [0.0, 0.0, 0.0, 0.0] {
                // No explicit clear/draw; keep contents.
                wgpu::LoadOp::Load
            } else {
                wgpu::LoadOp::Clear(wgpu::Color {
                    r: clear[0] as f64,
                    g: clear[1] as f64,
                    b: clear[2] as f64,
                    a: clear[3] as f64,
                })
            };

            self.ensure_basic_pipeline(format)?;
            let pipeline = self
                .pipeline_basic
                .as_ref()
                .ok_or(BackendError::Internal("missing pipeline after creation"))?;
            let texture = self
                .textures
                .get(&texture_id)
                .ok_or(BackendError::InvalidResource)?;

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aero-gpu-device pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            let mut bound_vb: Option<(u32, u64)> = None;

            for draw in pending_draws {
                if draw.pipeline_id != pipeline::BASIC_VERTEX_COLOR {
                    return Err(BackendError::Unsupported);
                }
                if draw.vertex.stride != 24 {
                    return Err(BackendError::Unsupported);
                }

                let buf = self
                    .buffers
                    .get(&draw.vertex.buffer_id)
                    .ok_or(BackendError::InvalidResource)?;

                // Validate vertex buffer bounds for this draw.
                let stride = u64::from(draw.vertex.stride);
                let end_vertex = draw
                    .first_vertex
                    .checked_add(draw.vertex_count)
                    .ok_or(BackendError::OutOfBounds)?;
                let required = stride
                    .checked_mul(end_vertex as u64)
                    .and_then(|v| v.checked_add(draw.vertex.offset))
                    .ok_or(BackendError::OutOfBounds)?;
                if required > buf.size_bytes {
                    return Err(BackendError::OutOfBounds);
                }

                pass.set_pipeline(pipeline);
                if bound_vb != Some((draw.vertex.buffer_id, draw.vertex.offset)) {
                    pass.set_vertex_buffer(0, buf.buf.slice(draw.vertex.offset..));
                    bound_vb = Some((draw.vertex.buffer_id, draw.vertex.offset));
                }

                if let Some(vp) = draw.viewport {
                    pass.set_viewport(vp.x, vp.y, vp.width, vp.height, 0.0, 1.0);
                }

                pass.draw(
                    draw.first_vertex..(draw.first_vertex + draw.vertex_count),
                    0..1,
                );
            }

            // Drop pass before issuing the copy.
            drop(pass);

            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &texture.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &readback,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }

        self.queue().submit(Some(encoder.finish()));

        // Map and wait.
        let slice = readback.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            tx.send(res).ok();
        });
        self.device().poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|_| BackendError::Internal("readback channel closed"))?
            .map_err(|_| BackendError::Internal("readback map failed"))?;

        let mapped = slice.get_mapped_range();
        let mut rgba8 = vec![0u8; (width * height * 4) as usize];
        for row in 0..height as usize {
            let src_start = row * bytes_per_row as usize;
            let dst_start = row * width as usize * 4;
            rgba8[dst_start..dst_start + width as usize * 4]
                .copy_from_slice(&mapped[src_start..src_start + width as usize * 4]);
        }
        drop(mapped);
        readback.unmap();

        Ok(PresentedFrame {
            width,
            height,
            rgba8,
        })
    }
}

impl GpuBackend for WebGpuBackend {
    fn create_buffer(&mut self, id: u32, size_bytes: u64, usage: u32) -> Result<(), BackendError> {
        if id == 0 || self.buffers.contains_key(&id) {
            return Err(BackendError::InvalidResource);
        }
        if (usage & buffer_usage::TRANSFER_DST) == 0 {
            // We rely on `queue.write_buffer` for uploads.
            return Err(BackendError::InvalidState(
                "buffer missing TRANSFER_DST usage",
            ));
        }

        let mut u = wgpu::BufferUsages::COPY_DST;
        if (usage & buffer_usage::TRANSFER_SRC) != 0 {
            u |= wgpu::BufferUsages::COPY_SRC;
        }
        if (usage & buffer_usage::VERTEX) != 0 {
            u |= wgpu::BufferUsages::VERTEX;
        }
        if (usage & buffer_usage::INDEX) != 0 {
            u |= wgpu::BufferUsages::INDEX;
        }
        if (usage & buffer_usage::UNIFORM) != 0 {
            u |= wgpu::BufferUsages::UNIFORM;
        }

        let buf = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-gpu-device buffer"),
            size: size_bytes,
            usage: u,
            mapped_at_creation: false,
        });

        self.buffers.insert(
            id,
            Buffer {
                size_bytes,
                usage,
                buf,
            },
        );
        Ok(())
    }

    fn destroy_buffer(&mut self, id: u32) -> Result<(), BackendError> {
        self.buffers
            .remove(&id)
            .ok_or(BackendError::InvalidResource)?;
        if let Some(binding) = self.vertex_buffer {
            if binding.buffer_id == id {
                self.vertex_buffer = None;
            }
        }
        Ok(())
    }

    fn write_buffer(&mut self, id: u32, dst_offset: u64, data: &[u8]) -> Result<(), BackendError> {
        let buf = self.buffers.get(&id).ok_or(BackendError::InvalidResource)?;
        if (buf.usage & buffer_usage::TRANSFER_DST) == 0 {
            return Err(BackendError::InvalidState(
                "buffer missing TRANSFER_DST usage",
            ));
        }
        let end = dst_offset
            .checked_add(data.len() as u64)
            .ok_or(BackendError::OutOfBounds)?;
        if end > buf.size_bytes {
            return Err(BackendError::OutOfBounds);
        }
        self.queue().write_buffer(&buf.buf, dst_offset, data);
        Ok(())
    }

    fn read_buffer(
        &self,
        id: u32,
        src_offset: u64,
        size_bytes: usize,
    ) -> Result<Vec<u8>, BackendError> {
        let buf = self.buffers.get(&id).ok_or(BackendError::InvalidResource)?;
        if (buf.usage & buffer_usage::TRANSFER_SRC) == 0 {
            return Err(BackendError::InvalidState(
                "buffer missing TRANSFER_SRC usage",
            ));
        }
        let end = src_offset
            .checked_add(size_bytes as u64)
            .ok_or(BackendError::OutOfBounds)?;
        if end > buf.size_bytes {
            return Err(BackendError::OutOfBounds);
        }

        let readback = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-gpu-device buffer readback"),
            size: size_bytes as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu-device buffer readback encoder"),
            });
        encoder.copy_buffer_to_buffer(&buf.buf, src_offset, &readback, 0, size_bytes as u64);
        self.queue().submit(Some(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            tx.send(res).ok();
        });
        self.device().poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|_| BackendError::Internal("readback channel closed"))?
            .map_err(|_| BackendError::Internal("readback map failed"))?;

        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        readback.unmap();
        Ok(out)
    }

    fn create_texture2d(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        format: u32,
        usage: u32,
    ) -> Result<(), BackendError> {
        if id == 0 || width == 0 || height == 0 || self.textures.contains_key(&id) {
            return Err(BackendError::InvalidResource);
        }
        let format = TextureFormat::from_u32(format).ok_or(BackendError::Unsupported)?;
        if (usage & texture_usage::RENDER_ATTACHMENT) == 0 {
            // MVP focuses on render targets + present.
            return Err(BackendError::InvalidState(
                "texture missing RENDER_ATTACHMENT usage",
            ));
        }

        let mut u = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if (usage & texture_usage::TRANSFER_DST) != 0 {
            u |= wgpu::TextureUsages::COPY_DST;
        }
        if (usage & texture_usage::TRANSFER_SRC) != 0 {
            u |= wgpu::TextureUsages::COPY_SRC;
        }
        if (usage & texture_usage::TEXTURE_BINDING) != 0 {
            u |= wgpu::TextureUsages::TEXTURE_BINDING;
        }

        let tex_format = match format {
            TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        };

        let texture = self.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-gpu-device texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: tex_format,
            usage: u,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.textures.insert(
            id,
            Texture2d {
                width,
                height,
                format,
                usage,
                texture,
                view,
            },
        );
        Ok(())
    }

    fn destroy_texture(&mut self, id: u32) -> Result<(), BackendError> {
        self.textures
            .remove(&id)
            .ok_or(BackendError::InvalidResource)?;
        if self.render_target == Some(id) {
            self.render_target = None;
        }
        Ok(())
    }

    fn write_texture2d(
        &mut self,
        id: u32,
        mip_level: u32,
        width: u32,
        height: u32,
        bytes_per_row: u32,
        data: &[u8],
    ) -> Result<(), BackendError> {
        if mip_level != 0 {
            return Err(BackendError::Unsupported);
        }
        let tex = self
            .textures
            .get(&id)
            .ok_or(BackendError::InvalidResource)?;
        if (tex.usage & texture_usage::TRANSFER_DST) == 0 {
            return Err(BackendError::InvalidState(
                "texture missing TRANSFER_DST usage",
            ));
        }
        if width != tex.width || height != tex.height {
            return Err(BackendError::Unsupported);
        }
        let min_bpr = width.checked_mul(4).ok_or(BackendError::OutOfBounds)?;
        if bytes_per_row < min_bpr {
            return Err(BackendError::OutOfBounds);
        }
        let expected = (bytes_per_row as usize)
            .checked_mul(height as usize)
            .ok_or(BackendError::OutOfBounds)?;
        if data.len() < expected {
            return Err(BackendError::OutOfBounds);
        }

        let padded_bpr = align_up(
            min_bpr as usize,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize,
        ) as u32;
        let upload_bytes = if padded_bpr == bytes_per_row {
            data[..expected].to_vec()
        } else {
            let mut out = vec![0u8; padded_bpr as usize * height as usize];
            for row in 0..height as usize {
                let src_start = row * bytes_per_row as usize;
                let dst_start = row * padded_bpr as usize;
                out[dst_start..dst_start + min_bpr as usize]
                    .copy_from_slice(&data[src_start..src_start + min_bpr as usize]);
            }
            out
        };

        self.queue().write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &upload_bytes,
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

    fn read_texture2d(
        &self,
        id: u32,
        mip_level: u32,
        width: u32,
        height: u32,
        bytes_per_row: u32,
    ) -> Result<Vec<u8>, BackendError> {
        if mip_level != 0 {
            return Err(BackendError::Unsupported);
        }
        let tex = self
            .textures
            .get(&id)
            .ok_or(BackendError::InvalidResource)?;
        if (tex.usage & texture_usage::TRANSFER_SRC) == 0 {
            return Err(BackendError::InvalidState(
                "texture missing TRANSFER_SRC usage",
            ));
        }
        if width != tex.width || height != tex.height {
            return Err(BackendError::Unsupported);
        }
        let min_bpr = width.checked_mul(4).ok_or(BackendError::OutOfBounds)?;
        if bytes_per_row < min_bpr {
            return Err(BackendError::OutOfBounds);
        }

        let padded_bpr = align_up(
            min_bpr as usize,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize,
        ) as u32;
        let readback_size = (padded_bpr as u64)
            .checked_mul(height as u64)
            .ok_or(BackendError::OutOfBounds)?;
        let readback = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-gpu-device texture readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu-device texture readback encoder"),
            });

        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &tex.texture,
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
        self.queue().submit(Some(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            tx.send(res).ok();
        });
        self.device().poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|_| BackendError::Internal("readback channel closed"))?
            .map_err(|_| BackendError::Internal("readback map failed"))?;

        let mapped = slice.get_mapped_range();
        let mut out = vec![0u8; bytes_per_row as usize * height as usize];
        for row in 0..height as usize {
            let src_start = row * padded_bpr as usize;
            let dst_start = row * bytes_per_row as usize;
            out[dst_start..dst_start + min_bpr as usize]
                .copy_from_slice(&mapped[src_start..src_start + min_bpr as usize]);
        }
        drop(mapped);
        readback.unmap();
        Ok(out)
    }

    fn set_render_target(&mut self, texture_id: u32) -> Result<(), BackendError> {
        if !self.textures.contains_key(&texture_id) {
            return Err(BackendError::InvalidResource);
        }
        self.render_target = Some(texture_id);
        Ok(())
    }

    fn clear(&mut self, rgba: [f32; 4]) -> Result<(), BackendError> {
        if self.render_target.is_none() {
            return Err(BackendError::InvalidState("no render target"));
        }
        self.pending_clear = Some(rgba);
        Ok(())
    }

    fn set_viewport(&mut self, viewport: Viewport) -> Result<(), BackendError> {
        self.viewport = Some(viewport);
        Ok(())
    }

    fn set_pipeline(&mut self, pipeline_id: u32) -> Result<(), BackendError> {
        if pipeline_id != pipeline::BASIC_VERTEX_COLOR {
            return Err(BackendError::Unsupported);
        }
        self.pipeline_id = pipeline_id;
        Ok(())
    }

    fn set_vertex_buffer(
        &mut self,
        buffer_id: u32,
        offset: u64,
        stride: u32,
    ) -> Result<(), BackendError> {
        if stride != 24 {
            return Err(BackendError::Unsupported);
        }
        if !self.buffers.contains_key(&buffer_id) {
            return Err(BackendError::InvalidResource);
        }
        self.vertex_buffer = Some(VertexBufferBinding {
            buffer_id,
            offset,
            stride,
        });
        Ok(())
    }

    fn draw(&mut self, vertex_count: u32, first_vertex: u32) -> Result<(), BackendError> {
        if self.render_target.is_none() {
            return Err(BackendError::InvalidState("no render target"));
        }
        if self.pipeline_id != pipeline::BASIC_VERTEX_COLOR {
            return Err(BackendError::InvalidState("pipeline not set"));
        }
        let vertex = self
            .vertex_buffer
            .ok_or(BackendError::InvalidState("vertex buffer not set"))?;

        self.pending_draws.push(PendingDraw {
            vertex_count,
            first_vertex,
            pipeline_id: self.pipeline_id,
            vertex,
            viewport: self.viewport,
        });
        Ok(())
    }

    fn present(&mut self, texture_id: u32) -> Result<(), BackendError> {
        let Some(rt) = self.render_target else {
            return Err(BackendError::InvalidState("no render target"));
        };
        if rt != texture_id {
            return Err(BackendError::InvalidState(
                "present texture != current render target",
            ));
        }

        let frame = self.flush_pending(texture_id)?;
        self.presented = Some(frame);
        Ok(())
    }

    fn take_presented_frame(&mut self) -> Option<PresentedFrame> {
        self.presented.take()
    }
}

fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}

const BASIC_VERTEX_COLOR_WGSL: &str = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.position = vec4<f32>(in.pos, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> {
    return color;
}
"#;
