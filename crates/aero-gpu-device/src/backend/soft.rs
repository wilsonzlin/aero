use std::collections::HashMap;

use crate::abi::{pipeline, TextureFormat};
use crate::backend::{BackendError, GpuBackend, PresentedFrame, Viewport};

#[derive(Clone, Debug)]
struct Texture2d {
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
struct VertexBufferBinding {
    buffer_id: u32,
    offset: u64,
    stride: u32,
}

/// Deterministic software backend used for host-side tests.
#[derive(Debug, Default)]
pub struct SoftGpuBackend {
    buffers: HashMap<u32, Vec<u8>>,
    textures: HashMap<u32, Texture2d>,

    render_target: Option<u32>,
    viewport: Option<Viewport>,

    pipeline_id: u32,
    vertex_buffer: Option<VertexBufferBinding>,

    presented: Option<PresentedFrame>,
}

impl SoftGpuBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn rt_mut(&mut self) -> Result<&mut Texture2d, BackendError> {
        let rt_id = self
            .render_target
            .ok_or(BackendError::InvalidState("no render target"))?;
        self.textures
            .get_mut(&rt_id)
            .ok_or(BackendError::InvalidResource)
    }

    fn effective_viewport(&self, rt: &Texture2d) -> Viewport {
        match self.viewport {
            Some(v) if v.width > 0.0 && v.height > 0.0 => v,
            _ => Viewport {
                x: 0.0,
                y: 0.0,
                width: rt.width as f32,
                height: rt.height as f32,
            },
        }
    }
}

impl GpuBackend for SoftGpuBackend {
    fn create_buffer(&mut self, id: u32, size_bytes: u64, _usage: u32) -> Result<(), BackendError> {
        if id == 0 {
            return Err(BackendError::InvalidResource);
        }
        if self.buffers.contains_key(&id) {
            return Err(BackendError::InvalidResource);
        }
        let size = usize::try_from(size_bytes).map_err(|_| BackendError::OutOfBounds)?;
        self.buffers.insert(id, vec![0; size]);
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
        let buf = self
            .buffers
            .get_mut(&id)
            .ok_or(BackendError::InvalidResource)?;
        let off = usize::try_from(dst_offset).map_err(|_| BackendError::OutOfBounds)?;
        let end = off
            .checked_add(data.len())
            .ok_or(BackendError::OutOfBounds)?;
        if end > buf.len() {
            return Err(BackendError::OutOfBounds);
        }
        buf[off..end].copy_from_slice(data);
        Ok(())
    }

    fn read_buffer(
        &self,
        id: u32,
        src_offset: u64,
        size_bytes: usize,
    ) -> Result<Vec<u8>, BackendError> {
        let buf = self.buffers.get(&id).ok_or(BackendError::InvalidResource)?;
        let off = usize::try_from(src_offset).map_err(|_| BackendError::OutOfBounds)?;
        let end = off
            .checked_add(size_bytes)
            .ok_or(BackendError::OutOfBounds)?;
        if end > buf.len() {
            return Err(BackendError::OutOfBounds);
        }
        Ok(buf[off..end].to_vec())
    }

    fn create_texture2d(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        format: u32,
        _usage: u32,
    ) -> Result<(), BackendError> {
        if id == 0 || width == 0 || height == 0 {
            return Err(BackendError::InvalidResource);
        }
        if self.textures.contains_key(&id) {
            return Err(BackendError::InvalidResource);
        }
        let Some(TextureFormat::Rgba8Unorm) = TextureFormat::from_u32(format) else {
            return Err(BackendError::Unsupported);
        };
        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or(BackendError::OutOfBounds)?;
        let byte_len = pixel_count
            .checked_mul(4)
            .ok_or(BackendError::OutOfBounds)?;
        self.textures.insert(
            id,
            Texture2d {
                width,
                height,
                rgba8: vec![0; byte_len],
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
            .get_mut(&id)
            .ok_or(BackendError::InvalidResource)?;
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

        for row in 0..height as usize {
            let src_start = row * bytes_per_row as usize;
            let dst_start = row * width as usize * 4;
            tex.rgba8[dst_start..dst_start + (width as usize * 4)]
                .copy_from_slice(&data[src_start..src_start + (width as usize * 4)]);
        }
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
        if width != tex.width || height != tex.height {
            return Err(BackendError::Unsupported);
        }
        let min_bpr = width.checked_mul(4).ok_or(BackendError::OutOfBounds)?;
        if bytes_per_row < min_bpr {
            return Err(BackendError::OutOfBounds);
        }

        let out_len = (bytes_per_row as usize)
            .checked_mul(height as usize)
            .ok_or(BackendError::OutOfBounds)?;
        let mut out = vec![0u8; out_len];
        for row in 0..height as usize {
            let src_start = row * width as usize * 4;
            let dst_start = row * bytes_per_row as usize;
            out[dst_start..dst_start + (width as usize * 4)]
                .copy_from_slice(&tex.rgba8[src_start..src_start + (width as usize * 4)]);
        }
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
        let rt = self.rt_mut()?;
        let r = (rgba[0].clamp(0.0, 1.0) * 255.0).round() as u8;
        let g = (rgba[1].clamp(0.0, 1.0) * 255.0).round() as u8;
        let b = (rgba[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        let a = (rgba[3].clamp(0.0, 1.0) * 255.0).round() as u8;
        for px in rt.rgba8.chunks_exact_mut(4) {
            px[0] = r;
            px[1] = g;
            px[2] = b;
            px[3] = a;
        }
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
        if stride < 24 {
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
        if self.pipeline_id != pipeline::BASIC_VERTEX_COLOR {
            return Err(BackendError::InvalidState("pipeline not set"));
        }
        let rt_id = self
            .render_target
            .ok_or(BackendError::InvalidState("no render target"))?;
        let viewport = {
            let rt = self
                .textures
                .get(&rt_id)
                .ok_or(BackendError::InvalidResource)?;
            self.effective_viewport(rt)
        };

        let binding = self
            .vertex_buffer
            .ok_or(BackendError::InvalidState("no vertex buffer"))?;
        let buf = self
            .buffers
            .get(&binding.buffer_id)
            .ok_or(BackendError::InvalidResource)?;

        let base = usize::try_from(binding.offset).map_err(|_| BackendError::OutOfBounds)?;
        let stride = binding.stride as usize;
        let start = base
            .checked_add(first_vertex as usize * stride)
            .ok_or(BackendError::OutOfBounds)?;
        let byte_len = (vertex_count as usize)
            .checked_mul(stride)
            .ok_or(BackendError::OutOfBounds)?;
        let end = start
            .checked_add(byte_len)
            .ok_or(BackendError::OutOfBounds)?;
        if end > buf.len() {
            return Err(BackendError::OutOfBounds);
        }

        let vertices_bytes = &buf[start..end];
        let mut verts = Vec::with_capacity(vertex_count as usize);
        for chunk in vertices_bytes
            .chunks_exact(stride)
            .take(vertex_count as usize)
        {
            let pos_x = f32::from_le_bytes(chunk[0..4].try_into().unwrap());
            let pos_y = f32::from_le_bytes(chunk[4..8].try_into().unwrap());
            let mut color = [0f32; 4];
            let mut off = 8;
            for c in &mut color {
                *c = f32::from_le_bytes(chunk[off..off + 4].try_into().unwrap());
                off += 4;
            }
            verts.push((pos_x, pos_y, color));
        }

        // Rasterize triangle list.
        if verts.len() < 3 {
            return Ok(());
        }

        let rt = self
            .textures
            .get_mut(&rt_id)
            .ok_or(BackendError::InvalidResource)?;
        for tri in verts.chunks_exact(3) {
            rasterize_triangle(rt, viewport, tri)?;
        }
        Ok(())
    }

    fn present(&mut self, texture_id: u32) -> Result<(), BackendError> {
        let tex = self
            .textures
            .get(&texture_id)
            .ok_or(BackendError::InvalidResource)?;
        self.presented = Some(PresentedFrame {
            width: tex.width,
            height: tex.height,
            rgba8: tex.rgba8.clone(),
        });
        Ok(())
    }

    fn take_presented_frame(&mut self) -> Option<PresentedFrame> {
        self.presented.take()
    }
}

fn rasterize_triangle(
    rt: &mut Texture2d,
    viewport: Viewport,
    tri: &[(f32, f32, [f32; 4])],
) -> Result<(), BackendError> {
    let (p0, p1, p2) = (&tri[0], &tri[1], &tri[2]);
    let (x0, y0) = ndc_to_screen((p0.0, p0.1), viewport);
    let (x1, y1) = ndc_to_screen((p1.0, p1.1), viewport);
    let (x2, y2) = ndc_to_screen((p2.0, p2.1), viewport);

    let min_x_f = x0.min(x1).min(x2).floor();
    let min_y_f = y0.min(y1).min(y2).floor();
    let max_x_f = x0.max(x1).max(x2).ceil();
    let max_y_f = y0.max(y1).max(y2).ceil();

    let min_x = (min_x_f as i32).clamp(0, rt.width as i32) as u32;
    let min_y = (min_y_f as i32).clamp(0, rt.height as i32) as u32;
    let max_x = (max_x_f as i32).clamp(0, rt.width as i32) as u32;
    let max_y = (max_y_f as i32).clamp(0, rt.height as i32) as u32;

    if min_x >= max_x || min_y >= max_y {
        return Ok(());
    }

    let area = edge(x0, y0, x1, y1, x2, y2);
    if area == 0.0 {
        return Ok(());
    }
    let inv_area = 1.0 / area;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let w0 = edge(x1, y1, x2, y2, px, py);
            let w1 = edge(x2, y2, x0, y0, px, py);
            let w2 = edge(x0, y0, x1, y1, px, py);

            let inside = if area > 0.0 {
                w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
            } else {
                w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
            };
            if !inside {
                continue;
            }

            let b0 = w0 * inv_area;
            let b1 = w1 * inv_area;
            let b2 = w2 * inv_area;

            let mut rgba = [0f32; 4];
            for i in 0..4 {
                rgba[i] = b0 * p0.2[i] + b1 * p1.2[i] + b2 * p2.2[i];
            }
            let dst = ((y * rt.width + x) as usize) * 4;
            rt.rgba8[dst] = (rgba[0].clamp(0.0, 1.0) * 255.0).round() as u8;
            rt.rgba8[dst + 1] = (rgba[1].clamp(0.0, 1.0) * 255.0).round() as u8;
            rt.rgba8[dst + 2] = (rgba[2].clamp(0.0, 1.0) * 255.0).round() as u8;
            rt.rgba8[dst + 3] = (rgba[3].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }

    Ok(())
}

fn ndc_to_screen(pos: (f32, f32), viewport: Viewport) -> (f32, f32) {
    let (x, y) = pos;
    let sx = viewport.x + (x * 0.5 + 0.5) * viewport.width;
    let sy = viewport.y + (-y * 0.5 + 0.5) * viewport.height;
    (sx, sy)
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}
