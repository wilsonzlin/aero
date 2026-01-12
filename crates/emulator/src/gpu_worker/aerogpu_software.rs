use std::collections::{HashMap, HashSet};

use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_ring as ring};
use memory::MemoryBus;

use crate::devices::aerogpu_regs::{irq_bits, AeroGpuRegs};
use crate::devices::aerogpu_ring::AeroGpuSubmitDesc;
use crate::devices::aerogpu_scanout::AeroGpuFormat;

const MAX_ALLOC_TABLE_SIZE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CMD_STREAM_SIZE_BYTES: usize = 64 * 1024 * 1024;
// D3D11 supports 32 input-assembler vertex buffer slots. The software executor shares the
// `SET_VERTEX_BUFFERS` protocol with the D3D11 path, so keep the full range available even though
// classic D3D9 only uses 16 streams.
const MAX_VERTEX_BUFFER_SLOTS: usize = 32;
const MAX_TEXTURE_SLOTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackingAccess {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug)]
struct AllocInfo {
    flags: u32,
    gpa: u64,
    size_bytes: u64,
}

#[derive(Clone, Debug)]
struct GuestBacking {
    alloc_id: u32,
    alloc_offset_bytes: u64,
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
    mip_levels: u32,
    array_layers: u32,
    row_pitch_bytes: u32,
    backing: Option<GuestBacking>,
    data: Vec<u8>,
    dirty: bool,
}

#[derive(Clone, Copy, Debug)]
enum Texture2DLayoutFormat {
    Uncompressed { bytes_per_texel: u32 },
    BlockCompressed { block_bytes: u32 },
}

#[derive(Clone, Copy, Debug)]
struct Texture2DLinearLayout {
    mip_levels: u32,
    array_layers: u32,
    mip0_row_pitch_bytes: u32,
    total_size_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
struct Texture2DSubresourceLayout {
    mip_w: u32,
    mip_h: u32,
    /// Byte distance between successive rows within this subresource.
    ///
    /// For mip0 this is `tex.row_pitch_bytes` (which may include padding); for mip>0 this is
    /// tight-packed based on the format.
    row_pitch_bytes: u32,
    /// Number of rows in this subresource's linear layout.
    ///
    /// For uncompressed formats this is the texel height; for BC formats this is the number of
    /// 4x4 block rows (ceil(mip_h/4)).
    #[allow(dead_code)]
    rows: u32,
    /// Byte offset into `tex.data` for the start of this (layer,mip) subresource.
    offset_bytes: u64,
}

#[derive(Clone, Debug)]
struct ShaderResource {
    #[allow(dead_code)]
    stage: u32,
    #[allow(dead_code)]
    dxbc: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct SamplerResource {
    filter: u32,
    address_u: u32,
    address_v: u32,
    #[allow(dead_code)]
    address_w: u32,
}

impl Default for SamplerResource {
    fn default() -> Self {
        Self {
            filter: cmd::AerogpuSamplerFilter::Nearest as u32,
            address_u: cmd::AerogpuSamplerAddressMode::ClampToEdge as u32,
            address_v: cmd::AerogpuSamplerAddressMode::ClampToEdge as u32,
            address_w: cmd::AerogpuSamplerAddressMode::ClampToEdge as u32,
        }
    }
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
    src_factor_alpha: u32,
    dst_factor_alpha: u32,
    blend_op_alpha: u32,
    blend_constant: [f32; 4],
    sample_mask: u32,
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
            src_factor_alpha: cmd::AerogpuBlendFactor::One as u32,
            dst_factor_alpha: cmd::AerogpuBlendFactor::Zero as u32,
            blend_op_alpha: cmd::AerogpuBlendOp::Add as u32,
            blend_constant: [0.0; 4],
            sample_mask: 0xFFFF_FFFF,
            write_mask: 0xF,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RasterizerState {
    cull_mode: u32,
    front_ccw: bool,
    scissor_enable: bool,
    depth_clip_enable: bool,
}

impl Default for RasterizerState {
    fn default() -> Self {
        // D3D11 defaults: solid fill, backface culling, clockwise front, scissor disabled,
        // depth clipping enabled.
        Self {
            cull_mode: cmd::AerogpuCullMode::Back as u32,
            front_ccw: false,
            scissor_enable: false,
            depth_clip_enable: true,
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

struct DepthTarget<'a> {
    tex: &'a mut Texture2DResource,
    state: DepthStencilState,
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
    samplers_vs: [u32; MAX_TEXTURE_SLOTS],
    samplers_ps: [u32; MAX_TEXTURE_SLOTS],
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
            samplers_vs: [0; MAX_TEXTURE_SLOTS],
            samplers_ps: [0; MAX_TEXTURE_SLOTS],
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
    samplers: HashMap<u32, SamplerResource>,
    shaders: HashMap<u32, ShaderResource>,
    input_layouts: HashMap<u32, InputLayoutResource>,
    shared_surfaces: HashMap<u64, u32>,
    retired_shared_surface_tokens: HashSet<u64>,
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
        self.samplers.clear();
        self.shaders.clear();
        self.input_layouts.clear();
        self.shared_surfaces.clear();
        self.retired_shared_surface_tokens.clear();
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
            if next == handle {
                break;
            }
            handle = next;
        }
        handle
    }

    fn retire_shared_surface_tokens_for_resource(&mut self, resource_handle: u32) {
        let tokens: Vec<u64> = self
            .shared_surfaces
            .iter()
            .filter_map(|(token, handle)| (*handle == resource_handle).then_some(*token))
            .collect();
        for token in tokens {
            self.shared_surfaces.remove(&token);
            self.retired_shared_surface_tokens.insert(token);
        }
    }

    fn parse_alloc_table(
        &self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        desc: &AeroGpuSubmitDesc,
    ) -> Option<HashMap<u32, AllocInfo>> {
        let gpa = desc.alloc_table_gpa;
        let size_bytes = desc.alloc_table_size_bytes;
        if gpa == 0 && size_bytes == 0 {
            return Some(HashMap::new());
        }
        if gpa == 0 || size_bytes == 0 {
            Self::record_error(regs);
            return None;
        }
        if gpa.checked_add(u64::from(size_bytes)).is_none() {
            Self::record_error(regs);
            return None;
        }
        if size_bytes < ring::AerogpuAllocTableHeader::SIZE_BYTES as u32 {
            Self::record_error(regs);
            return None;
        }

        // Forward-compat: the submit descriptor's `alloc_table_size_bytes` is the backing buffer
        // capacity, while the allocation table header's `size_bytes` field is bytes-used.
        //
        // Only read the prefix that the header declares (bounded by the descriptor capacity) to
        // avoid copying potentially large trailing bytes.
        let mut header_bytes = [0u8; ring::AerogpuAllocTableHeader::SIZE_BYTES];
        mem.read_physical(gpa, &mut header_bytes);
        let hdr = match ring::AerogpuAllocTableHeader::decode_from_le_bytes(&header_bytes) {
            Ok(v) => v,
            Err(_) => {
                Self::record_error(regs);
                return None;
            }
        };

        if hdr.magic != ring::AEROGPU_ALLOC_TABLE_MAGIC {
            Self::record_error(regs);
            return None;
        }
        if (hdr.abi_version >> 16) != (regs.abi_version >> 16) {
            Self::record_error(regs);
            return None;
        }
        let total_size = hdr.size_bytes as usize;
        if total_size > size_bytes as usize
            || total_size < ring::AerogpuAllocTableHeader::SIZE_BYTES
        {
            Self::record_error(regs);
            return None;
        }
        if total_size > MAX_ALLOC_TABLE_SIZE_BYTES {
            Self::record_error(regs);
            return None;
        }

        let mut buf = vec![0u8; total_size];
        mem.read_physical(gpa, &mut buf);

        // Forward-compat: newer guests may extend `aerogpu_alloc_entry` by increasing the declared
        // stride and appending fields. We only require the entry prefix we understand.
        if hdr.entry_stride_bytes < ring::AerogpuAllocEntry::SIZE_BYTES as u32 {
            Self::record_error(regs);
            return None;
        }

        let Ok(entry_count) = usize::try_from(hdr.entry_count) else {
            Self::record_error(regs);
            return None;
        };
        let entry_stride = hdr.entry_stride_bytes as usize;
        let Some(entries_bytes) = entry_count.checked_mul(entry_stride) else {
            Self::record_error(regs);
            return None;
        };
        let Some(required_bytes) =
            ring::AerogpuAllocTableHeader::SIZE_BYTES.checked_add(entries_bytes)
        else {
            Self::record_error(regs);
            return None;
        };
        if required_bytes > total_size {
            Self::record_error(regs);
            return None;
        }

        let mut out = HashMap::new();
        let mut off = ring::AerogpuAllocTableHeader::SIZE_BYTES;
        for _ in 0..entry_count {
            let Some(entry_bytes) = buf.get(off..off + ring::AerogpuAllocEntry::SIZE_BYTES) else {
                Self::record_error(regs);
                return None;
            };
            let entry = match ring::AerogpuAllocEntry::decode_from_le_bytes(entry_bytes) {
                Ok(v) => v,
                Err(_) => {
                    Self::record_error(regs);
                    return None;
                }
            };
            off += entry_stride;

            if entry.alloc_id == 0 || entry.size_bytes == 0 {
                Self::record_error(regs);
                return None;
            }
            if entry.gpa.checked_add(entry.size_bytes).is_none() {
                Self::record_error(regs);
                return None;
            }
            if out.contains_key(&entry.alloc_id) {
                Self::record_error(regs);
                return None;
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
        Some(out)
    }

    fn record_error(regs: &mut AeroGpuRegs) {
        regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
        regs.irq_status |= irq_bits::ERROR;
    }

    fn resolve_guest_backing_gpa(
        regs: &mut AeroGpuRegs,
        allocs: &HashMap<u32, AllocInfo>,
        backing: &GuestBacking,
        offset_bytes: u64,
        size_bytes: u64,
        access: BackingAccess,
    ) -> Option<u64> {
        let Some(alloc) = allocs.get(&backing.alloc_id) else {
            Self::record_error(regs);
            return None;
        };
        if access == BackingAccess::Write && (alloc.flags & ring::AEROGPU_ALLOC_FLAG_READONLY) != 0
        {
            Self::record_error(regs);
            return None;
        }

        let Some(end) = offset_bytes.checked_add(size_bytes) else {
            Self::record_error(regs);
            return None;
        };
        if end > backing.size_bytes {
            Self::record_error(regs);
            return None;
        }

        let Some(alloc_offset) = backing.alloc_offset_bytes.checked_add(offset_bytes) else {
            Self::record_error(regs);
            return None;
        };
        let Some(alloc_end) = alloc_offset.checked_add(size_bytes) else {
            Self::record_error(regs);
            return None;
        };
        if alloc_end > alloc.size_bytes {
            Self::record_error(regs);
            return None;
        }

        let Some(gpa) = alloc.gpa.checked_add(alloc_offset) else {
            Self::record_error(regs);
            return None;
        };
        if gpa.checked_add(size_bytes).is_none() {
            Self::record_error(regs);
            return None;
        }

        Some(gpa)
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

    fn flush_dirty_textures(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
    ) {
        for tex in self.textures.values_mut() {
            if !tex.dirty {
                continue;
            }
            let Some(backing) = tex.backing.as_ref() else {
                tex.dirty = false;
                continue;
            };
            let write_len = tex.data.len() as u64;
            let Some(dst_gpa) = Self::resolve_guest_backing_gpa(
                regs,
                allocs,
                backing,
                0,
                write_len,
                BackingAccess::Write,
            ) else {
                // Submission is invalid; don't keep retrying this writeback.
                tex.dirty = false;
                continue;
            };
            mem.write_physical(dst_gpa, &tex.data);
            tex.dirty = false;
        }
    }

    fn texture_2d_format_layout(format: AeroGpuFormat) -> Option<Texture2DLayoutFormat> {
        match format {
            AeroGpuFormat::B8G8R8A8Unorm
            | AeroGpuFormat::B8G8R8X8Unorm
            | AeroGpuFormat::R8G8B8A8Unorm
            | AeroGpuFormat::R8G8B8X8Unorm
            | AeroGpuFormat::B8G8R8A8UnormSrgb
            | AeroGpuFormat::B8G8R8X8UnormSrgb
            | AeroGpuFormat::R8G8B8A8UnormSrgb
            | AeroGpuFormat::R8G8B8X8UnormSrgb
            // Depth formats are treated as 4 bytes/texel for backing-size computations, matching
            // host executors and the rest of this software backend.
            | AeroGpuFormat::D24UnormS8Uint
            | AeroGpuFormat::D32Float => Some(Texture2DLayoutFormat::Uncompressed {
                bytes_per_texel: 4,
            }),
            AeroGpuFormat::B5G6R5Unorm | AeroGpuFormat::B5G5R5A1Unorm => {
                Some(Texture2DLayoutFormat::Uncompressed {
                    bytes_per_texel: 2,
                })
            }
            // Block-compressed formats (4x4 blocks).
            AeroGpuFormat::Bc1Unorm | AeroGpuFormat::Bc1UnormSrgb => {
                Some(Texture2DLayoutFormat::BlockCompressed {
                    block_bytes: 8,
                })
            }
            AeroGpuFormat::Bc2Unorm
            | AeroGpuFormat::Bc2UnormSrgb
            | AeroGpuFormat::Bc3Unorm
            | AeroGpuFormat::Bc3UnormSrgb
            | AeroGpuFormat::Bc7Unorm
            | AeroGpuFormat::Bc7UnormSrgb => Some(Texture2DLayoutFormat::BlockCompressed {
                block_bytes: 16,
            }),
            AeroGpuFormat::Invalid => None,
        }
    }

    fn full_mip_chain_len(width: u32, height: u32) -> u32 {
        let mut w = width.max(1);
        let mut h = height.max(1);
        let mut levels = 1u32;
        while w > 1 || h > 1 {
            w = (w >> 1).max(1);
            h = (h >> 1).max(1);
            levels = levels.saturating_add(1);
            if levels >= 32 {
                break;
            }
        }
        levels.max(1)
    }

    /// Compute the total byte size of a linearly-laid-out Texture2D (including mip chain and array
    /// layers) plus the effective mip0 row pitch.
    ///
    /// Layout rules (matches the host executors):
    /// - Memory is laid out as: `for layer in 0..array_layers`, `for mip in 0..mip_levels`, append
    ///   that subresource's bytes.
    /// - For mip `m`:
    ///   - `mip_w = max(1, width >> m)`, `mip_h = max(1, height >> m)`
    ///   - Uncompressed row pitch:
    ///     - mip0: use `row_pitch_bytes` if non-zero else `mip_w * bytes_per_texel`
    ///     - mip>0: tight-pack: `mip_w * bytes_per_texel`
    ///     - if `row_pitch_bytes` is provided, validate it is >= the minimum tight row pitch.
    ///   - Block-compressed (BC) row pitch:
    ///     - `blocks_w = ceil(mip_w/4)`, `blocks_h = ceil(mip_h/4)`
    ///     - mip0: use `row_pitch_bytes` if non-zero else `blocks_w * block_bytes`
    ///     - mip>0: tight-pack: `blocks_w * block_bytes`
    ///     - subresource size = `row_pitch * blocks_h`
    fn texture_2d_linear_layout(
        format: AeroGpuFormat,
        width: u32,
        height: u32,
        mip_levels: u32,
        array_layers: u32,
        row_pitch_bytes: u32,
    ) -> Option<Texture2DLinearLayout> {
        let max_mips = Self::full_mip_chain_len(width, height);
        let mip_levels = if mip_levels == 0 {
            max_mips
        } else {
            mip_levels.min(max_mips).max(1)
        };

        let array_layers = if array_layers == 0 { 1 } else { array_layers };

        let (layout, enforce_min_row_pitch) = match Self::texture_2d_format_layout(format) {
            Some(v) => (v, true),
            None => (
                // Unknown/invalid format: treat as opaque 4-byte texels for sizing, but avoid
                // enforcing row-pitch minimums so new/unknown formats don't become fatal.
                Texture2DLayoutFormat::Uncompressed { bytes_per_texel: 4 },
                false,
            ),
        };

        fn div_ceil_u32(n: u32, d: u32) -> u32 {
            debug_assert!(d != 0);
            n.div_ceil(d)
        }

        let mut total_size_bytes: u64 = 0;
        let mut mip0_row_pitch_bytes_out: Option<u32> = None;

        for _layer in 0..array_layers {
            for mip in 0..mip_levels {
                let mip_w = if mip >= 32 { 1 } else { (width >> mip).max(1) };
                let mip_h = if mip >= 32 { 1 } else { (height >> mip).max(1) };

                let (row_pitch_u64, rows_u64) = match layout {
                    Texture2DLayoutFormat::Uncompressed { bytes_per_texel } => {
                        let min_pitch_u64 =
                            u64::from(mip_w).checked_mul(u64::from(bytes_per_texel))?;
                        let row_pitch_u64 = if mip == 0 {
                            if row_pitch_bytes != 0 {
                                let rp = u64::from(row_pitch_bytes);
                                if enforce_min_row_pitch && rp < min_pitch_u64 {
                                    return None;
                                }
                                rp
                            } else {
                                min_pitch_u64
                            }
                        } else {
                            min_pitch_u64
                        };
                        (row_pitch_u64, u64::from(mip_h))
                    }
                    Texture2DLayoutFormat::BlockCompressed { block_bytes } => {
                        let blocks_w = div_ceil_u32(mip_w, 4);
                        let blocks_h = div_ceil_u32(mip_h, 4);
                        let min_pitch_u64 =
                            u64::from(blocks_w).checked_mul(u64::from(block_bytes))?;
                        let row_pitch_u64 = if mip == 0 {
                            if row_pitch_bytes != 0 {
                                let rp = u64::from(row_pitch_bytes);
                                if enforce_min_row_pitch && rp < min_pitch_u64 {
                                    return None;
                                }
                                rp
                            } else {
                                min_pitch_u64
                            }
                        } else {
                            min_pitch_u64
                        };
                        (row_pitch_u64, u64::from(blocks_h))
                    }
                };

                if mip == 0 && mip0_row_pitch_bytes_out.is_none() {
                    mip0_row_pitch_bytes_out = Some(u32::try_from(row_pitch_u64).ok()?);
                }

                let sub_size = row_pitch_u64.checked_mul(rows_u64)?;
                total_size_bytes = total_size_bytes.checked_add(sub_size)?;
            }
        }

        Some(Texture2DLinearLayout {
            mip_levels,
            array_layers,
            mip0_row_pitch_bytes: mip0_row_pitch_bytes_out?,
            total_size_bytes,
        })
    }

    fn texture_2d_subresource_layout(
        tex: &Texture2DResource,
        mip: u32,
        layer: u32,
    ) -> Option<Texture2DSubresourceLayout> {
        if tex.width == 0 || tex.height == 0 {
            return None;
        }
        if mip >= tex.mip_levels || layer >= tex.array_layers {
            return None;
        }

        let layout = Self::texture_2d_format_layout(tex.format).unwrap_or(
            // Unknown/invalid format: treat as opaque 4-byte texels, matching
            // `texture_2d_linear_layout`'s sizing fallback.
            Texture2DLayoutFormat::Uncompressed { bytes_per_texel: 4 },
        );

        fn div_ceil_u32(n: u32, d: u32) -> u32 {
            debug_assert!(d != 0);
            n.div_ceil(d)
        }

        fn mip_dim(dim: u32, mip: u32) -> u32 {
            if mip >= 32 {
                1
            } else {
                (dim >> mip).max(1)
            }
        }

        let mip_params = |mip: u32| -> Option<(u32, u32, u32, u32, u64)> {
            let mip_w = mip_dim(tex.width, mip);
            let mip_h = mip_dim(tex.height, mip);

            let (row_pitch_u64, rows_u64) = match layout {
                Texture2DLayoutFormat::Uncompressed { bytes_per_texel } => {
                    let tight_pitch_u64 =
                        u64::from(mip_w).checked_mul(u64::from(bytes_per_texel))?;
                    let row_pitch_u64 = if mip == 0 {
                        u64::from(tex.row_pitch_bytes)
                    } else {
                        tight_pitch_u64
                    };
                    (row_pitch_u64, u64::from(mip_h))
                }
                Texture2DLayoutFormat::BlockCompressed { block_bytes } => {
                    let blocks_w = div_ceil_u32(mip_w, 4);
                    let blocks_h = div_ceil_u32(mip_h, 4);
                    let tight_pitch_u64 =
                        u64::from(blocks_w).checked_mul(u64::from(block_bytes))?;
                    let row_pitch_u64 = if mip == 0 {
                        u64::from(tex.row_pitch_bytes)
                    } else {
                        tight_pitch_u64
                    };
                    (row_pitch_u64, u64::from(blocks_h))
                }
            };

            let row_pitch_bytes = u32::try_from(row_pitch_u64).ok()?;
            let rows = u32::try_from(rows_u64).ok()?;
            let size_bytes = row_pitch_u64.checked_mul(rows_u64)?;
            Some((mip_w, mip_h, row_pitch_bytes, rows, size_bytes))
        };

        // Compute offset using the same packing rules as `texture_2d_linear_layout`:
        // layer-major, mip-major.
        let mut offset_bytes: u64 = 0;
        for _ in 0..layer {
            for m in 0..tex.mip_levels {
                let (_, _, _, _, size_bytes) = mip_params(m)?;
                offset_bytes = offset_bytes.checked_add(size_bytes)?;
            }
        }
        for m in 0..mip {
            let (_, _, _, _, size_bytes) = mip_params(m)?;
            offset_bytes = offset_bytes.checked_add(size_bytes)?;
        }

        let (mip_w, mip_h, row_pitch_bytes, rows, size_bytes) = mip_params(mip)?;

        let end = offset_bytes.checked_add(size_bytes)?;
        if end > tex.data.len() as u64 {
            return None;
        }

        Some(Texture2DSubresourceLayout {
            mip_w,
            mip_h,
            row_pitch_bytes,
            rows,
            offset_bytes,
        })
    }

    fn texture_bytes_per_pixel(format: AeroGpuFormat) -> Option<usize> {
        match format {
            AeroGpuFormat::B8G8R8A8Unorm
            | AeroGpuFormat::B8G8R8X8Unorm
            | AeroGpuFormat::R8G8B8A8Unorm
            | AeroGpuFormat::R8G8B8X8Unorm
            | AeroGpuFormat::B8G8R8A8UnormSrgb
            | AeroGpuFormat::B8G8R8X8UnormSrgb
            | AeroGpuFormat::R8G8B8A8UnormSrgb
            | AeroGpuFormat::R8G8B8X8UnormSrgb
            | AeroGpuFormat::D24UnormS8Uint
            | AeroGpuFormat::D32Float => Some(4),
            AeroGpuFormat::B5G6R5Unorm | AeroGpuFormat::B5G5R5A1Unorm => Some(2),
            _ => None,
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
            AeroGpuFormat::B8G8R8A8Unorm
            | AeroGpuFormat::B8G8R8A8UnormSrgb
            | AeroGpuFormat::B8G8R8X8Unorm
            | AeroGpuFormat::B8G8R8X8UnormSrgb => Some([
                tex.data[off + 2], // r
                tex.data[off + 1], // g
                tex.data[off],     // b
                if matches!(
                    tex.format,
                    AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8A8UnormSrgb
                ) {
                    tex.data[off + 3]
                } else {
                    0xff
                },
            ]),
            AeroGpuFormat::R8G8B8A8Unorm
            | AeroGpuFormat::R8G8B8A8UnormSrgb
            | AeroGpuFormat::R8G8B8X8Unorm
            | AeroGpuFormat::R8G8B8X8UnormSrgb => Some([
                tex.data[off],
                tex.data[off + 1],
                tex.data[off + 2],
                if matches!(
                    tex.format,
                    AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8A8UnormSrgb
                ) {
                    tex.data[off + 3]
                } else {
                    0xff
                },
            ]),
            _ => None,
        }
    }

    fn address_texel(idx: i32, dim: i32, mode: u32) -> i32 {
        if dim <= 0 {
            return 0;
        }
        match mode {
            x if x == cmd::AerogpuSamplerAddressMode::Repeat as u32 => idx.rem_euclid(dim),
            x if x == cmd::AerogpuSamplerAddressMode::MirrorRepeat as u32 => {
                let period = dim.saturating_mul(2).max(1);
                let t = idx.rem_euclid(period);
                if t >= dim {
                    period - 1 - t
                } else {
                    t
                }
            }
            _ => idx.clamp(0, dim - 1),
        }
    }

    fn read_texel_rgba_f32(
        tex: &Texture2DResource,
        sampler: SamplerResource,
        x: i32,
        y: i32,
    ) -> [f32; 4] {
        if tex.width == 0 || tex.height == 0 {
            return [0.0, 0.0, 0.0, 1.0];
        }

        let x = Self::address_texel(x, tex.width as i32, sampler.address_u);
        let y = Self::address_texel(y, tex.height as i32, sampler.address_v);

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

    fn sample_texture_2d(
        tex: &Texture2DResource,
        sampler: SamplerResource,
        uv: (f32, f32),
    ) -> [f32; 4] {
        if tex.width == 0 || tex.height == 0 {
            return [0.0, 0.0, 0.0, 1.0];
        }
        // MVP: this software raster path does not implement full LOD selection or array slice
        // selection. For mipmapped/array textures, just sample mip0/layer0.

        let u = if uv.0.is_finite() { uv.0 } else { 0.0 };
        let v = if uv.1.is_finite() { uv.1 } else { 0.0 };

        match sampler.filter {
            x if x == cmd::AerogpuSamplerFilter::Linear as u32 => {
                fn lerp(a: f32, b: f32, t: f32) -> f32 {
                    a + (b - a) * t
                }

                let x = u * tex.width as f32 - 0.5;
                let y = v * tex.height as f32 - 0.5;
                let x0 = x.floor() as i32;
                let y0 = y.floor() as i32;
                let fx = x - x0 as f32;
                let fy = y - y0 as f32;
                let x1 = x0 + 1;
                let y1 = y0 + 1;

                let c00 = Self::read_texel_rgba_f32(tex, sampler, x0, y0);
                let c10 = Self::read_texel_rgba_f32(tex, sampler, x1, y0);
                let c01 = Self::read_texel_rgba_f32(tex, sampler, x0, y1);
                let c11 = Self::read_texel_rgba_f32(tex, sampler, x1, y1);

                let mut out = [0.0f32; 4];
                for i in 0..4 {
                    let a = lerp(c00[i], c10[i], fx);
                    let b = lerp(c01[i], c11[i], fx);
                    out[i] = lerp(a, b, fy);
                }
                out
            }
            _ => {
                // Nearest filter.
                let x = (u * tex.width as f32).floor() as i32;
                let y = (v * tex.height as f32).floor() as i32;
                Self::read_texel_rgba_f32(tex, sampler, x, y)
            }
        }
    }

    fn write_pixel_rgba_u8(tex: &mut Texture2DResource, off: usize, rgba: [u8; 4]) {
        if off + 4 > tex.data.len() {
            return;
        }
        let [r, g, b, a] = rgba;
        match tex.format {
            AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8A8UnormSrgb => {
                tex.data[off] = b;
                tex.data[off + 1] = g;
                tex.data[off + 2] = r;
                tex.data[off + 3] = a;
            }
            AeroGpuFormat::B8G8R8X8Unorm | AeroGpuFormat::B8G8R8X8UnormSrgb => {
                tex.data[off] = b;
                tex.data[off + 1] = g;
                tex.data[off + 2] = r;
                tex.data[off + 3] = 0xff;
            }
            AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8A8UnormSrgb => {
                tex.data[off] = r;
                tex.data[off + 1] = g;
                tex.data[off + 2] = b;
                tex.data[off + 3] = a;
            }
            AeroGpuFormat::R8G8B8X8Unorm | AeroGpuFormat::R8G8B8X8UnormSrgb => {
                tex.data[off] = r;
                tex.data[off + 1] = g;
                tex.data[off + 2] = b;
                tex.data[off + 3] = 0xff;
            }
            _ => {}
        }
    }

    fn blend_factor(factor: u32, src_a: f32, dst_a: f32, constant: f32) -> f32 {
        match factor {
            x if x == cmd::AerogpuBlendFactor::Zero as u32 => 0.0,
            x if x == cmd::AerogpuBlendFactor::One as u32 => 1.0,
            x if x == cmd::AerogpuBlendFactor::SrcAlpha as u32 => src_a,
            x if x == cmd::AerogpuBlendFactor::InvSrcAlpha as u32 => 1.0 - src_a,
            x if x == cmd::AerogpuBlendFactor::DestAlpha as u32 => dst_a,
            x if x == cmd::AerogpuBlendFactor::InvDestAlpha as u32 => 1.0 - dst_a,
            x if x == cmd::AerogpuBlendFactor::Constant as u32 => constant,
            x if x == cmd::AerogpuBlendFactor::InvConstant as u32 => 1.0 - constant,
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
        if (blend.sample_mask & 1) == 0 {
            return;
        }
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

            for i in 0..4 {
                let (src_factor, dst_factor, op) = if i == 3 {
                    (
                        blend.src_factor_alpha,
                        blend.dst_factor_alpha,
                        blend.blend_op_alpha,
                    )
                } else {
                    (blend.src_factor, blend.dst_factor, blend.blend_op)
                };
                let constant = blend.blend_constant[i].clamp(0.0, 1.0);
                let sf = Self::blend_factor(src_factor, src_a, dst_a, constant);
                let df = Self::blend_factor(dst_factor, src_a, dst_a, constant);

                let s = rgba[i].clamp(0.0, 1.0) * sf;
                let d = dst[i].clamp(0.0, 1.0) * df;
                out[i] = Self::blend_op(op, s, d).clamp(0.0, 1.0);
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
                    AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8A8UnormSrgb => {
                        tex.data[off] = b;
                        tex.data[off + 1] = g;
                        tex.data[off + 2] = r;
                        tex.data[off + 3] = a;
                    }
                    AeroGpuFormat::B8G8R8X8Unorm | AeroGpuFormat::B8G8R8X8UnormSrgb => {
                        tex.data[off] = b;
                        tex.data[off + 1] = g;
                        tex.data[off + 2] = r;
                        tex.data[off + 3] = 0xff;
                    }
                    AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8A8UnormSrgb => {
                        tex.data[off] = r;
                        tex.data[off + 1] = g;
                        tex.data[off + 2] = b;
                        tex.data[off + 3] = a;
                    }
                    AeroGpuFormat::R8G8B8X8Unorm | AeroGpuFormat::R8G8B8X8UnormSrgb => {
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

    #[allow(clippy::too_many_arguments)]
    fn rasterize_triangle(
        tex: &mut Texture2DResource,
        clip: (i32, i32, i32, i32),
        verts: [(f32, f32); 3],
        colors: [[f32; 4]; 3],
        blend: BlendState,
    ) {
        if (blend.sample_mask & 1) == 0 {
            return;
        }
        fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
            (bx - ax) * (py - ay) - (by - ay) * (px - ax)
        }

        let [v0, v1, v2] = verts;
        let [c0, c1, c2] = colors;

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

    #[allow(clippy::too_many_arguments)]
    fn rasterize_triangle_depth(
        tex: &mut Texture2DResource,
        depth: DepthTarget<'_>,
        clip: (i32, i32, i32, i32),
        verts: [(f32, f32, f32); 3],
        colors: [[f32; 4]; 3],
        blend: BlendState,
    ) {
        if (blend.sample_mask & 1) == 0 {
            return;
        }
        fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
            (bx - ax) * (py - ay) - (by - ay) * (px - ax)
        }

        let DepthTarget {
            tex: depth_tex,
            state: depth_state,
        } = depth;
        let [v0, v1, v2] = verts;
        let [c0, c1, c2] = colors;

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

    #[allow(clippy::too_many_arguments)]
    fn rasterize_triangle_textured(
        tex: &mut Texture2DResource,
        src_tex: &Texture2DResource,
        sampler: SamplerResource,
        clip: (i32, i32, i32, i32),
        verts: [(f32, f32); 3],
        uvs: [(f32, f32); 3],
        blend: BlendState,
    ) {
        if (blend.sample_mask & 1) == 0 {
            return;
        }
        fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
            (bx - ax) * (py - ay) - (by - ay) * (px - ax)
        }

        let [v0, v1, v2] = verts;
        let [uv0, uv1, uv2] = uvs;

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
                let out = Self::sample_texture_2d(src_tex, sampler, uv);
                Self::blend_and_write_pixel(tex, x, y, out, blend);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rasterize_triangle_depth_textured(
        tex: &mut Texture2DResource,
        depth: DepthTarget<'_>,
        src_tex: &Texture2DResource,
        sampler: SamplerResource,
        clip: (i32, i32, i32, i32),
        verts: [(f32, f32, f32); 3],
        uvs: [(f32, f32); 3],
        blend: BlendState,
    ) {
        if (blend.sample_mask & 1) == 0 {
            return;
        }
        fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
            (bx - ax) * (py - ay) - (by - ay) * (px - ax)
        }

        let DepthTarget {
            tex: depth_tex,
            state: depth_state,
        } = depth;
        let [v0, v1, v2] = verts;
        let [uv0, uv1, uv2] = uvs;

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
                let out = Self::sample_texture_2d(src_tex, sampler, uv);
                Self::blend_and_write_pixel(tex, x, y, out, blend);
            }
        }
    }

    fn draw_triangle_list(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
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

        let mut vertices: Vec<Vertex> = Vec::with_capacity(vertex_indices.len());

        for &idx in vertex_indices {
            if idx < 0 {
                continue;
            }
            let idx_u32 = idx as u32;

            // D3D11 path: ILAY blob present. MVP supports POSITION, optional COLOR, optional TEXCOORD0.
            if let Some(layout) = parsed_layout.as_ref() {
                if let Some(pos_el) = layout.position {
                    let pos =
                        match self.read_vertex_elem_position(regs, mem, allocs, pos_el, idx_u32) {
                            Some(v) => v,
                            None => continue,
                        };
                    let color = match layout.color {
                        Some(col_el) => {
                            match self.read_vertex_elem_f32x4(regs, mem, allocs, col_el, idx_u32) {
                                Some(v) => v,
                                None => continue,
                            }
                        }
                        None => [1.0, 1.0, 1.0, 1.0],
                    };
                    let uv = match layout.texcoord0 {
                        Some(uv_el) => {
                            match self.read_vertex_elem_f32x2(regs, mem, allocs, uv_el, idx_u32) {
                                Some(v) => v,
                                None => continue,
                            }
                        }
                        None => (0.0, 0.0),
                    };

                    // NDC -> viewport pixels.
                    let x = vp.x + (pos.0 * 0.5 + 0.5) * vp.width;
                    let y = vp.y + (1.0 - (pos.1 * 0.5 + 0.5)) * vp.height;
                    let z = vp.min_depth + pos.2 * (vp.max_depth - vp.min_depth);
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
            match self.read_vertex_d3d9(regs, mem, allocs, idx_u32) {
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

        let can_depth_test = if depth_state.depth_enable && ds_handle != 0 && ds_handle != rt_handle
        {
            self.textures.get(&ds_handle).is_some_and(|tex| {
                matches!(
                    tex.format,
                    AeroGpuFormat::D24UnormS8Uint | AeroGpuFormat::D32Float
                )
            })
        } else {
            false
        };

        if wants_sampling {
            let Some(src_tex) = self.textures.remove(&ps_tex0) else {
                Self::record_error(regs);
                return;
            };

            let ps_samp0 = self.state.samplers_ps[0];
            let sampler = if ps_samp0 == 0 {
                SamplerResource::default()
            } else {
                match self.samplers.get(&ps_samp0).copied() {
                    Some(s) => s,
                    None => {
                        Self::record_error(regs);
                        SamplerResource::default()
                    }
                }
            };

            if can_depth_test {
                let Some(mut depth_tex) = self.textures.remove(&ds_handle) else {
                    self.textures.insert(ps_tex0, src_tex);
                    Self::record_error(regs);
                    return;
                };

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
                        if rast.depth_clip_enable
                            && (tri[0].depth < vp.min_depth
                                || tri[0].depth > vp.max_depth
                                || tri[1].depth < vp.min_depth
                                || tri[1].depth > vp.max_depth
                                || tri[2].depth < vp.min_depth
                                || tri[2].depth > vp.max_depth)
                        {
                            continue;
                        }
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
                            DepthTarget {
                                tex: &mut depth_tex,
                                state: depth_state,
                            },
                            &src_tex,
                            sampler,
                            (clip_x0, clip_y0, clip_x1, clip_y1),
                            [
                                (tri[0].pos.0, tri[0].pos.1, tri[0].depth),
                                (tri[1].pos.0, tri[1].pos.1, tri[1].depth),
                                (tri[2].pos.0, tri[2].pos.1, tri[2].depth),
                            ],
                            [tri[0].uv, tri[1].uv, tri[2].uv],
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
                    if rast.depth_clip_enable
                        && (tri[0].depth < vp.min_depth
                            || tri[0].depth > vp.max_depth
                            || tri[1].depth < vp.min_depth
                            || tri[1].depth > vp.max_depth
                            || tri[2].depth < vp.min_depth
                            || tri[2].depth > vp.max_depth)
                    {
                        continue;
                    }
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
                        sampler,
                        (clip_x0, clip_y0, clip_x1, clip_y1),
                        [tri[0].pos, tri[1].pos, tri[2].pos],
                        [tri[0].uv, tri[1].uv, tri[2].uv],
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
                    if rast.depth_clip_enable
                        && (tri[0].depth < vp.min_depth
                            || tri[0].depth > vp.max_depth
                            || tri[1].depth < vp.min_depth
                            || tri[1].depth > vp.max_depth
                            || tri[2].depth < vp.min_depth
                            || tri[2].depth > vp.max_depth)
                    {
                        continue;
                    }
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
                        DepthTarget {
                            tex: &mut depth_tex,
                            state: depth_state,
                        },
                        (clip_x0, clip_y0, clip_x1, clip_y1),
                        [
                            (tri[0].pos.0, tri[0].pos.1, tri[0].depth),
                            (tri[1].pos.0, tri[1].pos.1, tri[1].depth),
                            (tri[2].pos.0, tri[2].pos.1, tri[2].depth),
                        ],
                        [tri[0].color, tri[1].color, tri[2].color],
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
                if rast.depth_clip_enable
                    && (tri[0].depth < vp.min_depth
                        || tri[0].depth > vp.max_depth
                        || tri[1].depth < vp.min_depth
                        || tri[1].depth > vp.max_depth
                        || tri[2].depth < vp.min_depth
                        || tri[2].depth > vp.max_depth)
                {
                    continue;
                }
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
                    [tri[0].pos, tri[1].pos, tri[2].pos],
                    [tri[0].color, tri[1].color, tri[2].color],
                    blend,
                );
            }

            tex.dirty = true;
        }
    }

    fn read_vertex_d3d9(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
        index: u32,
    ) -> Option<Vertex> {
        let binding = self
            .state
            .vertex_buffers
            .first()
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
        if !self.read_buffer_bytes(regs, mem, allocs, handle, start, &mut buf) {
            return None;
        }

        let x = f32::from_bits(u32::from_le_bytes(buf[0..4].try_into().unwrap()));
        let y = f32::from_bits(u32::from_le_bytes(buf[4..8].try_into().unwrap()));
        let z = f32::from_bits(u32::from_le_bytes(buf[8..12].try_into().unwrap()));
        let color_argb = u32::from_le_bytes(buf[16..20].try_into().unwrap());
        let a = ((color_argb >> 24) & 0xff) as f32 / 255.0;
        let r = ((color_argb >> 16) & 0xff) as f32 / 255.0;
        let g = ((color_argb >> 8) & 0xff) as f32 / 255.0;
        let b = (color_argb & 0xff) as f32 / 255.0;

        Some(Vertex {
            pos: (x, y),
            depth: z,
            uv: (0.0, 0.0),
            color: [r, g, b, a],
        })
    }

    fn read_vertex_elem_position(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
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
        if !self.read_buffer_bytes(regs, mem, allocs, handle, start, slice) {
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
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
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
        if !self.read_buffer_bytes(regs, mem, allocs, handle, start, &mut buf) {
            return None;
        }
        Some((
            f32::from_bits(u32::from_le_bytes(buf[0..4].try_into().unwrap())),
            f32::from_bits(u32::from_le_bytes(buf[4..8].try_into().unwrap())),
        ))
    }

    fn read_vertex_elem_f32x4(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
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
        if !self.read_buffer_bytes(regs, mem, allocs, handle, start, &mut buf) {
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
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        allocs: &HashMap<u32, AllocInfo>,
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
                let Some(src_gpa) = Self::resolve_guest_backing_gpa(
                    regs,
                    allocs,
                    backing,
                    offset,
                    out.len() as u64,
                    BackingAccess::Read,
                ) else {
                    return false;
                };
                mem.read_physical(src_gpa, out);
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
        if desc.cmd_gpa == 0 && desc.cmd_size_bytes == 0 {
            return;
        }
        if desc.cmd_gpa == 0 || desc.cmd_size_bytes == 0 {
            Self::record_error(regs);
            return;
        }
        if desc
            .cmd_gpa
            .checked_add(u64::from(desc.cmd_size_bytes))
            .is_none()
        {
            Self::record_error(regs);
            return;
        }
        if desc.cmd_size_bytes < cmd::AerogpuCmdStreamHeader::SIZE_BYTES as u32 {
            Self::record_error(regs);
            return;
        }

        // Forward-compat: the submit descriptor's `cmd_size_bytes` is the backing buffer capacity,
        // while the command stream header's `size_bytes` field indicates the bytes used.
        //
        // Only copy the used prefix to avoid allocating/copying potentially large trailing bytes.
        let mut header_bytes = [0u8; cmd::AerogpuCmdStreamHeader::SIZE_BYTES];
        mem.read_physical(desc.cmd_gpa, &mut header_bytes);
        let header = match cmd::decode_cmd_stream_header_le(&header_bytes) {
            Ok(v) => v,
            Err(_) => {
                Self::record_error(regs);
                return;
            }
        };

        if header.size_bytes > desc.cmd_size_bytes {
            Self::record_error(regs);
            return;
        }

        let stream_size = header.size_bytes as usize;
        if stream_size > MAX_CMD_STREAM_SIZE_BYTES {
            Self::record_error(regs);
            return;
        }

        let mut buf = vec![0u8; stream_size];
        mem.read_physical(desc.cmd_gpa, &mut buf);

        let iter = match cmd::AerogpuCmdStreamIter::new(&buf) {
            Ok(v) => v,
            Err(_) => {
                Self::record_error(regs);
                return;
            }
        };

        let Some(allocs) = self.parse_alloc_table(regs, mem, desc) else {
            return;
        };

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

        self.flush_dirty_textures(regs, mem, &allocs);
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

        // `AerogpuCmdOpcode` is a fixed protocol enum today. The wildcard arm at the bottom is
        // intentional forward-compatibility: if the protocol gains new opcodes, the software
        // backend should ignore them rather than failing compilation.
        #[allow(unreachable_patterns)]
        match op {
            cmd::AerogpuCmdOpcode::Nop | cmd::AerogpuCmdOpcode::DebugMarker => {}
            cmd::AerogpuCmdOpcode::CreateSampler
            | cmd::AerogpuCmdOpcode::DestroySampler
            | cmd::AerogpuCmdOpcode::SetSamplers
            | cmd::AerogpuCmdOpcode::SetConstantBuffers => {
                // The software backend is intentionally minimal and ignores GPU state
                // that is only needed for shader-based rendering.
            }
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
                if self.buffers.contains_key(&handle)
                    || self.textures.contains_key(&handle)
                    || self.shaders.contains_key(&handle)
                    || self.input_layouts.contains_key(&handle)
                    || self.resource_aliases.contains_key(&handle)
                    || self.texture_refcounts.contains_key(&handle)
                {
                    Self::record_error(regs);
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
                        alloc_offset_bytes: backing_offset_bytes,
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
                if self.buffers.contains_key(&handle)
                    || self.textures.contains_key(&handle)
                    || self.shaders.contains_key(&handle)
                    || self.input_layouts.contains_key(&handle)
                    || self.resource_aliases.contains_key(&handle)
                    || self.texture_refcounts.contains_key(&handle)
                {
                    Self::record_error(regs);
                    return true;
                }

                let format = AeroGpuFormat::from_u32(format_u32);
                let Some(layout) = Self::texture_2d_linear_layout(
                    format,
                    width,
                    height,
                    mip_levels,
                    array_layers,
                    row_pitch_bytes,
                ) else {
                    Self::record_error(regs);
                    return true;
                };

                let mip_levels = layout.mip_levels;
                let array_layers = layout.array_layers;
                let row_pitch_bytes = layout.mip0_row_pitch_bytes;
                let total_bytes = layout.total_size_bytes;

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
                        alloc_offset_bytes: backing_offset_bytes,
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
                    let Some(src_gpa) = Self::resolve_guest_backing_gpa(
                        regs,
                        allocs,
                        b,
                        0,
                        total_bytes,
                        BackingAccess::Read,
                    ) else {
                        return true;
                    };
                    mem.read_physical(src_gpa, &mut data);
                }

                self.textures.insert(
                    handle,
                    Texture2DResource {
                        width,
                        height,
                        format,
                        mip_levels,
                        array_layers,
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

                let mut destroyed_underlying = false;
                if let Some(count) = self.texture_refcounts.get_mut(&resolved) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        destroyed_underlying = true;
                        self.texture_refcounts.remove(&resolved);
                        self.textures.remove(&resolved);
                        self.retire_shared_surface_tokens_for_resource(resolved);
                        self.resource_aliases.retain(|_, v| *v != resolved);
                    }
                } else {
                    destroyed_underlying = self.textures.remove(&resolved).is_some();
                    self.retire_shared_surface_tokens_for_resource(resolved);
                    self.resource_aliases.retain(|_, v| *v != resolved);
                }
                self.state.render_targets.iter_mut().for_each(|rt| {
                    if *rt == handle || (destroyed_underlying && *rt == resolved) {
                        *rt = 0;
                    }
                });
                if self.state.depth_stencil == handle
                    || (destroyed_underlying && self.state.depth_stencil == resolved)
                {
                    self.state.depth_stencil = 0;
                }
                self.state.textures_vs.iter_mut().for_each(|tex| {
                    if *tex == handle || (destroyed_underlying && *tex == resolved) {
                        *tex = 0;
                    }
                });
                self.state.textures_ps.iter_mut().for_each(|tex| {
                    if *tex == handle || (destroyed_underlying && *tex == resolved) {
                        *tex = 0;
                    }
                });
                for vb in self.state.vertex_buffers.iter_mut() {
                    if vb.buffer == handle || (destroyed_underlying && vb.buffer == resolved) {
                        *vb = VertexBufferBinding::default();
                    }
                }
                if self.state.input_layout == handle
                    || (destroyed_underlying && self.state.input_layout == resolved)
                {
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
                    let start_usize = match usize::try_from(offset).ok() {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    let end_usize = match usize::try_from(end).ok() {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };
                    let Some(src_gpa) = Self::resolve_guest_backing_gpa(
                        regs,
                        allocs,
                        backing,
                        offset,
                        size,
                        BackingAccess::Read,
                    ) else {
                        return true;
                    };
                    mem.read_physical(src_gpa, &mut tex.data[start_usize..end_usize]);
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
                        let Some(dst_gpa) = Self::resolve_guest_backing_gpa(
                            regs,
                            allocs,
                            backing,
                            offset,
                            size,
                            BackingAccess::Write,
                        ) else {
                            return true;
                        };
                        mem.write_physical(dst_gpa, payload);
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
                    let Some(src_gpa) = Self::resolve_guest_backing_gpa(
                        regs,
                        allocs,
                        backing,
                        src_offset,
                        size,
                        BackingAccess::Read,
                    ) else {
                        return true;
                    };
                    mem.read_physical(src_gpa, &mut tmp);
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
                    let Some(dst_gpa) = Self::resolve_guest_backing_gpa(
                        regs,
                        allocs,
                        &backing,
                        dst_offset,
                        size,
                        BackingAccess::Write,
                    ) else {
                        return true;
                    };
                    mem.write_physical(dst_gpa, &tmp);
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

                let dst_handle = self.resolve_handle(dst);
                let src_handle = self.resolve_handle(src);

                let (src_format, src_region) = {
                    let Some(src_tex) = self.textures.get(&src_handle) else {
                        Self::record_error(regs);
                        return true;
                    };
                    if src_mip_level >= src_tex.mip_levels
                        || src_array_layer >= src_tex.array_layers
                    {
                        Self::record_error(regs);
                        return true;
                    }
                    let Some(src_sub) = Self::texture_2d_subresource_layout(
                        src_tex,
                        src_mip_level,
                        src_array_layer,
                    ) else {
                        Self::record_error(regs);
                        return true;
                    };

                    let Some(bpp) = Self::texture_bytes_per_pixel(src_tex.format) else {
                        // Unsupported format for raw-copy in the software backend.
                        return true;
                    };
                    if bpp != 4 {
                        // MVP: only 32bpp uncompressed texture copies are supported.
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
                    if src_x_end > src_sub.mip_w || src_y_end > src_sub.mip_h {
                        Self::record_error(regs);
                        return true;
                    }
                    let pitch = src_sub.row_pitch_bytes as usize;
                    if pitch < src_sub.mip_w as usize * bpp {
                        Self::record_error(regs);
                        return true;
                    }

                    let src_base = match usize::try_from(src_sub.offset_bytes).ok() {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return true;
                        }
                    };

                    let mut tmp = vec![0u8; region_size];
                    for row in 0..height_usize {
                        let sy = src_y as usize + row;
                        let src_off = src_base + sy * pitch + (src_x as usize) * bpp;
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
                    (src_tex.format, tmp)
                };

                let Some(dst_tex) = self.textures.get(&dst_handle) else {
                    Self::record_error(regs);
                    return true;
                };
                if dst_mip_level >= dst_tex.mip_levels || dst_array_layer >= dst_tex.array_layers {
                    Self::record_error(regs);
                    return true;
                }
                let Some(dst_sub) =
                    Self::texture_2d_subresource_layout(dst_tex, dst_mip_level, dst_array_layer)
                else {
                    Self::record_error(regs);
                    return true;
                };
                if dst_tex.format != src_format {
                    return true;
                }
                let Some(bpp) = Self::texture_bytes_per_pixel(dst_tex.format) else {
                    return true;
                };
                if bpp != 4 {
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
                if dst_x_end > dst_sub.mip_w || dst_y_end > dst_sub.mip_h {
                    Self::record_error(regs);
                    return true;
                }

                if flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST != 0 && dst_tex.backing.is_none() {
                    Self::record_error(regs);
                    return true;
                }

                let dst_pitch = dst_sub.row_pitch_bytes as usize;
                let row_bytes = width as usize * bpp;
                if dst_pitch < dst_sub.mip_w as usize * bpp {
                    Self::record_error(regs);
                    return true;
                }

                let Some(dst_tex) = self.textures.get_mut(&dst_handle) else {
                    Self::record_error(regs);
                    return true;
                };
                let dst_base = match usize::try_from(dst_sub.offset_bytes).ok() {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                let height_usize = height as usize;
                for row in 0..height_usize {
                    let dy = dst_y as usize + row;
                    let dst_off = dst_base + dy * dst_pitch + (dst_x as usize) * bpp;
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
                    if self.buffers.contains_key(&handle)
                        || self.textures.contains_key(&handle)
                        || self.shaders.contains_key(&handle)
                        || self.input_layouts.contains_key(&handle)
                        || self.resource_aliases.contains_key(&handle)
                        || self.texture_refcounts.contains_key(&handle)
                    {
                        Self::record_error(regs);
                        return true;
                    }
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
                    if self.buffers.contains_key(&handle)
                        || self.textures.contains_key(&handle)
                        || self.shaders.contains_key(&handle)
                        || self.input_layouts.contains_key(&handle)
                        || self.resource_aliases.contains_key(&handle)
                        || self.texture_refcounts.contains_key(&handle)
                    {
                        Self::record_error(regs);
                        return true;
                    }
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
                // `SET_BLEND_STATE` was extended over time; accept older 28-byte packets and
                // default missing fields (alpha=params, constant=0, sample_mask=0xFFFFFFFF).
                if packet.len() < 28 {
                    Self::record_error(regs);
                    return false;
                }

                let enable = u32::from_le_bytes(packet[8..12].try_into().unwrap()) != 0;
                let src_factor = u32::from_le_bytes(packet[12..16].try_into().unwrap());
                let dst_factor = u32::from_le_bytes(packet[16..20].try_into().unwrap());
                let blend_op = u32::from_le_bytes(packet[20..24].try_into().unwrap());
                let write_mask = packet[24];

                let src_factor_alpha = if packet.len() >= 32 {
                    u32::from_le_bytes(packet[28..32].try_into().unwrap())
                } else {
                    src_factor
                };
                let dst_factor_alpha = if packet.len() >= 36 {
                    u32::from_le_bytes(packet[32..36].try_into().unwrap())
                } else {
                    dst_factor
                };
                let blend_op_alpha = if packet.len() >= 40 {
                    u32::from_le_bytes(packet[36..40].try_into().unwrap())
                } else {
                    blend_op
                };

                let mut blend_constant = [0.0f32; 4];
                if packet.len() >= 44 {
                    blend_constant[0] =
                        f32::from_bits(u32::from_le_bytes(packet[40..44].try_into().unwrap()));
                }
                if packet.len() >= 48 {
                    blend_constant[1] =
                        f32::from_bits(u32::from_le_bytes(packet[44..48].try_into().unwrap()));
                }
                if packet.len() >= 52 {
                    blend_constant[2] =
                        f32::from_bits(u32::from_le_bytes(packet[48..52].try_into().unwrap()));
                }
                if packet.len() >= 56 {
                    blend_constant[3] =
                        f32::from_bits(u32::from_le_bytes(packet[52..56].try_into().unwrap()));
                }
                let sample_mask = if packet.len() >= 60 {
                    u32::from_le_bytes(packet[56..60].try_into().unwrap())
                } else {
                    0xFFFF_FFFF
                };

                self.state.blend = BlendState {
                    enable,
                    src_factor,
                    dst_factor,
                    blend_op,
                    src_factor_alpha,
                    dst_factor_alpha,
                    blend_op_alpha,
                    blend_constant,
                    sample_mask,
                    write_mask,
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
                let flags = u32::from_le(state.flags);
                self.state.rasterizer = RasterizerState {
                    cull_mode: u32::from_le(state.cull_mode),
                    front_ccw: u32::from_le(state.front_ccw) != 0,
                    scissor_enable: u32::from_le(state.scissor_enable) != 0,
                    depth_clip_enable: (flags & cmd::AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE)
                        == 0,
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
            cmd::AerogpuCmdOpcode::CreateSampler => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdCreateSampler>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.sampler_handle);
                if handle == 0 {
                    return true;
                }
                self.samplers.insert(
                    handle,
                    SamplerResource {
                        filter: u32::from_le(packet_cmd.filter),
                        address_u: u32::from_le(packet_cmd.address_u),
                        address_v: u32::from_le(packet_cmd.address_v),
                        address_w: u32::from_le(packet_cmd.address_w),
                    },
                );
            }
            cmd::AerogpuCmdOpcode::DestroySampler => {
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdDestroySampler>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };
                let handle = u32::from_le(packet_cmd.sampler_handle);
                if handle == 0 {
                    return true;
                }
                self.samplers.remove(&handle);
                self.state.samplers_vs.iter_mut().for_each(|sampler| {
                    if *sampler == handle {
                        *sampler = 0;
                    }
                });
                self.state.samplers_ps.iter_mut().for_each(|sampler| {
                    if *sampler == handle {
                        *sampler = 0;
                    }
                });
            }
            cmd::AerogpuCmdOpcode::SetSamplers => {
                if packet.len() < cmd::AerogpuCmdSetSamplers::SIZE_BYTES {
                    Self::record_error(regs);
                    return false;
                }
                let packet_cmd =
                    match Self::read_packed_prefix::<cmd::AerogpuCmdSetSamplers>(packet) {
                        Some(v) => v,
                        None => {
                            Self::record_error(regs);
                            return false;
                        }
                    };

                let shader_stage = u32::from_le(packet_cmd.shader_stage);
                let start_slot = u32::from_le(packet_cmd.start_slot) as usize;
                let sampler_count = u32::from_le(packet_cmd.sampler_count) as usize;
                let expected_size = match cmd::AerogpuCmdSetSamplers::SIZE_BYTES
                    .checked_add(sampler_count.saturating_mul(4))
                {
                    Some(v) => v,
                    None => {
                        Self::record_error(regs);
                        return true;
                    }
                };
                if packet.len() < expected_size {
                    Self::record_error(regs);
                    return true;
                }

                let mut off = cmd::AerogpuCmdSetSamplers::SIZE_BYTES;
                for i in 0..sampler_count {
                    let slot = match start_slot.checked_add(i) {
                        Some(v) => v,
                        None => break,
                    };
                    let Some(bytes) = packet.get(off..off + 4) else {
                        break;
                    };
                    off += 4;
                    if slot >= MAX_TEXTURE_SLOTS {
                        continue;
                    }
                    let sampler = u32::from_le_bytes(bytes.try_into().unwrap());
                    match shader_stage {
                        x if x == cmd::AerogpuShaderStage::Vertex as u32 => {
                            self.state.samplers_vs[slot] = sampler;
                        }
                        x if x == cmd::AerogpuShaderStage::Pixel as u32 => {
                            self.state.samplers_ps[slot] = sampler;
                        }
                        _ => {}
                    }
                }
            }
            cmd::AerogpuCmdOpcode::SetSamplerState
            | cmd::AerogpuCmdOpcode::SetRenderState
            | cmd::AerogpuCmdOpcode::SetConstantBuffers => {
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
                self.draw_triangle_list(regs, mem, allocs, &idxs);
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
                    if !self.read_buffer_bytes(regs, mem, allocs, ib_handle, idx_off, &mut tmp) {
                        break;
                    }
                    let raw = if index_size == 2 {
                        u16::from_le_bytes(tmp[0..2].try_into().unwrap()) as i32
                    } else {
                        i32::from_le_bytes(tmp[0..4].try_into().unwrap())
                    };
                    idxs.push(raw.wrapping_add(base_vertex));
                }
                self.draw_triangle_list(regs, mem, allocs, &idxs);
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
                    Self::record_error(regs);
                    return true;
                }
                if self.retired_shared_surface_tokens.contains(&token) {
                    Self::record_error(regs);
                    return true;
                }
                let underlying = self.resolve_handle(handle);
                if !self.texture_refcounts.contains_key(&underlying) {
                    Self::record_error(regs);
                    return true;
                }
                if self.retired_shared_surface_tokens.contains(&token) {
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
                    Self::record_error(regs);
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
                    // Idempotent: unknown tokens are a no-op (see `aerogpu_cmd.h` contract).
                    if self.shared_surfaces.remove(&token).is_some() {
                        self.retired_shared_surface_tokens.insert(token);
                    }
                }
            }
            cmd::AerogpuCmdOpcode::Present
            | cmd::AerogpuCmdOpcode::PresentEx
            | cmd::AerogpuCmdOpcode::Flush => {
                // No-op for software backend (work already executes at submit boundaries).
            }
            _ => {
                // Forward compatibility: ignore opcodes not yet implemented by the software
                // backend. Unknown numeric opcodes are already filtered by `from_u32` above.
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use memory::Bus;
    use std::collections::HashMap;

    #[test]
    fn parse_alloc_table_accepts_capacity_larger_than_header_size_bytes() {
        let entry_stride = ring::AerogpuAllocEntry::SIZE_BYTES;
        let entry_count = 1u32;
        let used_size =
            ring::AerogpuAllocTableHeader::SIZE_BYTES + (entry_count as usize * entry_stride);
        let capacity = used_size + 128;

        let alloc_table_gpa = 0x1000u64;
        let mut table = vec![0u8; capacity];
        table.fill(0xCD);
        table[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
        table[4..8].copy_from_slice(&AeroGpuRegs::default().abi_version.to_le_bytes());
        table[8..12].copy_from_slice(&(used_size as u32).to_le_bytes());
        table[12..16].copy_from_slice(&entry_count.to_le_bytes());
        table[16..20].copy_from_slice(&(entry_stride as u32).to_le_bytes());
        table[20..24].copy_from_slice(&0u32.to_le_bytes());

        let base = ring::AerogpuAllocTableHeader::SIZE_BYTES;
        table[base..base + 4].copy_from_slice(&1u32.to_le_bytes());
        table[base + 4..base + 8].copy_from_slice(&0u32.to_le_bytes()); // flags
        table[base + 8..base + 16].copy_from_slice(&0x2000u64.to_le_bytes()); // gpa
        table[base + 16..base + 24].copy_from_slice(&0x100u64.to_le_bytes()); // size_bytes
        table[base + 24..base + 32].copy_from_slice(&0u64.to_le_bytes()); // reserved0

        table[used_size..].fill(0xAB);

        let mut mem = Bus::new(0x4000);
        mem.write_physical(alloc_table_gpa, &table);

        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            cmd_gpa: 0,
            cmd_size_bytes: 0,
            alloc_table_gpa,
            alloc_table_size_bytes: capacity as u32,
            signal_fence: 0,
        };

        let exec = AeroGpuSoftwareExecutor::new();
        let mut regs = AeroGpuRegs::default();
        let allocs = exec
            .parse_alloc_table(&mut regs, &mut mem, &desc)
            .expect("alloc table should parse");
        assert_eq!(allocs.len(), 1);
        assert_eq!(allocs.get(&1).unwrap().size_bytes, 0x100);
        assert_eq!(regs.irq_status, 0);
    }

    #[test]
    fn parse_alloc_table_accepts_extended_entry_stride() {
        let entry_stride = ring::AerogpuAllocEntry::SIZE_BYTES + 16;
        let entry_count = 2u32;
        let total_size =
            ring::AerogpuAllocTableHeader::SIZE_BYTES + (entry_count as usize * entry_stride);

        let alloc_table_gpa = 0x1000u64;
        let mut table = vec![0u8; total_size];
        table[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
        table[4..8].copy_from_slice(&AeroGpuRegs::default().abi_version.to_le_bytes());
        table[8..12].copy_from_slice(&(total_size as u32).to_le_bytes());
        table[12..16].copy_from_slice(&entry_count.to_le_bytes());
        table[16..20].copy_from_slice(&(entry_stride as u32).to_le_bytes());
        table[20..24].copy_from_slice(&0u32.to_le_bytes());

        for i in 0..entry_count as usize {
            let base = ring::AerogpuAllocTableHeader::SIZE_BYTES + i * entry_stride;
            let alloc_id = (i as u32) + 1;
            table[base..base + 4].copy_from_slice(&alloc_id.to_le_bytes());
            table[base + 4..base + 8].copy_from_slice(&0u32.to_le_bytes()); // flags
            table[base + 8..base + 16].copy_from_slice(&(0x2000u64 + i as u64).to_le_bytes()); // gpa
            table[base + 16..base + 24].copy_from_slice(&0x100u64.to_le_bytes()); // size_bytes
            table[base + 24..base + 32].copy_from_slice(&0u64.to_le_bytes()); // reserved0

            // Extension bytes.
            table[base + ring::AerogpuAllocEntry::SIZE_BYTES..base + entry_stride].fill(0xAB);
        }

        let mut mem = Bus::new(0x4000);
        mem.write_physical(alloc_table_gpa, &table);

        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            cmd_gpa: 0,
            cmd_size_bytes: 0,
            alloc_table_gpa,
            alloc_table_size_bytes: total_size as u32,
            signal_fence: 0,
        };

        let exec = AeroGpuSoftwareExecutor::new();
        let mut regs = AeroGpuRegs::default();
        let allocs = exec
            .parse_alloc_table(&mut regs, &mut mem, &desc)
            .expect("alloc table should parse");
        assert_eq!(allocs.len(), 2);
        assert_eq!(allocs.get(&1).unwrap().size_bytes, 0x100);
        assert_eq!(allocs.get(&2).unwrap().size_bytes, 0x100);
        assert_eq!(regs.irq_status, 0);
    }

    #[test]
    fn execute_submission_records_error_on_inconsistent_cmd_descriptor() {
        let mut mem = Bus::new(0x4000);
        let mut regs = AeroGpuRegs::default();

        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            // cmd_size_bytes is non-zero but cmd_gpa is 0.
            cmd_gpa: 0,
            cmd_size_bytes: 24,
            alloc_table_gpa: 0,
            alloc_table_size_bytes: 0,
            signal_fence: 0,
        };

        let mut exec = AeroGpuSoftwareExecutor::new();
        exec.execute_submission(&mut regs, &mut mem, &desc);

        assert_eq!(regs.stats.malformed_submissions, 1);
        assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
    }

    #[test]
    fn execute_submission_records_error_on_cmd_descriptor_address_overflow() {
        let mut mem = Bus::new(0x4000);
        let mut regs = AeroGpuRegs::default();

        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            cmd_gpa: u64::MAX - 8,
            cmd_size_bytes: 24,
            alloc_table_gpa: 0,
            alloc_table_size_bytes: 0,
            signal_fence: 0,
        };

        let mut exec = AeroGpuSoftwareExecutor::new();
        exec.execute_submission(&mut regs, &mut mem, &desc);

        assert_eq!(regs.stats.malformed_submissions, 1);
        assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
    }

    #[test]
    fn execute_submission_accepts_cmd_buffer_larger_than_stream_size_bytes() {
        let cmd_gpa = 0x1000u64;
        let capacity = 64u32;
        let used_size = cmd::AerogpuCmdStreamHeader::SIZE_BYTES as u32;

        let mut stream = vec![0u8; capacity as usize];
        stream.fill(0xCD);
        stream[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream[4..8].copy_from_slice(&AeroGpuRegs::default().abi_version.to_le_bytes());
        stream[8..12].copy_from_slice(&used_size.to_le_bytes());
        stream[12..16].copy_from_slice(&0u32.to_le_bytes()); // flags
        stream[16..20].copy_from_slice(&0u32.to_le_bytes());
        stream[20..24].copy_from_slice(&0u32.to_le_bytes());

        stream[used_size as usize..].fill(0xAB);

        let mut mem = Bus::new(0x4000);
        mem.write_physical(cmd_gpa, &stream);

        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            cmd_gpa,
            cmd_size_bytes: capacity,
            alloc_table_gpa: 0,
            alloc_table_size_bytes: 0,
            signal_fence: 0,
        };

        let mut exec = AeroGpuSoftwareExecutor::new();
        let mut regs = AeroGpuRegs::default();
        exec.execute_submission(&mut regs, &mut mem, &desc);

        assert_eq!(regs.stats.malformed_submissions, 0);
        assert_eq!(regs.irq_status, 0);
    }

    #[test]
    fn execute_submission_create_texture2d_accepts_bc_with_mips() {
        let cmd_gpa = 0x1000u64;
        let cmd_size = (cmd::AerogpuCmdStreamHeader::SIZE_BYTES
            + cmd::AerogpuCmdCreateTexture2d::SIZE_BYTES) as u32;

        let handle = 1u32;
        let width = 8u32;
        let height = 8u32;
        let mip_levels = 2u32;
        let array_layers = 1u32;

        let mut stream = vec![0u8; cmd_size as usize];
        stream[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream[4..8].copy_from_slice(&AeroGpuRegs::default().abi_version.to_le_bytes());
        stream[8..12].copy_from_slice(&cmd_size.to_le_bytes());
        stream[12..16].copy_from_slice(&0u32.to_le_bytes()); // flags
        stream[16..20].copy_from_slice(&0u32.to_le_bytes());
        stream[20..24].copy_from_slice(&0u32.to_le_bytes());

        let cmd_off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
        let cmd_end = cmd_off + cmd::AerogpuCmdCreateTexture2d::SIZE_BYTES;
        let cmd_buf = &mut stream[cmd_off..cmd_end];
        cmd_buf[0..4]
            .copy_from_slice(&(cmd::AerogpuCmdOpcode::CreateTexture2d as u32).to_le_bytes());
        cmd_buf[4..8]
            .copy_from_slice(&(cmd::AerogpuCmdCreateTexture2d::SIZE_BYTES as u32).to_le_bytes());
        cmd_buf[8..12].copy_from_slice(&handle.to_le_bytes());
        cmd_buf[12..16].copy_from_slice(&0u32.to_le_bytes()); // usage_flags
        cmd_buf[16..20].copy_from_slice(&(AeroGpuFormat::Bc1Unorm as u32).to_le_bytes());
        cmd_buf[20..24].copy_from_slice(&width.to_le_bytes());
        cmd_buf[24..28].copy_from_slice(&height.to_le_bytes());
        cmd_buf[28..32].copy_from_slice(&mip_levels.to_le_bytes());
        cmd_buf[32..36].copy_from_slice(&array_layers.to_le_bytes());
        cmd_buf[36..40].copy_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes (tight-pack)
        cmd_buf[40..44].copy_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        cmd_buf[44..48].copy_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        cmd_buf[48..56].copy_from_slice(&0u64.to_le_bytes()); // reserved0

        let mut mem = Bus::new(0x4000);
        mem.write_physical(cmd_gpa, &stream);

        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            cmd_gpa,
            cmd_size_bytes: cmd_size,
            alloc_table_gpa: 0,
            alloc_table_size_bytes: 0,
            signal_fence: 0,
        };

        let mut exec = AeroGpuSoftwareExecutor::new();
        let mut regs = AeroGpuRegs::default();
        exec.execute_submission(&mut regs, &mut mem, &desc);

        assert_eq!(regs.stats.malformed_submissions, 0);
        assert_eq!(regs.irq_status & irq_bits::ERROR, 0);
        assert!(exec.textures.contains_key(&handle));
    }

    #[test]
    fn copy_texture2d_copies_between_mips() {
        let mut mem = Bus::new(0x1000);
        let mut regs = AeroGpuRegs::default();
        let allocs: HashMap<u32, AllocInfo> = HashMap::new();

        let mut exec = AeroGpuSoftwareExecutor::new();

        let src_handle = 1u32;
        let dst_handle = 2u32;

        let format = AeroGpuFormat::R8G8B8A8Unorm;
        let width = 4u32;
        let height = 4u32;
        let mip_levels = 2u32;
        let array_layers = 1u32;
        // Deliberately padded to ensure mip1 offset computation accounts for mip0 row pitch.
        let row_pitch_bytes = 20u32;

        let layout = AeroGpuSoftwareExecutor::texture_2d_linear_layout(
            format,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
        )
        .expect("layout should be valid");

        let total_size = layout.total_size_bytes as usize;
        let make_tex = || Texture2DResource {
            width,
            height,
            format,
            mip_levels: layout.mip_levels,
            array_layers: layout.array_layers,
            row_pitch_bytes: layout.mip0_row_pitch_bytes,
            backing: None,
            data: vec![0u8; total_size],
            dirty: false,
        };

        let mut src_tex = make_tex();
        let dst_tex = make_tex();

        // Fill mip1/layer0 with a recognizable pattern.
        let mip0_size = (layout.mip0_row_pitch_bytes as usize) * (height as usize);
        let mip1_w = (width >> 1).max(1) as usize;
        let mip1_h = (height >> 1).max(1) as usize;
        let mip1_row_pitch = mip1_w * 4;
        let mip1_size = mip1_row_pitch * mip1_h;
        let mip1_offset = mip0_size;
        assert!(mip1_offset + mip1_size <= src_tex.data.len());
        for i in 0..mip1_size {
            src_tex.data[mip1_offset + i] = (i as u8).wrapping_add(1);
        }

        exec.textures.insert(src_handle, src_tex);
        exec.textures.insert(dst_handle, dst_tex);
        exec.texture_refcounts.insert(src_handle, 1);
        exec.texture_refcounts.insert(dst_handle, 1);

        let mut packet = vec![0u8; cmd::AerogpuCmdCopyTexture2d::SIZE_BYTES];
        packet[0..4].copy_from_slice(&(cmd::AerogpuCmdOpcode::CopyTexture2d as u32).to_le_bytes());
        packet[4..8]
            .copy_from_slice(&(cmd::AerogpuCmdCopyTexture2d::SIZE_BYTES as u32).to_le_bytes());
        packet[8..12].copy_from_slice(&dst_handle.to_le_bytes());
        packet[12..16].copy_from_slice(&src_handle.to_le_bytes());
        packet[16..20].copy_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        packet[20..24].copy_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        packet[24..28].copy_from_slice(&1u32.to_le_bytes()); // src_mip_level
        packet[28..32].copy_from_slice(&0u32.to_le_bytes()); // src_array_layer
        packet[32..36].copy_from_slice(&1u32.to_le_bytes()); // dst_x
        packet[36..40].copy_from_slice(&1u32.to_le_bytes()); // dst_y
        packet[40..44].copy_from_slice(&0u32.to_le_bytes()); // src_x
        packet[44..48].copy_from_slice(&0u32.to_le_bytes()); // src_y
        packet[48..52].copy_from_slice(&2u32.to_le_bytes()); // width
        packet[52..56].copy_from_slice(&2u32.to_le_bytes()); // height
        packet[56..60].copy_from_slice(&0u32.to_le_bytes()); // flags
        packet[60..64].copy_from_slice(&0u32.to_le_bytes()); // reserved0

        assert!(exec.dispatch_cmd(&mut regs, &mut mem, &allocs, &packet));
        assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

        let dst_tex = exec.textures.get(&dst_handle).unwrap();
        // Destination is mip0/layer0, so base offset is 0 with padded row pitch.
        let rp = layout.mip0_row_pitch_bytes as usize;
        let row0 = 1usize;
        let row1 = 2usize;
        let col = 1usize;
        let off0 = row0 * rp + col * 4;
        let off1 = row1 * rp + col * 4;
        assert_eq!(&dst_tex.data[off0..off0 + 8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            &dst_tex.data[off1..off1 + 8],
            &[9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert!(dst_tex.dirty);
    }

    #[test]
    fn copy_texture2d_copies_between_array_layers() {
        let mut mem = Bus::new(0x1000);
        let mut regs = AeroGpuRegs::default();
        let allocs: HashMap<u32, AllocInfo> = HashMap::new();

        let mut exec = AeroGpuSoftwareExecutor::new();

        let src_handle = 1u32;
        let dst_handle = 2u32;

        let format = AeroGpuFormat::R8G8B8A8Unorm;
        let width = 4u32;
        let height = 4u32;
        let mip_levels = 2u32;
        let array_layers = 2u32;
        let row_pitch_bytes = 20u32;

        let layout = AeroGpuSoftwareExecutor::texture_2d_linear_layout(
            format,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
        )
        .expect("layout should be valid");

        let total_size = layout.total_size_bytes as usize;
        let make_tex = || Texture2DResource {
            width,
            height,
            format,
            mip_levels: layout.mip_levels,
            array_layers: layout.array_layers,
            row_pitch_bytes: layout.mip0_row_pitch_bytes,
            backing: None,
            data: vec![0u8; total_size],
            dirty: false,
        };

        let mut src_tex = make_tex();
        let dst_tex = make_tex();

        // Fill layer1/mip0 with a recognizable pattern.
        let mip0_size = (layout.mip0_row_pitch_bytes as usize) * (height as usize);
        let mip1_w = (width >> 1).max(1) as usize;
        let mip1_h = (height >> 1).max(1) as usize;
        let mip1_row_pitch = mip1_w * 4;
        let mip1_size = mip1_row_pitch * mip1_h;
        let layer_size = mip0_size + mip1_size;
        let layer1_mip0_offset = layer_size;
        assert!(layer1_mip0_offset + mip0_size <= src_tex.data.len());

        // Write a 2x2 region at the top-left of the slice.
        let rp = layout.mip0_row_pitch_bytes as usize;
        src_tex.data[layer1_mip0_offset..layer1_mip0_offset + 8]
            .copy_from_slice(&[0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7]);
        src_tex.data[layer1_mip0_offset + rp..layer1_mip0_offset + rp + 8]
            .copy_from_slice(&[0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF]);

        exec.textures.insert(src_handle, src_tex);
        exec.textures.insert(dst_handle, dst_tex);
        exec.texture_refcounts.insert(src_handle, 1);
        exec.texture_refcounts.insert(dst_handle, 1);

        let mut packet = vec![0u8; cmd::AerogpuCmdCopyTexture2d::SIZE_BYTES];
        packet[0..4].copy_from_slice(&(cmd::AerogpuCmdOpcode::CopyTexture2d as u32).to_le_bytes());
        packet[4..8]
            .copy_from_slice(&(cmd::AerogpuCmdCopyTexture2d::SIZE_BYTES as u32).to_le_bytes());
        packet[8..12].copy_from_slice(&dst_handle.to_le_bytes());
        packet[12..16].copy_from_slice(&src_handle.to_le_bytes());
        packet[16..20].copy_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        packet[20..24].copy_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        packet[24..28].copy_from_slice(&0u32.to_le_bytes()); // src_mip_level
        packet[28..32].copy_from_slice(&1u32.to_le_bytes()); // src_array_layer
        packet[32..36].copy_from_slice(&0u32.to_le_bytes()); // dst_x
        packet[36..40].copy_from_slice(&0u32.to_le_bytes()); // dst_y
        packet[40..44].copy_from_slice(&0u32.to_le_bytes()); // src_x
        packet[44..48].copy_from_slice(&0u32.to_le_bytes()); // src_y
        packet[48..52].copy_from_slice(&2u32.to_le_bytes()); // width
        packet[52..56].copy_from_slice(&2u32.to_le_bytes()); // height
        packet[56..60].copy_from_slice(&0u32.to_le_bytes()); // flags
        packet[60..64].copy_from_slice(&0u32.to_le_bytes()); // reserved0

        assert!(exec.dispatch_cmd(&mut regs, &mut mem, &allocs, &packet));
        assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

        let dst_tex = exec.textures.get(&dst_handle).unwrap();
        assert_eq!(
            &dst_tex.data[0..8],
            &[0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7]
        );
        assert_eq!(
            &dst_tex.data[rp..rp + 8],
            &[0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF]
        );
        assert!(dst_tex.dirty);
    }
}
