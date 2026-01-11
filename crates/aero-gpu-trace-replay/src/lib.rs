use aero_gpu_trace::{BlobKind, TraceReadError, TraceReader, TraceRecord};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AerogpuPrimitiveTopology,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Seek};

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
                    out.push(ReplayedFrame {
                        frame_index,
                        width: frame.width,
                        height: frame.height,
                        rgba8: frame.rgba8,
                    });
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
        let mut iter = AerogpuCmdStreamIter::new(bytes)
            .map_err(|err| format!("failed to decode cmd stream: {err:?}"))?;

        // Track offsets for diagnostics (iterator itself doesn't expose it).
        let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
        while let Some(packet) = iter.next() {
            let packet = packet
                .map_err(|err| format!("cmd packet decode error at offset {offset}: {err:?}"))?;

            match packet.opcode {
                Some(AerogpuCmdOpcode::Nop) | Some(AerogpuCmdOpcode::DebugMarker) => {}
                Some(AerogpuCmdOpcode::CreateBuffer) => self.cmd_create_buffer(packet.payload, mem)?,
                Some(AerogpuCmdOpcode::CreateTexture2d) => {
                    self.cmd_create_texture2d(packet.payload, mem)?
                }
                Some(AerogpuCmdOpcode::DestroyResource) => self.cmd_destroy_resource(packet.payload)?,
                Some(AerogpuCmdOpcode::SetRenderTargets) => self.cmd_set_render_targets(packet.payload)?,
                Some(AerogpuCmdOpcode::SetViewport) => self.cmd_set_viewport(packet.payload)?,
                Some(AerogpuCmdOpcode::SetVertexBuffers) => self.cmd_set_vertex_buffers(packet.payload)?,
                Some(AerogpuCmdOpcode::SetPrimitiveTopology) => self.cmd_set_primitive_topology(packet.payload)?,
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

    fn cmd_destroy_resource(&mut self, payload: &[u8]) -> Result<(), String> {
        if payload.len() < 8 {
            return Err("DESTROY_RESOURCE payload too small".into());
        }
        let handle = read_u32(payload, 0);
        self.buffers.remove(&handle);
        self.textures.remove(&handle);
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
        let rt = read_u32(payload, 8);
        if !self.textures.contains_key(&rt) {
            return Err(format!("unknown render target texture {rt}"));
        }
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
