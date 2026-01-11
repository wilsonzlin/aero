use std::collections::HashMap;

use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_ring as ring};
use memory::MemoryBus;

use crate::devices::aerogpu_regs::{irq_bits, AeroGpuRegs};
use crate::devices::aerogpu_ring::AeroGpuSubmitDesc;
use crate::devices::aerogpu_scanout::AeroGpuFormat;

const MAX_ALLOC_TABLE_SIZE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CMD_STREAM_SIZE_BYTES: usize = 64 * 1024 * 1024;
const MAX_VERTEX_BUFFER_SLOTS: usize = 16;

#[derive(Clone, Copy, Debug)]
struct AllocInfo {
    flags: u32,
    gpa: u64,
    size_bytes: u64,
}

#[derive(Clone, Debug)]
struct GuestBacking {
    #[allow(dead_code)]
    alloc_id: u32,
    alloc_flags: u32,
    gpa: u64,
    size_bytes: u64,
}

#[derive(Clone, Debug)]
struct BufferResource {
    size_bytes: u64,
    backing: Option<GuestBacking>,
    host_data: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Texture2DResource {
    width: u32,
    height: u32,
    format: AeroGpuFormat,
    row_pitch_bytes: u32,
    backing: Option<GuestBacking>,
    data: Vec<u8>,
    dirty: bool,
}

#[derive(Clone, Debug)]
struct ShaderResource {
    #[allow(dead_code)]
    stage: u32,
    #[allow(dead_code)]
    dxbc: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct InputElement {
    input_slot: u32,
    aligned_byte_offset: u32,
    dxgi_format: u32,
}

#[derive(Clone, Debug, Default)]
struct ParsedInputLayout {
    position: Option<InputElement>,
    color: Option<InputElement>,
}

#[derive(Clone, Debug)]
struct InputLayoutResource {
    #[allow(dead_code)]
    blob: Vec<u8>,
    parsed: ParsedInputLayout,
}

#[derive(Clone, Copy, Debug, Default)]
struct VertexBufferBinding {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u32,
}

#[derive(Clone, Copy, Debug)]
struct IndexBufferBinding {
    buffer: u32,
    format: u32,
    offset_bytes: u32,
}

#[derive(Clone, Copy, Debug)]
struct Viewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    #[allow(dead_code)]
    min_depth: f32,
    #[allow(dead_code)]
    max_depth: f32,
}

#[derive(Clone, Copy, Debug)]
struct Scissor {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Debug)]
struct Vertex {
    pos: (f32, f32),
    color: [f32; 4],
}

#[derive(Clone, Debug)]
struct PipelineState {
    render_targets: [u32; cmd::AEROGPU_MAX_RENDER_TARGETS],
    viewport: Option<Viewport>,
    scissor: Option<Scissor>,
    topology: u32,
    vertex_buffers: [VertexBufferBinding; MAX_VERTEX_BUFFER_SLOTS],
    index_buffer: Option<IndexBufferBinding>,
    input_layout: u32,
    vs: u32,
    ps: u32,
    cs: u32,
}

impl Default for PipelineState {
    fn default() -> Self {
        Self {
            render_targets: [0; cmd::AEROGPU_MAX_RENDER_TARGETS],
            viewport: None,
            scissor: None,
            topology: 4, // AEROGPU_TOPOLOGY_TRIANGLELIST
            vertex_buffers: [VertexBufferBinding::default(); MAX_VERTEX_BUFFER_SLOTS],
            index_buffer: None,
            input_layout: 0,
            vs: 0,
            ps: 0,
            cs: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AeroGpuSoftwareExecutor {
    buffers: HashMap<u32, BufferResource>,
    textures: HashMap<u32, Texture2DResource>,
    shaders: HashMap<u32, ShaderResource>,
    input_layouts: HashMap<u32, InputLayoutResource>,
    shared_surfaces: HashMap<u64, u32>,
    resource_aliases: HashMap<u32, u32>,
    state: PipelineState,
}

impl AeroGpuSoftwareExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.buffers.clear();
        self.textures.clear();
        self.shaders.clear();
        self.input_layouts.clear();
        self.shared_surfaces.clear();
        self.resource_aliases.clear();
        self.state = PipelineState::default();
    }

    fn resolve_handle(&self, mut handle: u32) -> u32 {
        // Imported resources can alias existing handles. Keep resolution bounded.
        for _ in 0..8 {
            let Some(&next) = self.resource_aliases.get(&handle) else {
                break;
            };
            handle = next;
        }
        handle
    }

    fn parse_alloc_table(
        &self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        desc: &AeroGpuSubmitDesc,
    ) -> HashMap<u32, AllocInfo> {
        let gpa = desc.alloc_table_gpa;
        let size_bytes = desc.alloc_table_size_bytes;
        if gpa == 0 || size_bytes == 0 {
            return HashMap::new();
        }

        let size: usize = match usize::try_from(size_bytes) {
            Ok(v) => v,
            Err(_) => {
                regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
                regs.irq_status |= irq_bits::ERROR;
                return HashMap::new();
            }
        };
        if size < ring::AerogpuAllocTableHeader::SIZE_BYTES {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
            return HashMap::new();
        }
        if size > MAX_ALLOC_TABLE_SIZE_BYTES {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
            return HashMap::new();
        }

        let mut buf = vec![0u8; size];
        mem.read_physical(gpa, &mut buf);
        let hdr = ring::AerogpuAllocTableHeader {
            magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            abi_version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            size_bytes: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            entry_count: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            entry_stride_bytes: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            reserved0: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
        };

        if hdr.magic != ring::AEROGPU_ALLOC_TABLE_MAGIC {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
            return HashMap::new();
        }
        if (hdr.abi_version >> 16) != (regs.abi_version >> 16) {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
            return HashMap::new();
        }
        let total_size = hdr.size_bytes as usize;
        if total_size > size || total_size < ring::AerogpuAllocTableHeader::SIZE_BYTES {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
            return HashMap::new();
        }
        if hdr.entry_stride_bytes as usize != ring::AerogpuAllocEntry::SIZE_BYTES {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
            return HashMap::new();
        }

        let mut out = HashMap::new();
        let mut off = ring::AerogpuAllocTableHeader::SIZE_BYTES;
        for _ in 0..hdr.entry_count {
            if off + ring::AerogpuAllocEntry::SIZE_BYTES > total_size {
                regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
                regs.irq_status |= irq_bits::ERROR;
                break;
            }
            let entry = ring::AerogpuAllocEntry {
                alloc_id: u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()),
                flags: u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()),
                gpa: u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap()),
                size_bytes: u64::from_le_bytes(buf[off + 16..off + 24].try_into().unwrap()),
                reserved0: u64::from_le_bytes(buf[off + 24..off + 32].try_into().unwrap()),
            };
            off += ring::AerogpuAllocEntry::SIZE_BYTES;

            if entry.alloc_id == 0 || entry.gpa == 0 || entry.size_bytes == 0 {
                continue;
            }
            out.insert(
                entry.alloc_id,
                AllocInfo {
                    flags: entry.flags,
                    gpa: entry.gpa,
                    size_bytes: entry.size_bytes,
                },
            );
        }
        out
    }

    fn record_error(regs: &mut AeroGpuRegs) {
        regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
        regs.irq_status |= irq_bits::ERROR;
    }

    fn cmd_read_u32(buf: &[u8], off: usize) -> Option<u32> {
        buf.get(off..off + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    }

    fn cmd_read_i32(buf: &[u8], off: usize) -> Option<i32> {
        Self::cmd_read_u32(buf, off).map(|v| v as i32)
    }

    fn cmd_read_u64(buf: &[u8], off: usize) -> Option<u64> {
        buf.get(off..off + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
    }

    fn cmd_read_f32_bits(buf: &[u8], off: usize) -> Option<f32> {
        Self::cmd_read_u32(buf, off).map(f32::from_bits)
    }

    fn parse_input_layout_blob(blob: &[u8]) -> ParsedInputLayout {
        let hdr_size = std::mem::size_of::<cmd::AerogpuInputLayoutBlobHeader>();
        let elem_size = std::mem::size_of::<cmd::AerogpuInputLayoutElementDxgi>();

        if blob.len() < hdr_size {
            return ParsedInputLayout::default();
        }
        let magic = u32::from_le_bytes(blob[0..4].try_into().unwrap());
        if magic != cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC {
            return ParsedInputLayout::default();
        }
        let version = u32::from_le_bytes(blob[4..8].try_into().unwrap());
        if version != cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION {
            return ParsedInputLayout::default();
        }
        let element_count = u32::from_le_bytes(blob[8..12].try_into().unwrap()) as usize;
        let mut off = hdr_size;
        let needed = match element_count.checked_mul(elem_size).and_then(|bytes| bytes.checked_add(off)) {
            Some(v) => v,
            None => return ParsedInputLayout::default(),
        };
        if needed > blob.len() {
            return ParsedInputLayout::default();
        }

        fn fnv1a32(s: &str) -> u32 {
            let mut hash = 2166136261u32;
            for b in s.as_bytes() {
                hash ^= *b as u32;
                hash = hash.wrapping_mul(16777619);
            }
            hash
        }
        let pos_hash = fnv1a32("POSITION");
        let col_hash = fnv1a32("COLOR");

        let mut out = ParsedInputLayout::default();
        for _ in 0..element_count {
            let semantic_name_hash = u32::from_le_bytes(blob[off..off + 4].try_into().unwrap());
            let semantic_index = u32::from_le_bytes(blob[off + 4..off + 8].try_into().unwrap());
            let dxgi_format = u32::from_le_bytes(blob[off + 8..off + 12].try_into().unwrap());
            let input_slot = u32::from_le_bytes(blob[off + 12..off + 16].try_into().unwrap());
            let aligned_byte_offset = u32::from_le_bytes(blob[off + 16..off + 20].try_into().unwrap());
            off += elem_size;

            if semantic_index != 0 {
                continue;
            }
            if semantic_name_hash == pos_hash {
                out.position = Some(InputElement {
                    input_slot,
                    aligned_byte_offset,
                    dxgi_format,
                });
            } else if semantic_name_hash == col_hash {
                out.color = Some(InputElement {
                    input_slot,
                    aligned_byte_offset,
                    dxgi_format,
                });
            }
        }
        out
    }

    fn flush_dirty_textures(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        for tex in self.textures.values_mut() {
            if !tex.dirty {
                continue;
            }
            let Some(backing) = tex.backing.as_ref() else {
                tex.dirty = false;
                continue;
            };
            if backing.alloc_flags & ring::AEROGPU_ALLOC_FLAG_READONLY != 0 {
                Self::record_error(regs);
                continue;
            }

            let write_len = tex.data.len() as u64;
            if write_len > backing.size_bytes {
                Self::record_error(regs);
                continue;
            }
            mem.write_physical(backing.gpa, &tex.data);
            tex.dirty = false;
        }
    }

    fn decode_color_f32_as_u8(rgba: [f32; 4]) -> [u8; 4] {
        fn f(v: f32) -> u8 {
            let v = v.clamp(0.0, 1.0);
            (v * 255.0 + 0.5).floor() as u8
        }
        [f(rgba[0]), f(rgba[1]), f(rgba[2]), f(rgba[3])]
    }

    fn write_pixel(tex: &mut Texture2DResource, x: i32, y: i32, rgba: [f32; 4]) {
        if x < 0 || y < 0 {
            return;
        }
        let (Ok(xu), Ok(yu)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        let width = tex.width as usize;
        let height = tex.height as usize;
        if xu >= width || yu >= height {
            return;
        }
        let bpp = match tex.format.bytes_per_pixel() {
            Some(v) => v,
            None => return,
        };
        if bpp != 4 {
            return;
        }
        let row_pitch = tex.row_pitch_bytes as usize;
        let off = yu * row_pitch + xu * bpp;
        if off + 4 > tex.data.len() {
            return;
        }
        let [r, g, b, a] = Self::decode_color_f32_as_u8(rgba);
        match tex.format {
            AeroGpuFormat::B8G8R8A8Unorm => {
                tex.data[off] = b;
                tex.data[off + 1] = g;
                tex.data[off + 2] = r;
                tex.data[off + 3] = a;
            }
            AeroGpuFormat::B8G8R8X8Unorm => {
                tex.data[off] = b;
                tex.data[off + 1] = g;
                tex.data[off + 2] = r;
                tex.data[off + 3] = 0xff;
            }
            AeroGpuFormat::R8G8B8A8Unorm => {
                tex.data[off] = r;
                tex.data[off + 1] = g;
                tex.data[off + 2] = b;
                tex.data[off + 3] = a;
            }
            AeroGpuFormat::R8G8B8X8Unorm => {
                tex.data[off] = r;
                tex.data[off + 1] = g;
                tex.data[off + 2] = b;
                tex.data[off + 3] = 0xff;
            }
            _ => {}
        }
    }

    fn clear_texture(tex: &mut Texture2DResource, rgba: [f32; 4]) {
        let width = tex.width as usize;
        let height = tex.height as usize;
        let row_pitch = tex.row_pitch_bytes as usize;
        if width == 0 || height == 0 || row_pitch < width.saturating_mul(4) {
            return;
        }
        let [r, g, b, a] = Self::decode_color_f32_as_u8(rgba);

        for y in 0..height {
            let row_start = y.saturating_mul(row_pitch);
            let row_end = row_start.saturating_add(width.saturating_mul(4));
            if row_end > tex.data.len() {
                break;
            }
            for x in 0..width {
                let off = row_start + x * 4;
                match tex.format {
                    AeroGpuFormat::B8G8R8A8Unorm => {
                        tex.data[off] = b;
                        tex.data[off + 1] = g;
                        tex.data[off + 2] = r;
                        tex.data[off + 3] = a;
                    }
                    AeroGpuFormat::B8G8R8X8Unorm => {
                        tex.data[off] = b;
                        tex.data[off + 1] = g;
                        tex.data[off + 2] = r;
                        tex.data[off + 3] = 0xff;
                    }
                    AeroGpuFormat::R8G8B8A8Unorm => {
                        tex.data[off] = r;
                        tex.data[off + 1] = g;
                        tex.data[off + 2] = b;
                        tex.data[off + 3] = a;
                    }
                    AeroGpuFormat::R8G8B8X8Unorm => {
                        tex.data[off] = r;
                        tex.data[off + 1] = g;
                        tex.data[off + 2] = b;
                        tex.data[off + 3] = 0xff;
                    }
                    _ => return,
                }
            }
        }
    }

    fn rasterize_triangle(
        tex: &mut Texture2DResource,
        clip: (i32, i32, i32, i32),
        v0: (f32, f32),
        v1: (f32, f32),
        v2: (f32, f32),
        c0: [f32; 4],
        c1: [f32; 4],
        c2: [f32; 4],
    ) {
        fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
            (bx - ax) * (py - ay) - (by - ay) * (px - ax)
        }

        let area = edge(v0.0, v0.1, v1.0, v1.1, v2.0, v2.1);
        if area == 0.0 {
            return;
        }

        let (sign, inv_area) = if area < 0.0 {
            (-1.0f32, 1.0f32 / (-area))
        } else {
            (1.0f32, 1.0f32 / area)
        };

        let min_x = v0.0.min(v1.0).min(v2.0).floor() as i32;
        let max_x = v0.0.max(v1.0).max(v2.0).ceil() as i32;
        let min_y = v0.1.min(v1.1).min(v2.1).floor() as i32;
        let max_y = v0.1.max(v1.1).max(v2.1).ceil() as i32;

        let (clip_x0, clip_y0, clip_x1, clip_y1) = clip;
        let start_x = min_x.max(clip_x0);
        let end_x = max_x.min(clip_x1);
        let start_y = min_y.max(clip_y0);
        let end_y = max_y.min(clip_y1);
        if start_x >= end_x || start_y >= end_y {
            return;
        }

        for y in start_y..end_y {
            for x in start_x..end_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let w0 = edge(v1.0, v1.1, v2.0, v2.1, px, py) * sign;
                let w1 = edge(v2.0, v2.1, v0.0, v0.1, px, py) * sign;
                let w2 = edge(v0.0, v0.1, v1.0, v1.1, px, py) * sign;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let w0 = w0 * inv_area;
                let w1 = w1 * inv_area;
                let w2 = w2 * inv_area;
                let mut out = [0.0f32; 4];
                for i in 0..4 {
                    out[i] = c0[i] * w0 + c1[i] * w1 + c2[i] * w2;
                }
                Self::write_pixel(tex, x, y, out);
            }
        }
    }

    fn draw_triangle_list(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        vertex_indices: &[i32],
    ) {
        let rt_handle = self.resolve_handle(self.state.render_targets[0]);
        if rt_handle == 0 {
            return;
        }

        let (tex_width, tex_height) = match self.textures.get(&rt_handle) {
            Some(tex) => (tex.width, tex.height),
            None => {
                Self::record_error(regs);
                return;
            }
        };

        let vp = self.state.viewport.unwrap_or(Viewport {
            x: 0.0,
            y: 0.0,
            width: tex_width as f32,
            height: tex_height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        });

        let mut clip_x0 = vp.x.floor() as i32;
        let mut clip_y0 = vp.y.floor() as i32;
        let mut clip_x1 = (vp.x + vp.width).ceil() as i32;
        let mut clip_y1 = (vp.y + vp.height).ceil() as i32;

        clip_x0 = clip_x0.max(0);
        clip_y0 = clip_y0.max(0);
        clip_x1 = clip_x1.min(tex_width as i32);
        clip_y1 = clip_y1.min(tex_height as i32);

        if let Some(sc) = self.state.scissor {
            let sc_x0 = sc.x.max(0);
            let sc_y0 = sc.y.max(0);
            let sc_x1 = (sc.x.saturating_add(sc.width)).min(tex_width as i32);
            let sc_y1 = (sc.y.saturating_add(sc.height)).min(tex_height as i32);
            clip_x0 = clip_x0.max(sc_x0);
            clip_y0 = clip_y0.max(sc_y0);
            clip_x1 = clip_x1.min(sc_x1);
            clip_y1 = clip_y1.min(sc_y1);
        }

        if clip_x0 >= clip_x1 || clip_y0 >= clip_y1 {
            return;
        }

        let input_layout_handle = self.resolve_handle(self.state.input_layout);
        let parsed_layout = if input_layout_handle == 0 {
            None
        } else {
            self.input_layouts.get(&input_layout_handle).map(|l| l.parsed.clone())
        };

        let mut vertices: Vec<Vertex> = Vec::new();
        vertices.reserve(vertex_indices.len());

        for &idx in vertex_indices {
            if idx < 0 {
                continue;
            }
            let idx_u32 = idx as u32;

            // D3D11 path: ILAY blob present and contains POSITION+COLOR.
            if let Some(layout) = parsed_layout.as_ref() {
                if let (Some(pos_el), Some(col_el)) = (layout.position, layout.color) {
                    let pos = match self.read_vertex_elem_f32x2(mem, pos_el, idx_u32) {
                        Some(v) => v,
                        None => continue,
                    };
                    let color = match self.read_vertex_elem_f32x4(mem, col_el, idx_u32) {
                        Some(v) => v,
                        None => continue,
                    };
                    // NDC -> viewport pixels.
                    let x = vp.x + (pos.0 * 0.5 + 0.5) * vp.width;
                    let y = vp.y + (1.0 - (pos.1 * 0.5 + 0.5)) * vp.height;
                    vertices.push(Vertex { pos: (x, y), color });
                    continue;
                }
            }

            // D3D9 path (FVF XYZRHW|DIFFUSE): stream 0 is {x,y,z,rhw,color_u32}.
            match self.read_vertex_d3d9(mem, idx_u32) {
                Some(v) => vertices.push(v),
                None => continue,
            }
        }

        if self.state.topology != 4 {
            // Only TRIANGLELIST supported for MVP.
            return;
        }

        let Some(tex) = self.textures.get_mut(&rt_handle) else {
            Self::record_error(regs);
            return;
        };
        for tri in vertices.chunks_exact(3) {
            Self::rasterize_triangle(
                tex,
                (clip_x0, clip_y0, clip_x1, clip_y1),
                tri[0].pos,
                tri[1].pos,
                tri[2].pos,
                tri[0].color,
                tri[1].color,
                tri[2].color,
            );
        }

        tex.dirty = true;
    }

    fn read_vertex_d3d9(&mut self, mem: &mut dyn MemoryBus, index: u32) -> Option<Vertex> {
        let binding = self.state.vertex_buffers.get(0).copied().unwrap_or_default();
        if binding.buffer == 0 {
            return None;
        }
        let handle = self.resolve_handle(binding.buffer);
        let stride = binding.stride_bytes as usize;
        if stride < 20 {
            return None;
        }
        let start = binding.offset_bytes as u64 + (index as u64) * (binding.stride_bytes as u64);

        let mut buf = [0u8; 20];
        if !self.read_buffer_bytes(mem, handle, start, &mut buf) {
            return None;
        }

        let x = f32::from_bits(u32::from_le_bytes(buf[0..4].try_into().unwrap()));
        let y = f32::from_bits(u32::from_le_bytes(buf[4..8].try_into().unwrap()));
        let color_argb = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        let a = ((color_argb >> 24) & 0xff) as f32 / 255.0;
        let r = ((color_argb >> 16) & 0xff) as f32 / 255.0;
        let g = ((color_argb >> 8) & 0xff) as f32 / 255.0;
        let b = ((color_argb >> 0) & 0xff) as f32 / 255.0;

        Some(Vertex {
            pos: (x, y),
            color: [r, g, b, a],
        })
    }

    fn read_vertex_elem_f32x2(&mut self, mem: &mut dyn MemoryBus, elem: InputElement, index: u32) -> Option<(f32, f32)> {
        if elem.dxgi_format != 16 {
            // DXGI_FORMAT_R32G32_FLOAT
            return None;
        }
        let slot = usize::try_from(elem.input_slot).ok()?;
        if slot >= self.state.vertex_buffers.len() {
            return None;
        }
        let binding = self.state.vertex_buffers[slot];
        if binding.buffer == 0 {
            return None;
        }
        let handle = self.resolve_handle(binding.buffer);
        let stride = binding.stride_bytes as u64;
        let start = binding.offset_bytes as u64 + (index as u64) * stride + elem.aligned_byte_offset as u64;
        let mut buf = [0u8; 8];
        if !self.read_buffer_bytes(mem, handle, start, &mut buf) {
            return None;
        }
        Some((
            f32::from_bits(u32::from_le_bytes(buf[0..4].try_into().unwrap())),
            f32::from_bits(u32::from_le_bytes(buf[4..8].try_into().unwrap())),
        ))
    }

    fn read_vertex_elem_f32x4(&mut self, mem: &mut dyn MemoryBus, elem: InputElement, index: u32) -> Option<[f32; 4]> {
        if elem.dxgi_format != 2 {
            // DXGI_FORMAT_R32G32B32A32_FLOAT
            return None;
        }
        let slot = usize::try_from(elem.input_slot).ok()?;
        if slot >= self.state.vertex_buffers.len() {
            return None;
        }
        let binding = self.state.vertex_buffers[slot];
        if binding.buffer == 0 {
            return None;
        }
        let handle = self.resolve_handle(binding.buffer);
        let stride = binding.stride_bytes as u64;
        let start = binding.offset_bytes as u64 + (index as u64) * stride + elem.aligned_byte_offset as u64;
        let mut buf = [0u8; 16];
        if !self.read_buffer_bytes(mem, handle, start, &mut buf) {
            return None;
        }
        Some([
            f32::from_bits(u32::from_le_bytes(buf[0..4].try_into().unwrap())),
            f32::from_bits(u32::from_le_bytes(buf[4..8].try_into().unwrap())),
            f32::from_bits(u32::from_le_bytes(buf[8..12].try_into().unwrap())),
            f32::from_bits(u32::from_le_bytes(buf[12..16].try_into().unwrap())),
        ])
    }

    fn read_buffer_bytes(&mut self, mem: &mut dyn MemoryBus, handle: u32, offset: u64, out: &mut [u8]) -> bool {
        let handle = self.resolve_handle(handle);
        if let Some(buf) = self.buffers.get(&handle) {
            if let Some(backing) = buf.backing.as_ref() {
                if offset.checked_add(out.len() as u64).is_none() || offset + out.len() as u64 > buf.size_bytes {
                    return false;
                }
                if offset + out.len() as u64 > backing.size_bytes {
                    return false;
                }
                mem.read_physical(backing.gpa + offset, out);
                return true;
            }
            let start = match usize::try_from(offset) {
                Ok(v) => v,
                Err(_) => return false,
            };
            let end = match start.checked_add(out.len()) {
                Some(v) => v,
                None => return false,
            };
            if end > buf.host_data.len() {
                return false;
            }
            out.copy_from_slice(&buf.host_data[start..end]);
            return true;
        }
        false
    }

    pub fn execute_submission(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        desc: &AeroGpuSubmitDesc,
    ) {
        if desc.cmd_gpa == 0 || desc.cmd_size_bytes == 0 {
            return;
        }
        let cmd_size: usize = match usize::try_from(desc.cmd_size_bytes) {
            Ok(v) => v,
            Err(_) => {
                Self::record_error(regs);
                return;
            }
        };
        if cmd_size > MAX_CMD_STREAM_SIZE_BYTES {
            Self::record_error(regs);
            return;
        }

        let mut buf = vec![0u8; cmd_size];
        mem.read_physical(desc.cmd_gpa, &mut buf);

        let stream_hdr = match cmd::decode_cmd_stream_header_le(&buf) {
            Ok(v) => v,
            Err(_) => {
                Self::record_error(regs);
                return;
            }
        };

        let total = stream_hdr.size_bytes as usize;
        if total > cmd_size {
            Self::record_error(regs);
            return;
        }

        let allocs = self.parse_alloc_table(regs, mem, desc);

        let mut offset = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
        while offset < total {
            let hdr = match cmd::decode_cmd_hdr_le(&buf[offset..total]) {
                Ok(v) => v,
                Err(_) => {
                    Self::record_error(regs);
                    break;
                }
            };
            let size_bytes = hdr.size_bytes as usize;
            if size_bytes == 0 {
                Self::record_error(regs);
                break;
            }
            if offset + size_bytes > total {
                Self::record_error(regs);
                break;
            }
            let packet = &buf[offset..offset + size_bytes];
            if !self.dispatch_cmd(regs, mem, &allocs, packet) {
                break;
            }
            offset += size_bytes;
        }

        self.flush_dirty_textures(regs, mem);
    }

    fn dispatch_cmd(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
        packet: &[u8],
    ) -> bool {
        if packet.len() < cmd::AerogpuCmdHdr::SIZE_BYTES {
            Self::record_error(regs);
            return false;
        }

        let opcode = u32::from_le_bytes(packet[0..4].try_into().unwrap());

        let Some(op) = cmd::AerogpuCmdOpcode::from_u32(opcode) else {
            // Unknown opcode: forward-compatible skip.
            return true;
        };

        match op {
            cmd::AerogpuCmdOpcode::Nop | cmd::AerogpuCmdOpcode::DebugMarker => {}
            cmd::AerogpuCmdOpcode::CreateBuffer => {
                if packet.len() < 40 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let size_bytes = Self::cmd_read_u64(packet, 16).unwrap_or(0);
                let backing_alloc_id = Self::cmd_read_u32(packet, 24).unwrap_or(0);
                let backing_offset_bytes = Self::cmd_read_u32(packet, 28).unwrap_or(0) as u64;

                if handle == 0 || size_bytes == 0 {
                    return true;
                }

                let mut backing = None;
                if backing_alloc_id != 0 {
                    let Some(alloc) = allocs.get(&backing_alloc_id) else {
                        Self::record_error(regs);
                        return true;
                    };
                    let Some(end) = backing_offset_bytes.checked_add(size_bytes) else {
                        Self::record_error(regs);
                        return true;
                    };
                    if end > alloc.size_bytes {
                        Self::record_error(regs);
                        return true;
                    }
                    backing = Some(GuestBacking {
                        alloc_id: backing_alloc_id,
                        alloc_flags: alloc.flags,
                        gpa: alloc.gpa + backing_offset_bytes,
                        size_bytes: size_bytes,
                    });
                }

                let host_data = if backing.is_some() {
                    Vec::new()
                } else {
                    match usize::try_from(size_bytes).ok() {
                        Some(sz) => vec![0u8; sz],
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    }
                };

                self.buffers.insert(
                    handle,
                    BufferResource {
                        size_bytes,
                        backing,
                        host_data,
                    },
                );
            }
            cmd::AerogpuCmdOpcode::CreateTexture2d => {
                if packet.len() < 56 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let format_u32 = Self::cmd_read_u32(packet, 16).unwrap_or(0);
                let width = Self::cmd_read_u32(packet, 20).unwrap_or(0);
                let height = Self::cmd_read_u32(packet, 24).unwrap_or(0);
                let mip_levels = Self::cmd_read_u32(packet, 28).unwrap_or(1);
                let array_layers = Self::cmd_read_u32(packet, 32).unwrap_or(1);
                let row_pitch_bytes = Self::cmd_read_u32(packet, 36).unwrap_or(0);
                let backing_alloc_id = Self::cmd_read_u32(packet, 40).unwrap_or(0);
                let backing_offset_bytes = Self::cmd_read_u32(packet, 44).unwrap_or(0) as u64;

                if handle == 0 || width == 0 || height == 0 {
                    return true;
                }

                // MVP: mipmapped/array textures are parsed but treated as invalid for rendering.
                if mip_levels != 1 || array_layers != 1 {
                    Self::record_error(regs);
                    return true;
                }

                let format = AeroGpuFormat::from_u32(format_u32);
                let Some(bpp) = format.bytes_per_pixel() else {
                    Self::record_error(regs);
                    return true;
                };
                if bpp != 4 {
                    Self::record_error(regs);
                    return true;
                }

                let min_pitch = width.saturating_mul(bpp as u32);
                let row_pitch_bytes = if row_pitch_bytes == 0 { min_pitch } else { row_pitch_bytes };
                if row_pitch_bytes < min_pitch {
                    Self::record_error(regs);
                    return true;
                }

                let total_bytes = (row_pitch_bytes as u64).saturating_mul(height as u64);
                let mut backing = None;
                if backing_alloc_id != 0 {
                    let Some(alloc) = allocs.get(&backing_alloc_id) else {
                        Self::record_error(regs);
                        return true;
                    };
                    let Some(end) = backing_offset_bytes.checked_add(total_bytes) else {
                        Self::record_error(regs);
                        return true;
                    };
                    if end > alloc.size_bytes {
                        Self::record_error(regs);
                        return true;
                    }
                    backing = Some(GuestBacking {
                        alloc_id: backing_alloc_id,
                        alloc_flags: alloc.flags,
                        gpa: alloc.gpa + backing_offset_bytes,
                        size_bytes: total_bytes,
                    });
                }

                let total_usize = match usize::try_from(total_bytes).ok() {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                let mut data = vec![0u8; total_usize];
                if let Some(b) = backing.as_ref() {
                    mem.read_physical(b.gpa, &mut data);
                }

                self.textures.insert(
                    handle,
                    Texture2DResource {
                        width,
                        height,
                        format,
                        row_pitch_bytes,
                        backing,
                        data,
                        dirty: false,
                    },
                );
            }
            cmd::AerogpuCmdOpcode::DestroyResource => {
                if packet.len() < 16 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let resolved = self.resolve_handle(handle);
                self.buffers.remove(&resolved);
                self.textures.remove(&resolved);
                self.resource_aliases.remove(&handle);
                self.state.render_targets.iter_mut().for_each(|rt| {
                    if *rt == handle || *rt == resolved {
                        *rt = 0;
                    }
                });
                for vb in self.state.vertex_buffers.iter_mut() {
                    if vb.buffer == handle || vb.buffer == resolved {
                        *vb = VertexBufferBinding::default();
                    }
                }
                if self.state.input_layout == handle || self.state.input_layout == resolved {
                    self.state.input_layout = 0;
                }
            }
            cmd::AerogpuCmdOpcode::ResourceDirtyRange => {
                if packet.len() < 32 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let offset = Self::cmd_read_u64(packet, 16).unwrap_or(0);
                let size = Self::cmd_read_u64(packet, 24).unwrap_or(0);
                let handle = self.resolve_handle(handle);
                if size == 0 {
                    return true;
                }
                if let Some(tex) = self.textures.get_mut(&handle) {
                    let Some(backing) = tex.backing.as_ref() else {
                        return true;
                    };
                    let end = match offset.checked_add(size) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    if end > tex.data.len() as u64 || end > backing.size_bytes {
                        Self::record_error(regs);
                        return true;
                    }
                    let start_usize = offset as usize;
                    let end_usize = end as usize;
                    mem.read_physical(backing.gpa + offset, &mut tex.data[start_usize..end_usize]);
                }
            }
            cmd::AerogpuCmdOpcode::UploadResource => {
                if packet.len() < 32 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let offset = Self::cmd_read_u64(packet, 16).unwrap_or(0);
                let size = Self::cmd_read_u64(packet, 24).unwrap_or(0);
                let payload_off = 32;
                let payload_size = match usize::try_from(size).ok() {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                if payload_off + payload_size > packet.len() {
                    Self::record_error(regs);
                    return true;
                }
                let payload = &packet[payload_off..payload_off + payload_size];
                let handle = self.resolve_handle(handle);

                if let Some(buf) = self.buffers.get_mut(&handle) {
                    let end = match offset.checked_add(size) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    if end > buf.size_bytes {
                        Self::record_error(regs);
                        return true;
                    }
                    if let Some(backing) = buf.backing.as_ref() {
                        if backing.alloc_flags & ring::AEROGPU_ALLOC_FLAG_READONLY != 0 {
                            Self::record_error(regs);
                            return true;
                        }
                        if end > backing.size_bytes {
                            Self::record_error(regs);
                            return true;
                        }
                        mem.write_physical(backing.gpa + offset, payload);
                    } else {
                        let start = offset as usize;
                        let end = end as usize;
                        if end > buf.host_data.len() {
                            Self::record_error(regs);
                            return true;
                        }
                        buf.host_data[start..end].copy_from_slice(payload);
                    }
                } else if let Some(tex) = self.textures.get_mut(&handle) {
                    let end = match offset.checked_add(size) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    if end > tex.data.len() as u64 {
                        Self::record_error(regs);
                        return true;
                    }
                    let start = offset as usize;
                    let end_usize = end as usize;
                    tex.data[start..end_usize].copy_from_slice(payload);
                    tex.dirty = true;
                }
            }
            cmd::AerogpuCmdOpcode::CopyBuffer => {
                if packet.len() < 48 {
                    Self::record_error(regs);
                    return false;
                }

                let dst = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let src = Self::cmd_read_u32(packet, 12).unwrap_or(0);
                let dst_offset = Self::cmd_read_u64(packet, 16).unwrap_or(0);
                let src_offset = Self::cmd_read_u64(packet, 24).unwrap_or(0);
                let size = Self::cmd_read_u64(packet, 32).unwrap_or(0);
                let flags = Self::cmd_read_u32(packet, 40).unwrap_or(0);

                if size == 0 {
                    return true;
                }

                let size_usize = match usize::try_from(size).ok() {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };

                let dst_handle = self.resolve_handle(dst);
                let src_handle = self.resolve_handle(src);

                let Some(src_buf) = self.buffers.get(&src_handle) else {
                    Self::record_error(regs);
                    return true;
                };
                let Some(dst_buf) = self.buffers.get(&dst_handle) else {
                    Self::record_error(regs);
                    return true;
                };

                let src_end = match src_offset.checked_add(size) {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                let dst_end = match dst_offset.checked_add(size) {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };

                if src_end > src_buf.size_bytes || dst_end > dst_buf.size_bytes {
                    Self::record_error(regs);
                    return true;
                }

                if flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST != 0 && dst_buf.backing.is_none() {
                    Self::record_error(regs);
                    return true;
                }

                let mut tmp = vec![0u8; size_usize];
                if let Some(backing) = src_buf.backing.as_ref() {
                    if src_end > backing.size_bytes {
                        Self::record_error(regs);
                        return true;
                    }
                    mem.read_physical(backing.gpa + src_offset, &mut tmp);
                } else {
                    let start = match usize::try_from(src_offset).ok() {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    let end = match start.checked_add(size_usize) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    if end > src_buf.host_data.len() {
                        Self::record_error(regs);
                        return true;
                    }
                    tmp.copy_from_slice(&src_buf.host_data[start..end]);
                }

                if let Some(backing) = self
                    .buffers
                    .get(&dst_handle)
                    .and_then(|buf| buf.backing.as_ref())
                    .cloned()
                {
                    if backing.alloc_flags & ring::AEROGPU_ALLOC_FLAG_READONLY != 0 {
                        Self::record_error(regs);
                        return true;
                    }
                    if dst_end > backing.size_bytes {
                        Self::record_error(regs);
                        return true;
                    }
                    mem.write_physical(backing.gpa + dst_offset, &tmp);
                } else if let Some(buf) = self.buffers.get_mut(&dst_handle) {
                    let start = match usize::try_from(dst_offset).ok() {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    let end = match start.checked_add(size_usize) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    if end > buf.host_data.len() {
                        Self::record_error(regs);
                        return true;
                    }
                    buf.host_data[start..end].copy_from_slice(&tmp);
                }
            }
            cmd::AerogpuCmdOpcode::CopyTexture2d => {
                if packet.len() < 64 {
                    Self::record_error(regs);
                    return false;
                }

                let dst = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let src = Self::cmd_read_u32(packet, 12).unwrap_or(0);
                let dst_mip_level = Self::cmd_read_u32(packet, 16).unwrap_or(0);
                let dst_array_layer = Self::cmd_read_u32(packet, 20).unwrap_or(0);
                let src_mip_level = Self::cmd_read_u32(packet, 24).unwrap_or(0);
                let src_array_layer = Self::cmd_read_u32(packet, 28).unwrap_or(0);
                let dst_x = Self::cmd_read_u32(packet, 32).unwrap_or(0);
                let dst_y = Self::cmd_read_u32(packet, 36).unwrap_or(0);
                let src_x = Self::cmd_read_u32(packet, 40).unwrap_or(0);
                let src_y = Self::cmd_read_u32(packet, 44).unwrap_or(0);
                let width = Self::cmd_read_u32(packet, 48).unwrap_or(0);
                let height = Self::cmd_read_u32(packet, 52).unwrap_or(0);
                let flags = Self::cmd_read_u32(packet, 56).unwrap_or(0);

                if width == 0 || height == 0 {
                    return true;
                }
                if dst_mip_level != 0 || dst_array_layer != 0 || src_mip_level != 0 || src_array_layer != 0 {
                    Self::record_error(regs);
                    return true;
                }

                let dst_handle = self.resolve_handle(dst);
                let src_handle = self.resolve_handle(src);

                let (src_format, src_pitch, src_w, src_h, src_region) = {
                    let Some(src_tex) = self.textures.get(&src_handle) else {
                        Self::record_error(regs);
                        return true;
                    };
                    let Some(bpp) = src_tex.format.bytes_per_pixel() else {
                        Self::record_error(regs);
                        return true;
                    };
                    if bpp != 4 {
                        Self::record_error(regs);
                        return true;
                    }
                    let row_bytes = match width.checked_mul(bpp as u32) {
                        Some(v) => v as usize,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    let height_usize = match usize::try_from(height).ok() {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    let region_size = match row_bytes.checked_mul(height_usize) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };

                    let src_x_end = match src_x.checked_add(width) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    let src_y_end = match src_y.checked_add(height) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    if src_x_end > src_tex.width || src_y_end > src_tex.height {
                        Self::record_error(regs);
                        return true;
                    }
                    let pitch = src_tex.row_pitch_bytes as usize;
                    if pitch < src_tex.width as usize * bpp {
                        Self::record_error(regs);
                        return true;
                    }

                    let mut tmp = vec![0u8; region_size];
                    for row in 0..height_usize {
                        let sy = src_y as usize + row;
                        let src_off = sy * pitch + (src_x as usize) * bpp;
                        let dst_off = row * row_bytes;
                        if src_off + row_bytes > src_tex.data.len() || dst_off + row_bytes > tmp.len() {
                            Self::record_error(regs);
                            return true;
                        }
                        tmp[dst_off..dst_off + row_bytes]
                            .copy_from_slice(&src_tex.data[src_off..src_off + row_bytes]);
                    }
                    (src_tex.format, pitch, src_tex.width, src_tex.height, tmp)
                };

                let Some(dst_tex) = self.textures.get(&dst_handle) else {
                    Self::record_error(regs);
                    return true;
                };
                if dst_tex.format != src_format {
                    Self::record_error(regs);
                    return true;
                }
                let Some(bpp) = dst_tex.format.bytes_per_pixel() else {
                    Self::record_error(regs);
                    return true;
                };
                if bpp != 4 {
                    Self::record_error(regs);
                    return true;
                }
                let dst_x_end = match dst_x.checked_add(width) {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                let dst_y_end = match dst_y.checked_add(height) {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                if dst_x_end > dst_tex.width || dst_y_end > dst_tex.height {
                    Self::record_error(regs);
                    return true;
                }

                if flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST != 0 && dst_tex.backing.is_none() {
                    Self::record_error(regs);
                    return true;
                }

                let dst_pitch = dst_tex.row_pitch_bytes as usize;
                let row_bytes = width as usize * bpp;
                if dst_pitch < dst_tex.width as usize * bpp {
                    Self::record_error(regs);
                    return true;
                }

                let Some(dst_tex) = self.textures.get_mut(&dst_handle) else {
                    Self::record_error(regs);
                    return true;
                };
                let height_usize = height as usize;
                for row in 0..height_usize {
                    let dy = dst_y as usize + row;
                    let dst_off = dy * dst_pitch + (dst_x as usize) * bpp;
                    let src_off = row * row_bytes;
                    if dst_off + row_bytes > dst_tex.data.len()
                        || src_off + row_bytes > src_region.len()
                    {
                        Self::record_error(regs);
                        return true;
                    }
                    dst_tex.data[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&src_region[src_off..src_off + row_bytes]);
                }
                dst_tex.dirty = true;

                let _ = (src_pitch, src_w, src_h);
            }
            cmd::AerogpuCmdOpcode::CreateShaderDxbc => {
                if packet.len() < 24 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let stage = Self::cmd_read_u32(packet, 12).unwrap_or(0);
                let dxbc_size = Self::cmd_read_u32(packet, 16).unwrap_or(0) as usize;
                let payload_off = 24;
                if payload_off + dxbc_size > packet.len() {
                    Self::record_error(regs);
                    return true;
                }
                let dxbc = packet[payload_off..payload_off + dxbc_size].to_vec();
                if handle != 0 {
                    self.shaders.insert(handle, ShaderResource { stage, dxbc });
                }
            }
            cmd::AerogpuCmdOpcode::DestroyShader => {
                if packet.len() < 16 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                self.shaders.remove(&handle);
            }
            cmd::AerogpuCmdOpcode::BindShaders => {
                if packet.len() < 24 {
                    Self::record_error(regs);
                    return false;
                }
                self.state.vs = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                self.state.ps = Self::cmd_read_u32(packet, 12).unwrap_or(0);
                self.state.cs = Self::cmd_read_u32(packet, 16).unwrap_or(0);
            }
            cmd::AerogpuCmdOpcode::SetShaderConstantsF => {
                // Currently ignored by the software backend.
            }
            cmd::AerogpuCmdOpcode::CreateInputLayout => {
                if packet.len() < 20 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let blob_size = Self::cmd_read_u32(packet, 12).unwrap_or(0) as usize;
                let payload_off = 20;
                if payload_off + blob_size > packet.len() {
                    Self::record_error(regs);
                    return true;
                }
                let blob = packet[payload_off..payload_off + blob_size].to_vec();
                let parsed = Self::parse_input_layout_blob(&blob);
                if handle != 0 {
                    self.input_layouts.insert(handle, InputLayoutResource { blob, parsed });
                }
            }
            cmd::AerogpuCmdOpcode::DestroyInputLayout => {
                if packet.len() < 16 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                self.input_layouts.remove(&handle);
                if self.state.input_layout == handle {
                    self.state.input_layout = 0;
                }
            }
            cmd::AerogpuCmdOpcode::SetInputLayout => {
                if packet.len() < 16 {
                    Self::record_error(regs);
                    return false;
                }
                self.state.input_layout = Self::cmd_read_u32(packet, 8).unwrap_or(0);
            }
            cmd::AerogpuCmdOpcode::SetBlendState
            | cmd::AerogpuCmdOpcode::SetDepthStencilState
            | cmd::AerogpuCmdOpcode::SetRasterizerState
            | cmd::AerogpuCmdOpcode::SetTexture
            | cmd::AerogpuCmdOpcode::SetSamplerState
            | cmd::AerogpuCmdOpcode::SetRenderState => {
                // Parsed but currently ignored by the software backend.
            }
            cmd::AerogpuCmdOpcode::SetRenderTargets => {
                if packet.len() < 48 {
                    Self::record_error(regs);
                    return false;
                }
                // color_count ignored for now; we accept RT0 and clear the rest.
                for i in 0..cmd::AEROGPU_MAX_RENDER_TARGETS {
                    let off = 16 + i * 4;
                    self.state.render_targets[i] = Self::cmd_read_u32(packet, off).unwrap_or(0);
                }
            }
            cmd::AerogpuCmdOpcode::SetViewport => {
                if packet.len() < 32 {
                    Self::record_error(regs);
                    return false;
                }
                let x = Self::cmd_read_f32_bits(packet, 8).unwrap_or(0.0);
                let y = Self::cmd_read_f32_bits(packet, 12).unwrap_or(0.0);
                let width = Self::cmd_read_f32_bits(packet, 16).unwrap_or(0.0);
                let height = Self::cmd_read_f32_bits(packet, 20).unwrap_or(0.0);
                let min_depth = Self::cmd_read_f32_bits(packet, 24).unwrap_or(0.0);
                let max_depth = Self::cmd_read_f32_bits(packet, 28).unwrap_or(1.0);
                self.state.viewport = Some(Viewport {
                    x,
                    y,
                    width,
                    height,
                    min_depth,
                    max_depth,
                });
            }
            cmd::AerogpuCmdOpcode::SetScissor => {
                if packet.len() < 24 {
                    Self::record_error(regs);
                    return false;
                }
                let x = Self::cmd_read_i32(packet, 8).unwrap_or(0);
                let y = Self::cmd_read_i32(packet, 12).unwrap_or(0);
                let width = Self::cmd_read_i32(packet, 16).unwrap_or(0);
                let height = Self::cmd_read_i32(packet, 20).unwrap_or(0);
                self.state.scissor = Some(Scissor { x, y, width, height });
            }
            cmd::AerogpuCmdOpcode::SetVertexBuffers => {
                if packet.len() < 16 {
                    Self::record_error(regs);
                    return false;
                }
                let start_slot = Self::cmd_read_u32(packet, 8).unwrap_or(0) as usize;
                let buffer_count = Self::cmd_read_u32(packet, 12).unwrap_or(0) as usize;
                let binding_size = std::mem::size_of::<cmd::AerogpuVertexBufferBinding>();
                let expected = match buffer_count
                    .checked_mul(binding_size)
                    .and_then(|bytes| bytes.checked_add(16))
                {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                if expected > packet.len() {
                    Self::record_error(regs);
                    return true;
                }

                for i in 0..buffer_count {
                    let slot = start_slot + i;
                    if slot >= self.state.vertex_buffers.len() {
                        continue;
                    }
                    let base = 16 + i * binding_size;
                    let buffer = Self::cmd_read_u32(packet, base).unwrap_or(0);
                    let stride_bytes = Self::cmd_read_u32(packet, base + 4).unwrap_or(0);
                    let offset_bytes = Self::cmd_read_u32(packet, base + 8).unwrap_or(0);
                    self.state.vertex_buffers[slot] = VertexBufferBinding {
                        buffer,
                        stride_bytes,
                        offset_bytes,
                    };
                }
            }
            cmd::AerogpuCmdOpcode::SetIndexBuffer => {
                if packet.len() < 24 {
                    Self::record_error(regs);
                    return false;
                }
                let buffer = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let format = Self::cmd_read_u32(packet, 12).unwrap_or(0);
                let offset_bytes = Self::cmd_read_u32(packet, 16).unwrap_or(0);
                if buffer == 0 {
                    self.state.index_buffer = None;
                } else {
                    self.state.index_buffer = Some(IndexBufferBinding {
                        buffer,
                        format,
                        offset_bytes,
                    });
                }
            }
            cmd::AerogpuCmdOpcode::SetPrimitiveTopology => {
                if packet.len() < 16 {
                    Self::record_error(regs);
                    return false;
                }
                self.state.topology = Self::cmd_read_u32(packet, 8).unwrap_or(self.state.topology);
            }
            cmd::AerogpuCmdOpcode::Clear => {
                if packet.len() < 36 {
                    Self::record_error(regs);
                    return false;
                }
                let flags = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                if flags & cmd::AEROGPU_CLEAR_COLOR == 0 {
                    return true;
                }
                let r = Self::cmd_read_f32_bits(packet, 12).unwrap_or(0.0);
                let g = Self::cmd_read_f32_bits(packet, 16).unwrap_or(0.0);
                let b = Self::cmd_read_f32_bits(packet, 20).unwrap_or(0.0);
                let a = Self::cmd_read_f32_bits(packet, 24).unwrap_or(1.0);
                let rt_handle = self.resolve_handle(self.state.render_targets[0]);
                let Some(tex) = self.textures.get_mut(&rt_handle) else {
                    return true;
                };
                Self::clear_texture(tex, [r, g, b, a]);
                tex.dirty = true;
            }
            cmd::AerogpuCmdOpcode::Draw => {
                if packet.len() < 24 {
                    Self::record_error(regs);
                    return false;
                }
                let vertex_count = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let first_vertex = Self::cmd_read_u32(packet, 16).unwrap_or(0);
                if vertex_count == 0 {
                    return true;
                }
                let mut idxs = Vec::with_capacity(vertex_count as usize);
                for i in 0..vertex_count {
                    idxs.push((first_vertex + i) as i32);
                }
                self.draw_triangle_list(regs, mem, &idxs);
            }
            cmd::AerogpuCmdOpcode::DrawIndexed => {
                if packet.len() < 28 {
                    Self::record_error(regs);
                    return false;
                }
                let index_count = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let first_index = Self::cmd_read_u32(packet, 16).unwrap_or(0);
                let base_vertex = Self::cmd_read_i32(packet, 20).unwrap_or(0);
                if index_count == 0 {
                    return true;
                }
                let Some(ib) = self.state.index_buffer else {
                    return true;
                };
                let ib_handle = self.resolve_handle(ib.buffer);
                let index_size = match ib.format {
                    0 => 2usize, // UINT16
                    1 => 4usize, // UINT32
                    _ => 2usize,
                };
                let mut idxs = Vec::with_capacity(index_count as usize);
                let mut tmp = vec![0u8; index_size];
                for i in 0..index_count {
                    let idx_off = ib.offset_bytes as u64
                        + ((first_index + i) as u64) * (index_size as u64);
                    if !self.read_buffer_bytes(mem, ib_handle, idx_off, &mut tmp) {
                        break;
                    }
                    let raw = if index_size == 2 {
                        u16::from_le_bytes(tmp[0..2].try_into().unwrap()) as i32
                    } else {
                        i32::from_le_bytes(tmp[0..4].try_into().unwrap())
                    };
                    idxs.push(raw.wrapping_add(base_vertex));
                }
                self.draw_triangle_list(regs, mem, &idxs);
            }
            cmd::AerogpuCmdOpcode::ExportSharedSurface => {
                if packet.len() < 24 {
                    Self::record_error(regs);
                    return false;
                }
                let handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let token = Self::cmd_read_u64(packet, 16).unwrap_or(0);
                if handle != 0 && token != 0 {
                    self.shared_surfaces.insert(token, self.resolve_handle(handle));
                }
            }
            cmd::AerogpuCmdOpcode::ImportSharedSurface => {
                if packet.len() < 24 {
                    Self::record_error(regs);
                    return false;
                }
                let out_handle = Self::cmd_read_u32(packet, 8).unwrap_or(0);
                let token = Self::cmd_read_u64(packet, 16).unwrap_or(0);
                if out_handle == 0 || token == 0 {
                    return true;
                }
                let Some(&src_handle) = self.shared_surfaces.get(&token) else {
                    Self::record_error(regs);
                    return true;
                };
                self.resource_aliases.insert(out_handle, src_handle);
            }
            cmd::AerogpuCmdOpcode::Present
            | cmd::AerogpuCmdOpcode::PresentEx
            | cmd::AerogpuCmdOpcode::Flush => {
                // No-op for software backend (work already executes at submit boundaries).
            }
        }
        true
    }

}
