use aero_gpu_trace::{BlobKind, TraceReadError, TraceReader, TraceRecord};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdDecodeError, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AerogpuCompareFunc, AerogpuCullMode,
    AerogpuFillMode, AerogpuIndexFormat, AerogpuPrimitiveTopology, AerogpuSamplerAddressMode,
    AerogpuSamplerFilter, AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_STAGE_EX_MIN_ABI_MINOR,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Seek};

pub mod alloc_table_dump;
pub mod cmd_stream_decode;
pub mod submit_decode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedFrame {
    pub frame_index: u32,
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

impl ReplayedFrame {
    pub fn sha256(&self) -> String {
        sha256_hex(&frame_hash_bytes(self.width, self.height, &self.rgba8))
    }
}

#[derive(Debug)]
pub enum ReplayError {
    Trace(TraceReadError),
    MissingBlob {
        blob_id: u64,
    },
    WrongBlobKind {
        blob_id: u64,
        expected: BlobKind,
        found: BlobKind,
    },
    CommandStream(String),
    FrameNotPresented {
        frame_index: u32,
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::Trace(err) => write!(f, "failed to read trace: {err:?}"),
            ReplayError::MissingBlob { blob_id } => write!(f, "missing blob id {blob_id}"),
            ReplayError::WrongBlobKind {
                blob_id,
                expected,
                found,
            } => write!(
                f,
                "blob {blob_id} has unexpected kind {found:?} (expected {expected:?})"
            ),
            ReplayError::CommandStream(msg) => write!(f, "command stream error: {msg}"),
            ReplayError::FrameNotPresented { frame_index } => {
                write!(f, "frame {frame_index} ended without a present")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<TraceReadError> for ReplayError {
    fn from(value: TraceReadError) -> Self {
        Self::Trace(value)
    }
}

/// Replay a trace containing `RecordType::AerogpuSubmission` records and return the presented frames.
///
/// This is intentionally minimal and exists to provide a deterministic regression test surface.
pub fn replay_trace<R: Read + Seek>(reader: R) -> Result<Vec<ReplayedFrame>, ReplayError> {
    replay_trace_filtered(reader, None)
}

/// Replay a trace and return only the frames matching `frame_filter`.
///
/// When `frame_filter` is `Some(n)`, the replayer still processes earlier frames to build up the
/// necessary state (blobs/resources), but it avoids retaining skipped frames and stops once the
/// requested frame is presented.
pub fn replay_trace_filtered<R: Read + Seek>(
    reader: R,
    frame_filter: Option<u32>,
) -> Result<Vec<ReplayedFrame>, ReplayError> {
    let mut reader = TraceReader::open(reader)?;
    let mut blobs: HashMap<u64, (BlobKind, Vec<u8>)> = HashMap::new();
    let mut executor = AerogpuSoftwareExecutor::new();
    let mut out = Vec::new();

    let entries = reader.frame_entries().to_vec();
    for entry in entries {
        let records = reader.read_records_in_range(entry.start_offset, entry.end_offset)?;
        let mut current_frame = None;

        for record in records {
            match record {
                TraceRecord::BeginFrame { frame_index } => {
                    current_frame = Some(frame_index);
                }
                TraceRecord::Present { frame_index } => {
                    let frame = executor
                        .take_presented_frame()
                        .ok_or(ReplayError::FrameNotPresented { frame_index })?;
                    let replayed = ReplayedFrame {
                        frame_index,
                        width: frame.width,
                        height: frame.height,
                        rgba8: frame.rgba8,
                    };

                    if frame_filter.is_none() || frame_filter == Some(frame_index) {
                        out.push(replayed);
                    }
                    if frame_filter == Some(frame_index) {
                        return Ok(out);
                    }
                    current_frame = Some(frame_index);
                }
                TraceRecord::Packet { .. } => {
                    // Old toy packet streams are not handled here (yet).
                }
                TraceRecord::Blob {
                    blob_id,
                    kind,
                    bytes,
                } => {
                    blobs.insert(blob_id, (kind, bytes));
                }
                TraceRecord::AerogpuSubmission {
                    cmd_stream_blob_id,
                    alloc_table_blob_id,
                    memory_ranges,
                    ..
                } => {
                    let cmd_stream =
                        get_blob(&blobs, cmd_stream_blob_id, BlobKind::AerogpuCmdStream)?;

                    let mut mem = SubmissionMemory::default();
                    if alloc_table_blob_id != 0 {
                        let _alloc_table =
                            get_blob(&blobs, alloc_table_blob_id, BlobKind::AerogpuAllocTable)?;
                        // For now the replayer relies on the per-range entries, not the raw table.
                    }

                    for range in &memory_ranges {
                        let bytes = get_blob(&blobs, range.blob_id, BlobKind::AerogpuAllocMemory)?;
                        mem.insert(range.alloc_id, range.gpa, range.size_bytes, bytes.to_vec());
                    }

                    executor
                        .process_cmd_stream(cmd_stream, &mem)
                        .map_err(ReplayError::CommandStream)?;
                }
            }
        }

        if current_frame.is_none() {
            // This should not happen for TOC-bound frames.
            continue;
        }
    }

    Ok(out)
}

fn get_blob(
    blobs: &HashMap<u64, (BlobKind, Vec<u8>)>,
    blob_id: u64,
    expected: BlobKind,
) -> Result<&[u8], ReplayError> {
    let (kind, bytes) = blobs
        .get(&blob_id)
        .ok_or(ReplayError::MissingBlob { blob_id })?;
    if *kind != expected {
        return Err(ReplayError::WrongBlobKind {
            blob_id,
            expected,
            found: *kind,
        });
    }
    Ok(bytes)
}

fn frame_hash_bytes(width: u32, height: u32, rgba8: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + rgba8.len());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(rgba8);
    bytes
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[derive(Clone, Debug)]
struct FrameBuffer {
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Viewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct VertexBufferBinding {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u32,
}

#[derive(Default)]
struct AerogpuSoftwareExecutor {
    buffers: HashMap<u32, Vec<u8>>,
    textures: HashMap<u32, FrameBuffer>,
    texture_views: HashMap<u32, u32>,

    render_target: Option<u32>,
    viewport: Option<Viewport>,
    vertex_buffer0: Option<VertexBufferBinding>,

    presented: Option<FrameBuffer>,
}

impl AerogpuSoftwareExecutor {
    fn new() -> Self {
        Self::default()
    }

    fn take_presented_frame(&mut self) -> Option<FrameBuffer> {
        self.presented.take()
    }

    fn process_cmd_stream(&mut self, bytes: &[u8], mem: &SubmissionMemory) -> Result<(), String> {
        let iter = AerogpuCmdStreamIter::new(bytes)
            .map_err(|err| format!("failed to decode cmd stream: {err:?}"))?;

        // Track offsets for diagnostics (iterator itself doesn't expose it).
        let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
        for packet in iter {
            let packet = packet
                .map_err(|err| format!("cmd packet decode error at offset {offset}: {err:?}"))?;

            match packet.opcode {
                Some(AerogpuCmdOpcode::Nop) | Some(AerogpuCmdOpcode::DebugMarker) => {}
                Some(AerogpuCmdOpcode::CreateBuffer) => {
                    self.cmd_create_buffer(packet.payload, mem)?
                }
                Some(AerogpuCmdOpcode::CreateTexture2d) => {
                    self.cmd_create_texture2d(packet.payload, mem)?
                }
                Some(AerogpuCmdOpcode::CreateTextureView) => {
                    self.cmd_create_texture_view(packet.payload)?
                }
                Some(AerogpuCmdOpcode::DestroyTextureView) => {
                    self.cmd_destroy_texture_view(packet.payload)?
                }
                Some(AerogpuCmdOpcode::DestroyResource) => {
                    self.cmd_destroy_resource(packet.payload)?
                }
                Some(AerogpuCmdOpcode::SetRenderTargets) => {
                    self.cmd_set_render_targets(packet.payload)?
                }
                Some(AerogpuCmdOpcode::SetViewport) => self.cmd_set_viewport(packet.payload)?,
                Some(AerogpuCmdOpcode::SetVertexBuffers) => {
                    self.cmd_set_vertex_buffers(packet.payload)?
                }
                Some(AerogpuCmdOpcode::SetPrimitiveTopology) => {
                    self.cmd_set_primitive_topology(packet.payload)?
                }
                Some(AerogpuCmdOpcode::Clear) => self.cmd_clear(packet.payload)?,
                Some(AerogpuCmdOpcode::Draw) => self.cmd_draw(packet.payload)?,
                Some(AerogpuCmdOpcode::Present) => self.cmd_present(packet.payload)?,
                _ => {
                    // Unknown/unsupported opcode: skip.
                }
            }

            let cmd_size = packet.hdr.size_bytes as usize;
            offset = offset
                .checked_add(cmd_size)
                .ok_or_else(|| "cmd stream offset overflow".to_string())?;
        }

        Ok(())
    }

    fn cmd_create_buffer(&mut self, payload: &[u8], mem: &SubmissionMemory) -> Result<(), String> {
        // struct aerogpu_cmd_create_buffer (payload excludes hdr):
        // u32 buffer_handle; u32 usage_flags; u64 size_bytes; u32 backing_alloc_id; u32 backing_offset_bytes; u64 reserved0;
        if payload.len() < 32 {
            return Err("CREATE_BUFFER payload too small".into());
        }
        let buffer_handle = read_u32(payload, 0);
        let size_bytes = read_u64(payload, 8);
        let backing_alloc_id = read_u32(payload, 16);
        let backing_offset_bytes = read_u32(payload, 20) as usize;

        let size_usize = usize::try_from(size_bytes).map_err(|_| "buffer too large")?;
        let mut buf = vec![0u8; size_usize];
        if backing_alloc_id != 0 {
            let alloc = mem
                .get(backing_alloc_id)
                .ok_or_else(|| format!("missing alloc {backing_alloc_id} for CREATE_BUFFER"))?;
            let end = backing_offset_bytes
                .checked_add(size_usize)
                .ok_or_else(|| "buffer backing range overflow".to_string())?;
            if end > alloc.bytes.len() {
                return Err("buffer backing range out of bounds".into());
            }
            buf.copy_from_slice(&alloc.bytes[backing_offset_bytes..end]);
        }

        self.buffers.insert(buffer_handle, buf);
        Ok(())
    }

    fn cmd_create_texture2d(
        &mut self,
        payload: &[u8],
        _mem: &SubmissionMemory,
    ) -> Result<(), String> {
        // struct aerogpu_cmd_create_texture2d (payload excludes hdr):
        // u32 handle; u32 usage; u32 format; u32 width; u32 height; u32 mip_levels; u32 array_layers;
        // u32 row_pitch; u32 backing_alloc_id; u32 backing_offset_bytes; u64 reserved0;
        if payload.len() < 48 {
            return Err("CREATE_TEXTURE2D payload too small".into());
        }

        let texture_handle = read_u32(payload, 0);
        let format = read_u32(payload, 8);
        let width = read_u32(payload, 12);
        let height = read_u32(payload, 16);
        let mip_levels = read_u32(payload, 20);
        let array_layers = read_u32(payload, 24);
        let backing_alloc_id = read_u32(payload, 32);

        if mip_levels != 1 || array_layers != 1 {
            return Err("only mip_levels=1, array_layers=1 supported".into());
        }
        // aerogpu_format: accept only R8G8B8A8_UNORM for now.
        if format != AerogpuFormat::R8G8B8A8Unorm as u32 {
            return Err(format!("unsupported texture format {format}"));
        }
        if backing_alloc_id != 0 {
            return Err("backing_alloc_id textures not supported in replayer yet".into());
        }

        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| "texture too large".to_string())?;
        let byte_len = pixel_count
            .checked_mul(4)
            .ok_or_else(|| "texture too large".to_string())?;
        self.textures.insert(
            texture_handle,
            FrameBuffer {
                width,
                height,
                rgba8: vec![0u8; byte_len],
            },
        );
        Ok(())
    }

    fn cmd_create_texture_view(&mut self, payload: &[u8]) -> Result<(), String> {
        // struct aerogpu_cmd_create_texture_view (payload excludes hdr):
        // u32 view_handle; u32 texture_handle; u32 format; u32 base_mip_level; u32 mip_level_count;
        // u32 base_array_layer; u32 array_layer_count; u64 reserved0;
        if payload.len() < 36 {
            return Err("CREATE_TEXTURE_VIEW payload too small".into());
        }
        let view_handle = read_u32(payload, 0);
        let texture_handle = read_u32(payload, 4);
        if !self.textures.contains_key(&texture_handle) {
            return Err(format!(
                "unknown texture {texture_handle} for CREATE_TEXTURE_VIEW"
            ));
        }
        // The software executor does not model mip/array subresources; treat a view as an alias to
        // the base texture.
        self.texture_views.insert(view_handle, texture_handle);
        Ok(())
    }

    fn cmd_destroy_texture_view(&mut self, payload: &[u8]) -> Result<(), String> {
        // struct aerogpu_cmd_destroy_texture_view (payload excludes hdr):
        // u32 view_handle; u32 reserved0;
        if payload.len() < 8 {
            return Err("DESTROY_TEXTURE_VIEW payload too small".into());
        }
        let view_handle = read_u32(payload, 0);
        self.texture_views.remove(&view_handle);
        Ok(())
    }

    fn resolve_texture_handle(&self, handle: u32) -> Option<u32> {
        if self.textures.contains_key(&handle) {
            return Some(handle);
        }
        self.texture_views
            .get(&handle)
            .copied()
            .filter(|h| self.textures.contains_key(h))
    }

    fn cmd_destroy_resource(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 8 {
            return Err("DESTROY_RESOURCE payload too small".into());
        }
        let handle = read_u32(payload, 0);
        self.buffers.remove(&handle);
        self.textures.remove(&handle);
        self.texture_views.remove(&handle);
        self.texture_views.retain(|_, tex| *tex != handle);
        if self.render_target == Some(handle) {
            self.render_target = None;
        }
        Ok(())
    }

    fn cmd_set_render_targets(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 40 {
            return Err("SET_RENDER_TARGETS payload too small".into());
        }
        let color_count = read_u32(payload, 0);
        if color_count == 0 {
            self.render_target = None;
            return Ok(());
        }
        let rt_raw = read_u32(payload, 8);
        let rt = self
            .resolve_texture_handle(rt_raw)
            .ok_or_else(|| format!("unknown render target texture/view {rt_raw}"))?;
        self.render_target = Some(rt);
        Ok(())
    }

    fn cmd_set_viewport(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 24 {
            return Err("SET_VIEWPORT payload too small".into());
        }
        self.viewport = Some(Viewport {
            x: f32::from_bits(read_u32(payload, 0)),
            y: f32::from_bits(read_u32(payload, 4)),
            width: f32::from_bits(read_u32(payload, 8)),
            height: f32::from_bits(read_u32(payload, 12)),
        });
        Ok(())
    }

    fn cmd_set_vertex_buffers(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 8 {
            return Err("SET_VERTEX_BUFFERS payload too small".into());
        }
        let start_slot = read_u32(payload, 0);
        let buffer_count = read_u32(payload, 4) as usize;
        if start_slot != 0 {
            return Err("only start_slot=0 supported".into());
        }
        let expected = 8 + buffer_count * AerogpuVertexBufferBinding::SIZE_BYTES;
        if payload.len() < expected {
            return Err("SET_VERTEX_BUFFERS payload truncated".into());
        }
        if buffer_count == 0 {
            self.vertex_buffer0 = None;
            return Ok(());
        }
        let binding_off = 8;
        let buffer = read_u32(payload, binding_off);
        let stride_bytes = read_u32(payload, binding_off + 4);
        let offset_bytes = read_u32(payload, binding_off + 8);
        if !self.buffers.contains_key(&buffer) {
            return Err(format!("unknown vertex buffer {buffer}"));
        }
        self.vertex_buffer0 = Some(VertexBufferBinding {
            buffer,
            stride_bytes,
            offset_bytes,
        });
        Ok(())
    }

    fn cmd_set_primitive_topology(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 8 {
            return Err("SET_PRIMITIVE_TOPOLOGY payload too small".into());
        }
        let topology = read_u32(payload, 0);
        if topology != AerogpuPrimitiveTopology::TriangleList as u32 {
            return Err(format!("unsupported primitive topology {topology}"));
        }
        Ok(())
    }

    fn cmd_clear(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 28 {
            return Err("CLEAR payload too small".into());
        }
        let flags = read_u32(payload, 0);
        if (flags & AEROGPU_CLEAR_COLOR) == 0 {
            return Ok(());
        }

        let rt_id = self
            .render_target
            .ok_or_else(|| "CLEAR without render target".to_string())?;
        let rt = self
            .textures
            .get_mut(&rt_id)
            .ok_or_else(|| "missing render target".to_string())?;

        let r = f32::from_bits(read_u32(payload, 4));
        let g = f32::from_bits(read_u32(payload, 8));
        let b = f32::from_bits(read_u32(payload, 12));
        let a = f32::from_bits(read_u32(payload, 16));
        let rgba8 = [
            (r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (b.clamp(0.0, 1.0) * 255.0).round() as u8,
            (a.clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
        for px in rt.rgba8.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba8);
        }
        Ok(())
    }

    fn cmd_draw(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 16 {
            return Err("DRAW payload too small".into());
        }
        let vertex_count = read_u32(payload, 0);
        let instance_count = read_u32(payload, 4);
        let first_vertex = read_u32(payload, 8);
        let first_instance = read_u32(payload, 12);
        if instance_count != 1 || first_instance != 0 {
            return Err("instancing not supported".into());
        }
        self.draw_triangle_list(vertex_count, first_vertex)?;
        Ok(())
    }

    fn cmd_present(&mut self, _payload: &[u8]) -> Result<(), String> {
        let rt_id = self
            .render_target
            .ok_or_else(|| "PRESENT without render target".to_string())?;
        let rt = self
            .textures
            .get(&rt_id)
            .ok_or_else(|| "missing render target".to_string())?;
        self.presented = Some(rt.clone());
        Ok(())
    }

    fn draw_triangle_list(&mut self, vertex_count: u32, first_vertex: u32) -> Result<(), String> {
        if vertex_count < 3 {
            return Ok(());
        }
        let rt_id = self
            .render_target
            .ok_or_else(|| "DRAW without render target".to_string())?;
        let viewport = {
            let rt = self
                .textures
                .get(&rt_id)
                .ok_or_else(|| "missing render target".to_string())?;
            effective_viewport(self.viewport, rt)
        };

        let binding = self
            .vertex_buffer0
            .ok_or_else(|| "DRAW without vertex buffer".to_string())?;
        if binding.stride_bytes < 24 {
            return Err("vertex stride < 24 not supported".into());
        }
        let buf = self
            .buffers
            .get(&binding.buffer)
            .ok_or_else(|| "missing vertex buffer".to_string())?;

        let base = (binding.offset_bytes as usize)
            .checked_add(first_vertex as usize * binding.stride_bytes as usize)
            .ok_or_else(|| "vertex buffer offset overflow".to_string())?;
        let byte_len = (vertex_count as usize)
            .checked_mul(binding.stride_bytes as usize)
            .ok_or_else(|| "vertex buffer size overflow".to_string())?;
        let end = base
            .checked_add(byte_len)
            .ok_or_else(|| "vertex buffer size overflow".to_string())?;
        if end > buf.len() {
            return Err("vertex buffer out of bounds".into());
        }

        let vertices_bytes = &buf[base..end];
        let mut verts = Vec::with_capacity(vertex_count as usize);
        for chunk in vertices_bytes
            .chunks_exact(binding.stride_bytes as usize)
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

        let rt = self
            .textures
            .get_mut(&rt_id)
            .ok_or_else(|| "missing render target".to_string())?;
        for tri in verts.chunks_exact(3) {
            rasterize_triangle(rt, viewport, tri);
        }
        Ok(())
    }
}

fn effective_viewport(viewport: Option<Viewport>, rt: &FrameBuffer) -> Viewport {
    match viewport {
        Some(v) if v.width > 0.0 && v.height > 0.0 => v,
        _ => Viewport {
            x: 0.0,
            y: 0.0,
            width: rt.width as f32,
            height: rt.height as f32,
        },
    }
}

fn rasterize_triangle(rt: &mut FrameBuffer, viewport: Viewport, tri: &[(f32, f32, [f32; 4])]) {
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
        return;
    }

    let area = edge(x0, y0, x1, y1, x2, y2);
    if area == 0.0 {
        return;
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
            for (i, value) in rgba.iter_mut().enumerate() {
                *value = b0 * p0.2[i] + b1 * p1.2[i] + b2 * p2.2[i];
            }
            let dst = ((y * rt.width + x) as usize) * 4;
            rt.rgba8[dst] = (rgba[0].clamp(0.0, 1.0) * 255.0).round() as u8;
            rt.rgba8[dst + 1] = (rgba[1].clamp(0.0, 1.0) * 255.0).round() as u8;
            rt.rgba8[dst + 2] = (rgba[2].clamp(0.0, 1.0) * 255.0).round() as u8;
            rt.rgba8[dst + 3] = (rgba[3].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
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

fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap())
}

#[derive(Clone, Debug)]
struct SubmissionAlloc {
    #[allow(dead_code)]
    gpa: u64,
    #[allow(dead_code)]
    size_bytes: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct SubmissionMemory {
    allocs: HashMap<u32, SubmissionAlloc>,
}

impl SubmissionMemory {
    fn insert(&mut self, alloc_id: u32, gpa: u64, size_bytes: u64, bytes: Vec<u8>) {
        self.allocs.insert(
            alloc_id,
            SubmissionAlloc {
                gpa,
                size_bytes,
                bytes,
            },
        );
    }

    fn get(&self, alloc_id: u32) -> Option<&SubmissionAlloc> {
        self.allocs.get(&alloc_id)
    }
}

#[derive(Debug)]
pub enum CmdStreamDecodeError {
    Header(AerogpuCmdDecodeError),
    Packet {
        offset: usize,
        err: AerogpuCmdDecodeError,
    },
    UnknownOpcode {
        offset: usize,
        opcode_id: u32,
    },
    Payload {
        offset: usize,
        opcode: AerogpuCmdOpcode,
        err: AerogpuCmdDecodeError,
    },
    MalformedPayload {
        offset: usize,
        opcode: AerogpuCmdOpcode,
        msg: &'static str,
    },
    OffsetOverflow {
        offset: usize,
        size_bytes: u32,
    },
}

impl fmt::Display for CmdStreamDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CmdStreamDecodeError::Header(err) => {
                write!(f, "failed to decode command stream header: {err:?}")
            }
            CmdStreamDecodeError::Packet { offset, err } => write!(
                f,
                "command packet decode error at offset 0x{offset:08X}: {err:?}"
            ),
            CmdStreamDecodeError::UnknownOpcode { offset, opcode_id } => write!(
                f,
                "unknown opcode_id=0x{opcode_id:08X} at offset 0x{offset:08X}"
            ),
            CmdStreamDecodeError::Payload { offset, opcode, err } => write!(
                f,
                "failed to decode {opcode:?} payload at offset 0x{offset:08X}: {err:?}"
            ),
            CmdStreamDecodeError::MalformedPayload { offset, opcode, msg } => write!(
                f,
                "malformed {opcode:?} payload at offset 0x{offset:08X}: {msg}"
            ),
            CmdStreamDecodeError::OffsetOverflow { offset, size_bytes } => write!(
                f,
                "command stream offset overflow at offset 0x{offset:08X} adding size_bytes={size_bytes}"
            ),
        }
    }
}

impl std::error::Error for CmdStreamDecodeError {}

fn u32_le_at(bytes: &[u8], off: usize) -> Option<u32> {
    let b = bytes.get(off..off + 4)?;
    Some(u32::from_le_bytes(b.try_into().unwrap()))
}

fn u64_le_at(bytes: &[u8], off: usize) -> Option<u64> {
    let b = bytes.get(off..off + 8)?;
    Some(u64::from_le_bytes(b.try_into().unwrap()))
}

fn i32_le_at(bytes: &[u8], off: usize) -> Option<i32> {
    let b = bytes.get(off..off + 4)?;
    Some(i32::from_le_bytes(b.try_into().unwrap()))
}

fn f32_bits_at(bytes: &[u8], off: usize) -> Option<f32> {
    Some(f32::from_bits(u32_le_at(bytes, off)?))
}

fn fmt_f32_3(v: f32) -> String {
    // Fixed precision yields more stable/grep-friendly output than the shortest-repr formatter.
    format!("{v:.3}")
}

fn hex_prefix(bytes: &[u8], max_len: usize) -> String {
    let mut out = String::new();
    let take = bytes.len().min(max_len);
    for b in &bytes[..take] {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    if bytes.len() > max_len {
        out.push_str("..");
    }
    out
}

fn stage_ex_name(stage_ex: u32) -> &'static str {
    // Human-readable names for `stage_ex` discriminators (DXBC program type IDs).
    //
    // Note: `stage_ex=1` (Vertex DXBC program type) is intentionally invalid in AeroGPU; vertex
    // shaders must be encoded via the legacy `shader_stage = VERTEX` value for clarity.
    match stage_ex {
        1 => "InvalidVertex",
        2 => "Geometry",
        3 => "Hull",
        4 => "Domain",
        5 => "Compute",
        _ => "Unknown",
    }
}

fn shader_stage_name(shader_stage: u32) -> Option<String> {
    AerogpuShaderStage::from_u32(shader_stage).map(|s| format!("{s:?}"))
}

fn topology_name(topology: u32) -> Option<String> {
    AerogpuPrimitiveTopology::from_u32(topology).map(|t| format!("{t:?}"))
}

fn index_format_name(format: u32) -> Option<String> {
    AerogpuIndexFormat::from_u32(format).map(|f| format!("{f:?}"))
}

fn blend_factor_name(factor: u32) -> Option<String> {
    AerogpuBlendFactor::from_u32(factor).map(|f| format!("{f:?}"))
}

fn blend_op_name(op: u32) -> Option<String> {
    AerogpuBlendOp::from_u32(op).map(|o| format!("{o:?}"))
}

fn compare_func_name(func: u32) -> Option<String> {
    AerogpuCompareFunc::from_u32(func).map(|f| format!("{f:?}"))
}

fn fill_mode_name(mode: u32) -> Option<String> {
    AerogpuFillMode::from_u32(mode).map(|m| format!("{m:?}"))
}

fn cull_mode_name(mode: u32) -> Option<String> {
    AerogpuCullMode::from_u32(mode).map(|m| format!("{m:?}"))
}

fn sampler_filter_name(filter: u32) -> Option<String> {
    AerogpuSamplerFilter::from_u32(filter).map(|f| format!("{f:?}"))
}

fn sampler_address_mode_name(mode: u32) -> Option<String> {
    AerogpuSamplerAddressMode::from_u32(mode).map(|m| format!("{m:?}"))
}

/// Decode an AeroGPU command stream (`aerogpu_cmd_stream_header` + packet sequence) and return a
/// stable, grep-friendly opcode listing.
///
/// This is intended for inspecting raw dumps from Win7 guests (e.g. via dbgctl) without writing
/// ad-hoc parsers.
pub fn decode_cmd_stream_listing(
    bytes: &[u8],
    strict: bool,
) -> Result<String, CmdStreamDecodeError> {
    let iter = AerogpuCmdStreamIter::new(bytes).map_err(CmdStreamDecodeError::Header)?;
    let header = *iter.header();

    // Avoid taking references to packed fields (UB) by copying them into locals first.
    let magic = header.magic;
    let abi_version = header.abi_version;
    let stream_size_bytes = header.size_bytes;
    let flags = header.flags;
    let reserved0 = header.reserved0;
    let reserved1 = header.reserved1;

    let abi_major = (abi_version >> 16) as u16;
    let abi_minor = (abi_version & 0xFFFF) as u16;

    let mut out = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "header magic=0x{magic:08X} abi={abi_major}.{abi_minor} size_bytes={stream_size_bytes} flags=0x{flags:08X} reserved0=0x{reserved0:08X} reserved1=0x{reserved1:08X} file_len={}",
        bytes.len()
    );

    // Track offsets explicitly; the iterator currently does not expose them.
    let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    for pkt in iter {
        let pkt = pkt.map_err(|err| CmdStreamDecodeError::Packet { offset, err })?;

        let opcode_id = pkt.hdr.opcode;
        let size_bytes = pkt.hdr.size_bytes;

        let mut line = String::new();
        match pkt.opcode {
            Some(opcode) => {
                let _ = write!(
                    line,
                    "0x{offset:08X} {opcode:?} size_bytes={size_bytes} opcode_id=0x{opcode_id:08X}"
                );

                match opcode {
                    AerogpuCmdOpcode::Nop => {}
                    AerogpuCmdOpcode::DebugMarker => {
                        let marker_bytes = pkt.payload;
                        let marker = String::from_utf8_lossy(marker_bytes)
                            .trim_end_matches('\0')
                            .replace('\n', "\\n");
                        let marker = marker.chars().take(80).collect::<String>();
                        let _ = write!(line, " marker=\"{marker}\"");
                    }

                    AerogpuCmdOpcode::CreateBuffer => {
                        if pkt.payload.len() < 32 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 32 bytes",
                            });
                        }
                        let buffer_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let usage_flags = u32_le_at(pkt.payload, 4).unwrap();
                        let buffer_size_bytes = u64_le_at(pkt.payload, 8).unwrap();
                        let backing_alloc_id = u32_le_at(pkt.payload, 16).unwrap();
                        let backing_offset_bytes = u32_le_at(pkt.payload, 20).unwrap();
                        let _ = write!(
                            line,
                            " buffer_handle={buffer_handle} usage_flags=0x{usage_flags:08X} buffer_size_bytes={buffer_size_bytes} backing_alloc_id={backing_alloc_id} backing_offset_bytes={backing_offset_bytes}"
                        );
                    }
                    AerogpuCmdOpcode::CreateTexture2d => {
                        if pkt.payload.len() < 48 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 48 bytes",
                            });
                        }
                        let texture_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let usage_flags = u32_le_at(pkt.payload, 4).unwrap();
                        let format = u32_le_at(pkt.payload, 8).unwrap();
                        let width = u32_le_at(pkt.payload, 12).unwrap();
                        let height = u32_le_at(pkt.payload, 16).unwrap();
                        let mip_levels = u32_le_at(pkt.payload, 20).unwrap();
                        let array_layers = u32_le_at(pkt.payload, 24).unwrap();
                        let row_pitch_bytes = u32_le_at(pkt.payload, 28).unwrap();
                        let backing_alloc_id = u32_le_at(pkt.payload, 32).unwrap();
                        let backing_offset_bytes = u32_le_at(pkt.payload, 36).unwrap();

                        let format_name = AerogpuFormat::from_u32(format).map(|f| format!("{f:?}"));

                        let _ = write!(
                            line,
                            " texture_handle={texture_handle} usage_flags=0x{usage_flags:08X} format=0x{format:08X}"
                        );
                        if let Some(name) = format_name {
                            let _ = write!(line, " format_name={name}");
                        }
                        let _ = write!(
                            line,
                            " width={width} height={height} mip_levels={mip_levels} array_layers={array_layers} row_pitch_bytes={row_pitch_bytes} backing_alloc_id={backing_alloc_id} backing_offset_bytes={backing_offset_bytes}"
                        );
                    }
                    AerogpuCmdOpcode::DestroyResource => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let resource_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let _ = write!(line, " resource_handle={resource_handle}");
                    }
                    AerogpuCmdOpcode::ResourceDirtyRange => {
                        if pkt.payload.len() < 24 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 24 bytes",
                            });
                        }
                        let resource_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let range_offset_bytes = u64_le_at(pkt.payload, 8).unwrap();
                        let range_size_bytes = u64_le_at(pkt.payload, 16).unwrap();
                        let _ = write!(
                            line,
                            " resource_handle={resource_handle} offset_bytes={range_offset_bytes} range_size_bytes={range_size_bytes}"
                        );
                    }
                    AerogpuCmdOpcode::UploadResource => {
                        let (cmd, data) =
                            pkt.decode_upload_resource_payload_le().map_err(|err| {
                                CmdStreamDecodeError::Payload {
                                    offset,
                                    opcode,
                                    err,
                                }
                            })?;
                        // Avoid taking references to packed fields.
                        let resource_handle = cmd.resource_handle;
                        let upload_offset_bytes = cmd.offset_bytes;
                        let upload_size_bytes = cmd.size_bytes;
                        let prefix = hex_prefix(data, 16);
                        let _ = write!(
                            line,
                            " resource_handle={} offset_bytes={} upload_size_bytes={} data_len={} data_prefix={}",
                            resource_handle,
                            upload_offset_bytes,
                            upload_size_bytes,
                            data.len(),
                            prefix
                        );
                    }
                    AerogpuCmdOpcode::CopyBuffer => {
                        if pkt.payload.len() < 40 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 40 bytes",
                            });
                        }
                        let dst_buffer = u32_le_at(pkt.payload, 0).unwrap();
                        let src_buffer = u32_le_at(pkt.payload, 4).unwrap();
                        let dst_offset_bytes = u64_le_at(pkt.payload, 8).unwrap();
                        let src_offset_bytes = u64_le_at(pkt.payload, 16).unwrap();
                        let copy_size_bytes = u64_le_at(pkt.payload, 24).unwrap();
                        let flags = u32_le_at(pkt.payload, 32).unwrap();
                        let _ = write!(
                            line,
                            " dst_buffer={dst_buffer} src_buffer={src_buffer} dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes} copy_size_bytes={copy_size_bytes} flags=0x{flags:08X}"
                        );
                    }
                    AerogpuCmdOpcode::CopyTexture2d => {
                        if pkt.payload.len() < 56 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 56 bytes",
                            });
                        }
                        let dst_texture = u32_le_at(pkt.payload, 0).unwrap();
                        let src_texture = u32_le_at(pkt.payload, 4).unwrap();
                        let dst_mip_level = u32_le_at(pkt.payload, 8).unwrap();
                        let dst_array_layer = u32_le_at(pkt.payload, 12).unwrap();
                        let src_mip_level = u32_le_at(pkt.payload, 16).unwrap();
                        let src_array_layer = u32_le_at(pkt.payload, 20).unwrap();
                        let dst_x = u32_le_at(pkt.payload, 24).unwrap();
                        let dst_y = u32_le_at(pkt.payload, 28).unwrap();
                        let src_x = u32_le_at(pkt.payload, 32).unwrap();
                        let src_y = u32_le_at(pkt.payload, 36).unwrap();
                        let width = u32_le_at(pkt.payload, 40).unwrap();
                        let height = u32_le_at(pkt.payload, 44).unwrap();
                        let flags = u32_le_at(pkt.payload, 48).unwrap();
                        let _ = write!(
                            line,
                            " dst_texture={dst_texture} src_texture={src_texture} dst_mip_level={dst_mip_level} dst_array_layer={dst_array_layer} src_mip_level={src_mip_level} src_array_layer={src_array_layer} dst_xy={dst_x},{dst_y} src_xy={src_x},{src_y} size={width}x{height} flags=0x{flags:08X}"
                        );
                    }
                    AerogpuCmdOpcode::CreateTextureView => {
                        if pkt.payload.len() < 36 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 36 bytes",
                            });
                        }

                        let view_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let texture_handle = u32_le_at(pkt.payload, 4).unwrap();
                        let format = u32_le_at(pkt.payload, 8).unwrap();
                        let base_mip_level = u32_le_at(pkt.payload, 12).unwrap();
                        let mip_level_count = u32_le_at(pkt.payload, 16).unwrap();
                        let base_array_layer = u32_le_at(pkt.payload, 20).unwrap();
                        let array_layer_count = u32_le_at(pkt.payload, 24).unwrap();

                        let format_name = AerogpuFormat::from_u32(format).map(|f| format!("{f:?}"));

                        let _ = write!(
                            line,
                            " view_handle={view_handle} texture_handle={texture_handle} format=0x{format:08X}"
                        );
                        if let Some(name) = format_name {
                            let _ = write!(line, " format_name={name}");
                        }
                        let _ = write!(
                            line,
                            " base_mip_level={base_mip_level} mip_level_count={mip_level_count} base_array_layer={base_array_layer} array_layer_count={array_layer_count}"
                        );
                    }
                    AerogpuCmdOpcode::DestroyTextureView => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let view_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let _ = write!(line, " view_handle={view_handle}");
                    }
                    AerogpuCmdOpcode::CreateShaderDxbc => {
                        let (cmd, dxbc) =
                            pkt.decode_create_shader_dxbc_payload_le().map_err(|err| {
                                CmdStreamDecodeError::Payload {
                                    offset,
                                    opcode,
                                    err,
                                }
                            })?;
                        // Avoid taking references to packed fields.
                        let shader_handle = cmd.shader_handle;
                        let stage = cmd.stage;
                        let stage_ex = cmd.reserved0;
                        let dxbc_size_bytes = cmd.dxbc_size_bytes;
                        let _ = write!(line, " shader_handle={shader_handle} stage={stage}");
                        if let Some(name) = shader_stage_name(stage) {
                            let _ = write!(line, " stage_name={name}");
                        }
                        let _ = write!(
                            line,
                            " dxbc_size_bytes={dxbc_size_bytes} dxbc_prefix={}",
                            hex_prefix(dxbc, 16)
                        );
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && stage == 2
                            && stage_ex != 0
                        {
                            // `CREATE_SHADER_DXBC` uses `reserved0` as a `stage_ex` tag when
                            // `stage == COMPUTE` (see `docs/16-gpu-command-abi.md`).
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }
                    }
                    AerogpuCmdOpcode::DestroyShader => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let shader_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let _ = write!(line, " shader_handle={shader_handle}");
                    }
                    AerogpuCmdOpcode::CreateInputLayout => {
                        let (cmd, blob) =
                            pkt.decode_create_input_layout_payload_le().map_err(|err| {
                                CmdStreamDecodeError::Payload {
                                    offset,
                                    opcode,
                                    err,
                                }
                            })?;
                        // Avoid taking references to packed fields.
                        let input_layout_handle = cmd.input_layout_handle;
                        let blob_size_bytes = cmd.blob_size_bytes;
                        let _ = write!(
                            line,
                            " input_layout_handle={} blob_size_bytes={} blob_prefix={}",
                            input_layout_handle,
                            blob_size_bytes,
                            hex_prefix(blob, 16)
                        );
                    }
                    AerogpuCmdOpcode::DestroyInputLayout => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let input_layout_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let _ = write!(line, " input_layout_handle={input_layout_handle}");
                    }
                    AerogpuCmdOpcode::SetInputLayout => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let input_layout_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let _ = write!(line, " input_layout_handle={input_layout_handle}");
                    }

                    AerogpuCmdOpcode::SetRenderTargets => {
                        if pkt.payload.len() < 40 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 40 bytes",
                            });
                        }
                        let color_count = u32_le_at(pkt.payload, 0).unwrap();
                        let depth_stencil = u32_le_at(pkt.payload, 4).unwrap();
                        let mut colors = Vec::new();
                        let max = (color_count as usize).min(8);
                        for i in 0..max {
                            colors.push(u32_le_at(pkt.payload, 8 + i * 4).unwrap());
                        }
                        let _ = write!(
                            line,
                            " color_count={color_count} depth_stencil={depth_stencil} colors={:?}",
                            colors
                        );
                    }
                    AerogpuCmdOpcode::SetViewport => {
                        if pkt.payload.len() < 24 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 24 bytes",
                            });
                        }
                        let x = f32_bits_at(pkt.payload, 0).unwrap();
                        let y = f32_bits_at(pkt.payload, 4).unwrap();
                        let width = f32_bits_at(pkt.payload, 8).unwrap();
                        let height = f32_bits_at(pkt.payload, 12).unwrap();
                        let min_depth = f32_bits_at(pkt.payload, 16).unwrap();
                        let max_depth = f32_bits_at(pkt.payload, 20).unwrap();
                        let _ = write!(
                            line,
                            " x={} y={} width={} height={} min_depth={} max_depth={}",
                            fmt_f32_3(x),
                            fmt_f32_3(y),
                            fmt_f32_3(width),
                            fmt_f32_3(height),
                            fmt_f32_3(min_depth),
                            fmt_f32_3(max_depth)
                        );
                    }
                    AerogpuCmdOpcode::SetScissor => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let x = i32_le_at(pkt.payload, 0).unwrap();
                        let y = i32_le_at(pkt.payload, 4).unwrap();
                        let width = i32_le_at(pkt.payload, 8).unwrap();
                        let height = i32_le_at(pkt.payload, 12).unwrap();
                        let _ = write!(line, " x={x} y={y} width={width} height={height}");
                    }

                    AerogpuCmdOpcode::SetBlendState => {
                        if pkt.payload.len() < 52 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 52 bytes",
                            });
                        }
                        let enable = u32_le_at(pkt.payload, 0).unwrap();
                        let src_factor = u32_le_at(pkt.payload, 4).unwrap();
                        let dst_factor = u32_le_at(pkt.payload, 8).unwrap();
                        let blend_op = u32_le_at(pkt.payload, 12).unwrap();
                        let color_write_mask = pkt.payload[16];
                        let sample_mask = u32_le_at(pkt.payload, 48).unwrap();
                        let _ = write!(line, " enable={enable} src_factor={src_factor}");
                        if let Some(name) = blend_factor_name(src_factor) {
                            let _ = write!(line, " src_factor_name={name}");
                        }
                        let _ = write!(line, " dst_factor={dst_factor}");
                        if let Some(name) = blend_factor_name(dst_factor) {
                            let _ = write!(line, " dst_factor_name={name}");
                        }
                        let _ = write!(line, " blend_op={blend_op}");
                        if let Some(name) = blend_op_name(blend_op) {
                            let _ = write!(line, " blend_op_name={name}");
                        }
                        let _ = write!(
                            line,
                            " color_write_mask=0x{color_write_mask:02X} sample_mask=0x{sample_mask:08X}"
                        );
                    }
                    AerogpuCmdOpcode::SetDepthStencilState => {
                        if pkt.payload.len() < 20 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 20 bytes",
                            });
                        }
                        let depth_enable = u32_le_at(pkt.payload, 0).unwrap();
                        let depth_write_enable = u32_le_at(pkt.payload, 4).unwrap();
                        let depth_func = u32_le_at(pkt.payload, 8).unwrap();
                        let stencil_enable = u32_le_at(pkt.payload, 12).unwrap();
                        let stencil_read_mask = pkt.payload[16];
                        let stencil_write_mask = pkt.payload[17];
                        let _ = write!(
                            line,
                            " depth_enable={depth_enable} depth_write_enable={depth_write_enable} depth_func={depth_func}"
                        );
                        if let Some(name) = compare_func_name(depth_func) {
                            let _ = write!(line, " depth_func_name={name}");
                        }
                        let _ = write!(
                            line,
                            " stencil_enable={stencil_enable} stencil_read_mask=0x{stencil_read_mask:02X} stencil_write_mask=0x{stencil_write_mask:02X}"
                        );
                    }
                    AerogpuCmdOpcode::SetRasterizerState => {
                        if pkt.payload.len() < 24 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 24 bytes",
                            });
                        }
                        let fill_mode = u32_le_at(pkt.payload, 0).unwrap();
                        let cull_mode = u32_le_at(pkt.payload, 4).unwrap();
                        let front_ccw = u32_le_at(pkt.payload, 8).unwrap();
                        let scissor_enable = u32_le_at(pkt.payload, 12).unwrap();
                        let depth_bias = i32_le_at(pkt.payload, 16).unwrap();
                        let flags = u32_le_at(pkt.payload, 20).unwrap();
                        let _ = write!(line, " fill_mode={fill_mode}");
                        if let Some(name) = fill_mode_name(fill_mode) {
                            let _ = write!(line, " fill_mode_name={name}");
                        }
                        let _ = write!(line, " cull_mode={cull_mode}");
                        if let Some(name) = cull_mode_name(cull_mode) {
                            let _ = write!(line, " cull_mode_name={name}");
                        }
                        let _ = write!(
                            line,
                            " front_ccw={front_ccw} scissor_enable={scissor_enable} depth_bias={depth_bias} flags=0x{flags:08X}"
                        );
                    }

                    AerogpuCmdOpcode::Clear => {
                        if pkt.payload.len() < 28 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 28 bytes",
                            });
                        }
                        let flags = u32_le_at(pkt.payload, 0).unwrap();
                        let color = [
                            f32_bits_at(pkt.payload, 4).unwrap(),
                            f32_bits_at(pkt.payload, 8).unwrap(),
                            f32_bits_at(pkt.payload, 12).unwrap(),
                            f32_bits_at(pkt.payload, 16).unwrap(),
                        ];
                        let depth = f32_bits_at(pkt.payload, 20).unwrap();
                        let stencil = u32_le_at(pkt.payload, 24).unwrap();

                        let _ = write!(line, " flags=0x{flags:08X}");
                        if (flags & AEROGPU_CLEAR_COLOR) != 0 {
                            let _ = write!(
                                line,
                                " color_rgba=[{},{},{},{}]",
                                fmt_f32_3(color[0]),
                                fmt_f32_3(color[1]),
                                fmt_f32_3(color[2]),
                                fmt_f32_3(color[3])
                            );
                        }
                        let _ = write!(line, " depth={}", fmt_f32_3(depth));
                        let _ = write!(line, " stencil={stencil}");
                    }
                    AerogpuCmdOpcode::Draw => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let vertex_count = u32_le_at(pkt.payload, 0).unwrap();
                        let instance_count = u32_le_at(pkt.payload, 4).unwrap();
                        let first_vertex = u32_le_at(pkt.payload, 8).unwrap();
                        let first_instance = u32_le_at(pkt.payload, 12).unwrap();
                        let _ = write!(
                            line,
                            " vertex_count={vertex_count} instance_count={instance_count} first_vertex={first_vertex} first_instance={first_instance}"
                        );
                    }
                    AerogpuCmdOpcode::DrawIndexed => {
                        if pkt.payload.len() < 20 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 20 bytes",
                            });
                        }
                        let index_count = u32_le_at(pkt.payload, 0).unwrap();
                        let instance_count = u32_le_at(pkt.payload, 4).unwrap();
                        let first_index = u32_le_at(pkt.payload, 8).unwrap();
                        let base_vertex = i32_le_at(pkt.payload, 12).unwrap();
                        let first_instance = u32_le_at(pkt.payload, 16).unwrap();
                        let _ = write!(
                            line,
                            " index_count={index_count} instance_count={instance_count} first_index={first_index} base_vertex={base_vertex} first_instance={first_instance}"
                        );
                    }
                    AerogpuCmdOpcode::Present => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let scanout_id = u32_le_at(pkt.payload, 0).unwrap();
                        let flags = u32_le_at(pkt.payload, 4).unwrap();
                        let _ = write!(line, " scanout_id={scanout_id} flags=0x{flags:08X}");
                    }
                    AerogpuCmdOpcode::PresentEx => {
                        if pkt.payload.len() < 12 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 12 bytes",
                            });
                        }
                        let scanout_id = u32_le_at(pkt.payload, 0).unwrap();
                        let flags = u32_le_at(pkt.payload, 4).unwrap();
                        let d3d9_present_flags = u32_le_at(pkt.payload, 8).unwrap();
                        let _ = write!(
                            line,
                            " scanout_id={scanout_id} flags=0x{flags:08X} d3d9_present_flags=0x{d3d9_present_flags:08X}"
                        );
                    }
                    AerogpuCmdOpcode::BindShaders => {
                        let (cmd, ex) = pkt.decode_bind_shaders_payload_le().map_err(|err| {
                            CmdStreamDecodeError::Payload {
                                offset,
                                opcode,
                                err,
                            }
                        })?;
                        // Avoid taking references to packed fields.
                        let vs = cmd.vs;
                        let ps = cmd.ps;
                        let cs = cmd.cs;
                        let _ = write!(line, " vs={vs} ps={ps} cs={cs}");
                        if let Some(ex) = ex {
                            let gs = ex.gs;
                            let hs = ex.hs;
                            let ds = ex.ds;
                            let _ = write!(line, " gs={gs} hs={hs} ds={ds}");
                        } else {
                            let gs = cmd.gs();
                            let _ = write!(line, " gs={gs}");
                        }
                    }

                    AerogpuCmdOpcode::SetTexture => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let shader_stage = u32_le_at(pkt.payload, 0).unwrap();
                        let slot = u32_le_at(pkt.payload, 4).unwrap();
                        let texture = u32_le_at(pkt.payload, 8).unwrap();
                        let stage_ex = u32_le_at(pkt.payload, 12).unwrap();
                        let _ = write!(line, " shader_stage={shader_stage}");
                        if let Some(name) = shader_stage_name(shader_stage) {
                            let _ = write!(line, " shader_stage_name={name}");
                        }
                        let _ = write!(line, " slot={slot} texture={texture}");
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && shader_stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }
                    }
                    AerogpuCmdOpcode::SetSamplerState => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let shader_stage = u32_le_at(pkt.payload, 0).unwrap();
                        let slot = u32_le_at(pkt.payload, 4).unwrap();
                        let state = u32_le_at(pkt.payload, 8).unwrap();
                        let value = u32_le_at(pkt.payload, 12).unwrap();
                        let _ = write!(line, " shader_stage={shader_stage}");
                        if let Some(name) = shader_stage_name(shader_stage) {
                            let _ = write!(line, " shader_stage_name={name}");
                        }
                        let _ = write!(line, " slot={slot} state={state} value=0x{value:08X}");
                    }
                    AerogpuCmdOpcode::SetRenderState => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let state = u32_le_at(pkt.payload, 0).unwrap();
                        let value = u32_le_at(pkt.payload, 4).unwrap();
                        let _ = write!(line, " state={state} value=0x{value:08X}");
                    }
                    AerogpuCmdOpcode::CreateSampler => {
                        if pkt.payload.len() < 20 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 20 bytes",
                            });
                        }
                        let sampler_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let filter = u32_le_at(pkt.payload, 4).unwrap();
                        let address_u = u32_le_at(pkt.payload, 8).unwrap();
                        let address_v = u32_le_at(pkt.payload, 12).unwrap();
                        let address_w = u32_le_at(pkt.payload, 16).unwrap();
                        let _ = write!(line, " sampler_handle={sampler_handle} filter={filter}");
                        if let Some(name) = sampler_filter_name(filter) {
                            let _ = write!(line, " filter_name={name}");
                        }
                        let _ = write!(line, " address_u={address_u}");
                        if let Some(name) = sampler_address_mode_name(address_u) {
                            let _ = write!(line, " address_u_name={name}");
                        }
                        let _ = write!(line, " address_v={address_v}");
                        if let Some(name) = sampler_address_mode_name(address_v) {
                            let _ = write!(line, " address_v_name={name}");
                        }
                        let _ = write!(line, " address_w={address_w}");
                        if let Some(name) = sampler_address_mode_name(address_w) {
                            let _ = write!(line, " address_w_name={name}");
                        }
                    }
                    AerogpuCmdOpcode::DestroySampler => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let sampler_handle = u32_le_at(pkt.payload, 0).unwrap();
                        let _ = write!(line, " sampler_handle={sampler_handle}");
                    }
                    AerogpuCmdOpcode::SetSamplers => {
                        let (cmd, handles) =
                            pkt.decode_set_samplers_payload_le().map_err(|err| {
                                CmdStreamDecodeError::Payload {
                                    offset,
                                    opcode,
                                    err,
                                }
                            })?;
                        // Avoid taking references to packed fields.
                        let shader_stage = cmd.shader_stage;
                        let start_slot = cmd.start_slot;
                        let sampler_count = cmd.sampler_count;
                        let stage_ex = cmd.reserved0;
                        let _ = write!(line, " shader_stage={shader_stage}");
                        if let Some(name) = shader_stage_name(shader_stage) {
                            let _ = write!(line, " shader_stage_name={name}");
                        }
                        let _ = write!(
                            line,
                            " start_slot={start_slot} sampler_count={sampler_count}"
                        );
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && shader_stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }
                        if let Some(first) = handles.first() {
                            let sampler0 = *first;
                            let _ = write!(line, " sampler0={sampler0}");
                        }
                    }
                    AerogpuCmdOpcode::SetShaderConstantsF => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let stage = u32_le_at(pkt.payload, 0).unwrap();
                        let start_register = u32_le_at(pkt.payload, 4).unwrap();
                        let vec4_count = u32_le_at(pkt.payload, 8).unwrap();
                        let stage_ex = u32_le_at(pkt.payload, 12).unwrap();
                        let float_count = vec4_count.checked_mul(4).ok_or(
                            CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "vec4_count overflow",
                            },
                        )? as usize;
                        let payload_len = 16usize
                            .checked_add(float_count.checked_mul(4).ok_or(
                                CmdStreamDecodeError::MalformedPayload {
                                    offset,
                                    opcode,
                                    msg: "vec4_count overflow",
                                },
                            )?)
                            .ok_or(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "vec4_count overflow",
                            })?;
                        if pkt.payload.len() < payload_len {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "payload truncated for vec4_count",
                            });
                        }
                        let _ = write!(line, " stage={stage}");
                        if let Some(name) = shader_stage_name(stage) {
                            let _ = write!(line, " stage_name={name}");
                        }
                        let _ = write!(
                            line,
                            " start_register={start_register} vec4_count={vec4_count}"
                        );
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }
                        let data = &pkt.payload[16..payload_len];
                        let _ = write!(
                            line,
                            " data_len={} data_prefix={}",
                            data.len(),
                            hex_prefix(data, 16)
                        );
                    }
                    AerogpuCmdOpcode::SetShaderConstantsI => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let stage = u32_le_at(pkt.payload, 0).unwrap();
                        let start_register = u32_le_at(pkt.payload, 4).unwrap();
                        let vec4_count = u32_le_at(pkt.payload, 8).unwrap();
                        let stage_ex = u32_le_at(pkt.payload, 12).unwrap();
                        let int_count = vec4_count.checked_mul(4).ok_or(
                            CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "vec4_count overflow",
                            },
                        )? as usize;
                        let payload_len = 16usize
                            .checked_add(int_count.checked_mul(4).ok_or(
                                CmdStreamDecodeError::MalformedPayload {
                                    offset,
                                    opcode,
                                    msg: "vec4_count overflow",
                                },
                            )?)
                            .ok_or(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "vec4_count overflow",
                            })?;
                        if pkt.payload.len() < payload_len {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "payload truncated for vec4_count",
                            });
                        }
                        let _ = write!(line, " stage={stage}");
                        if let Some(name) = shader_stage_name(stage) {
                            let _ = write!(line, " stage_name={name}");
                        }
                        let _ = write!(
                            line,
                            " start_register={start_register} vec4_count={vec4_count}"
                        );
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }
                        let data = &pkt.payload[16..payload_len];
                        let _ = write!(
                            line,
                            " data_len={} data_prefix={}",
                            data.len(),
                            hex_prefix(data, 16)
                        );
                    }
                    AerogpuCmdOpcode::SetShaderConstantsB => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let stage = u32_le_at(pkt.payload, 0).unwrap();
                        let start_register = u32_le_at(pkt.payload, 4).unwrap();
                        let bool_count = u32_le_at(pkt.payload, 8).unwrap();
                        let stage_ex = u32_le_at(pkt.payload, 12).unwrap();
                        let payload_len = 16usize
                            .checked_add((bool_count as usize).checked_mul(16).ok_or(
                                CmdStreamDecodeError::MalformedPayload {
                                    offset,
                                    opcode,
                                    msg: "bool_count overflow",
                                },
                            )?)
                            .ok_or(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "bool_count overflow",
                            })?;
                        if pkt.payload.len() < payload_len {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "payload truncated for bool_count",
                            });
                        }
                        let _ = write!(line, " stage={stage}");
                        if let Some(name) = shader_stage_name(stage) {
                            let _ = write!(line, " stage_name={name}");
                        }
                        let _ = write!(
                            line,
                            " start_register={start_register} bool_count={bool_count}"
                        );
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }
                        let data = &pkt.payload[16..payload_len];
                        let _ = write!(
                            line,
                            " data_len={} data_prefix={}",
                            data.len(),
                            hex_prefix(data, 16)
                        );
                    }
                    AerogpuCmdOpcode::SetConstantBuffers => {
                        let (cmd, bindings) = pkt
                            .decode_set_constant_buffers_payload_le()
                            .map_err(|err| CmdStreamDecodeError::Payload {
                                offset,
                                opcode,
                                err,
                            })?;
                        // Avoid taking references to packed fields.
                        let shader_stage = cmd.shader_stage;
                        let start_slot = cmd.start_slot;
                        let buffer_count = cmd.buffer_count;
                        let stage_ex = cmd.reserved0;

                        let _ = write!(line, " shader_stage={shader_stage}");
                        if let Some(name) = shader_stage_name(shader_stage) {
                            let _ = write!(line, " shader_stage_name={name}");
                        }
                        let _ =
                            write!(line, " start_slot={start_slot} buffer_count={buffer_count}");
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && shader_stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }

                        if let Some(b0) = bindings.first() {
                            // Avoid taking references to packed fields.
                            let cb0_buffer = b0.buffer;
                            let cb0_offset_bytes = b0.offset_bytes;
                            let cb0_size_bytes = b0.size_bytes;
                            let _ = write!(
                                line,
                                " cb0_buffer={cb0_buffer} cb0_offset_bytes={cb0_offset_bytes} cb0_size_bytes={cb0_size_bytes}"
                            );
                        }
                    }
                    AerogpuCmdOpcode::SetShaderResourceBuffers => {
                        let (cmd, bindings) = pkt
                            .decode_set_shader_resource_buffers_payload_le()
                            .map_err(|err| CmdStreamDecodeError::Payload {
                            offset,
                            opcode,
                            err,
                        })?;
                        // Avoid taking references to packed fields.
                        let shader_stage = cmd.shader_stage;
                        let start_slot = cmd.start_slot;
                        let buffer_count = cmd.buffer_count;
                        let stage_ex = cmd.reserved0;

                        let _ = write!(line, " shader_stage={shader_stage}");
                        if let Some(name) = shader_stage_name(shader_stage) {
                            let _ = write!(line, " shader_stage_name={name}");
                        }
                        let _ =
                            write!(line, " start_slot={start_slot} buffer_count={buffer_count}");
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && shader_stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }

                        if let Some(b0) = bindings.first() {
                            // Avoid taking references to packed fields.
                            let srv0_buffer = b0.buffer;
                            let srv0_offset_bytes = b0.offset_bytes;
                            let srv0_size_bytes = b0.size_bytes;
                            let _ = write!(
                                line,
                                " srv0_buffer={srv0_buffer} srv0_offset_bytes={srv0_offset_bytes} srv0_size_bytes={srv0_size_bytes}"
                            );
                        }
                    }
                    AerogpuCmdOpcode::SetUnorderedAccessBuffers => {
                        let (cmd, bindings) = pkt
                            .decode_set_unordered_access_buffers_payload_le()
                            .map_err(|err| CmdStreamDecodeError::Payload {
                                offset,
                                opcode,
                                err,
                            })?;
                        // Avoid taking references to packed fields.
                        let shader_stage = cmd.shader_stage;
                        let start_slot = cmd.start_slot;
                        let uav_count = cmd.uav_count;
                        let stage_ex = cmd.reserved0;

                        let _ = write!(line, " shader_stage={shader_stage}");
                        if let Some(name) = shader_stage_name(shader_stage) {
                            let _ = write!(line, " shader_stage_name={name}");
                        }
                        let _ = write!(line, " start_slot={start_slot} uav_count={uav_count}");
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR
                            && shader_stage == 2
                            && stage_ex != 0
                        {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }

                        if let Some(b0) = bindings.first() {
                            // Avoid taking references to packed fields.
                            let uav0_buffer = b0.buffer;
                            let uav0_offset_bytes = b0.offset_bytes;
                            let uav0_size_bytes = b0.size_bytes;
                            let uav0_initial_count = b0.initial_count;
                            let _ = write!(
                                line,
                                " uav0_buffer={uav0_buffer} uav0_offset_bytes={uav0_offset_bytes} uav0_size_bytes={uav0_size_bytes} uav0_initial_count={uav0_initial_count}"
                            );
                        }
                    }
                    AerogpuCmdOpcode::Dispatch => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let group_count_x = u32_le_at(pkt.payload, 0).unwrap();
                        let group_count_y = u32_le_at(pkt.payload, 4).unwrap();
                        let group_count_z = u32_le_at(pkt.payload, 8).unwrap();
                        let stage_ex = u32_le_at(pkt.payload, 12).unwrap();
                        let _ = write!(
                            line,
                            " group_count_x={group_count_x} group_count_y={group_count_y} group_count_z={group_count_z}"
                        );
                        // `DISPATCH.reserved0` is repurposed as a `stage_ex` selector for
                        // extended-stage compute execution (GS/HS/DS compute emulation). Gate
                        // interpretation by ABI minor to avoid misinterpreting garbage in older
                        // cmd streams.
                        if abi_minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR && stage_ex != 0 {
                            let _ = write!(
                                line,
                                " stage_ex={stage_ex} stage_ex_name={}",
                                stage_ex_name(stage_ex)
                            );
                        } else if stage_ex != 0 {
                            let _ = write!(line, " reserved0=0x{stage_ex:08X}");
                        }
                    }
                    AerogpuCmdOpcode::SetVertexBuffers => {
                        let (cmd, bindings) =
                            pkt.decode_set_vertex_buffers_payload_le().map_err(|err| {
                                CmdStreamDecodeError::Payload {
                                    offset,
                                    opcode,
                                    err,
                                }
                            })?;
                        let start_slot = cmd.start_slot;
                        let buffer_count = cmd.buffer_count;
                        let _ =
                            write!(line, " start_slot={start_slot} buffer_count={buffer_count}");
                        if let Some(b0) = bindings.first() {
                            // Avoid taking references to packed fields.
                            let vb0_buffer = b0.buffer;
                            let vb0_stride_bytes = b0.stride_bytes;
                            let vb0_offset_bytes = b0.offset_bytes;
                            let _ = write!(
                                line,
                                " vb0_buffer={vb0_buffer} vb0_stride_bytes={vb0_stride_bytes} vb0_offset_bytes={vb0_offset_bytes}"
                            );
                        }
                    }
                    AerogpuCmdOpcode::SetIndexBuffer => {
                        if pkt.payload.len() < 16 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 16 bytes",
                            });
                        }
                        let buffer = u32_le_at(pkt.payload, 0).unwrap();
                        let format = u32_le_at(pkt.payload, 4).unwrap();
                        let offset_bytes = u32_le_at(pkt.payload, 8).unwrap();
                        let reserved0 = u32_le_at(pkt.payload, 12).unwrap();
                        let _ = write!(
                            line,
                            " buffer={buffer} format={format} offset_bytes={offset_bytes}"
                        );
                        if let Some(name) = index_format_name(format) {
                            let _ = write!(line, " format_name={name}");
                        }
                        if reserved0 != 0 {
                            let _ = write!(line, " reserved0=0x{reserved0:08X}");
                        }
                    }
                    AerogpuCmdOpcode::SetPrimitiveTopology => {
                        if pkt.payload.len() < 8 {
                            return Err(CmdStreamDecodeError::MalformedPayload {
                                offset,
                                opcode,
                                msg: "expected at least 8 bytes",
                            });
                        }
                        let topology = u32_le_at(pkt.payload, 0).unwrap();
                        let _ = write!(line, " topology={topology}");
                        if let Some(name) = topology_name(topology) {
                            let _ = write!(line, " topology_name={name}");
                        }
                    }

                    _ => {
                        // Decoders for the less-common opcodes can be added as needed; keep the
                        // listing stable and always show opcode_id/size_bytes for forward-compat.
                        let _ = write!(line, " payload_len={}", pkt.payload.len());
                    }
                }
            }
            None => {
                if strict {
                    return Err(CmdStreamDecodeError::UnknownOpcode { offset, opcode_id });
                }
                let _ = write!(
                    line,
                    "0x{offset:08X} Unknown size_bytes={size_bytes} opcode_id=0x{opcode_id:08X} payload_len={}",
                    pkt.payload.len()
                );
            }
        }

        out.push_str(&line);
        out.push('\n');

        offset = offset
            .checked_add(size_bytes as usize)
            .ok_or(CmdStreamDecodeError::OffsetOverflow { offset, size_bytes })?;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
    use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

    fn push_u32_le(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u64_le(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn push_packet(out: &mut Vec<u8>, opcode: u32, payload: &[u8]) {
        let size_bytes = 8u32 + payload.len() as u32;
        assert_eq!(size_bytes % 4, 0, "packet size must be 4-byte aligned");
        push_u32_le(out, opcode);
        push_u32_le(out, size_bytes);
        out.extend_from_slice(payload);
    }

    #[test]
    fn software_executor_resolves_texture_views_in_set_render_targets() {
        // Regression guard: newer command streams may bind RTs via a texture view handle rather
        // than the base texture handle.
        let mut bytes = Vec::new();
        push_u32_le(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
        push_u32_le(&mut bytes, AEROGPU_ABI_VERSION_U32);
        push_u32_le(&mut bytes, 0); // patched later
        push_u32_le(&mut bytes, 0); // flags
        push_u32_le(&mut bytes, 0); // reserved0
        push_u32_le(&mut bytes, 0); // reserved1

        // CREATE_TEXTURE2D(texture_handle=1, format=R8G8B8A8_UNORM, 4x4, mip=1, array=1).
        let mut payload = Vec::new();
        push_u32_le(&mut payload, 1); // texture_handle
        push_u32_le(&mut payload, 0); // usage_flags
        push_u32_le(&mut payload, AerogpuFormat::R8G8B8A8Unorm as u32);
        push_u32_le(&mut payload, 4); // width
        push_u32_le(&mut payload, 4); // height
        push_u32_le(&mut payload, 1); // mip_levels
        push_u32_le(&mut payload, 1); // array_layers
        push_u32_le(&mut payload, 0); // row_pitch_bytes
        push_u32_le(&mut payload, 0); // backing_alloc_id
        push_u32_le(&mut payload, 0); // backing_offset_bytes
        push_u64_le(&mut payload, 0); // reserved0
        assert_eq!(payload.len(), 48);
        push_packet(
            &mut bytes,
            AerogpuCmdOpcode::CreateTexture2d as u32,
            &payload,
        );

        // CREATE_TEXTURE_VIEW(view_handle=2, texture_handle=1, format=R8G8B8A8_UNORM, mip 0..1, layer 0..1).
        payload.clear();
        push_u32_le(&mut payload, 2); // view_handle
        push_u32_le(&mut payload, 1); // texture_handle
        push_u32_le(&mut payload, AerogpuFormat::R8G8B8A8Unorm as u32);
        push_u32_le(&mut payload, 0); // base_mip_level
        push_u32_le(&mut payload, 1); // mip_level_count
        push_u32_le(&mut payload, 0); // base_array_layer
        push_u32_le(&mut payload, 1); // array_layer_count
        push_u64_le(&mut payload, 0); // reserved0
        assert_eq!(payload.len(), 36);
        push_packet(
            &mut bytes,
            AerogpuCmdOpcode::CreateTextureView as u32,
            &payload,
        );

        // SET_RENDER_TARGETS(color_count=1, colors[0]=view_handle=2).
        payload.clear();
        push_u32_le(&mut payload, 1); // color_count
        push_u32_le(&mut payload, 0); // depth_stencil
        push_u32_le(&mut payload, 2); // colors[0] = view_handle
        for _ in 1..aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_MAX_RENDER_TARGETS {
            push_u32_le(&mut payload, 0);
        }
        assert_eq!(payload.len(), 40);
        push_packet(
            &mut bytes,
            AerogpuCmdOpcode::SetRenderTargets as u32,
            &payload,
        );

        // CLEAR(COLOR=green).
        payload.clear();
        push_u32_le(&mut payload, AEROGPU_CLEAR_COLOR);
        push_u32_le(&mut payload, 0.0f32.to_bits()); // r
        push_u32_le(&mut payload, 1.0f32.to_bits()); // g
        push_u32_le(&mut payload, 0.0f32.to_bits()); // b
        push_u32_le(&mut payload, 1.0f32.to_bits()); // a
        push_u32_le(&mut payload, 1.0f32.to_bits()); // depth
        push_u32_le(&mut payload, 0); // stencil
        assert_eq!(payload.len(), 28);
        push_packet(&mut bytes, AerogpuCmdOpcode::Clear as u32, &payload);

        // PRESENT.
        payload.clear();
        push_u32_le(&mut payload, 0); // scanout_id
        push_u32_le(&mut payload, 0); // flags
        assert_eq!(payload.len(), 8);
        push_packet(&mut bytes, AerogpuCmdOpcode::Present as u32, &payload);

        // Patch header.size_bytes.
        let size_bytes = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());

        let mut exec = AerogpuSoftwareExecutor::new();
        let mem = SubmissionMemory::default();
        exec.process_cmd_stream(&bytes, &mem)
            .expect("process cmd stream");
        let frame = exec.take_presented_frame().expect("presented frame");
        assert_eq!(frame.width, 4);
        assert_eq!(frame.height, 4);
        assert_eq!(&frame.rgba8[..4], &[0, 255, 0, 255]);
    }
}
