use std::collections::HashMap;

use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_ring as ring};
use memory::MemoryBus;

use crate::devices::aerogpu_regs::{irq_bits, AeroGpuRegs};
use crate::devices::aerogpu_ring::AeroGpuSubmitDesc;
use crate::devices::aerogpu_scanout::AeroGpuFormat;

const MAX_ALLOC_TABLE_SIZE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CMD_STREAM_SIZE_BYTES: usize = 64 * 1024 * 1024;
const MAX_VERTEX_BUFFER_SLOTS: usize = 16;
const MAX_TEXTURE_SLOTS: usize = 16;

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
    texcoord0: Option<InputElement>,
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
    depth: f32,
    uv: (f32, f32),
    color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
struct BlendState {
    enable: bool,
    src_factor: u32,
    dst_factor: u32,
    blend_op: u32,
    write_mask: u8,
}

impl Default for BlendState {
    fn default() -> Self {
        Self {
            enable: false,
            // "no blend" is handled by enable=false, but keep a sensible default anyway.
            src_factor: cmd::AerogpuBlendFactor::One as u32,
            dst_factor: cmd::AerogpuBlendFactor::Zero as u32,
            blend_op: cmd::AerogpuBlendOp::Add as u32,
            write_mask: 0xF,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RasterizerState {
    cull_mode: u32,
    front_ccw: bool,
    scissor_enable: bool,
}

impl Default for RasterizerState {
    fn default() -> Self {
        // D3D11 defaults: solid fill, backface culling, clockwise front, scissor disabled.
        Self {
            cull_mode: cmd::AerogpuCullMode::Back as u32,
            front_ccw: false,
            scissor_enable: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DepthStencilState {
    depth_enable: bool,
    depth_write_enable: bool,
    depth_func: u32,
}

impl Default for DepthStencilState {
    fn default() -> Self {
        Self {
            depth_enable: false,
            depth_write_enable: false,
            depth_func: cmd::AerogpuCompareFunc::Always as u32,
        }
    }
}

#[derive(Clone, Debug)]
struct PipelineState {
    render_targets: [u32; cmd::AEROGPU_MAX_RENDER_TARGETS],
    depth_stencil: u32,
    viewport: Option<Viewport>,
    scissor: Option<Scissor>,
    topology: u32,
    blend: BlendState,
    rasterizer: RasterizerState,
    depth_stencil_state: DepthStencilState,
    textures_vs: [u32; MAX_TEXTURE_SLOTS],
    textures_ps: [u32; MAX_TEXTURE_SLOTS],
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
            depth_stencil: 0,
            viewport: None,
            scissor: None,
            topology: 4, // AEROGPU_TOPOLOGY_TRIANGLELIST
            blend: BlendState::default(),
            rasterizer: RasterizerState::default(),
            depth_stencil_state: DepthStencilState::default(),
            textures_vs: [0; MAX_TEXTURE_SLOTS],
            textures_ps: [0; MAX_TEXTURE_SLOTS],
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
    texture_refcounts: HashMap<u32, u32>,
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
        self.texture_refcounts.clear();
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
                regs.stats.malformed_submissions =
                    regs.stats.malformed_submissions.saturating_add(1);
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
        let hdr = match ring::AerogpuAllocTableHeader::decode_from_le_bytes(&buf) {
            Ok(v) => v,
            Err(_) => {
                regs.stats.malformed_submissions =
                    regs.stats.malformed_submissions.saturating_add(1);
                regs.irq_status |= irq_bits::ERROR;
                return HashMap::new();
            }
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
                regs.stats.malformed_submissions =
                    regs.stats.malformed_submissions.saturating_add(1);
                regs.irq_status |= irq_bits::ERROR;
                break;
            }
            let entry = match ring::AerogpuAllocEntry::decode_from_le_bytes(
                &buf[off..off + ring::AerogpuAllocEntry::SIZE_BYTES],
            ) {
                Ok(v) => v,
                Err(_) => {
                    regs.stats.malformed_submissions =
                        regs.stats.malformed_submissions.saturating_add(1);
                    regs.irq_status |= irq_bits::ERROR;
                    break;
                }
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

    fn read_packed_prefix<T: Copy>(buf: &[u8]) -> Option<T> {
        if buf.len() < core::mem::size_of::<T>() {
            return None;
        }

        // SAFETY: Bounds checked above and `read_unaligned` avoids alignment requirements.
        Some(unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const T) })
    }

    fn parse_input_layout_blob(blob: &[u8]) -> ParsedInputLayout {
        let hdr_size = cmd::AerogpuInputLayoutBlobHeader::SIZE_BYTES;
        let elem_size = cmd::AerogpuInputLayoutElementDxgi::SIZE_BYTES;

        if blob.len() < hdr_size {
            return ParsedInputLayout::default();
        }
        let hdr = match Self::read_packed_prefix::<cmd::AerogpuInputLayoutBlobHeader>(blob) {
            Some(v) => v,
            None => return ParsedInputLayout::default(),
        };
        let magic = u32::from_le(hdr.magic);
        if magic != cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC {
            return ParsedInputLayout::default();
        }
        let version = u32::from_le(hdr.version);
        if version != cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION {
            return ParsedInputLayout::default();
        }
        let element_count = u32::from_le(hdr.element_count) as usize;
        let mut off = hdr_size;
        let needed = match element_count
            .checked_mul(elem_size)
            .and_then(|bytes| bytes.checked_add(off))
        {
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
        let tex_hash = fnv1a32("TEXCOORD");

        let mut out = ParsedInputLayout::default();
        for _ in 0..element_count {
            let elem = match Self::read_packed_prefix::<cmd::AerogpuInputLayoutElementDxgi>(
                &blob[off..],
            ) {
                Some(v) => v,
                None => return ParsedInputLayout::default(),
            };
            let semantic_name_hash = u32::from_le(elem.semantic_name_hash);
            let semantic_index = u32::from_le(elem.semantic_index);
            let dxgi_format = u32::from_le(elem.dxgi_format);
            let input_slot = u32::from_le(elem.input_slot);
            let aligned_byte_offset = u32::from_le(elem.aligned_byte_offset);
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
            } else if semantic_name_hash == tex_hash {
                out.texcoord0 = Some(InputElement {
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

    fn texture_bytes_per_pixel(format: AeroGpuFormat) -> Option<usize> {
        match format {
            AeroGpuFormat::B8G8R8A8Unorm
            | AeroGpuFormat::B8G8R8X8Unorm
            | AeroGpuFormat::R8G8B8A8Unorm
            | AeroGpuFormat::R8G8B8X8Unorm
            | AeroGpuFormat::D24UnormS8Uint
            | AeroGpuFormat::D32Float => Some(4),
            AeroGpuFormat::B5G6R5Unorm | AeroGpuFormat::B5G5R5A1Unorm => Some(2),
            AeroGpuFormat::Invalid => None,
        }
    }

    fn decode_color_f32_as_u8(rgba: [f32; 4]) -> [u8; 4] {
        fn f(v: f32) -> u8 {
            let v = v.clamp(0.0, 1.0);
            (v * 255.0 + 0.5).floor() as u8
        }
        [f(rgba[0]), f(rgba[1]), f(rgba[2]), f(rgba[3])]
    }

    fn read_pixel_rgba_u8(tex: &Texture2DResource, off: usize) -> Option<[u8; 4]> {
        if off + 4 > tex.data.len() {
            return None;
        }
        match tex.format {
            AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8X8Unorm => Some([
                tex.data[off + 2], // r
                tex.data[off + 1], // g
                tex.data[off + 0], // b
                if matches!(tex.format, AeroGpuFormat::B8G8R8A8Unorm) {
                    tex.data[off + 3]
                } else {
                    0xff
                },
            ]),
            AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8X8Unorm => Some([
                tex.data[off + 0],
                tex.data[off + 1],
                tex.data[off + 2],
                if matches!(tex.format, AeroGpuFormat::R8G8B8A8Unorm) {
                    tex.data[off + 3]
                } else {
                    0xff
                },
            ]),
            _ => None,
        }
    }

    fn sample_texture_point_clamp(tex: &Texture2DResource, uv: (f32, f32)) -> [f32; 4] {
        if tex.width == 0 || tex.height == 0 {
            return [0.0, 0.0, 0.0, 1.0];
        }

        let u = uv.0.clamp(0.0, 1.0);
        let v = uv.1.clamp(0.0, 1.0);

        let mut x = (u * tex.width as f32).floor() as i32;
        let mut y = (v * tex.height as f32).floor() as i32;

        x = x.clamp(0, tex.width.saturating_sub(1) as i32);
        y = y.clamp(0, tex.height.saturating_sub(1) as i32);

        let off = y as usize * tex.row_pitch_bytes as usize + x as usize * 4;
        let Some(rgba_u8) = Self::read_pixel_rgba_u8(tex, off) else {
            return [0.0, 0.0, 0.0, 1.0];
        };
        [
            rgba_u8[0] as f32 / 255.0,
            rgba_u8[1] as f32 / 255.0,
            rgba_u8[2] as f32 / 255.0,
            rgba_u8[3] as f32 / 255.0,
        ]
    }

    fn write_pixel_rgba_u8(tex: &mut Texture2DResource, off: usize, rgba: [u8; 4]) {
        if off + 4 > tex.data.len() {
            return;
        }
        let [r, g, b, a] = rgba;
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

    fn blend_factor(factor: u32, src_a: f32, dst_a: f32) -> f32 {
        match factor {
            x if x == cmd::AerogpuBlendFactor::Zero as u32 => 0.0,
            x if x == cmd::AerogpuBlendFactor::One as u32 => 1.0,
            x if x == cmd::AerogpuBlendFactor::SrcAlpha as u32 => src_a,
            x if x == cmd::AerogpuBlendFactor::InvSrcAlpha as u32 => 1.0 - src_a,
            x if x == cmd::AerogpuBlendFactor::DestAlpha as u32 => dst_a,
            x if x == cmd::AerogpuBlendFactor::InvDestAlpha as u32 => 1.0 - dst_a,
            _ => 1.0,
        }
    }

    fn blend_op(op: u32, src: f32, dst: f32) -> f32 {
        match op {
            x if x == cmd::AerogpuBlendOp::Add as u32 => src + dst,
            x if x == cmd::AerogpuBlendOp::Subtract as u32 => src - dst,
            x if x == cmd::AerogpuBlendOp::RevSubtract as u32 => dst - src,
            x if x == cmd::AerogpuBlendOp::Min as u32 => src.min(dst),
            x if x == cmd::AerogpuBlendOp::Max as u32 => src.max(dst),
            _ => src + dst,
        }
    }

    fn blend_and_write_pixel(
        tex: &mut Texture2DResource,
        x: i32,
        y: i32,
        rgba: [f32; 4],
        blend: BlendState,
    ) {
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
        let Some(dst_u8) = Self::read_pixel_rgba_u8(tex, off) else {
            return;
        };
        let dst = [
            dst_u8[0] as f32 / 255.0,
            dst_u8[1] as f32 / 255.0,
            dst_u8[2] as f32 / 255.0,
            dst_u8[3] as f32 / 255.0,
        ];

        let mut out = rgba;
        if blend.enable {
            let src_a = rgba[3].clamp(0.0, 1.0);
            let dst_a = dst[3].clamp(0.0, 1.0);
            let sf = Self::blend_factor(blend.src_factor, src_a, dst_a);
            let df = Self::blend_factor(blend.dst_factor, src_a, dst_a);

            for i in 0..4 {
                let s = rgba[i].clamp(0.0, 1.0) * sf;
                let d = dst[i].clamp(0.0, 1.0) * df;
                out[i] = Self::blend_op(blend.blend_op, s, d).clamp(0.0, 1.0);
            }
        }

        let mut out_u8 = Self::decode_color_f32_as_u8(out);

        // Apply color write mask in RGBA order.
        if (blend.write_mask & 0b0001) == 0 {
            out_u8[0] = dst_u8[0];
        }
        if (blend.write_mask & 0b0010) == 0 {
            out_u8[1] = dst_u8[1];
        }
        if (blend.write_mask & 0b0100) == 0 {
            out_u8[2] = dst_u8[2];
        }
        if (blend.write_mask & 0b1000) == 0 {
            out_u8[3] = dst_u8[3];
        }

        Self::write_pixel_rgba_u8(tex, off, out_u8);
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

    fn depth_compare(func: u32, src: f32, dst: f32) -> bool {
        match func {
            x if x == cmd::AerogpuCompareFunc::Never as u32 => false,
            x if x == cmd::AerogpuCompareFunc::Less as u32 => src < dst,
            x if x == cmd::AerogpuCompareFunc::Equal as u32 => src == dst,
            x if x == cmd::AerogpuCompareFunc::LessEqual as u32 => src <= dst,
            x if x == cmd::AerogpuCompareFunc::Greater as u32 => src > dst,
            x if x == cmd::AerogpuCompareFunc::NotEqual as u32 => src != dst,
            x if x == cmd::AerogpuCompareFunc::GreaterEqual as u32 => src >= dst,
            x if x == cmd::AerogpuCompareFunc::Always as u32 => true,
            _ => true,
        }
    }

    fn read_depth_stencil(tex: &Texture2DResource, off: usize) -> Option<(f32, u8)> {
        if off + 4 > tex.data.len() {
            return None;
        }

        match tex.format {
            AeroGpuFormat::D32Float => {
                let bits = u32::from_le_bytes(tex.data[off..off + 4].try_into().unwrap());
                Some((f32::from_bits(bits), 0))
            }
            AeroGpuFormat::D24UnormS8Uint => {
                let v = u32::from_le_bytes(tex.data[off..off + 4].try_into().unwrap());
                let depth_bits = v & 0x00ff_ffff;
                let depth = depth_bits as f32 / 0x00ff_ffff as f32;
                let stencil = (v >> 24) as u8;
                Some((depth, stencil))
            }
            _ => None,
        }
    }

    fn write_depth_stencil(tex: &mut Texture2DResource, off: usize, depth: f32, stencil: u8) {
        if off + 4 > tex.data.len() {
            return;
        }

        match tex.format {
            AeroGpuFormat::D32Float => {
                let bits = depth.clamp(0.0, 1.0).to_bits();
                tex.data[off..off + 4].copy_from_slice(&bits.to_le_bytes());
            }
            AeroGpuFormat::D24UnormS8Uint => {
                let depth_bits = (depth.clamp(0.0, 1.0) * 0x00ff_ffff as f32 + 0.5).floor() as u32;
                let v = (depth_bits & 0x00ff_ffff) | ((stencil as u32) << 24);
                tex.data[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
            _ => {}
        }
    }

    fn clear_depth_stencil(tex: &mut Texture2DResource, flags: u32, depth: f32, stencil: u8) {
        let width = tex.width as usize;
        let height = tex.height as usize;
        let row_pitch = tex.row_pitch_bytes as usize;
        if width == 0 || height == 0 || row_pitch < width.saturating_mul(4) {
            return;
        }

        match tex.format {
            AeroGpuFormat::D32Float => {
                if (flags & cmd::AEROGPU_CLEAR_DEPTH) == 0 {
                    return;
                }
                let bits = depth.clamp(0.0, 1.0).to_bits().to_le_bytes();
                for y in 0..height {
                    let row_start = y.saturating_mul(row_pitch);
                    let row_end = row_start.saturating_add(width.saturating_mul(4));
                    if row_end > tex.data.len() {
                        break;
                    }
                    for x in 0..width {
                        let off = row_start + x * 4;
                        tex.data[off..off + 4].copy_from_slice(&bits);
                    }
                }
            }
            AeroGpuFormat::D24UnormS8Uint => {
                let clear_depth = (flags & cmd::AEROGPU_CLEAR_DEPTH) != 0;
                let clear_stencil = (flags & cmd::AEROGPU_CLEAR_STENCIL) != 0;
                if !clear_depth && !clear_stencil {
                    return;
                }

                let depth_bits =
                    (depth.clamp(0.0, 1.0) * 0x00ff_ffff as f32 + 0.5).floor() as u32 & 0x00ff_ffff;
                for y in 0..height {
                    let row_start = y.saturating_mul(row_pitch);
                    let row_end = row_start.saturating_add(width.saturating_mul(4));
                    if row_end > tex.data.len() {
                        break;
                    }
                    for x in 0..width {
                        let off = row_start + x * 4;
                        let old = u32::from_le_bytes(tex.data[off..off + 4].try_into().unwrap());
                        let mut v = old;
                        if clear_depth {
                            v = (v & 0xff00_0000) | depth_bits;
                        }
                        if clear_stencil {
                            v = (v & 0x00ff_ffff) | ((stencil as u32) << 24);
                        }
                        tex.data[off..off + 4].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            _ => {}
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
        blend: BlendState,
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
                Self::blend_and_write_pixel(tex, x, y, out, blend);
            }
        }
    }

    fn rasterize_triangle_depth(
        tex: &mut Texture2DResource,
        depth_tex: &mut Texture2DResource,
        depth_state: DepthStencilState,
        clip: (i32, i32, i32, i32),
        v0: (f32, f32, f32),
        v1: (f32, f32, f32),
        v2: (f32, f32, f32),
        c0: [f32; 4],
        c1: [f32; 4],
        c2: [f32; 4],
        blend: BlendState,
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

        let depth_pitch = depth_tex.row_pitch_bytes as usize;

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

                let depth = (v0.2 * w0 + v1.2 * w1 + v2.2 * w2).clamp(0.0, 1.0);

                let (Ok(xu), Ok(yu)) = (usize::try_from(x), usize::try_from(y)) else {
                    continue;
                };
                let depth_off = yu * depth_pitch + xu * 4;
                let Some((dst_depth, dst_stencil)) = Self::read_depth_stencil(depth_tex, depth_off)
                else {
                    continue;
                };
                if !Self::depth_compare(depth_state.depth_func, depth, dst_depth) {
                    continue;
                }

                if depth_state.depth_write_enable {
                    Self::write_depth_stencil(depth_tex, depth_off, depth, dst_stencil);
                    depth_tex.dirty = true;
                }

                let mut out = [0.0f32; 4];
                for i in 0..4 {
                    out[i] = c0[i] * w0 + c1[i] * w1 + c2[i] * w2;
                }
                Self::blend_and_write_pixel(tex, x, y, out, blend);
            }
        }
    }

    fn rasterize_triangle_textured(
        tex: &mut Texture2DResource,
        src_tex: &Texture2DResource,
        clip: (i32, i32, i32, i32),
        v0: (f32, f32),
        v1: (f32, f32),
        v2: (f32, f32),
        uv0: (f32, f32),
        uv1: (f32, f32),
        uv2: (f32, f32),
        blend: BlendState,
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

                let uv = (
                    uv0.0 * w0 + uv1.0 * w1 + uv2.0 * w2,
                    uv0.1 * w0 + uv1.1 * w1 + uv2.1 * w2,
                );
                let out = Self::sample_texture_point_clamp(src_tex, uv);
                Self::blend_and_write_pixel(tex, x, y, out, blend);
            }
        }
    }

    fn rasterize_triangle_depth_textured(
        tex: &mut Texture2DResource,
        depth_tex: &mut Texture2DResource,
        src_tex: &Texture2DResource,
        depth_state: DepthStencilState,
        clip: (i32, i32, i32, i32),
        v0: (f32, f32, f32),
        v1: (f32, f32, f32),
        v2: (f32, f32, f32),
        uv0: (f32, f32),
        uv1: (f32, f32),
        uv2: (f32, f32),
        blend: BlendState,
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

        let depth_pitch = depth_tex.row_pitch_bytes as usize;

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

                let depth = (v0.2 * w0 + v1.2 * w1 + v2.2 * w2).clamp(0.0, 1.0);

                let (Ok(xu), Ok(yu)) = (usize::try_from(x), usize::try_from(y)) else {
                    continue;
                };
                let depth_off = yu * depth_pitch + xu * 4;
                let Some((dst_depth, dst_stencil)) = Self::read_depth_stencil(depth_tex, depth_off)
                else {
                    continue;
                };
                if !Self::depth_compare(depth_state.depth_func, depth, dst_depth) {
                    continue;
                }

                if depth_state.depth_write_enable {
                    Self::write_depth_stencil(depth_tex, depth_off, depth, dst_stencil);
                    depth_tex.dirty = true;
                }

                let uv = (
                    uv0.0 * w0 + uv1.0 * w1 + uv2.0 * w2,
                    uv0.1 * w0 + uv1.1 * w1 + uv2.1 * w2,
                );
                let out = Self::sample_texture_point_clamp(src_tex, uv);
                Self::blend_and_write_pixel(tex, x, y, out, blend);
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

        let rast = self.state.rasterizer;
        if rast.scissor_enable {
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
        }

        if clip_x0 >= clip_x1 || clip_y0 >= clip_y1 {
            return;
        }

        let input_layout_handle = self.resolve_handle(self.state.input_layout);
        let parsed_layout = if input_layout_handle == 0 {
            None
        } else {
            self.input_layouts
                .get(&input_layout_handle)
                .map(|l| l.parsed.clone())
        };

        let mut vertices: Vec<Vertex> = Vec::new();
        vertices.reserve(vertex_indices.len());

        for &idx in vertex_indices {
            if idx < 0 {
                continue;
            }
            let idx_u32 = idx as u32;

            // D3D11 path: ILAY blob present. MVP supports POSITION, optional COLOR, optional TEXCOORD0.
            if let Some(layout) = parsed_layout.as_ref() {
                if let Some(pos_el) = layout.position {
                    let pos = match self.read_vertex_elem_position(mem, pos_el, idx_u32) {
                        Some(v) => v,
                        None => continue,
                    };
                    let color = match layout.color {
                        Some(col_el) => match self.read_vertex_elem_f32x4(mem, col_el, idx_u32) {
                            Some(v) => v,
                            None => continue,
                        },
                        None => [1.0, 1.0, 1.0, 1.0],
                    };
                    let uv = match layout.texcoord0 {
                        Some(uv_el) => match self.read_vertex_elem_f32x2(mem, uv_el, idx_u32) {
                            Some(v) => v,
                            None => continue,
                        },
                        None => (0.0, 0.0),
                    };

                    // NDC -> viewport pixels.
                    let x = vp.x + (pos.0 * 0.5 + 0.5) * vp.width;
                    let y = vp.y + (1.0 - (pos.1 * 0.5 + 0.5)) * vp.height;
                    let z = (vp.min_depth + pos.2 * (vp.max_depth - vp.min_depth)).clamp(0.0, 1.0);
                    vertices.push(Vertex {
                        pos: (x, y),
                        depth: z,
                        uv,
                        color,
                    });
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

        let blend = self.state.blend;
        let depth_state = self.state.depth_stencil_state;
        let ds_handle = self.resolve_handle(self.state.depth_stencil);
        let ps_tex0 = self.resolve_handle(self.state.textures_ps[0]);
        let wants_sampling = ps_tex0 != 0
            && ps_tex0 != rt_handle
            && ps_tex0 != ds_handle
            && parsed_layout
                .as_ref()
                .and_then(|layout| layout.texcoord0)
                .is_some();

        let can_depth_test = depth_state.depth_enable && ds_handle != 0 && ds_handle != rt_handle;

        if wants_sampling {
            let Some(src_tex) = self.textures.remove(&ps_tex0) else {
                Self::record_error(regs);
                return;
            };

            let sampleable = matches!(
                src_tex.format,
                AeroGpuFormat::B8G8R8A8Unorm
                    | AeroGpuFormat::B8G8R8X8Unorm
                    | AeroGpuFormat::R8G8B8A8Unorm
                    | AeroGpuFormat::R8G8B8X8Unorm
            );
            if !sampleable {
                self.textures.insert(ps_tex0, src_tex);
                Self::record_error(regs);
                return;
            }

            if can_depth_test {
                let Some(mut depth_tex) = self.textures.remove(&ds_handle) else {
                    self.textures.insert(ps_tex0, src_tex);
                    Self::record_error(regs);
                    return;
                };

                if !matches!(
                    depth_tex.format,
                    AeroGpuFormat::D24UnormS8Uint | AeroGpuFormat::D32Float
                ) {
                    self.textures.insert(ds_handle, depth_tex);
                    self.textures.insert(ps_tex0, src_tex);
                    Self::record_error(regs);
                    return;
                }

                // Clamp clip to depth texture bounds as well.
                let clip_x1 = clip_x1.min(depth_tex.width as i32);
                let clip_y1 = clip_y1.min(depth_tex.height as i32);
                if clip_x0 >= clip_x1 || clip_y0 >= clip_y1 {
                    self.textures.insert(ds_handle, depth_tex);
                    self.textures.insert(ps_tex0, src_tex);
                    return;
                }

                {
                    let Some(tex) = self.textures.get_mut(&rt_handle) else {
                        self.textures.insert(ds_handle, depth_tex);
                        self.textures.insert(ps_tex0, src_tex);
                        Self::record_error(regs);
                        return;
                    };

                    for tri in vertices.chunks_exact(3) {
                        if rast.cull_mode != cmd::AerogpuCullMode::None as u32 {
                            let area = (tri[1].pos.0 - tri[0].pos.0)
                                * (tri[2].pos.1 - tri[0].pos.1)
                                - (tri[1].pos.1 - tri[0].pos.1) * (tri[2].pos.0 - tri[0].pos.0);
                            if area != 0.0 {
                                let front_facing = if rast.front_ccw {
                                    area < 0.0
                                } else {
                                    area > 0.0
                                };
                                let culled = match rast.cull_mode {
                                    x if x == cmd::AerogpuCullMode::Front as u32 => front_facing,
                                    x if x == cmd::AerogpuCullMode::Back as u32 => !front_facing,
                                    _ => false,
                                };
                                if culled {
                                    continue;
                                }
                            }
                        }

                        Self::rasterize_triangle_depth_textured(
                            tex,
                            &mut depth_tex,
                            &src_tex,
                            depth_state,
                            (clip_x0, clip_y0, clip_x1, clip_y1),
                            (tri[0].pos.0, tri[0].pos.1, tri[0].depth),
                            (tri[1].pos.0, tri[1].pos.1, tri[1].depth),
                            (tri[2].pos.0, tri[2].pos.1, tri[2].depth),
                            tri[0].uv,
                            tri[1].uv,
                            tri[2].uv,
                            blend,
                        );
                    }

                    tex.dirty = true;
                }

                self.textures.insert(ds_handle, depth_tex);
            } else {
                let Some(tex) = self.textures.get_mut(&rt_handle) else {
                    self.textures.insert(ps_tex0, src_tex);
                    Self::record_error(regs);
                    return;
                };
                for tri in vertices.chunks_exact(3) {
                    if rast.cull_mode != cmd::AerogpuCullMode::None as u32 {
                        let area = (tri[1].pos.0 - tri[0].pos.0) * (tri[2].pos.1 - tri[0].pos.1)
                            - (tri[1].pos.1 - tri[0].pos.1) * (tri[2].pos.0 - tri[0].pos.0);
                        if area != 0.0 {
                            let front_facing = if rast.front_ccw {
                                area < 0.0
                            } else {
                                area > 0.0
                            };
                            let culled = match rast.cull_mode {
                                x if x == cmd::AerogpuCullMode::Front as u32 => front_facing,
                                x if x == cmd::AerogpuCullMode::Back as u32 => !front_facing,
                                _ => false,
                            };
                            if culled {
                                continue;
                            }
                        }
                    }
                    Self::rasterize_triangle_textured(
                        tex,
                        &src_tex,
                        (clip_x0, clip_y0, clip_x1, clip_y1),
                        tri[0].pos,
                        tri[1].pos,
                        tri[2].pos,
                        tri[0].uv,
                        tri[1].uv,
                        tri[2].uv,
                        blend,
                    );
                }

                tex.dirty = true;
            }

            self.textures.insert(ps_tex0, src_tex);
        } else if can_depth_test {
            let Some(mut depth_tex) = self.textures.remove(&ds_handle) else {
                Self::record_error(regs);
                return;
            };

            if !matches!(
                depth_tex.format,
                AeroGpuFormat::D24UnormS8Uint | AeroGpuFormat::D32Float
            ) {
                self.textures.insert(ds_handle, depth_tex);
                Self::record_error(regs);
                return;
            }

            // Clamp clip to depth texture bounds as well.
            let clip_x1 = clip_x1.min(depth_tex.width as i32);
            let clip_y1 = clip_y1.min(depth_tex.height as i32);
            if clip_x0 >= clip_x1 || clip_y0 >= clip_y1 {
                self.textures.insert(ds_handle, depth_tex);
                return;
            }

            {
                let Some(tex) = self.textures.get_mut(&rt_handle) else {
                    self.textures.insert(ds_handle, depth_tex);
                    Self::record_error(regs);
                    return;
                };

                for tri in vertices.chunks_exact(3) {
                    if rast.cull_mode != cmd::AerogpuCullMode::None as u32 {
                        let area = (tri[1].pos.0 - tri[0].pos.0) * (tri[2].pos.1 - tri[0].pos.1)
                            - (tri[1].pos.1 - tri[0].pos.1) * (tri[2].pos.0 - tri[0].pos.0);
                        if area != 0.0 {
                            let front_facing = if rast.front_ccw {
                                area < 0.0
                            } else {
                                area > 0.0
                            };
                            let culled = match rast.cull_mode {
                                x if x == cmd::AerogpuCullMode::Front as u32 => front_facing,
                                x if x == cmd::AerogpuCullMode::Back as u32 => !front_facing,
                                _ => false,
                            };
                            if culled {
                                continue;
                            }
                        }
                    }

                    Self::rasterize_triangle_depth(
                        tex,
                        &mut depth_tex,
                        depth_state,
                        (clip_x0, clip_y0, clip_x1, clip_y1),
                        (tri[0].pos.0, tri[0].pos.1, tri[0].depth),
                        (tri[1].pos.0, tri[1].pos.1, tri[1].depth),
                        (tri[2].pos.0, tri[2].pos.1, tri[2].depth),
                        tri[0].color,
                        tri[1].color,
                        tri[2].color,
                        blend,
                    );
                }

                tex.dirty = true;
            }

            self.textures.insert(ds_handle, depth_tex);
        } else {
            let Some(tex) = self.textures.get_mut(&rt_handle) else {
                Self::record_error(regs);
                return;
            };
            for tri in vertices.chunks_exact(3) {
                if rast.cull_mode != cmd::AerogpuCullMode::None as u32 {
                    let area = (tri[1].pos.0 - tri[0].pos.0) * (tri[2].pos.1 - tri[0].pos.1)
                        - (tri[1].pos.1 - tri[0].pos.1) * (tri[2].pos.0 - tri[0].pos.0);
                    if area != 0.0 {
                        let front_facing = if rast.front_ccw {
                            area < 0.0
                        } else {
                            area > 0.0
                        };
                        let culled = match rast.cull_mode {
                            x if x == cmd::AerogpuCullMode::Front as u32 => front_facing,
                            x if x == cmd::AerogpuCullMode::Back as u32 => !front_facing,
                            _ => false,
                        };
                        if culled {
                            continue;
                        }
                    }
                }
                Self::rasterize_triangle(
                    tex,
                    (clip_x0, clip_y0, clip_x1, clip_y1),
                    tri[0].pos,
                    tri[1].pos,
                    tri[2].pos,
                    tri[0].color,
                    tri[1].color,
                    tri[2].color,
                    blend,
                );
            }

            tex.dirty = true;
        }
    }

    fn read_vertex_d3d9(&mut self, mem: &mut dyn MemoryBus, index: u32) -> Option<Vertex> {
        let binding = self
            .state
            .vertex_buffers
            .get(0)
            .copied()
            .unwrap_or_default();
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
        let z = f32::from_bits(u32::from_le_bytes(buf[8..12].try_into().unwrap()));
        let color_argb = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        let a = ((color_argb >> 24) & 0xff) as f32 / 255.0;
        let r = ((color_argb >> 16) & 0xff) as f32 / 255.0;
        let g = ((color_argb >> 8) & 0xff) as f32 / 255.0;
        let b = ((color_argb >> 0) & 0xff) as f32 / 255.0;

        Some(Vertex {
            pos: (x, y),
            depth: z,
            uv: (0.0, 0.0),
            color: [r, g, b, a],
        })
    }

    fn read_vertex_elem_position(
        &mut self,
        mem: &mut dyn MemoryBus,
        elem: InputElement,
        index: u32,
    ) -> Option<(f32, f32, f32)> {
        let components = match elem.dxgi_format {
            16 => 2, // DXGI_FORMAT_R32G32_FLOAT
            6 => 3,  // DXGI_FORMAT_R32G32B32_FLOAT
            2 => 4,  // DXGI_FORMAT_R32G32B32A32_FLOAT
            _ => return None,
        };
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
        let start =
            binding.offset_bytes as u64 + (index as u64) * stride + elem.aligned_byte_offset as u64;
        let bytes = components * 4;
        let mut buf = [0u8; 16];
        let slice = &mut buf[..bytes];
        if !self.read_buffer_bytes(mem, handle, start, slice) {
            return None;
        }
        let x = f32::from_bits(u32::from_le_bytes(buf[0..4].try_into().unwrap()));
        let y = f32::from_bits(u32::from_le_bytes(buf[4..8].try_into().unwrap()));
        let z = if components >= 3 {
            f32::from_bits(u32::from_le_bytes(buf[8..12].try_into().unwrap()))
        } else {
            0.0
        };
        Some((x, y, z))
    }

    fn read_vertex_elem_f32x2(
        &mut self,
        mem: &mut dyn MemoryBus,
        elem: InputElement,
        index: u32,
    ) -> Option<(f32, f32)> {
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
        let start =
            binding.offset_bytes as u64 + (index as u64) * stride + elem.aligned_byte_offset as u64;
        let mut buf = [0u8; 8];
        if !self.read_buffer_bytes(mem, handle, start, &mut buf) {
            return None;
        }
        Some((
            f32::from_bits(u32::from_le_bytes(buf[0..4].try_into().unwrap())),
            f32::from_bits(u32::from_le_bytes(buf[4..8].try_into().unwrap())),
        ))
    }

    fn read_vertex_elem_f32x4(
        &mut self,
        mem: &mut dyn MemoryBus,
        elem: InputElement,
        index: u32,
    ) -> Option<[f32; 4]> {
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
        let start =
            binding.offset_bytes as u64 + (index as u64) * stride + elem.aligned_byte_offset as u64;
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

    fn read_buffer_bytes(
        &mut self,
        mem: &mut dyn MemoryBus,
        handle: u32,
        offset: u64,
        out: &mut [u8],
    ) -> bool {
        let handle = self.resolve_handle(handle);
        if let Some(buf) = self.buffers.get(&handle) {
            if let Some(backing) = buf.backing.as_ref() {
                if offset.checked_add(out.len() as u64).is_none()
                    || offset + out.len() as u64 > buf.size_bytes
                {
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

        let iter = match cmd::AerogpuCmdStreamIter::new(&buf) {
            Ok(v) => v,
            Err(_) => {
                Self::record_error(regs);
                return;
            }
        };
        let stream_size = iter.header().size_bytes as usize;

        let allocs = self.parse_alloc_table(regs, mem, desc);

        let mut offset = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
        for packet in iter {
            let packet = match packet {
                Ok(v) => v,
                Err(_) => {
                    Self::record_error(regs);
                    break;
                }
            };
            let cmd_size = packet.hdr.size_bytes as usize;
            let end = match offset.checked_add(cmd_size) {
                Some(v) => v,
                None => {
                    Self::record_error(regs);
                    break;
                }
            };
            if end > stream_size {
                Self::record_error(regs);
                break;
            }
            let Some(packet_bytes) = buf.get(offset..end) else {
                Self::record_error(regs);
                break;
            };
            if !self.dispatch_cmd(regs, mem, &allocs, packet_bytes) {
                break;
            }
            offset = end;
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
        let hdr = match cmd::decode_cmd_hdr_le(packet) {
            Ok(v) => v,
            Err(_) => {
                Self::record_error(regs);
                return false;
            }
        };

        let Some(op) = cmd::AerogpuCmdOpcode::from_u32(hdr.opcode) else {
            // Unknown opcode: forward-compatible skip.
            return true;
        };

        match op {
            cmd::AerogpuCmdOpcode::Nop | cmd::AerogpuCmdOpcode::DebugMarker => {}
            cmd::AerogpuCmdOpcode::CreateBuffer => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdCreateBuffer>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.buffer_handle);
                let size_bytes = u64::from_le(packet_cmd.size_bytes);
                let backing_alloc_id = u32::from_le(packet_cmd.backing_alloc_id);
                let backing_offset_bytes = u32::from_le(packet_cmd.backing_offset_bytes) as u64;

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
                        size_bytes,
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
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdCreateTexture2d>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.texture_handle);
                let format_u32 = u32::from_le(packet_cmd.format);
                let width = u32::from_le(packet_cmd.width);
                let height = u32::from_le(packet_cmd.height);
                let mip_levels = u32::from_le(packet_cmd.mip_levels);
                let array_layers = u32::from_le(packet_cmd.array_layers);
                let row_pitch_bytes = u32::from_le(packet_cmd.row_pitch_bytes);
                let backing_alloc_id = u32::from_le(packet_cmd.backing_alloc_id);
                let backing_offset_bytes = u32::from_le(packet_cmd.backing_offset_bytes) as u64;

                if handle == 0 || width == 0 || height == 0 {
                    return true;
                }

                // MVP: mipmapped/array textures are parsed but treated as invalid for rendering.
                if mip_levels != 1 || array_layers != 1 {
                    Self::record_error(regs);
                    return true;
                }

                let format = AeroGpuFormat::from_u32(format_u32);
                let Some(bpp) = Self::texture_bytes_per_pixel(format) else {
                    Self::record_error(regs);
                    return true;
                };
                if bpp != 4 {
                    Self::record_error(regs);
                    return true;
                }

                let min_pitch = width.saturating_mul(bpp as u32);
                let row_pitch_bytes = if row_pitch_bytes == 0 {
                    min_pitch
                } else {
                    row_pitch_bytes
                };
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
                // Track live references so shared-surface aliases can keep the
                // underlying texture alive after the original handle is destroyed.
                self.texture_refcounts.insert(handle, 1);
            }
            cmd::AerogpuCmdOpcode::DestroyResource => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdDestroyResource>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.resource_handle);
                let resolved = self.resolve_handle(handle);
                self.buffers.remove(&resolved);
                self.resource_aliases.remove(&handle);

                if let Some(count) = self.texture_refcounts.get_mut(&resolved) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.texture_refcounts.remove(&resolved);
                        self.textures.remove(&resolved);
                        self.shared_surfaces.retain(|_, v| *v != resolved);
                        self.resource_aliases.retain(|_, v| *v != resolved);
                    }
                } else {
                    self.textures.remove(&resolved);
                    self.shared_surfaces.retain(|_, v| *v != resolved);
                    self.resource_aliases.retain(|_, v| *v != resolved);
                }
                self.state.render_targets.iter_mut().for_each(|rt| {
                    if *rt == handle || *rt == resolved {
                        *rt = 0;
                    }
                });
                if self.state.depth_stencil == handle || self.state.depth_stencil == resolved {
                    self.state.depth_stencil = 0;
                }
                self.state.textures_vs.iter_mut().for_each(|tex| {
                    if *tex == handle || *tex == resolved {
                        *tex = 0;
                    }
                });
                self.state.textures_ps.iter_mut().for_each(|tex| {
                    if *tex == handle || *tex == resolved {
                        *tex = 0;
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
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdResourceDirtyRange>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.resource_handle);
                let offset = u64::from_le(packet_cmd.offset_bytes);
                let size = u64::from_le(packet_cmd.size_bytes);
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
                if packet.len() < cmd::AerogpuCmdUploadResource::SIZE_BYTES {
                    Self::record_error(regs);
                    return false;
                }
                let (packet_cmd, payload) = match cmd::decode_cmd_upload_resource_payload_le(packet)
                {
                    Ok(v) => v,
                    Err(_) => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                let handle = self.resolve_handle(packet_cmd.resource_handle);
                let offset = packet_cmd.offset_bytes;
                let size = packet_cmd.size_bytes;

                if size == 0 {
                    return true;
                }

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
                let packet_cmd = match Self::read_packed_prefix::<cmd::AerogpuCmdCopyBuffer>(packet)
                {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return false;
                    }
                };

                let dst = u32::from_le(packet_cmd.dst_buffer);
                let src = u32::from_le(packet_cmd.src_buffer);
                let dst_offset = u64::from_le(packet_cmd.dst_offset_bytes);
                let src_offset = u64::from_le(packet_cmd.src_offset_bytes);
                let size = u64::from_le(packet_cmd.size_bytes);
                let flags = u32::from_le(packet_cmd.flags);

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
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdCopyTexture2d>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };

                let dst = u32::from_le(packet_cmd.dst_texture);
                let src = u32::from_le(packet_cmd.src_texture);
                let dst_mip_level = u32::from_le(packet_cmd.dst_mip_level);
                let dst_array_layer = u32::from_le(packet_cmd.dst_array_layer);
                let src_mip_level = u32::from_le(packet_cmd.src_mip_level);
                let src_array_layer = u32::from_le(packet_cmd.src_array_layer);
                let dst_x = u32::from_le(packet_cmd.dst_x);
                let dst_y = u32::from_le(packet_cmd.dst_y);
                let src_x = u32::from_le(packet_cmd.src_x);
                let src_y = u32::from_le(packet_cmd.src_y);
                let width = u32::from_le(packet_cmd.width);
                let height = u32::from_le(packet_cmd.height);
                let flags = u32::from_le(packet_cmd.flags);

                if width == 0 || height == 0 {
                    return true;
                }
                if dst_mip_level != 0
                    || dst_array_layer != 0
                    || src_mip_level != 0
                    || src_array_layer != 0
                {
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
                    let Some(bpp) = Self::texture_bytes_per_pixel(src_tex.format) else {
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
                        if src_off + row_bytes > src_tex.data.len()
                            || dst_off + row_bytes > tmp.len()
                        {
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
                let Some(bpp) = Self::texture_bytes_per_pixel(dst_tex.format) else {
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
                if packet.len() < core::mem::size_of::<cmd::AerogpuCmdCreateShaderDxbc>() {
                    Self::record_error(regs);
                    return false;
                }
                let (packet_cmd, dxbc_bytes) =
                    match cmd::decode_cmd_create_shader_dxbc_payload_le(packet) {
                        Ok(v) => v,
                        Err(_) => {
                            Self::record_error(regs);
                            return true;
                        }
                    };

                let handle = packet_cmd.shader_handle;
                let stage = packet_cmd.stage;
                let dxbc = dxbc_bytes.to_vec();
                if handle != 0 {
                    self.shaders.insert(handle, ShaderResource { stage, dxbc });
                }
            }
            cmd::AerogpuCmdOpcode::DestroyShader => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdDestroyShader>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.shader_handle);
                self.shaders.remove(&handle);
            }
            cmd::AerogpuCmdOpcode::BindShaders => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdBindShaders>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                self.state.vs = u32::from_le(packet_cmd.vs);
                self.state.ps = u32::from_le(packet_cmd.ps);
                self.state.cs = u32::from_le(packet_cmd.cs);
            }
            cmd::AerogpuCmdOpcode::SetShaderConstantsF => {
                // Currently ignored by the software backend.
            }
            cmd::AerogpuCmdOpcode::CreateInputLayout => {
                if packet.len() < cmd::AerogpuCmdCreateInputLayout::SIZE_BYTES {
                    Self::record_error(regs);
                    return false;
                }
                let (packet_cmd, blob_bytes) =
                    match cmd::decode_cmd_create_input_layout_blob_le(packet) {
                        Ok(v) => v,
                        Err(_) => {
                            Self::record_error(regs);
                            return true;
                        }
                    };

                let handle = packet_cmd.input_layout_handle;
                let blob = blob_bytes.to_vec();
                let parsed = Self::parse_input_layout_blob(&blob);
                if handle != 0 {
                    self.input_layouts
                        .insert(handle, InputLayoutResource { blob, parsed });
                }
            }
            cmd::AerogpuCmdOpcode::DestroyInputLayout => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdDestroyInputLayout>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.input_layout_handle);
                self.input_layouts.remove(&handle);
                if self.state.input_layout == handle {
                    self.state.input_layout = 0;
                }
            }
            cmd::AerogpuCmdOpcode::SetInputLayout => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetInputLayout>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                self.state.input_layout = u32::from_le(packet_cmd.input_layout_handle);
            }
            cmd::AerogpuCmdOpcode::SetBlendState => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetBlendState>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };

                let state = packet_cmd.state;
                self.state.blend = BlendState {
                    enable: u32::from_le(state.enable) != 0,
                    src_factor: u32::from_le(state.src_factor),
                    dst_factor: u32::from_le(state.dst_factor),
                    blend_op: u32::from_le(state.blend_op),
                    write_mask: state.color_write_mask,
                };
            }
            cmd::AerogpuCmdOpcode::SetRasterizerState => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetRasterizerState>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };

                let state = packet_cmd.state;
                self.state.rasterizer = RasterizerState {
                    cull_mode: u32::from_le(state.cull_mode),
                    front_ccw: u32::from_le(state.front_ccw) != 0,
                    scissor_enable: u32::from_le(state.scissor_enable) != 0,
                };
            }
            cmd::AerogpuCmdOpcode::SetDepthStencilState => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetDepthStencilState>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };

                let state = packet_cmd.state;
                self.state.depth_stencil_state = DepthStencilState {
                    depth_enable: u32::from_le(state.depth_enable) != 0,
                    depth_write_enable: u32::from_le(state.depth_write_enable) != 0,
                    depth_func: u32::from_le(state.depth_func),
                };
            }
            cmd::AerogpuCmdOpcode::SetTexture => {
                let packet_cmd = match Self::read_packed_prefix::<cmd::AerogpuCmdSetTexture>(packet)
                {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return false;
                    }
                };

                let shader_stage = u32::from_le(packet_cmd.shader_stage);
                let slot = u32::from_le(packet_cmd.slot) as usize;
                let texture = u32::from_le(packet_cmd.texture);
                if slot >= MAX_TEXTURE_SLOTS {
                    return true;
                }
                match shader_stage {
                    x if x == cmd::AerogpuShaderStage::Vertex as u32 => {
                        self.state.textures_vs[slot] = texture;
                    }
                    x if x == cmd::AerogpuShaderStage::Pixel as u32 => {
                        self.state.textures_ps[slot] = texture;
                    }
                    _ => {}
                }
            }
            cmd::AerogpuCmdOpcode::SetSamplerState | cmd::AerogpuCmdOpcode::SetRenderState => {
                // Parsed but currently ignored by the software backend.
            }
            cmd::AerogpuCmdOpcode::SetRenderTargets => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetRenderTargets>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };

                // color_count ignored for now; we accept RT0 and clear the rest.
                self.state.depth_stencil = u32::from_le(packet_cmd.depth_stencil);
                let colors = packet_cmd.colors;
                for (dst, &src) in self.state.render_targets.iter_mut().zip(colors.iter()) {
                    *dst = u32::from_le(src);
                }
            }
            cmd::AerogpuCmdOpcode::SetViewport => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetViewport>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let x = f32::from_bits(u32::from_le(packet_cmd.x_f32));
                let y = f32::from_bits(u32::from_le(packet_cmd.y_f32));
                let width = f32::from_bits(u32::from_le(packet_cmd.width_f32));
                let height = f32::from_bits(u32::from_le(packet_cmd.height_f32));
                let min_depth = f32::from_bits(u32::from_le(packet_cmd.min_depth_f32));
                let max_depth = f32::from_bits(u32::from_le(packet_cmd.max_depth_f32));
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
                let packet_cmd = match Self::read_packed_prefix::<cmd::AerogpuCmdSetScissor>(packet)
                {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return false;
                    }
                };
                self.state.scissor = Some(Scissor {
                    x: i32::from_le(packet_cmd.x),
                    y: i32::from_le(packet_cmd.y),
                    width: i32::from_le(packet_cmd.width),
                    height: i32::from_le(packet_cmd.height),
                });
            }
            cmd::AerogpuCmdOpcode::SetVertexBuffers => {
                if packet.len() < core::mem::size_of::<cmd::AerogpuCmdSetVertexBuffers>() {
                    Self::record_error(regs);
                    return false;
                }

                let (packet_cmd, bindings) =
                    match cmd::decode_cmd_set_vertex_buffers_bindings_le(packet) {
                        Ok(v) => v,
                        Err(_) => {
                            Self::record_error(regs);
                            return true;
                        }
                    };

                let start_slot = packet_cmd.start_slot as usize;
                for (i, binding_ref) in bindings.iter().enumerate() {
                    let slot = start_slot + i;
                    if slot >= self.state.vertex_buffers.len() {
                        continue;
                    }
                    let binding = *binding_ref;
                    self.state.vertex_buffers[slot] = VertexBufferBinding {
                        buffer: u32::from_le(binding.buffer),
                        stride_bytes: u32::from_le(binding.stride_bytes),
                        offset_bytes: u32::from_le(binding.offset_bytes),
                    };
                }
            }
            cmd::AerogpuCmdOpcode::SetIndexBuffer => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetIndexBuffer>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let buffer = u32::from_le(packet_cmd.buffer);
                let format = u32::from_le(packet_cmd.format);
                let offset_bytes = u32::from_le(packet_cmd.offset_bytes);
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
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetPrimitiveTopology>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                self.state.topology = u32::from_le(packet_cmd.topology);
            }
            cmd::AerogpuCmdOpcode::Clear => {
                let packet_cmd = match Self::read_packed_prefix::<cmd::AerogpuCmdClear>(packet) {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return false;
                    }
                };
                let flags = u32::from_le(packet_cmd.flags);
                if (flags & cmd::AEROGPU_CLEAR_COLOR) != 0 {
                    let r = f32::from_bits(u32::from_le(packet_cmd.color_rgba_f32[0]));
                    let g = f32::from_bits(u32::from_le(packet_cmd.color_rgba_f32[1]));
                    let b = f32::from_bits(u32::from_le(packet_cmd.color_rgba_f32[2]));
                    let a = f32::from_bits(u32::from_le(packet_cmd.color_rgba_f32[3]));
                    let rt_handle = self.resolve_handle(self.state.render_targets[0]);
                    if let Some(tex) = self.textures.get_mut(&rt_handle) {
                        Self::clear_texture(tex, [r, g, b, a]);
                        tex.dirty = true;
                    }
                }

                if (flags & (cmd::AEROGPU_CLEAR_DEPTH | cmd::AEROGPU_CLEAR_STENCIL)) != 0 {
                    let depth = f32::from_bits(u32::from_le(packet_cmd.depth_f32));
                    let stencil = u32::from_le(packet_cmd.stencil) as u8;
                    let ds_handle = self.resolve_handle(self.state.depth_stencil);
                    if let Some(tex) = self.textures.get_mut(&ds_handle) {
                        Self::clear_depth_stencil(tex, flags, depth, stencil);
                        tex.dirty = true;
                    }
                }
            }
            cmd::AerogpuCmdOpcode::Draw => {
                let packet_cmd = match Self::read_packed_prefix::<cmd::AerogpuCmdDraw>(packet) {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return false;
                    }
                };
                let vertex_count = u32::from_le(packet_cmd.vertex_count);
                let first_vertex = u32::from_le(packet_cmd.first_vertex);
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
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdDrawIndexed>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let index_count = u32::from_le(packet_cmd.index_count);
                let first_index = u32::from_le(packet_cmd.first_index);
                let base_vertex = i32::from_le(packet_cmd.base_vertex);
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
                    let idx_off =
                        ib.offset_bytes as u64 + ((first_index + i) as u64) * (index_size as u64);
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
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdExportSharedSurface>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.resource_handle);
                let token = u64::from_le(packet_cmd.share_token);
                if handle == 0 || token == 0 {
                    return true;
                }
                let underlying = self.resolve_handle(handle);
                if !self.texture_refcounts.contains_key(&underlying) {
                    Self::record_error(regs);
                    return true;
                }
                if let Some(&existing) = self.shared_surfaces.get(&token) {
                    if existing != underlying {
                        Self::record_error(regs);
                    }
                    return true;
                }
                self.shared_surfaces.insert(token, underlying);
            }
            cmd::AerogpuCmdOpcode::ImportSharedSurface => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdImportSharedSurface>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let out_handle = u32::from_le(packet_cmd.out_resource_handle);
                let token = u64::from_le(packet_cmd.share_token);
                if out_handle == 0 || token == 0 {
                    return true;
                }
                let Some(&src_handle) = self.shared_surfaces.get(&token) else {
                    Self::record_error(regs);
                    return true;
                };
                if let Some(&existing) = self.resource_aliases.get(&out_handle) {
                    if existing != src_handle {
                        Self::record_error(regs);
                    }
                    return true;
                }
                if out_handle == src_handle {
                    // Import is idempotent if the output handle is already bound to the underlying
                    // resource (the host tracks a per-handle refcount).
                    if !self.texture_refcounts.contains_key(&src_handle) {
                        Self::record_error(regs);
                    }
                    return true;
                }
                if self.textures.contains_key(&out_handle)
                    || self.buffers.contains_key(&out_handle)
                    || self.shaders.contains_key(&out_handle)
                    || self.input_layouts.contains_key(&out_handle)
                {
                    Self::record_error(regs);
                    return true;
                }

                let Some(count) = self.texture_refcounts.get_mut(&src_handle) else {
                    Self::record_error(regs);
                    return true;
                };
                *count = count.saturating_add(1);

                self.resource_aliases.insert(out_handle, src_handle);
            }
            cmd::AerogpuCmdOpcode::ReleaseSharedSurface => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdReleaseSharedSurface>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let token = u64::from_le(packet_cmd.share_token);
                if token != 0 {
                    self.shared_surfaces.remove(&token);
                }
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
