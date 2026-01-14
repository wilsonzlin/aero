//! Host-side executor for the stable AeroGPU guest↔host command stream.
//!
//! The "real" executor is expected to translate the full AeroGPU IR into WebGPU
//! commands. For now we implement a minimal subset needed for validating
//! guest-memory-backed resources (`alloc_table` + `backing_alloc_id`) and
//! `RESOURCE_DIRTY_RANGE` uploads.

use std::collections::HashMap;
use std::ops::Range;

use futures_intrusive::channel::shared::oneshot_channel;

use crate::guest_memory::{GuestMemory, GuestMemoryError};
use crate::{
    decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8,
    expand_b5g5r5a1_unorm_to_rgba8, expand_b5g6r5_unorm_to_rgba8, pack_rgba8_to_b5g5r5a1_unorm,
    pack_rgba8_to_b5g6r5_unorm, TextureUploadTransform,
};

use crate::protocol::{parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError};
use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_pci as pci, aerogpu_ring as ring};

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, ExecutorError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(ExecutorError::TruncatedPacket)?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn align_down_u64(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    value & !(alignment - 1)
}

fn align_up_u64(value: u64, alignment: u64) -> Result<u64, ExecutorError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or_else(|| ExecutorError::Validation("alignment overflow".into()))
}

fn align_up_u32(value: u32, alignment: u32) -> Result<u32, ExecutorError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or_else(|| ExecutorError::Validation("alignment overflow".into()))
}

fn map_buffer_usage_flags(usage_flags: u32) -> wgpu::BufferUsages {
    let mut wgpu_usage = wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;

    // Buffers that can be bound as SRV/UAV/raw/structured are represented as `var<storage>` in WGSL.
    // wgpu validates that any buffer used in a storage binding was created with
    // `wgpu::BufferUsages::STORAGE`.
    let mut needs_storage = (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_STORAGE) != 0;

    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER) != 0 {
        wgpu_usage |= wgpu::BufferUsages::VERTEX;
        needs_storage = true;
    }
    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_INDEX_BUFFER) != 0 {
        wgpu_usage |= wgpu::BufferUsages::INDEX;
        needs_storage = true;
    }

    // IA buffers may be consumed by compute prepasses (vertex pulling / expansion). WebGPU requires
    // them to be created with `STORAGE` in order to bind as `var<storage>`.
    if needs_storage {
        wgpu_usage |= wgpu::BufferUsages::STORAGE;
    }

    if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER) != 0 {
        wgpu_usage |= wgpu::BufferUsages::UNIFORM;
    }

    wgpu_usage
}

fn is_x8_format(format_raw: u32) -> bool {
    format_raw == pci::AerogpuFormat::B8G8R8X8Unorm as u32
        || format_raw == pci::AerogpuFormat::R8G8B8X8Unorm as u32
        || format_raw == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
        || format_raw == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32
}

fn is_bc_format(format_raw: u32) -> bool {
    format_raw == pci::AerogpuFormat::BC1RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC1RgbaUnormSrgb as u32
        || format_raw == pci::AerogpuFormat::BC2RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC2RgbaUnormSrgb as u32
        || format_raw == pci::AerogpuFormat::BC3RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC3RgbaUnormSrgb as u32
        || format_raw == pci::AerogpuFormat::BC7RgbaUnorm as u32
        || format_raw == pci::AerogpuFormat::BC7RgbaUnormSrgb as u32
}

#[derive(Debug, Clone, Copy)]
struct TextureCopyLayout {
    block_w: u32,
    block_h: u32,
    block_bytes: u32,

    /// Number of rows in `ImageDataLayout` / guest memory layout (texel rows for uncompressed,
    /// block rows for BC).
    rows_in_layout: u32,
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
}

fn texture_copy_layout(
    width: u32,
    height: u32,
    format_raw: u32,
) -> Result<TextureCopyLayout, ExecutorError> {
    let (block_w, block_h, block_bytes) = match format_raw {
        v if v == pci::AerogpuFormat::B8G8R8A8Unorm as u32
            || v == pci::AerogpuFormat::B8G8R8X8Unorm as u32
            || v == pci::AerogpuFormat::R8G8B8A8Unorm as u32
            || v == pci::AerogpuFormat::R8G8B8X8Unorm as u32
            || v == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
            || v == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
            || v == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32
            || v == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
        {
            (1, 1, 4)
        }
        v if v == pci::AerogpuFormat::B5G6R5Unorm as u32
            || v == pci::AerogpuFormat::B5G5R5A1Unorm as u32 =>
        {
            (1, 1, 2)
        }
        v if v == pci::AerogpuFormat::BC1RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC1RgbaUnormSrgb as u32 =>
        {
            (4, 4, 8)
        }
        v if v == pci::AerogpuFormat::BC2RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC2RgbaUnormSrgb as u32
            || v == pci::AerogpuFormat::BC3RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC3RgbaUnormSrgb as u32
            || v == pci::AerogpuFormat::BC7RgbaUnorm as u32
            || v == pci::AerogpuFormat::BC7RgbaUnormSrgb as u32 =>
        {
            (4, 4, 16)
        }
        _ => {
            return Err(ExecutorError::Validation(format!(
                "unsupported aerogpu_format={format_raw}"
            )))
        }
    };

    let blocks_w = width.div_ceil(block_w);
    let blocks_h = height.div_ceil(block_h);
    let unpadded_bytes_per_row = blocks_w
        .checked_mul(block_bytes)
        .ok_or_else(|| ExecutorError::Validation("texture bytes_per_row overflow".into()))?;
    let padded_bytes_per_row =
        align_up_u32(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)?;
    Ok(TextureCopyLayout {
        block_w,
        block_h,
        block_bytes,
        rows_in_layout: blocks_h,
        unpadded_bytes_per_row,
        padded_bytes_per_row,
    })
}

fn upload_resource_texture_row_range(
    handle: u32,
    offset_bytes: u64,
    size_bytes: u64,
    row_pitch_bytes: u64,
    rows_in_layout: u32,
) -> Result<(u32, u32, u32), ExecutorError> {
    if row_pitch_bytes == 0 {
        return Err(ExecutorError::Validation(format!(
            "UPLOAD_RESOURCE texture {handle} is missing row_pitch_bytes"
        )));
    }
    let start_row_u64 = offset_bytes / row_pitch_bytes;
    let row_count_u64 = size_bytes / row_pitch_bytes;
    let end_row_u64 = start_row_u64.checked_add(row_count_u64).ok_or_else(|| {
        ExecutorError::Validation("UPLOAD_RESOURCE texture row range overflow".into())
    })?;
    if end_row_u64 > u64::from(rows_in_layout) {
        return Err(ExecutorError::Validation(format!(
            "UPLOAD_RESOURCE out of bounds for texture {handle} (rows {start_row_u64}..{end_row_u64}, rows_in_layout={rows_in_layout})"
        )));
    }

    Ok((
        u32::try_from(start_row_u64).map_err(|_| {
            ExecutorError::Validation("UPLOAD_RESOURCE texture start_row out of range".into())
        })?,
        u32::try_from(row_count_u64).map_err(|_| {
            ExecutorError::Validation("UPLOAD_RESOURCE texture row_count out of range".into())
        })?,
        u32::try_from(end_row_u64).map_err(|_| {
            ExecutorError::Validation("UPLOAD_RESOURCE texture end_row out of range".into())
        })?,
    ))
}

fn mip_dim(base: u32, mip_level: u32) -> u32 {
    // D3D and WebGPU clamp mip dimensions to 1x1 at the tail.
    base.checked_shr(mip_level).unwrap_or(0).max(1)
}

fn build_texture2d_subresource_layouts(
    format_raw: u32,
    width: u32,
    height: u32,
    mip_levels: u32,
    array_layers: u32,
    mip0_row_pitch_bytes: u32,
) -> Result<(Vec<TextureSubresourceLayout>, u64), ExecutorError> {
    if width == 0 || height == 0 {
        return Err(ExecutorError::Validation(
            "CREATE_TEXTURE2D width/height must be non-zero".into(),
        ));
    }
    if mip_levels == 0 || array_layers == 0 {
        return Err(ExecutorError::Validation(
            "CREATE_TEXTURE2D mip_levels/array_layers must be non-zero".into(),
        ));
    }

    let mip_levels_usize = usize::try_from(mip_levels).map_err(|_| {
        ExecutorError::Validation("CREATE_TEXTURE2D mip_levels out of range for usize".into())
    })?;
    let array_layers_usize = usize::try_from(array_layers).map_err(|_| {
        ExecutorError::Validation("CREATE_TEXTURE2D array_layers out of range for usize".into())
    })?;

    let subresource_count = mip_levels_usize
        .checked_mul(array_layers_usize)
        .ok_or_else(|| {
            ExecutorError::Validation("CREATE_TEXTURE2D subresource count overflow".into())
        })?;

    let mut layouts = Vec::new();
    layouts.try_reserve_exact(subresource_count).map_err(|_| {
        ExecutorError::Validation("CREATE_TEXTURE2D subresource layout allocation failed".into())
    })?;

    let mut offset_bytes = 0u64;
    for array_layer in 0..array_layers {
        for mip_level in 0..mip_levels {
            let mip_w = mip_dim(width, mip_level);
            let mip_h = mip_dim(height, mip_level);
            let layout = texture_copy_layout(mip_w, mip_h, format_raw)?;

            let tight_row_pitch = layout.unpadded_bytes_per_row;
            let row_pitch_bytes = if mip_level == 0 {
                if mip0_row_pitch_bytes != 0 {
                    mip0_row_pitch_bytes
                } else {
                    tight_row_pitch
                }
            } else {
                tight_row_pitch
            };

            if row_pitch_bytes < tight_row_pitch {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D mip{mip_level} row_pitch_bytes={row_pitch_bytes} smaller than minimum row size {tight_row_pitch}"
                )));
            }

            let size_bytes = u64::from(row_pitch_bytes)
                .checked_mul(u64::from(layout.rows_in_layout))
                .ok_or_else(|| {
                    ExecutorError::Validation("CREATE_TEXTURE2D subresource size overflow".into())
                })?;

            layouts.push(TextureSubresourceLayout {
                mip_level,
                array_layer,
                width: mip_w,
                height: mip_h,
                offset_bytes,
                row_pitch_bytes,
                rows_in_layout: layout.rows_in_layout,
                size_bytes,
            });

            offset_bytes = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("CREATE_TEXTURE2D total texture size overflow".into())
            })?;
        }
    }

    Ok((layouts, offset_bytes))
}

fn force_opaque_alpha_rgba8(pixels: &mut [u8]) {
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 0xFF;
    }
}

fn map_cmd_stream_parse_error(err: AeroGpuCmdStreamParseError) -> ExecutorError {
    match err {
        AeroGpuCmdStreamParseError::BufferTooSmall => ExecutorError::TruncatedStream,
        AeroGpuCmdStreamParseError::InvalidMagic(found) => ExecutorError::BadStreamMagic(found),
        AeroGpuCmdStreamParseError::UnsupportedAbiMajor { found } => {
            ExecutorError::Validation(format!("unsupported ABI major version {found}"))
        }
        AeroGpuCmdStreamParseError::InvalidSizeBytes {
            size_bytes,
            buffer_len,
        } => ExecutorError::BadStreamSize {
            size_bytes,
            buffer_len,
        },
        AeroGpuCmdStreamParseError::InvalidCmdSizeBytes(found) => {
            ExecutorError::InvalidPacketSize(found)
        }
        AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(found) => {
            ExecutorError::MisalignedPacketSize(found)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("command stream too small")]
    TruncatedStream,
    #[error("invalid command stream magic 0x{0:08x}")]
    BadStreamMagic(u32),
    #[error("invalid command stream size_bytes={size_bytes} (buffer_len={buffer_len})")]
    BadStreamSize { size_bytes: u32, buffer_len: usize },
    #[error("truncated packet")]
    TruncatedPacket,
    #[error("invalid packet size_bytes={0}")]
    InvalidPacketSize(u32),
    #[error("packet size_bytes={0} is not 4-byte aligned")]
    MisalignedPacketSize(u32),

    #[error("validation error: {0}")]
    Validation(String),

    #[error(transparent)]
    GuestMemory(#[from] GuestMemoryError),
}

#[derive(Debug, Clone)]
pub enum ExecutorEvent {
    Error { at: usize, message: String },
}

#[derive(Debug, Clone)]
pub struct ExecutionReport {
    pub packets_processed: u32,
    pub events: Vec<ExecutorEvent>,
}

impl ExecutionReport {
    pub fn is_ok(&self) -> bool {
        !self
            .events
            .iter()
            .any(|e| matches!(e, ExecutorEvent::Error { .. }))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AllocEntry {
    pub flags: u32,
    pub gpa: u64,
    pub size_bytes: u64,
}

#[derive(Debug, Default, Clone)]
pub struct AllocTable {
    entries: HashMap<u32, AllocEntry>,
}

impl AllocTable {
    pub fn new(
        entries: impl IntoIterator<Item = (u32, AllocEntry)>,
    ) -> Result<Self, ExecutorError> {
        let mut map = HashMap::<u32, AllocEntry>::new();
        for (alloc_id, entry) in entries {
            if alloc_id == 0 {
                return Err(ExecutorError::Validation(
                    "alloc table entry alloc_id must be non-zero".into(),
                ));
            }
            if entry.size_bytes == 0 {
                return Err(ExecutorError::Validation(format!(
                    "alloc table entry {alloc_id} has size_bytes=0"
                )));
            }
            if entry.gpa.checked_add(entry.size_bytes).is_none() {
                return Err(ExecutorError::Validation(format!(
                    "alloc table entry {alloc_id} gpa+size overflow"
                )));
            }
            if let Some(existing) = map.insert(alloc_id, entry) {
                return Err(ExecutorError::Validation(format!(
                    "alloc table contains duplicate alloc_id={alloc_id} (gpa0=0x{:x} size0={} gpa1=0x{:x} size1={})",
                    existing.gpa, existing.size_bytes, entry.gpa, entry.size_bytes
                )));
            }
        }
        Ok(Self { entries: map })
    }

    pub fn get(&self, alloc_id: u32) -> Option<&AllocEntry> {
        self.entries.get(&alloc_id)
    }

    fn resolve_gpa(&self, alloc_id: u32, offset: u64, size: u64) -> Result<u64, ExecutorError> {
        let entry = self.get(alloc_id).ok_or_else(|| {
            ExecutorError::Validation(format!("missing alloc table entry for alloc_id={alloc_id}"))
        })?;

        let end = offset.checked_add(size).ok_or_else(|| {
            ExecutorError::Validation("alloc table range offset+size overflow".into())
        })?;
        if end > entry.size_bytes {
            return Err(ExecutorError::Validation(format!(
                "alloc table range out of bounds for alloc_id={alloc_id} (offset=0x{offset:x}, size=0x{size:x}, alloc_size=0x{:x})",
                entry.size_bytes
            )));
        }

        let gpa = entry
            .gpa
            .checked_add(offset)
            .ok_or_else(|| ExecutorError::Validation("alloc table gpa+offset overflow".into()))?;
        if gpa.checked_add(size).is_none() {
            return Err(ExecutorError::Validation(
                "alloc table gpa+size overflow".into(),
            ));
        }

        Ok(gpa)
    }

    pub fn decode_from_guest_memory(
        guest_memory: &mut dyn GuestMemory,
        table_gpa: u64,
        table_size_bytes: u32,
    ) -> Result<Self, ExecutorError> {
        const MAX_ALLOC_TABLE_SIZE_BYTES: u32 = 16 * 1024 * 1024;

        if table_gpa == 0 || table_size_bytes == 0 {
            return Err(ExecutorError::Validation(
                "alloc table gpa/size must be non-zero".into(),
            ));
        }
        if table_gpa.checked_add(u64::from(table_size_bytes)).is_none() {
            return Err(ExecutorError::Validation(
                "alloc table gpa+size overflow".into(),
            ));
        }

        let table_size = table_size_bytes as usize;
        if table_size < ring::AerogpuAllocTableHeader::SIZE_BYTES {
            return Err(ExecutorError::Validation(format!(
                "alloc table size_bytes too small (got {table_size_bytes}, need {})",
                ring::AerogpuAllocTableHeader::SIZE_BYTES
            )));
        }

        let mut header_bytes = [0u8; ring::AerogpuAllocTableHeader::SIZE_BYTES];
        guest_memory.read(table_gpa, &mut header_bytes)?;
        let header =
            ring::AerogpuAllocTableHeader::decode_from_le_bytes(&header_bytes).map_err(|err| {
                ExecutorError::Validation(format!("failed to decode alloc table header: {err:?}"))
            })?;
        header.validate_prefix().map_err(|err| {
            ExecutorError::Validation(format!("invalid alloc table header: {err:?}"))
        })?;

        let size_bytes = header.size_bytes;
        if size_bytes > MAX_ALLOC_TABLE_SIZE_BYTES {
            return Err(ExecutorError::Validation(format!(
                "alloc table header size_bytes too large (got {size_bytes}, max {MAX_ALLOC_TABLE_SIZE_BYTES})"
            )));
        }
        let size_usize = size_bytes as usize;
        if size_usize < ring::AerogpuAllocTableHeader::SIZE_BYTES || size_usize > table_size {
            return Err(ExecutorError::Validation(format!(
                "invalid alloc table header size_bytes={size_bytes} (provided buffer size={table_size_bytes})"
            )));
        }
        // Forward-compat: newer guests may extend `aerogpu_alloc_entry` and increase the declared
        // stride; we only read the entry prefix we understand.
        if header.entry_stride_bytes < ring::AerogpuAllocEntry::SIZE_BYTES as u32 {
            return Err(ExecutorError::Validation(format!(
                "invalid alloc table entry_stride_bytes={} (expected at least {})",
                header.entry_stride_bytes,
                ring::AerogpuAllocEntry::SIZE_BYTES
            )));
        }

        let entry_count = header.entry_count;
        let entry_stride_bytes = header.entry_stride_bytes;

        let entry_count_usize = usize::try_from(entry_count).map_err(|_| {
            ExecutorError::Validation(
                "alloc table entry_count is out of range for host usize".into(),
            )
        })?;
        let mut entries = Vec::<(u32, AllocEntry)>::new();
        if entries.try_reserve_exact(entry_count_usize).is_err() {
            return Err(ExecutorError::Validation(format!(
                "alloc table too large to allocate (entry_count={entry_count})"
            )));
        }
        for i in 0..entry_count {
            let entry_offset = (i as u64)
                .checked_mul(entry_stride_bytes as u64)
                .ok_or_else(|| {
                    ExecutorError::Validation("alloc table entry offset overflow".into())
                })?;
            let entry_gpa = table_gpa
                .checked_add(ring::AerogpuAllocTableHeader::SIZE_BYTES as u64)
                .and_then(|gpa| gpa.checked_add(entry_offset))
                .ok_or_else(|| {
                    ExecutorError::Validation("alloc table entry gpa overflow".into())
                })?;
            let mut entry_bytes = [0u8; ring::AerogpuAllocEntry::SIZE_BYTES];
            guest_memory.read(entry_gpa, &mut entry_bytes)?;

            let entry =
                ring::AerogpuAllocEntry::decode_from_le_bytes(&entry_bytes).map_err(|err| {
                    ExecutorError::Validation(format!(
                        "failed to decode alloc table entry {i}: {err:?}"
                    ))
                })?;
            entries.push((
                entry.alloc_id,
                AllocEntry {
                    flags: entry.flags,
                    gpa: entry.gpa,
                    size_bytes: entry.size_bytes,
                },
            ));
        }

        AllocTable::new(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct GuestBufferBacking {
    alloc_id: u32,
    alloc_offset_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct GuestTextureBacking {
    alloc_id: u32,
    alloc_offset_bytes: u64,
    size_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct TextureSubresourceLayout {
    mip_level: u32,
    array_layer: u32,
    width: u32,
    height: u32,
    offset_bytes: u64,
    row_pitch_bytes: u32,
    rows_in_layout: u32,
    size_bytes: u64,
}

#[derive(Debug)]
struct BufferResource {
    buffer: wgpu::Buffer,
    size_bytes: u64,
    usage_flags: u32,
    backing: Option<GuestBufferBacking>,
    dirty_ranges: Vec<Range<u64>>,
}

#[derive(Debug)]
struct TextureResource {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    usage_flags: u32,
    format_raw: u32,
    row_pitch_bytes: u32,
    width: u32,
    height: u32,
    mip_levels: u32,
    array_layers: u32,
    format: wgpu::TextureFormat,
    upload_transform: TextureUploadTransform,
    subresource_layouts: Vec<TextureSubresourceLayout>,
    backing: Option<GuestTextureBacking>,
    dirty_ranges: Vec<Range<u64>>,
}

#[derive(Debug, Clone, Copy)]
struct CreateTexture2dArgs {
    texture_handle: u32,
    usage_flags: u32,
    format: u32,
    width: u32,
    height: u32,
    mip_levels: u32,
    array_layers: u32,
    row_pitch_bytes: u32,
    backing_alloc_id: u32,
    backing_offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct BoundVertexBuffer {
    buffer: u32,
    stride_bytes: u32,
    offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct BoundIndexBuffer {
    buffer: u32,
    format: wgpu::IndexFormat,
    offset_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct CopyBufferArgs {
    dst_buffer: u32,
    src_buffer: u32,
    dst_offset_bytes: u64,
    src_offset_bytes: u64,
    size_bytes: u64,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
struct CopyTexture2dArgs {
    dst_texture: u32,
    src_texture: u32,
    dst_mip_level: u32,
    dst_array_layer: u32,
    src_mip_level: u32,
    src_array_layer: u32,
    dst_x: u32,
    dst_y: u32,
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
struct DrawIndexedArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

#[derive(Debug, Default)]
struct ExecutorState {
    render_target: Option<u32>,
    vertex_buffer: Option<BoundVertexBuffer>,
    index_buffer: Option<BoundIndexBuffer>,
    pixel_texture0: Option<u32>,
}

/// Minimal host-side executor that implements the resource backing + dirty-range upload logic.
///
/// This is currently test-focused and only implements a subset of the full AeroGPU IR.
pub struct AeroGpuExecutor {
    device: wgpu::Device,
    queue: wgpu::Queue,

    buffers: HashMap<u32, BufferResource>,
    textures: HashMap<u32, TextureResource>,

    state: ExecutorState,

    pipelines: HashMap<(wgpu::TextureFormat, bool), wgpu::RenderPipeline>,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

#[derive(Debug, Clone, Copy)]
struct TextureWritebackPlan {
    base_gpa: u64,
    row_pitch: u64,
    rows: u32,
    format_raw: u32,
    is_x8: bool,
    staging_padded_bytes_per_row: u32,
    staging_unpadded_bytes_per_row: u32,
    dst_unpadded_bytes_per_row: u32,
}

#[derive(Debug)]
enum PendingWriteback {
    Buffer {
        staging: wgpu::Buffer,
        dst_gpa: u64,
        size_bytes: u64,
    },
    Texture2d {
        staging: wgpu::Buffer,
        plan: TextureWritebackPlan,
    },
}

impl AeroGpuExecutor {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, ExecutorError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aerogpu.executor.shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var tex0: texture_2d<f32>;
@group(0) @binding(1) var samp0: sampler;

struct VSIn {
  @location(0) pos: vec2<f32>,
};

@vertex
fn vs_main(in: VSIn) -> @builtin(position) vec4<f32> {
  return vec4<f32>(in.pos, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
  // Sample the center of texel (0,0) so tests remain deterministic even when the
  // source texture is larger than 1x1 and contains non-uniform data.
  let dims = vec2<f32>(textureDimensions(tex0));
  let uv = vec2<f32>(0.5, 0.5) / dims;
  return textureSample(tex0, samp0, uv);
}
"#
                .into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aerogpu.executor.bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aerogpu.executor.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aerogpu.executor.pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            }],
        }];

        let mut pipelines = HashMap::new();
        for fmt in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ] {
            for (is_x8, write_mask) in [
                (false, wgpu::ColorWrites::ALL),
                // X8 render targets treat alpha as always opaque. Avoid writing alpha so a clear
                // that forces alpha=1.0 stays intact across draws.
                (true, wgpu::ColorWrites::ALL & !wgpu::ColorWrites::ALPHA),
            ] {
                let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("aerogpu.executor.pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: "vs_main",
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &vertex_buffers,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: "fs_main",
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: fmt,
                            blend: Some(wgpu::BlendState::REPLACE),
                            write_mask,
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
                pipelines.insert((fmt, is_x8), pipeline);
            }
        }

        Ok(Self {
            device,
            queue,
            buffers: HashMap::new(),
            textures: HashMap::new(),
            state: ExecutorState::default(),
            pipelines,
            bind_group_layout,
            sampler,
        })
    }

    /// Reset executor state and drop all tracked resources.
    ///
    /// This is primarily intended for tests so they can reuse a single `wgpu::Device` without
    /// repeatedly creating/destroying devices (which can trigger allocator crashes on some
    /// backends/drivers).
    pub fn reset(&mut self) {
        self.buffers.clear();
        self.textures.clear();
        self.state = ExecutorState::default();
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn texture(&self, handle: u32) -> Option<&wgpu::Texture> {
        self.textures.get(&handle).map(|t| &t.texture)
    }

    pub fn process_cmd_stream(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> ExecutionReport {
        // Avoid partially executing streams on wasm when `WRITEBACK_DST` is present. The writeback
        // requires `wgpu::Buffer::map_async` completion, which is delivered via the JS event loop
        // and cannot be waited on synchronously.
        #[cfg(target_arch = "wasm32")]
        {
            let stream = match parse_cmd_stream(bytes) {
                Ok(stream) => stream,
                Err(err) => {
                    return ExecutionReport {
                        packets_processed: 0,
                        events: vec![ExecutorEvent::Error {
                            at: 0,
                            message: map_cmd_stream_parse_error(err).to_string(),
                        }],
                    };
                }
            };
            let writeback_at = stream.cmds.iter().position(|cmd| match cmd {
                AeroGpuCmd::CopyBuffer { flags, .. } | AeroGpuCmd::CopyTexture2d { flags, .. } => {
                    (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0
                }
                _ => false,
            });
            if let Some(at) = writeback_at {
                return ExecutionReport {
                    packets_processed: 0,
                    events: vec![ExecutorEvent::Error {
                        at,
                        message: format!(
                            "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async); first WRITEBACK_DST at packet {at}"
                        ),
                    }],
                };
            }
        }

        let mut pending_writebacks = Vec::new();
        match self.execute_cmd_stream_internal(
            bytes,
            guest_memory,
            alloc_table,
            &mut pending_writebacks,
        ) {
            Ok(packets_processed) => {
                if pending_writebacks.is_empty() {
                    ExecutionReport {
                        packets_processed,
                        events: Vec::new(),
                    }
                } else {
                    #[cfg(target_arch = "wasm32")]
                    {
                        ExecutionReport {
                            packets_processed,
                            events: vec![ExecutorEvent::Error {
                                at: 0,
                                message: "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async)".into(),
                            }],
                        }
                    }

                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        match self
                            .flush_pending_writebacks_blocking(pending_writebacks, guest_memory)
                        {
                            Ok(()) => ExecutionReport {
                                packets_processed,
                                events: Vec::new(),
                            },
                            Err(err) => ExecutionReport {
                                packets_processed,
                                events: vec![ExecutorEvent::Error {
                                    at: 0,
                                    message: err.to_string(),
                                }],
                            },
                        }
                    }
                }
            }
            Err((at, err, packets_processed)) => ExecutionReport {
                packets_processed,
                events: vec![ExecutorEvent::Error {
                    at,
                    message: err.to_string(),
                }],
            },
        }
    }

    pub fn process_submission_from_guest_memory(
        &mut self,
        guest_memory: &mut dyn GuestMemory,
        cmd_gpa: u64,
        cmd_size_bytes: u32,
        alloc_table_gpa: u64,
        alloc_table_size_bytes: u32,
    ) -> ExecutionReport {
        let alloc_table = if alloc_table_gpa == 0 && alloc_table_size_bytes == 0 {
            None
        } else {
            match AllocTable::decode_from_guest_memory(
                guest_memory,
                alloc_table_gpa,
                alloc_table_size_bytes,
            ) {
                Ok(table) => Some(table),
                Err(err) => {
                    return ExecutionReport {
                        packets_processed: 0,
                        events: vec![ExecutorEvent::Error {
                            at: 0,
                            message: format!("failed to decode alloc table: {err}"),
                        }],
                    };
                }
            }
        };

        if cmd_gpa == 0 && cmd_size_bytes == 0 {
            return ExecutionReport {
                packets_processed: 0,
                events: Vec::new(),
            };
        }
        if cmd_gpa == 0 || cmd_size_bytes == 0 {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message:
                        "invalid command stream descriptor: cmd_gpa and cmd_size_bytes must be both zero or both non-zero"
                            .into(),
                }],
            };
        }

        const MAX_CMD_STREAM_SIZE_BYTES: u32 = 64 * 1024 * 1024;
        if cmd_gpa.checked_add(u64::from(cmd_size_bytes)).is_none() {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: "command stream gpa+size overflow".into(),
                }],
            };
        }

        // Forward-compat: `cmd_size_bytes` is the backing buffer size, while the command stream
        // header's `size_bytes` indicates how many bytes are actually used.
        let header_size = cmd::AerogpuCmdStreamHeader::SIZE_BYTES as u32;
        if cmd_size_bytes < header_size {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("command stream buffer too small: {cmd_size_bytes} bytes"),
                }],
            };
        }

        let mut header_bytes = [0u8; cmd::AerogpuCmdStreamHeader::SIZE_BYTES];
        if let Err(err) = guest_memory.read(cmd_gpa, &mut header_bytes) {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("failed to read command stream header: {err}"),
                }],
            };
        }

        let header = match cmd::decode_cmd_stream_header_le(&header_bytes) {
            Ok(hdr) => hdr,
            Err(err) => {
                return ExecutionReport {
                    packets_processed: 0,
                    events: vec![ExecutorEvent::Error {
                        at: 0,
                        message: format!("failed to decode command stream header: {err:?}"),
                    }],
                };
            }
        };

        let declared_size_bytes = header.size_bytes;
        if declared_size_bytes > cmd_size_bytes {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!(
                        "command stream header size_bytes too large (size_bytes={declared_size_bytes} > cmd_size_bytes={cmd_size_bytes})"
                    ),
                }],
            };
        }
        if declared_size_bytes > MAX_CMD_STREAM_SIZE_BYTES {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("command stream too large: {declared_size_bytes} bytes"),
                }],
            };
        }

        let cmd_size = declared_size_bytes as usize;
        let mut cmd_bytes = Vec::<u8>::new();
        if cmd_bytes.try_reserve_exact(cmd_size).is_err() {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!(
                        "failed to allocate command stream buffer of size {declared_size_bytes} bytes"
                    ),
                }],
            };
        }
        cmd_bytes.resize(cmd_size, 0u8);
        if let Err(err) = guest_memory.read(cmd_gpa, &mut cmd_bytes) {
            return ExecutionReport {
                packets_processed: 0,
                events: vec![ExecutorEvent::Error {
                    at: 0,
                    message: format!("failed to read command stream bytes: {err}"),
                }],
            };
        }

        match alloc_table.as_ref() {
            Some(table) => self.process_cmd_stream(&cmd_bytes, guest_memory, Some(table)),
            None => self.process_cmd_stream(&cmd_bytes, guest_memory, None),
        }
    }

    pub fn execute_cmd_stream(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        #[cfg(target_arch = "wasm32")]
        {
            let stream = parse_cmd_stream(bytes).map_err(map_cmd_stream_parse_error)?;
            let writeback_at = stream.cmds.iter().position(|cmd| match cmd {
                AeroGpuCmd::CopyBuffer { flags, .. } | AeroGpuCmd::CopyTexture2d { flags, .. } => {
                    (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0
                }
                _ => false,
            });
            if let Some(at) = writeback_at {
                return Err(ExecutorError::Validation(
                    format!(
                        "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async); first WRITEBACK_DST at packet {at}"
                    ),
                ));
            }
        }

        let mut pending_writebacks = Vec::new();
        self.execute_cmd_stream_internal(bytes, guest_memory, alloc_table, &mut pending_writebacks)
            .map(|_| ())
            .map_err(|(_, err, _)| err)?;

        if pending_writebacks.is_empty() {
            Ok(())
        } else {
            #[cfg(target_arch = "wasm32")]
            {
                Err(ExecutorError::Validation(
                    "WRITEBACK_DST requires async execution on wasm (call execute_cmd_stream_async)"
                        .into(),
                ))
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.flush_pending_writebacks_blocking(pending_writebacks, guest_memory)?;
                Ok(())
            }
        }
    }

    /// WASM-friendly async variant of `execute_cmd_stream`.
    ///
    /// On WASM targets, `wgpu::Buffer::map_async` completion is delivered via the JS event loop,
    /// so synchronous waiting would deadlock. This method awaits writeback staging buffer maps
    /// when `AEROGPU_COPY_FLAG_WRITEBACK_DST` is used.
    pub async fn execute_cmd_stream_async(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let mut pending_writebacks = Vec::new();
        self.execute_cmd_stream_internal(bytes, guest_memory, alloc_table, &mut pending_writebacks)
            .map(|_| ())
            .map_err(|(_, err, _)| err)?;

        if !pending_writebacks.is_empty() {
            self.flush_pending_writebacks_async(pending_writebacks, guest_memory)
                .await?;
        }

        Ok(())
    }

    fn execute_cmd_stream_internal(
        &mut self,
        bytes: &[u8],
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<u32, (usize, ExecutorError, u32)> {
        let stream =
            parse_cmd_stream(bytes).map_err(|err| (0, map_cmd_stream_parse_error(err), 0))?;

        let mut packets_processed = 0u32;
        for cmd in stream.cmds {
            let result = match cmd {
                AeroGpuCmd::CreateBuffer {
                    buffer_handle,
                    usage_flags,
                    size_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                } => self.exec_create_buffer(
                    buffer_handle,
                    usage_flags,
                    size_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                    alloc_table,
                ),
                AeroGpuCmd::CreateTexture2d {
                    texture_handle,
                    usage_flags,
                    format,
                    width,
                    height,
                    mip_levels,
                    array_layers,
                    row_pitch_bytes,
                    backing_alloc_id,
                    backing_offset_bytes,
                } => self.exec_create_texture2d(
                    CreateTexture2dArgs {
                        texture_handle,
                        usage_flags,
                        format,
                        width,
                        height,
                        mip_levels,
                        array_layers,
                        row_pitch_bytes,
                        backing_alloc_id,
                        backing_offset_bytes,
                    },
                    alloc_table,
                ),
                AeroGpuCmd::DestroyResource { resource_handle } => {
                    self.exec_destroy_resource(resource_handle)
                }
                AeroGpuCmd::ResourceDirtyRange {
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                } => self.exec_resource_dirty_range(
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                    alloc_table,
                ),
                AeroGpuCmd::UploadResource {
                    resource_handle,
                    offset_bytes,
                    size_bytes,
                    data,
                } => self.exec_upload_resource(resource_handle, offset_bytes, size_bytes, data),
                AeroGpuCmd::CopyBuffer {
                    dst_buffer,
                    src_buffer,
                    dst_offset_bytes,
                    src_offset_bytes,
                    size_bytes,
                    flags,
                } => self.exec_copy_buffer(
                    CopyBufferArgs {
                        dst_buffer,
                        src_buffer,
                        dst_offset_bytes,
                        src_offset_bytes,
                        size_bytes,
                        flags,
                    },
                    guest_memory,
                    alloc_table,
                    pending_writebacks,
                ),
                AeroGpuCmd::CopyTexture2d {
                    dst_texture,
                    src_texture,
                    dst_mip_level,
                    dst_array_layer,
                    src_mip_level,
                    src_array_layer,
                    dst_x,
                    dst_y,
                    src_x,
                    src_y,
                    width,
                    height,
                    flags,
                } => self.exec_copy_texture2d(
                    CopyTexture2dArgs {
                        dst_texture,
                        src_texture,
                        dst_mip_level,
                        dst_array_layer,
                        src_mip_level,
                        src_array_layer,
                        dst_x,
                        dst_y,
                        src_x,
                        src_y,
                        width,
                        height,
                        flags,
                    },
                    guest_memory,
                    alloc_table,
                    pending_writebacks,
                ),
                AeroGpuCmd::SetRenderTargets {
                    color_count,
                    depth_stencil,
                    colors,
                } => self.exec_set_render_targets(color_count, depth_stencil, colors),
                AeroGpuCmd::SetVertexBuffers {
                    start_slot,
                    buffer_count,
                    bindings_bytes,
                } => self.exec_set_vertex_buffers(start_slot, buffer_count, bindings_bytes),
                AeroGpuCmd::SetIndexBuffer {
                    buffer,
                    format,
                    offset_bytes,
                } => self.exec_set_index_buffer(buffer, format, offset_bytes),
                AeroGpuCmd::SetTexture {
                    shader_stage,
                    slot,
                    texture,
                    ..
                } => self.exec_set_texture(shader_stage, slot, texture),
                AeroGpuCmd::Clear {
                    flags,
                    color_rgba_f32,
                    depth_f32,
                    stencil,
                } => self.exec_clear(flags, color_rgba_f32, depth_f32, stencil),
                AeroGpuCmd::Draw {
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                } => self.exec_draw(
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                    guest_memory,
                    alloc_table,
                ),
                AeroGpuCmd::DrawIndexed {
                    index_count,
                    instance_count,
                    first_index,
                    base_vertex,
                    first_instance,
                } => self.exec_draw_indexed(
                    DrawIndexedArgs {
                        index_count,
                        instance_count,
                        first_index,
                        base_vertex,
                        first_instance,
                    },
                    guest_memory,
                    alloc_table,
                ),
                _ => Ok(()),
            };

            match result {
                Ok(()) => packets_processed += 1,
                Err(err) => {
                    // Drop partially-recorded work, but still flush any pending `queue.write_*`
                    // uploads so they don't remain queued indefinitely and reorder with later
                    // submissions.
                    self.queue.submit([]);
                    return Err((packets_processed as usize, err, packets_processed));
                }
            }
        }

        Ok(packets_processed)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn flush_pending_writebacks_blocking(
        &self,
        pending: Vec<PendingWriteback>,
        guest_memory: &mut dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let len: usize = size_bytes.try_into().map_err(|_| {
                        ExecutorError::Validation("COPY_BUFFER: size_bytes out of range".into())
                    })?;
                    let bytes =
                        self.read_buffer_to_vec_blocking(&staging, size_bytes, "COPY_BUFFER")?;
                    if bytes.len() != len {
                        return Err(ExecutorError::Validation(
                            "COPY_BUFFER: internal writeback size mismatch".into(),
                        ));
                    }
                    guest_memory.write(dst_gpa, &bytes)?;
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let staging_size = u64::from(plan.staging_padded_bytes_per_row)
                        .checked_mul(u64::from(plan.rows))
                        .ok_or_else(|| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: staging size overflow".into(),
                            )
                        })?;
                    let mut bytes =
                        self.read_buffer_to_vec_blocking(&staging, staging_size, "COPY_TEXTURE2D")?;

                    let staging_row_bytes_usize: usize = plan
                        .staging_unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: row size out of range".into(),
                            )
                        })?;
                    let dst_row_bytes_usize: usize =
                        plan.dst_unpadded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: row size out of range".into(),
                            )
                        })?;
                    let padded_bpr_usize: usize =
                        plan.staging_padded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: bytes_per_row out of range".into(),
                            )
                        })?;

                    if plan.is_x8 {
                        for row in 0..plan.rows as usize {
                            let start = row * padded_bpr_usize;
                            let end = start + staging_row_bytes_usize;
                            force_opaque_alpha_rgba8(bytes.get_mut(start..end).ok_or_else(
                                || ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into()),
                            )?);
                        }
                    }

                    for row in 0..plan.rows as u64 {
                        let src_off = row as usize * padded_bpr_usize;
                        let src_end = src_off + staging_row_bytes_usize;
                        let staging_row_slice = bytes.get(src_off..src_end).ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into())
                        })?;
                        let dst_gpa = plan
                            .base_gpa
                            .checked_add(row.checked_mul(plan.row_pitch).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch mul)".into(),
                                )
                            })?)
                            .ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch add)".into(),
                                )
                            })?;

                        match plan.format_raw {
                            v if v == pci::AerogpuFormat::B5G6R5Unorm as u32 => {
                                let mut packed = vec![0u8; dst_row_bytes_usize];
                                pack_rgba8_to_b5g6r5_unorm(staging_row_slice, &mut packed);
                                guest_memory.write(dst_gpa, &packed)?;
                            }
                            v if v == pci::AerogpuFormat::B5G5R5A1Unorm as u32 => {
                                let mut packed = vec![0u8; dst_row_bytes_usize];
                                pack_rgba8_to_b5g5r5a1_unorm(staging_row_slice, &mut packed);
                                guest_memory.write(dst_gpa, &packed)?;
                            }
                            _ => {
                                let row_bytes_slice = staging_row_slice
                                    .get(..dst_row_bytes_usize)
                                    .ok_or_else(|| {
                                        ExecutorError::Validation(
                                            "COPY_TEXTURE2D: staging OOB".into(),
                                        )
                                    })?;
                                guest_memory.write(dst_gpa, row_bytes_slice)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn flush_pending_writebacks_async(
        &self,
        pending: Vec<PendingWriteback>,
        guest_memory: &mut dyn GuestMemory,
    ) -> Result<(), ExecutorError> {
        for writeback in pending {
            match writeback {
                PendingWriteback::Buffer {
                    staging,
                    dst_gpa,
                    size_bytes,
                } => {
                    let len: usize = size_bytes.try_into().map_err(|_| {
                        ExecutorError::Validation("COPY_BUFFER: size_bytes out of range".into())
                    })?;
                    let bytes = self
                        .read_buffer_to_vec_async(&staging, size_bytes, "COPY_BUFFER")
                        .await?;
                    if bytes.len() != len {
                        return Err(ExecutorError::Validation(
                            "COPY_BUFFER: internal writeback size mismatch".into(),
                        ));
                    }
                    guest_memory.write(dst_gpa, &bytes)?;
                }
                PendingWriteback::Texture2d { staging, plan } => {
                    let staging_size = u64::from(plan.staging_padded_bytes_per_row)
                        .checked_mul(u64::from(plan.rows))
                        .ok_or_else(|| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: staging size overflow".into(),
                            )
                        })?;
                    let mut bytes = self
                        .read_buffer_to_vec_async(&staging, staging_size, "COPY_TEXTURE2D")
                        .await?;

                    let staging_row_bytes_usize: usize = plan
                        .staging_unpadded_bytes_per_row
                        .try_into()
                        .map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: row size out of range".into(),
                            )
                        })?;
                    let dst_row_bytes_usize: usize =
                        plan.dst_unpadded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: row size out of range".into(),
                            )
                        })?;
                    let padded_bpr_usize: usize =
                        plan.staging_padded_bytes_per_row.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "COPY_TEXTURE2D: bytes_per_row out of range".into(),
                            )
                        })?;

                    if plan.is_x8 {
                        for row in 0..plan.rows as usize {
                            let start = row * padded_bpr_usize;
                            let end = start + staging_row_bytes_usize;
                            force_opaque_alpha_rgba8(bytes.get_mut(start..end).ok_or_else(
                                || ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into()),
                            )?);
                        }
                    }

                    for row in 0..plan.rows as u64 {
                        let src_off = row as usize * padded_bpr_usize;
                        let src_end = src_off + staging_row_bytes_usize;
                        let staging_row_slice = bytes.get(src_off..src_end).ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: staging OOB".into())
                        })?;
                        let dst_gpa = plan
                            .base_gpa
                            .checked_add(row.checked_mul(plan.row_pitch).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch mul)".into(),
                                )
                            })?)
                            .ok_or_else(|| {
                                ExecutorError::Validation(
                                    "COPY_TEXTURE2D: dst GPA overflow (row pitch add)".into(),
                                )
                            })?;

                        match plan.format_raw {
                            v if v == pci::AerogpuFormat::B5G6R5Unorm as u32 => {
                                let mut packed = vec![0u8; dst_row_bytes_usize];
                                pack_rgba8_to_b5g6r5_unorm(staging_row_slice, &mut packed);
                                guest_memory.write(dst_gpa, &packed)?;
                            }
                            v if v == pci::AerogpuFormat::B5G5R5A1Unorm as u32 => {
                                let mut packed = vec![0u8; dst_row_bytes_usize];
                                pack_rgba8_to_b5g5r5a1_unorm(staging_row_slice, &mut packed);
                                guest_memory.write(dst_gpa, &packed)?;
                            }
                            _ => {
                                let row_bytes_slice = staging_row_slice
                                    .get(..dst_row_bytes_usize)
                                    .ok_or_else(|| {
                                        ExecutorError::Validation(
                                            "COPY_TEXTURE2D: staging OOB".into(),
                                        )
                                    })?;
                                guest_memory.write(dst_gpa, row_bytes_slice)?;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn exec_create_buffer(
        &mut self,
        buffer_handle: u32,
        usage_flags: u32,
        size_bytes: u64,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        if buffer_handle == 0 {
            return Err(ExecutorError::Validation(
                "CREATE_BUFFER buffer_handle must be non-zero".into(),
            ));
        }
        if self.textures.contains_key(&buffer_handle) {
            return Err(ExecutorError::Validation(format!(
                "CREATE_BUFFER handle {buffer_handle} is already bound to a texture"
            )));
        }

        if size_bytes == 0 {
            return Err(ExecutorError::Validation(
                "CREATE_BUFFER size_bytes must be > 0".into(),
            ));
        }
        if !size_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT) {
            return Err(ExecutorError::Validation(format!(
                "CREATE_BUFFER size_bytes must be a multiple of {} (got {size_bytes})",
                wgpu::COPY_BUFFER_ALIGNMENT
            )));
        }

        let backing = if backing_alloc_id == 0 {
            None
        } else {
            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_BUFFER backing_alloc_id={backing_alloc_id} requires alloc_table"
                ))
            })?;
            let entry = table.get(backing_alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_BUFFER unknown backing_alloc_id={backing_alloc_id}"
                ))
            })?;

            let backing_offset = u64::from(backing_offset_bytes);
            let end = backing_offset
                .checked_add(size_bytes)
                .ok_or_else(|| ExecutorError::Validation("buffer backing range overflow".into()))?;
            if end > entry.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_BUFFER backing range out of bounds (offset=0x{backing_offset:x}, size=0x{size_bytes:x}, alloc_size=0x{:x})",
                    entry.size_bytes
                )));
            }

            Some(GuestBufferBacking {
                alloc_id: backing_alloc_id,
                alloc_offset_bytes: backing_offset,
            })
        };

        let wgpu_usage = map_buffer_usage_flags(usage_flags);

        if let Some(existing) = self.buffers.get_mut(&buffer_handle) {
            if existing.size_bytes != size_bytes || existing.usage_flags != usage_flags {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_* for existing handle {buffer_handle} has mismatched immutable properties; destroy and recreate the handle"
                )));
            }
            existing.backing = backing;
            return Ok(());
        }

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu.executor.buffer"),
            size: size_bytes,
            usage: wgpu_usage,
            mapped_at_creation: false,
        });

        self.buffers.insert(
            buffer_handle,
            BufferResource {
                buffer,
                size_bytes,
                usage_flags,
                backing,
                dirty_ranges: Vec::new(),
            },
        );
        Ok(())
    }

    fn map_format(
        &self,
        format: u32,
        width: u32,
        height: u32,
        mip_level_count: u32,
    ) -> Result<(wgpu::TextureFormat, TextureUploadTransform), ExecutorError> {
        let bc_enabled = self
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        // WebGPU validation for block-compressed (BC) textures can be stricter than D3D: some
        // backends conservatively require block alignment for any mip level that still contains at
        // least one full block. If the requested mip chain is incompatible, fall back to RGBA8 +
        // CPU decompression so `create_texture` stays robust even when BC is enabled on the
        // device.
        let bc_native_ok = bc_enabled
            && crate::wgpu_bc_texture_dimensions_compatible(width, height, mip_level_count);

        match format {
            v if v == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                || v == pci::AerogpuFormat::B8G8R8X8Unorm as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Bgra8Unorm,
                    TextureUploadTransform::Direct,
                ))
            }
            v if v == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
                || v == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Bgra8UnormSrgb,
                    TextureUploadTransform::Direct,
                ))
            }
            v if v == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                || v == pci::AerogpuFormat::R8G8B8X8Unorm as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Rgba8Unorm,
                    TextureUploadTransform::Direct,
                ))
            }
            v if v == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32
                || v == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
            {
                Ok((
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    TextureUploadTransform::Direct,
                ))
            }

            v if v == pci::AerogpuFormat::B5G6R5Unorm as u32 => Ok((
                wgpu::TextureFormat::Rgba8Unorm,
                TextureUploadTransform::B5G6R5ToRgba8,
            )),
            v if v == pci::AerogpuFormat::B5G5R5A1Unorm as u32 => Ok((
                wgpu::TextureFormat::Rgba8Unorm,
                TextureUploadTransform::B5G5R5A1ToRgba8,
            )),

            // BC formats: sample/upload directly when BC compression is enabled on the device,
            // otherwise CPU-decompress into RGBA8 (to avoid requiring BC sampling support).
            v if v == pci::AerogpuFormat::BC1RgbaUnorm as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc1RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc1ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC1RgbaUnormSrgb as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc1RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc1ToRgba8,
                    ))
                }
            }

            v if v == pci::AerogpuFormat::BC2RgbaUnorm as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc2RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc2ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC2RgbaUnormSrgb as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc2RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc2ToRgba8,
                    ))
                }
            }

            v if v == pci::AerogpuFormat::BC3RgbaUnorm as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc3RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc3ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC3RgbaUnormSrgb as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc3RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc3ToRgba8,
                    ))
                }
            }

            v if v == pci::AerogpuFormat::BC7RgbaUnorm as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc7RgbaUnorm,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8Unorm,
                        TextureUploadTransform::Bc7ToRgba8,
                    ))
                }
            }
            v if v == pci::AerogpuFormat::BC7RgbaUnormSrgb as u32 => {
                if bc_native_ok {
                    Ok((
                        wgpu::TextureFormat::Bc7RgbaUnormSrgb,
                        TextureUploadTransform::Direct,
                    ))
                } else {
                    Ok((
                        wgpu::TextureFormat::Rgba8UnormSrgb,
                        TextureUploadTransform::Bc7ToRgba8,
                    ))
                }
            }

            _ => Err(ExecutorError::Validation(format!(
                "unsupported aerogpu_format={format}"
            ))),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_create_texture2d(
        &mut self,
        args: CreateTexture2dArgs,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let CreateTexture2dArgs {
            texture_handle,
            usage_flags,
            format,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
            backing_alloc_id,
            backing_offset_bytes,
        } = args;

        if texture_handle == 0 {
            return Err(ExecutorError::Validation(
                "CREATE_TEXTURE2D texture_handle must be non-zero".into(),
            ));
        }
        if self.buffers.contains_key(&texture_handle) {
            return Err(ExecutorError::Validation(format!(
                "CREATE_TEXTURE2D handle {texture_handle} is already bound to a buffer"
            )));
        }

        if mip_levels == 0 || array_layers == 0 {
            return Err(ExecutorError::Validation(format!(
                "CREATE_TEXTURE2D mip_levels/array_layers must be non-zero (got mip_levels={mip_levels}, array_layers={array_layers})"
            )));
        }
        // Guard against invalid / pathological mip counts (WebGPU requires mip_level_count to be
        // within the possible chain length for the given dimensions).
        let max_dim = width.max(height);
        let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
        if mip_levels > max_mip_levels {
            return Err(ExecutorError::Validation(format!(
                "CREATE_TEXTURE2D mip_levels too large for dimensions (width={width}, height={height}, mip_levels={mip_levels}, max_mip_levels={max_mip_levels})"
            )));
        }
        // The executor's render pipeline always uses a single-mip view for render targets.
        if mip_levels != 1
            && (usage_flags
                & (cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET
                    | cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL))
                != 0
        {
            return Err(ExecutorError::Validation(
                "CREATE_TEXTURE2D render targets only support mip_levels=1 for now".into(),
            ));
        }

        let (wgpu_format, upload_transform) = self.map_format(format, width, height, mip_levels)?;
        let layout = texture_copy_layout(width, height, format)?;

        if row_pitch_bytes != 0 && row_pitch_bytes < layout.unpadded_bytes_per_row {
            return Err(ExecutorError::Validation(format!(
                "CREATE_TEXTURE2D row_pitch_bytes={row_pitch_bytes} smaller than minimum row size {}",
                layout.unpadded_bytes_per_row
            )));
        }

        let (subresource_layouts, total_size_bytes) = build_texture2d_subresource_layouts(
            format,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
        )?;

        let backing = if backing_alloc_id == 0 {
            None
        } else {
            if row_pitch_bytes == 0 {
                return Err(ExecutorError::Validation(
                    "CREATE_TEXTURE2D row_pitch_bytes must be non-zero when backing_alloc_id != 0"
                        .into(),
                ));
            }

            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D backing_alloc_id={backing_alloc_id} requires alloc_table"
                ))
            })?;
            let entry = table.get(backing_alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D unknown backing_alloc_id={backing_alloc_id}"
                ))
            })?;

            let backing_offset = u64::from(backing_offset_bytes);
            let required_bytes = total_size_bytes;
            let end = backing_offset.checked_add(required_bytes).ok_or_else(|| {
                ExecutorError::Validation("texture backing range overflow".into())
            })?;
            if end > entry.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_TEXTURE2D backing range out of bounds (offset=0x{backing_offset:x}, size=0x{required_bytes:x}, alloc_size=0x{:x})",
                    entry.size_bytes
                )));
            }

            Some(GuestTextureBacking {
                alloc_id: backing_alloc_id,
                alloc_offset_bytes: backing_offset,
                size_bytes: required_bytes,
            })
        };

        if let Some(existing) = self.textures.get_mut(&texture_handle) {
            if existing.usage_flags != usage_flags
                || existing.format_raw != format
                || existing.width != width
                || existing.height != height
                || existing.mip_levels != mip_levels
                || existing.array_layers != array_layers
                || existing.row_pitch_bytes != row_pitch_bytes
            {
                return Err(ExecutorError::Validation(format!(
                    "CREATE_* for existing handle {texture_handle} has mismatched immutable properties; destroy and recreate the handle"
                )));
            }

            existing.backing = backing;
            return Ok(());
        }

        let mut usage = wgpu::TextureUsages::empty();
        if (usage_flags & cmd::AEROGPU_RESOURCE_USAGE_TEXTURE) != 0 {
            usage |= wgpu::TextureUsages::TEXTURE_BINDING;
        }
        if (usage_flags
            & (cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET
                | cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL))
            != 0
        {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        // Conservative: allow queue.write_texture and readback in tests.
        usage |= wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu.executor.texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: array_layers,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage,
            view_formats: &[],
        });

        // The stable executor's draw pipeline is 2D-only, so bind a single 2D slice view even for
        // array textures. Copy/transfer operations still use the underlying `wgpu::Texture`.
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("aerogpu.executor.texture2d.view"),
            format: None,
            dimension: Some(wgpu::TextureViewDimension::D2),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: None,
            base_array_layer: 0,
            array_layer_count: Some(1),
        });

        self.textures.insert(
            texture_handle,
            TextureResource {
                texture,
                view,
                usage_flags,
                format_raw: format,
                row_pitch_bytes,
                width,
                height,
                mip_levels,
                array_layers,
                format: wgpu_format,
                upload_transform,
                subresource_layouts,
                backing,
                dirty_ranges: Vec::new(),
            },
        );
        Ok(())
    }

    fn exec_destroy_resource(&mut self, handle: u32) -> Result<(), ExecutorError> {
        self.buffers.remove(&handle);
        self.textures.remove(&handle);
        if self.state.render_target == Some(handle) {
            self.state.render_target = None;
        }
        if self.state.vertex_buffer.map(|v| v.buffer) == Some(handle) {
            self.state.vertex_buffer = None;
        }
        if self.state.index_buffer.map(|v| v.buffer) == Some(handle) {
            self.state.index_buffer = None;
        }
        if self.state.pixel_texture0 == Some(handle) {
            self.state.pixel_texture0 = None;
        }
        Ok(())
    }

    fn exec_resource_dirty_range(
        &mut self,
        handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        if size_bytes == 0 {
            return Ok(());
        }

        if let Some(buffer) = self.buffers.get_mut(&handle) {
            if let Some(backing) = buffer.backing {
                // Validate that the current submission contains an allocation-table entry
                // for this alloc_id. This catches "tracked allocation then split" bugs
                // where the command references guest memory without an alloc-table entry.
                alloc_table
                    .and_then(|t| t.get(backing.alloc_id))
                    .ok_or_else(|| {
                        ExecutorError::Validation(format!(
                            "RESOURCE_DIRTY_RANGE missing alloc table entry for alloc_id={}",
                            backing.alloc_id
                        ))
                    })?;
            } else {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE on host-owned buffer {handle} is not supported (use UPLOAD_RESOURCE)"
                )));
            }
            let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("RESOURCE_DIRTY_RANGE buffer range overflow".into())
            })?;
            if end > buffer.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE out of bounds for buffer {handle} (offset=0x{offset_bytes:x}, size=0x{size_bytes:x}, buffer_size=0x{:x})",
                    buffer.size_bytes
                )));
            }
            let aligned_start = align_down_u64(offset_bytes, wgpu::COPY_BUFFER_ALIGNMENT);
            let aligned_end = align_up_u64(end, wgpu::COPY_BUFFER_ALIGNMENT)?;
            buffer.dirty_ranges.push(aligned_start..aligned_end);
            coalesce_ranges(&mut buffer.dirty_ranges);
            return Ok(());
        }

        if let Some(tex) = self.textures.get_mut(&handle) {
            let Some(backing) = tex.backing else {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE on host-owned texture {handle} is not supported (use UPLOAD_RESOURCE)"
                )));
            };
            alloc_table
                .and_then(|t| t.get(backing.alloc_id))
                .ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "RESOURCE_DIRTY_RANGE missing alloc table entry for alloc_id={}",
                        backing.alloc_id
                    ))
                })?;
            let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("RESOURCE_DIRTY_RANGE texture range overflow".into())
            })?;
            if end > backing.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "RESOURCE_DIRTY_RANGE out of bounds for texture {handle} (offset=0x{offset_bytes:x}, size=0x{size_bytes:x}, backing_size=0x{:x})",
                    backing.size_bytes
                )));
            }
            tex.dirty_ranges.push(offset_bytes..end);
            coalesce_ranges(&mut tex.dirty_ranges);
            return Ok(());
        }

        Err(ExecutorError::Validation(format!(
            "RESOURCE_DIRTY_RANGE for unknown resource {handle}"
        )))
    }

    fn exec_upload_resource(
        &mut self,
        handle: u32,
        offset_bytes: u64,
        size_bytes: u64,
        data: &[u8],
    ) -> Result<(), ExecutorError> {
        if size_bytes == 0 {
            return Ok(());
        }

        let data_len = usize::try_from(size_bytes).map_err(|_| {
            ExecutorError::Validation("UPLOAD_RESOURCE size_bytes too large".into())
        })?;
        if data.len() != data_len {
            return Err(ExecutorError::Validation(format!(
                "UPLOAD_RESOURCE payload size mismatch (expected {data_len}, found {})",
                data.len()
            )));
        }

        if let Some(buffer) = self.buffers.get_mut(&handle) {
            if buffer.backing.is_some() {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE on guest-backed buffer {handle} is not supported (use RESOURCE_DIRTY_RANGE)"
                )));
            }

            if !offset_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
                || !size_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE buffer offset_bytes and size_bytes must be multiples of {} (handle={handle} offset_bytes={offset_bytes} size_bytes={size_bytes})",
                    wgpu::COPY_BUFFER_ALIGNMENT
                )));
            }

            let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("UPLOAD_RESOURCE buffer range overflow".into())
            })?;
            if end > buffer.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE out of bounds for buffer {handle} (offset=0x{offset_bytes:x}, size=0x{size_bytes:x}, buffer_size=0x{:x})",
                    buffer.size_bytes
                )));
            }

            self.queue.write_buffer(&buffer.buffer, offset_bytes, data);
            return Ok(());
        }

        if let Some(tex) = self.textures.get_mut(&handle) {
            if tex.backing.is_some() {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE on guest-backed texture {handle} is not supported (use RESOURCE_DIRTY_RANGE)"
                )));
            }

            let total_bytes = tex
                .subresource_layouts
                .last()
                .and_then(|last| last.offset_bytes.checked_add(last.size_bytes))
                .ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "UPLOAD_RESOURCE texture {handle} has invalid subresource layouts"
                    ))
                })?;

            let end = offset_bytes.checked_add(size_bytes).ok_or_else(|| {
                ExecutorError::Validation("UPLOAD_RESOURCE texture range overflow".into())
            })?;
            if end > total_bytes {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE out of bounds for texture {handle} (offset=0x{offset_bytes:x}, size=0x{size_bytes:x}, texture_size=0x{total_bytes:x})"
                )));
            }

            let mut bytes_consumed = 0u64;
            for sub in &tex.subresource_layouts {
                let sub_start = sub.offset_bytes;
                let sub_end = sub
                    .offset_bytes
                    .checked_add(sub.size_bytes)
                    .ok_or_else(|| {
                        ExecutorError::Validation("subresource range overflow".into())
                    })?;

                let inter_start = sub_start.max(offset_bytes);
                let inter_end = sub_end.min(end);
                if inter_start >= inter_end {
                    continue;
                }

                let chunk_offset = inter_start.checked_sub(sub_start).ok_or_else(|| {
                    ExecutorError::Validation("subresource offset underflow".into())
                })?;
                let chunk_size = inter_end.checked_sub(inter_start).ok_or_else(|| {
                    ExecutorError::Validation("subresource size underflow".into())
                })?;

                let payload_start = usize::try_from(inter_start - offset_bytes).map_err(|_| {
                    ExecutorError::Validation(
                        "UPLOAD_RESOURCE texture payload offset out of range".into(),
                    )
                })?;
                let payload_len = usize::try_from(chunk_size).map_err(|_| {
                    ExecutorError::Validation(
                        "UPLOAD_RESOURCE texture payload length out of range".into(),
                    )
                })?;
                let payload_end = payload_start.checked_add(payload_len).ok_or_else(|| {
                    ExecutorError::Validation("UPLOAD_RESOURCE payload slice overflow".into())
                })?;
                let chunk_data = data.get(payload_start..payload_end).ok_or_else(|| {
                    ExecutorError::Validation("UPLOAD_RESOURCE payload slice out of bounds".into())
                })?;

                let row_pitch = u64::from(sub.row_pitch_bytes);
                if row_pitch == 0 {
                    return Err(ExecutorError::Validation(format!(
                        "UPLOAD_RESOURCE texture {handle} has subresource with row_pitch_bytes=0 (mip_level={} array_layer={})",
                        sub.mip_level, sub.array_layer
                    )));
                }
                if !chunk_offset.is_multiple_of(row_pitch) || !chunk_size.is_multiple_of(row_pitch)
                {
                    return Err(ExecutorError::Validation(format!(
                        "UPLOAD_RESOURCE for texture {handle} must be row-aligned within the destination subresource (mip_level={} array_layer={} row_pitch_bytes={} offset_bytes=0x{chunk_offset:x} size_bytes=0x{chunk_size:x})",
                        sub.mip_level, sub.array_layer, sub.row_pitch_bytes
                    )));
                }

                let (start_row, row_count, _end_row) = upload_resource_texture_row_range(
                    handle,
                    chunk_offset,
                    chunk_size,
                    row_pitch,
                    sub.rows_in_layout,
                )?;

                let origin_y = if is_bc_format(tex.format_raw) {
                    start_row.checked_mul(4).ok_or_else(|| {
                        ExecutorError::Validation("UPLOAD_RESOURCE: origin.y overflow".into())
                    })?
                } else {
                    start_row
                };
                let remaining_height = sub.height.checked_sub(origin_y).ok_or_else(|| {
                    ExecutorError::Validation("UPLOAD_RESOURCE: origin.y out of bounds".into())
                })?;
                let max_height = if is_bc_format(tex.format_raw) {
                    row_count.checked_mul(4).ok_or_else(|| {
                        ExecutorError::Validation("UPLOAD_RESOURCE: height overflow".into())
                    })?
                } else {
                    row_count
                };
                let copy_height = remaining_height.min(max_height);

                // Direct path uploads the bytes as-is (with row repacking for 256-byte alignment).
                // BC fallback path decompresses into RGBA8 first.
                let mut owned_bytes = Vec::<u8>::new();
                let (bytes, bytes_per_row, rows_per_image, extent_height) = match tex
                    .upload_transform
                {
                    TextureUploadTransform::Direct => {
                        let layout = texture_copy_layout(sub.width, copy_height, tex.format_raw)?;
                        if sub.row_pitch_bytes < layout.unpadded_bytes_per_row {
                            return Err(ExecutorError::Validation(format!(
                                "UPLOAD_RESOURCE texture row_pitch_bytes={} smaller than minimum row size {}",
                                sub.row_pitch_bytes, layout.unpadded_bytes_per_row
                            )));
                        }

                        let upload_bpr = layout.padded_bytes_per_row;
                        let is_x8 = is_x8_format(tex.format_raw);
                        let needs_repack = upload_bpr != sub.row_pitch_bytes || is_x8;

                        if needs_repack {
                            owned_bytes.resize(upload_bpr as usize * row_count as usize, 0);
                            for row in 0..row_count as usize {
                                let src_start = row * sub.row_pitch_bytes as usize;
                                let src_end = src_start + layout.unpadded_bytes_per_row as usize;
                                let dst_start = row * upload_bpr as usize;
                                owned_bytes
                                    [dst_start..dst_start + layout.unpadded_bytes_per_row as usize]
                                    .copy_from_slice(&chunk_data[src_start..src_end]);

                                if is_x8 {
                                    force_opaque_alpha_rgba8(
                                        &mut owned_bytes[dst_start
                                            ..dst_start + layout.unpadded_bytes_per_row as usize],
                                    );
                                }
                            }
                            (
                                owned_bytes.as_slice(),
                                upload_bpr,
                                // For BC formats, `rows_per_image` is expressed in block rows.
                                // For uncompressed formats, it is expressed in texel rows.
                                row_count,
                                copy_height,
                            )
                        } else {
                            (chunk_data, upload_bpr, row_count, copy_height)
                        }
                    }
                    TextureUploadTransform::B5G6R5ToRgba8
                    | TextureUploadTransform::B5G5R5A1ToRgba8 => {
                        // Strip any per-row padding and expand into RGBA8.
                        let b5_layout =
                            texture_copy_layout(sub.width, copy_height, tex.format_raw)?;
                        if sub.row_pitch_bytes < b5_layout.unpadded_bytes_per_row {
                            return Err(ExecutorError::Validation(format!(
                                "UPLOAD_RESOURCE texture row_pitch_bytes={} smaller than minimum row size {}",
                                sub.row_pitch_bytes, b5_layout.unpadded_bytes_per_row
                            )));
                        }

                        let rgba_layout = texture_copy_layout(
                            sub.width,
                            copy_height,
                            pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                        )?;
                        let upload_bpr = rgba_layout.padded_bytes_per_row;

                        owned_bytes.resize(upload_bpr as usize * row_count as usize, 0);
                        for row in 0..row_count as usize {
                            let src_start = row * sub.row_pitch_bytes as usize;
                            let src_end = src_start + b5_layout.unpadded_bytes_per_row as usize;
                            let dst_start = row * upload_bpr as usize;
                            let dst_end = dst_start + rgba_layout.unpadded_bytes_per_row as usize;
                            match tex.upload_transform {
                                TextureUploadTransform::B5G6R5ToRgba8 => {
                                    expand_b5g6r5_unorm_to_rgba8(
                                        &chunk_data[src_start..src_end],
                                        &mut owned_bytes[dst_start..dst_end],
                                    );
                                }
                                TextureUploadTransform::B5G5R5A1ToRgba8 => {
                                    expand_b5g5r5a1_unorm_to_rgba8(
                                        &chunk_data[src_start..src_end],
                                        &mut owned_bytes[dst_start..dst_end],
                                    );
                                }
                                _ => unreachable!(),
                            }
                        }

                        (owned_bytes.as_slice(), upload_bpr, row_count, copy_height)
                    }
                    TextureUploadTransform::Bc1ToRgba8
                    | TextureUploadTransform::Bc2ToRgba8
                    | TextureUploadTransform::Bc3ToRgba8
                    | TextureUploadTransform::Bc7ToRgba8 => {
                        // Strip any per-row padding and decompress into RGBA8.
                        let bc_layout =
                            texture_copy_layout(sub.width, copy_height, tex.format_raw)?;
                        if sub.row_pitch_bytes < bc_layout.unpadded_bytes_per_row {
                            return Err(ExecutorError::Validation(format!(
                                "UPLOAD_RESOURCE texture row_pitch_bytes={} smaller than minimum row size {}",
                                sub.row_pitch_bytes, bc_layout.unpadded_bytes_per_row
                            )));
                        }

                        let mut packed_bc = vec![
                            0u8;
                            bc_layout.unpadded_bytes_per_row as usize
                                * row_count as usize
                        ];
                        for row in 0..row_count as usize {
                            let src_start = row * sub.row_pitch_bytes as usize;
                            let src_end = src_start + bc_layout.unpadded_bytes_per_row as usize;
                            let dst_start = row * bc_layout.unpadded_bytes_per_row as usize;
                            packed_bc
                                [dst_start..dst_start + bc_layout.unpadded_bytes_per_row as usize]
                                .copy_from_slice(&chunk_data[src_start..src_end]);
                        }

                        let decompressed = match tex.upload_transform {
                            TextureUploadTransform::Bc1ToRgba8 => {
                                decompress_bc1_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            TextureUploadTransform::Bc2ToRgba8 => {
                                decompress_bc2_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            TextureUploadTransform::Bc3ToRgba8 => {
                                decompress_bc3_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            TextureUploadTransform::Bc7ToRgba8 => {
                                decompress_bc7_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            _ => unreachable!(),
                        };

                        let rgba_layout = texture_copy_layout(
                            sub.width,
                            copy_height,
                            pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                        )?;
                        let upload_bpr = rgba_layout.padded_bytes_per_row;
                        if upload_bpr == rgba_layout.unpadded_bytes_per_row {
                            owned_bytes = decompressed;
                        } else {
                            owned_bytes.resize(upload_bpr as usize * copy_height as usize, 0);
                            for row in 0..copy_height as usize {
                                let src_start = row * rgba_layout.unpadded_bytes_per_row as usize;
                                let src_end =
                                    src_start + rgba_layout.unpadded_bytes_per_row as usize;
                                let dst_start = row * upload_bpr as usize;
                                owned_bytes[dst_start
                                    ..dst_start + rgba_layout.unpadded_bytes_per_row as usize]
                                    .copy_from_slice(&decompressed[src_start..src_end]);
                            }
                        }

                        (owned_bytes.as_slice(), upload_bpr, copy_height, copy_height)
                    }
                };

                let mut copy_extent_width = sub.width;
                let mut copy_extent_height = extent_height;
                if tex.upload_transform == TextureUploadTransform::Direct
                    && is_bc_format(tex.format_raw)
                {
                    copy_extent_width = align_up_u32(copy_extent_width, 4)?;
                    copy_extent_height = align_up_u32(copy_extent_height, 4)?;
                }

                self.queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture: &tex.texture,
                        mip_level: sub.mip_level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: origin_y,
                            z: sub.array_layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(rows_per_image),
                    },
                    wgpu::Extent3d {
                        width: copy_extent_width,
                        height: copy_extent_height,
                        depth_or_array_layers: 1,
                    },
                );

                bytes_consumed = bytes_consumed.checked_add(chunk_size).ok_or_else(|| {
                    ExecutorError::Validation(
                        "UPLOAD_RESOURCE texture bytes_consumed overflow".into(),
                    )
                })?;
            }

            if bytes_consumed != size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "UPLOAD_RESOURCE texture payload was not fully consumed (expected 0x{size_bytes:x}, consumed 0x{bytes_consumed:x})"
                )));
            }
            return Ok(());
        }

        Err(ExecutorError::Validation(format!(
            "UPLOAD_RESOURCE for unknown resource {handle}"
        )))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_buffer_to_vec_blocking(
        &self,
        buffer: &wgpu::Buffer,
        size_bytes: u64,
        context: &'static str,
    ) -> Result<Vec<u8>, ExecutorError> {
        let slice = buffer.slice(..size_bytes);
        let state = std::sync::Arc::new((
            std::sync::Mutex::new(None::<Result<(), wgpu::BufferAsyncError>>),
            std::sync::Condvar::new(),
        ));
        let state_clone = state.clone();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let (lock, cv) = &*state_clone;
            *lock.lock().unwrap() = Some(res);
            cv.notify_one();
        });

        self.device.poll(wgpu::Maintain::Wait);

        let (lock, cv) = &*state;
        let mut guard = lock.lock().unwrap();
        while guard.is_none() {
            guard = cv.wait(guard).unwrap();
        }
        let map_res = guard.take().unwrap();
        map_res.map_err(|err| {
            ExecutorError::Validation(format!("{context}: writeback map_async failed: {err:?}"))
        })?;

        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    async fn read_buffer_to_vec_async(
        &self,
        buffer: &wgpu::Buffer,
        size_bytes: u64,
        context: &'static str,
    ) -> Result<Vec<u8>, ExecutorError> {
        let slice = buffer.slice(..size_bytes);
        let (sender, receiver) = oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            sender.send(res).ok();
        });
        crate::wgpu_async::receive_oneshot_with_wgpu_poll(&self.device, receiver)
            .await
            .ok_or_else(|| {
                ExecutorError::Validation(format!("{context}: map_async sender dropped"))
            })?
            .map_err(|err| {
                ExecutorError::Validation(format!("{context}: map_async failed: {err:?}"))
            })?;

        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_copy_buffer(
        &mut self,
        args: CopyBufferArgs,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<(), ExecutorError> {
        let CopyBufferArgs {
            dst_buffer,
            src_buffer,
            dst_offset_bytes,
            src_offset_bytes,
            size_bytes,
            flags,
        } = args;
        if size_bytes == 0 {
            return Ok(());
        }

        let writeback = (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            return Err(ExecutorError::Validation(format!(
                "COPY_BUFFER: unsupported flags 0x{flags:08X}"
            )));
        }

        if dst_buffer == 0 || src_buffer == 0 {
            return Err(ExecutorError::Validation(
                "COPY_BUFFER: resource handles must be non-zero".into(),
            ));
        }
        if dst_buffer == src_buffer {
            return Err(ExecutorError::Validation(
                "COPY_BUFFER: src==dst is not supported".into(),
            ));
        }

        if !dst_offset_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            || !src_offset_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
            || !size_bytes.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
        {
            return Err(ExecutorError::Validation(format!(
                "COPY_BUFFER: offsets and size must be {}-byte aligned (dst_offset_bytes={dst_offset_bytes} src_offset_bytes={src_offset_bytes} size_bytes={size_bytes})",
                wgpu::COPY_BUFFER_ALIGNMENT
            )));
        }

        let (src_size, dst_size, dst_backing) = {
            let src = self.buffers.get(&src_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown src buffer {src_buffer}"))
            })?;
            let dst = self.buffers.get(&dst_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))
            })?;
            let dst_backing = if writeback {
                Some(dst.backing.ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "COPY_BUFFER: WRITEBACK_DST requires dst buffer to be guest-backed (handle={dst_buffer})"
                    ))
                })?)
            } else {
                None
            };
            (src.size_bytes, dst.size_bytes, dst_backing)
        };

        let src_end = src_offset_bytes
            .checked_add(size_bytes)
            .ok_or_else(|| ExecutorError::Validation("COPY_BUFFER: src range overflow".into()))?;
        let dst_end = dst_offset_bytes
            .checked_add(size_bytes)
            .ok_or_else(|| ExecutorError::Validation("COPY_BUFFER: dst range overflow".into()))?;
        if src_end > src_size || dst_end > dst_size {
            return Err(ExecutorError::Validation(
                "COPY_BUFFER: out of bounds".into(),
            ));
        }

        let dst_writeback_gpa = if writeback {
            let dst_backing = dst_backing.ok_or_else(|| {
                ExecutorError::Validation(
                    "COPY_BUFFER: internal error: missing dst guest backing for writeback".into(),
                )
            })?;
            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation("COPY_BUFFER: WRITEBACK_DST requires alloc_table".into())
            })?;
            let entry = table.get(dst_backing.alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_BUFFER: missing alloc table entry for alloc_id={} (dst_buffer={dst_buffer})",
                    dst_backing.alloc_id
                ))
            })?;
            if (entry.flags & ring::AEROGPU_ALLOC_FLAG_READONLY) != 0 {
                return Err(ExecutorError::Validation(format!(
                    "COPY_BUFFER: dst_buffer={dst_buffer} backing alloc_id={} is READONLY",
                    dst_backing.alloc_id
                )));
            }
            let alloc_offset = dst_backing
                .alloc_offset_bytes
                .checked_add(dst_offset_bytes)
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_BUFFER: dst alloc offset overflow".into())
                })?;
            Some(table.resolve_gpa(dst_backing.alloc_id, alloc_offset, size_bytes)?)
        } else {
            None
        };

        // Flush any pending CPU writes before the copy reads/writes the buffers.
        self.flush_buffer_if_dirty(src_buffer, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(dst_buffer, guest_memory, alloc_table)?;

        let (src, dst) = {
            let src = self.buffers.get(&src_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown src buffer {src_buffer}"))
            })?;
            let dst = self.buffers.get(&dst_buffer).ok_or_else(|| {
                ExecutorError::Validation(format!("COPY_BUFFER: unknown dst buffer {dst_buffer}"))
            })?;
            (&src.buffer, &dst.buffer)
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.copy_buffer.encoder"),
            });
        encoder.copy_buffer_to_buffer(src, src_offset_bytes, dst, dst_offset_bytes, size_bytes);
        let staging = if writeback {
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu.executor.copy_buffer.writeback"),
                size: size_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(dst, dst_offset_bytes, &staging, 0, size_bytes);
            Some(staging)
        } else {
            None
        };
        self.queue.submit([encoder.finish()]);

        if let Some(dst_gpa) = dst_writeback_gpa {
            let Some(staging) = staging else {
                return Err(ExecutorError::Validation(
                    "COPY_BUFFER: missing staging buffer for writeback".into(),
                ));
            };
            pending_writebacks.push(PendingWriteback::Buffer {
                staging,
                dst_gpa,
                size_bytes,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_copy_texture2d(
        &mut self,
        args: CopyTexture2dArgs,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
        pending_writebacks: &mut Vec<PendingWriteback>,
    ) -> Result<(), ExecutorError> {
        let CopyTexture2dArgs {
            dst_texture,
            src_texture,
            dst_mip_level,
            dst_array_layer,
            src_mip_level,
            src_array_layer,
            dst_x,
            dst_y,
            src_x,
            src_y,
            width,
            height,
            flags,
        } = args;
        if width == 0 || height == 0 {
            return Ok(());
        }

        let writeback = (flags & cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0;
        if (flags & !cmd::AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0 {
            return Err(ExecutorError::Validation(format!(
                "COPY_TEXTURE2D: unsupported flags 0x{flags:08X}"
            )));
        }

        if dst_texture == 0 || src_texture == 0 {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: resource handles must be non-zero".into(),
            ));
        }
        if dst_texture == src_texture {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: src==dst is not supported".into(),
            ));
        }
        let (
            src_extent,
            dst_extent,
            src_format_raw,
            dst_format_raw,
            src_format,
            dst_format,
            dst_upload_transform,
            dst_backing,
            dst_subresource_layout,
        ) = {
            let src = self.textures.get(&src_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown src texture {src_texture}"
                ))
            })?;
            let dst = self.textures.get(&dst_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown dst texture {dst_texture}"
                ))
            })?;
            if src_mip_level >= src.mip_levels {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: src_mip_level out of bounds (got {src_mip_level}, mip_levels={})",
                    src.mip_levels
                )));
            }
            if dst_mip_level >= dst.mip_levels {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: dst_mip_level out of bounds (got {dst_mip_level}, mip_levels={})",
                    dst.mip_levels
                )));
            }
            if src_array_layer >= src.array_layers {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: src_array_layer out of bounds (got {src_array_layer}, array_layers={})",
                    src.array_layers
                )));
            }
            if dst_array_layer >= dst.array_layers {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: dst_array_layer out of bounds (got {dst_array_layer}, array_layers={})",
                    dst.array_layers
                )));
            }

            let src_extent = (
                mip_dim(src.width, src_mip_level),
                mip_dim(src.height, src_mip_level),
            );
            let dst_extent = (
                mip_dim(dst.width, dst_mip_level),
                mip_dim(dst.height, dst_mip_level),
            );

            let dst_backing = if writeback {
                Some(dst.backing.ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "COPY_TEXTURE2D: WRITEBACK_DST requires dst texture to be guest-backed (handle={dst_texture})"
                    ))
                })?)
            } else {
                None
            };

            let dst_subresource_layout = if writeback {
                let idx = (dst_array_layer as usize)
                    .checked_mul(dst.mip_levels as usize)
                    .and_then(|v| v.checked_add(dst_mip_level as usize))
                    .ok_or_else(|| {
                        ExecutorError::Validation(
                            "COPY_TEXTURE2D: dst subresource index overflow".into(),
                        )
                    })?;
                Some(*dst.subresource_layouts.get(idx).ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: dst subresource out of range".into())
                })?)
            } else {
                None
            };

            (
                src_extent,
                dst_extent,
                src.format_raw,
                dst.format_raw,
                src.format,
                dst.format,
                dst.upload_transform,
                dst_backing,
                dst_subresource_layout,
            )
        };

        if src_format_raw != dst_format_raw {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: format mismatch".into(),
            ));
        }
        if src_format != dst_format {
            return Err(ExecutorError::Validation(format!(
                "COPY_TEXTURE2D: internal format mismatch: src={src_format:?} dst={dst_format:?}"
            )));
        }
        let dst_is_x8 = is_x8_format(dst_format_raw);

        let src_end_x = src_x
            .checked_add(width)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: src rect overflow".into()))?;
        let src_end_y = src_y
            .checked_add(height)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: src rect overflow".into()))?;
        let dst_end_x = dst_x
            .checked_add(width)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: dst rect overflow".into()))?;
        let dst_end_y = dst_y
            .checked_add(height)
            .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: dst rect overflow".into()))?;

        if src_end_x > src_extent.0 || src_end_y > src_extent.1 {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: src rect out of bounds".into(),
            ));
        }
        if dst_end_x > dst_extent.0 || dst_end_y > dst_extent.1 {
            return Err(ExecutorError::Validation(
                "COPY_TEXTURE2D: dst rect out of bounds".into(),
            ));
        }

        if is_bc_format(dst_format_raw) {
            // AeroGPU/D3D BC copy rules:
            // - origins must be multiples of 4
            // - sizes must be multiples of 4 unless the copy reaches the mip edge
            //
            // Note: even when the logical copy size reaches the edge, wgpu/WebGPU validation
            // expects a "physical" copy size rounded up to whole blocks; we pad before encoding
            // the wgpu copy (see below).
            if !src_x.is_multiple_of(4)
                || !src_y.is_multiple_of(4)
                || !dst_x.is_multiple_of(4)
                || !dst_y.is_multiple_of(4)
            {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: BC origin must be 4x4 block-aligned (src=({src_x},{src_y}) dst=({dst_x},{dst_y}))"
                )));
            }

            if !width.is_multiple_of(4) && (src_end_x != src_extent.0 || dst_end_x != dst_extent.0)
            {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: BC width must be a multiple of 4 unless the copy reaches the mip edge of both textures (src_end_x={src_end_x} src_width={} dst_end_x={dst_end_x} dst_width={})",
                    src_extent.0, dst_extent.0
                )));
            }
            if !height.is_multiple_of(4) && (src_end_y != src_extent.1 || dst_end_y != dst_extent.1)
            {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: BC height must be a multiple of 4 unless the copy reaches the mip edge of both textures (src_end_y={src_end_y} src_height={} dst_end_y={dst_end_y} dst_height={})",
                    src_extent.1, dst_extent.1
                )));
            }
        }

        let writeback_plan = if writeback {
            let dst_sub = dst_subresource_layout.ok_or_else(|| {
                ExecutorError::Validation(
                    "COPY_TEXTURE2D: internal error: missing dst subresource layout".into(),
                )
            })?;
            let dst_backing = dst_backing.ok_or_else(|| {
                ExecutorError::Validation(
                    "COPY_TEXTURE2D: internal error: missing dst guest backing for writeback"
                        .into(),
                )
            })?;
            let table = alloc_table.ok_or_else(|| {
                ExecutorError::Validation(
                    "COPY_TEXTURE2D: WRITEBACK_DST requires alloc_table".into(),
                )
            })?;
            let entry = table.get(dst_backing.alloc_id).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: missing alloc table entry for alloc_id={} (dst_texture={dst_texture})",
                    dst_backing.alloc_id
                ))
            })?;
            if (entry.flags & ring::AEROGPU_ALLOC_FLAG_READONLY) != 0 {
                return Err(ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: dst_texture={dst_texture} backing alloc_id={} is READONLY",
                    dst_backing.alloc_id
                )));
            }

            if is_bc_format(dst_format_raw)
                && dst_upload_transform != TextureUploadTransform::Direct
            {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: WRITEBACK_DST for BC textures requires TEXTURE_COMPRESSION_BC"
                        .into(),
                ));
            }

            let region_layout = texture_copy_layout(width, height, dst_format_raw)?;
            let staging_layout = match dst_upload_transform {
                TextureUploadTransform::Direct => region_layout,
                TextureUploadTransform::B5G6R5ToRgba8 | TextureUploadTransform::B5G5R5A1ToRgba8 => {
                    texture_copy_layout(width, height, pci::AerogpuFormat::R8G8B8A8Unorm as u32)?
                }
                TextureUploadTransform::Bc1ToRgba8
                | TextureUploadTransform::Bc2ToRgba8
                | TextureUploadTransform::Bc3ToRgba8
                | TextureUploadTransform::Bc7ToRgba8 => {
                    return Err(ExecutorError::Validation(
                        "COPY_TEXTURE2D: WRITEBACK_DST requires a direct texture format".into(),
                    ));
                }
            };

            let dst_x_blocks = dst_x.checked_div(region_layout.block_w).ok_or_else(|| {
                ExecutorError::Validation("COPY_TEXTURE2D: dst_x div overflow".into())
            })?;
            let dst_y_blocks = dst_y.checked_div(region_layout.block_h).ok_or_else(|| {
                ExecutorError::Validation("COPY_TEXTURE2D: dst_y div overflow".into())
            })?;
            let dst_x_bytes = u64::from(dst_x_blocks)
                .checked_mul(u64::from(region_layout.block_bytes))
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: dst_x overflow".into())
                })?;

            let row_pitch = u64::from(dst_sub.row_pitch_bytes);
            if row_pitch == 0 {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: missing dst row_pitch_bytes for writeback".into(),
                ));
            }

            let start_offset =
                dst_backing
                    .alloc_offset_bytes
                    .checked_add(dst_sub.offset_bytes)
                    .ok_or_else(|| {
                        ExecutorError::Validation(
                            "COPY_TEXTURE2D: subresource offset overflow".into(),
                        )
                    })?
                    .checked_add(u64::from(dst_y_blocks).checked_mul(row_pitch).ok_or_else(
                        || ExecutorError::Validation("COPY_TEXTURE2D: dst_y overflow".into()),
                    )?)
                    .and_then(|v| v.checked_add(dst_x_bytes))
                    .ok_or_else(|| {
                        ExecutorError::Validation("COPY_TEXTURE2D: backing overflow".into())
                    })?;

            let row_end = dst_x_bytes
                .checked_add(u64::from(region_layout.unpadded_bytes_per_row))
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: row end overflow".into())
                })?;
            if row_end > row_pitch {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: writeback row range exceeds dst row_pitch_bytes".into(),
                ));
            }

            let last_row_start = start_offset
                .checked_add(
                    u64::from(region_layout.rows_in_layout)
                        .checked_sub(1)
                        .ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: height underflow".into())
                        })?
                        .checked_mul(row_pitch)
                        .ok_or_else(|| {
                            ExecutorError::Validation("COPY_TEXTURE2D: row offset overflow".into())
                        })?,
                )
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: row offset overflow".into())
                })?;
            let end_offset = last_row_start
                .checked_add(u64::from(region_layout.unpadded_bytes_per_row))
                .ok_or_else(|| ExecutorError::Validation("COPY_TEXTURE2D: end overflow".into()))?;
            let validate_size = end_offset.checked_sub(start_offset).ok_or_else(|| {
                ExecutorError::Validation("COPY_TEXTURE2D: size underflow".into())
            })?;
            let backing_end = dst_backing
                .alloc_offset_bytes
                .checked_add(dst_backing.size_bytes)
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: backing overflow".into())
                })?;
            if end_offset > backing_end {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: writeback out of bounds".into(),
                ));
            }

            let base_gpa = table.resolve_gpa(dst_backing.alloc_id, start_offset, validate_size)?;

            Some(TextureWritebackPlan {
                base_gpa,
                row_pitch,
                rows: region_layout.rows_in_layout,
                format_raw: dst_format_raw,
                is_x8: dst_is_x8,
                staging_padded_bytes_per_row: staging_layout.padded_bytes_per_row,
                staging_unpadded_bytes_per_row: staging_layout.unpadded_bytes_per_row,
                dst_unpadded_bytes_per_row: region_layout.unpadded_bytes_per_row,
            })
        } else {
            None
        };

        // Flush any pending CPU writes before the copy reads/writes the textures.
        self.flush_texture_if_dirty(src_texture, guest_memory, alloc_table)?;
        self.flush_texture_if_dirty(dst_texture, guest_memory, alloc_table)?;

        let (src, dst) = {
            let src = self.textures.get(&src_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown src texture {src_texture}"
                ))
            })?;
            let dst = self.textures.get(&dst_texture).ok_or_else(|| {
                ExecutorError::Validation(format!(
                    "COPY_TEXTURE2D: unknown dst texture {dst_texture}"
                ))
            })?;
            (&src.texture, &dst.texture)
        };

        let (wgpu_copy_width, wgpu_copy_height) = if is_bc_format(dst_format_raw)
            && dst_upload_transform == TextureUploadTransform::Direct
        {
            (align_up_u32(width, 4)?, align_up_u32(height, 4)?)
        } else {
            (width, height)
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.copy_texture2d.encoder"),
            });
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: src,
                mip_level: src_mip_level,
                origin: wgpu::Origin3d {
                    x: src_x,
                    y: src_y,
                    z: src_array_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: dst,
                mip_level: dst_mip_level,
                origin: wgpu::Origin3d {
                    x: dst_x,
                    y: dst_y,
                    z: dst_array_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: wgpu_copy_width,
                height: wgpu_copy_height,
                depth_or_array_layers: 1,
            },
        );
        let staging = if let Some(plan) = writeback_plan {
            let size_bytes = u64::from(plan.staging_padded_bytes_per_row)
                .checked_mul(u64::from(plan.rows))
                .ok_or_else(|| {
                    ExecutorError::Validation("COPY_TEXTURE2D: staging size overflow".into())
                })?;
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu.executor.copy_texture2d.writeback"),
                size: size_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: dst,
                    mip_level: dst_mip_level,
                    origin: wgpu::Origin3d {
                        x: dst_x,
                        y: dst_y,
                        z: dst_array_layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &staging,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(plan.staging_padded_bytes_per_row),
                        rows_per_image: Some(plan.rows),
                    },
                },
                wgpu::Extent3d {
                    width: wgpu_copy_width,
                    height: wgpu_copy_height,
                    depth_or_array_layers: 1,
                },
            );
            Some(staging)
        } else {
            None
        };
        self.queue.submit([encoder.finish()]);

        if let Some(plan) = writeback_plan {
            let Some(staging) = staging else {
                return Err(ExecutorError::Validation(
                    "COPY_TEXTURE2D: missing staging buffer for writeback".into(),
                ));
            };
            pending_writebacks.push(PendingWriteback::Texture2d { staging, plan });
        }
        Ok(())
    }

    fn exec_set_render_targets(
        &mut self,
        color_count: u32,
        _depth_stencil: u32,
        colors: [u32; cmd::AEROGPU_MAX_RENDER_TARGETS],
    ) -> Result<(), ExecutorError> {
        if color_count > 1 {
            return Err(ExecutorError::Validation(
                "only color_count<=1 is supported".into(),
            ));
        }
        let color0 = colors[0];
        if color_count == 0 || color0 == 0 {
            self.state.render_target = None;
            return Ok(());
        }
        let tex = self.textures.get(&color0).ok_or_else(|| {
            ExecutorError::Validation(format!("SET_RENDER_TARGETS unknown texture {color0}"))
        })?;
        let is_x8 = is_x8_format(tex.format_raw);
        if !self.pipelines.contains_key(&(tex.format, is_x8)) {
            return Err(ExecutorError::Validation(format!(
                "render target format {:?} not supported by executor",
                tex.format
            )));
        }
        self.state.render_target = Some(color0);
        Ok(())
    }

    fn exec_set_vertex_buffers(
        &mut self,
        start_slot: u32,
        buffer_count: u32,
        bindings_bytes: &[u8],
    ) -> Result<(), ExecutorError> {
        if start_slot != 0 {
            return Err(ExecutorError::Validation(
                "only start_slot=0 is supported".into(),
            ));
        }
        if buffer_count == 0 {
            self.state.vertex_buffer = None;
            return Ok(());
        }

        let expected_size = (buffer_count as usize)
            .checked_mul(cmd::AerogpuVertexBufferBinding::SIZE_BYTES)
            .ok_or_else(|| {
                ExecutorError::Validation("vertex buffer binding size overflow".into())
            })?;
        if bindings_bytes.len() < expected_size {
            return Err(ExecutorError::TruncatedPacket);
        }

        // Only track slot 0 for now.
        let buffer = read_u32_le(bindings_bytes, 0)?;
        let stride_bytes = read_u32_le(bindings_bytes, 4)?;
        let offset_bytes = read_u32_le(bindings_bytes, 8)?;

        if buffer == 0 {
            self.state.vertex_buffer = None;
            return Ok(());
        }
        if !self.buffers.contains_key(&buffer) {
            return Err(ExecutorError::Validation(format!(
                "SET_VERTEX_BUFFERS unknown buffer {buffer}"
            )));
        }

        self.state.vertex_buffer = Some(BoundVertexBuffer {
            buffer,
            stride_bytes,
            offset_bytes,
        });
        Ok(())
    }

    fn exec_set_index_buffer(
        &mut self,
        buffer: u32,
        format_raw: u32,
        offset_bytes: u32,
    ) -> Result<(), ExecutorError> {
        if buffer == 0 {
            self.state.index_buffer = None;
            return Ok(());
        }

        if !self.buffers.contains_key(&buffer) {
            return Err(ExecutorError::Validation(format!(
                "SET_INDEX_BUFFER unknown buffer {buffer}"
            )));
        }

        let format = match format_raw {
            v if v == cmd::AerogpuIndexFormat::Uint16 as u32 => wgpu::IndexFormat::Uint16,
            v if v == cmd::AerogpuIndexFormat::Uint32 as u32 => wgpu::IndexFormat::Uint32,
            _ => {
                return Err(ExecutorError::Validation(format!(
                    "SET_INDEX_BUFFER unknown index format {format_raw}"
                )))
            }
        };

        let align = match format {
            wgpu::IndexFormat::Uint16 => 2,
            wgpu::IndexFormat::Uint32 => 4,
        };
        if !(offset_bytes as u64).is_multiple_of(align) {
            return Err(ExecutorError::Validation(format!(
                "SET_INDEX_BUFFER offset_bytes must be aligned to {align} (got {offset_bytes})"
            )));
        }

        let buf_size = self.buffers.get(&buffer).unwrap().size_bytes;
        if offset_bytes as u64 > buf_size {
            return Err(ExecutorError::Validation(format!(
                "SET_INDEX_BUFFER offset_bytes {offset_bytes} out of bounds for buffer {buffer} (size={buf_size})"
            )));
        }

        self.state.index_buffer = Some(BoundIndexBuffer {
            buffer,
            format,
            offset_bytes,
        });
        Ok(())
    }

    fn exec_set_texture(
        &mut self,
        _shader_stage: u32,
        slot: u32,
        texture: u32,
    ) -> Result<(), ExecutorError> {
        if slot != 0 {
            return Err(ExecutorError::Validation(
                "only texture slot 0 is supported".into(),
            ));
        }
        if texture == 0 {
            self.state.pixel_texture0 = None;
            return Ok(());
        }
        if !self.textures.contains_key(&texture) {
            return Err(ExecutorError::Validation(format!(
                "SET_TEXTURE unknown texture {texture}"
            )));
        }
        self.state.pixel_texture0 = Some(texture);
        Ok(())
    }

    fn exec_clear(
        &mut self,
        flags: u32,
        color_rgba_f32: [u32; 4],
        _depth_f32: u32,
        _stencil: u32,
    ) -> Result<(), ExecutorError> {
        if flags & cmd::AEROGPU_CLEAR_COLOR == 0 {
            return Ok(());
        }

        let Some(rt) = self.state.render_target else {
            return Err(ExecutorError::Validation(
                "CLEAR requires a bound render target".into(),
            ));
        };
        let rt_tex = self.textures.get(&rt).ok_or_else(|| {
            ExecutorError::Validation(format!("CLEAR render target {rt} missing"))
        })?;

        let r = f32::from_bits(color_rgba_f32[0]);
        let g = f32::from_bits(color_rgba_f32[1]);
        let b = f32::from_bits(color_rgba_f32[2]);
        let mut a = f32::from_bits(color_rgba_f32[3]);
        if is_x8_format(rt_tex.format_raw) {
            a = 1.0;
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.clear.encoder"),
            });

        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu.executor.clear.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_tex.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r as f64,
                            g: g as f64,
                            b: b as f64,
                            a: a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
        }

        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn exec_draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let Some(rt) = self.state.render_target else {
            return Err(ExecutorError::Validation(
                "DRAW requires a bound render target".into(),
            ));
        };
        let Some(vb) = self.state.vertex_buffer else {
            return Err(ExecutorError::Validation(
                "DRAW requires a bound vertex buffer".into(),
            ));
        };
        let Some(tex0) = self.state.pixel_texture0 else {
            return Err(ExecutorError::Validation(
                "DRAW requires a bound pixel texture slot 0".into(),
            ));
        };

        // Upload pending dirty ranges for any guest-backed resources used by this draw.
        self.flush_texture_if_dirty(rt, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(vb.buffer, guest_memory, alloc_table)?;
        self.flush_texture_if_dirty(tex0, guest_memory, alloc_table)?;

        let rt_tex = self
            .textures
            .get(&rt)
            .ok_or_else(|| ExecutorError::Validation(format!("DRAW render target {rt} missing")))?;
        let vb_res = self.buffers.get(&vb.buffer).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW vertex buffer {} missing", vb.buffer))
        })?;
        let tex0_res = self
            .textures
            .get(&tex0)
            .ok_or_else(|| ExecutorError::Validation(format!("DRAW texture {tex0} missing")))?;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu.executor.bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex0_res.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.draw.encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu.executor.draw.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_tex.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            let rt_is_x8 = is_x8_format(rt_tex.format_raw);
            let pipeline = self
                .pipelines
                .get(&(rt_tex.format, rt_is_x8))
                .ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "no pipeline configured for render target format {:?}",
                        rt_tex.format
                    ))
                })?;
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(
                0,
                vb_res
                    .buffer
                    .slice(vb.offset_bytes as u64..vb_res.size_bytes),
            );
            // The stride is baked into the pipeline; validate to avoid confusing failures.
            if vb.stride_bytes != 8 {
                return Err(ExecutorError::Validation(format!(
                    "vertex buffer stride_bytes must be 8 for the built-in pipeline (got {})",
                    vb.stride_bytes
                )));
            }
            pass.draw(
                first_vertex..first_vertex.saturating_add(vertex_count),
                first_instance..first_instance.saturating_add(instance_count),
            );
        }

        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_draw_indexed(
        &mut self,
        args: DrawIndexedArgs,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let DrawIndexedArgs {
            index_count,
            instance_count,
            first_index,
            base_vertex,
            first_instance,
        } = args;
        let Some(rt) = self.state.render_target else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound render target".into(),
            ));
        };
        let Some(vb) = self.state.vertex_buffer else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound vertex buffer".into(),
            ));
        };
        let Some(ib) = self.state.index_buffer else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound index buffer".into(),
            ));
        };
        let Some(tex0) = self.state.pixel_texture0 else {
            return Err(ExecutorError::Validation(
                "DRAW_INDEXED requires a bound pixel texture slot 0".into(),
            ));
        };

        self.flush_texture_if_dirty(rt, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(vb.buffer, guest_memory, alloc_table)?;
        self.flush_buffer_if_dirty(ib.buffer, guest_memory, alloc_table)?;
        self.flush_texture_if_dirty(tex0, guest_memory, alloc_table)?;

        let rt_tex = self.textures.get(&rt).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED render target {rt} missing"))
        })?;
        let vb_res = self.buffers.get(&vb.buffer).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED vertex buffer {} missing", vb.buffer))
        })?;
        let ib_res = self.buffers.get(&ib.buffer).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED index buffer {} missing", ib.buffer))
        })?;

        let tex0_res = self.textures.get(&tex0).ok_or_else(|| {
            ExecutorError::Validation(format!("DRAW_INDEXED texture {tex0} missing"))
        })?;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aerogpu.executor.bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tex0_res.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aerogpu.executor.draw_indexed.encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aerogpu.executor.draw_indexed.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt_tex.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            let rt_is_x8 = is_x8_format(rt_tex.format_raw);
            let pipeline = self
                .pipelines
                .get(&(rt_tex.format, rt_is_x8))
                .ok_or_else(|| {
                    ExecutorError::Validation(format!(
                        "no pipeline configured for render target format {:?}",
                        rt_tex.format
                    ))
                })?;
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(
                0,
                vb_res
                    .buffer
                    .slice(vb.offset_bytes as u64..vb_res.size_bytes),
            );
            if vb.stride_bytes != 8 {
                return Err(ExecutorError::Validation(format!(
                    "vertex buffer stride_bytes must be 8 for the built-in pipeline (got {})",
                    vb.stride_bytes
                )));
            }

            if ib.offset_bytes as u64 > ib_res.size_bytes {
                return Err(ExecutorError::Validation(format!(
                    "index buffer offset out of bounds (offset={}, size={})",
                    ib.offset_bytes, ib_res.size_bytes
                )));
            }
            pass.set_index_buffer(
                ib_res
                    .buffer
                    .slice(ib.offset_bytes as u64..ib_res.size_bytes),
                ib.format,
            );

            pass.draw_indexed(
                first_index..first_index.saturating_add(index_count),
                base_vertex,
                first_instance..first_instance.saturating_add(instance_count),
            );
        }

        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn flush_buffer_if_dirty(
        &mut self,
        handle: u32,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let Some(buffer) = self.buffers.get_mut(&handle) else {
            return Err(ExecutorError::Validation(format!(
                "unknown buffer {handle}"
            )));
        };
        let Some(backing) = buffer.backing else {
            // Host-owned buffers are updated through UPLOAD_RESOURCE.
            return Ok(());
        };
        if buffer.dirty_ranges.is_empty() {
            return Ok(());
        }

        let table = alloc_table.ok_or_else(|| {
            ExecutorError::Validation(format!(
                "dirty guest-backed buffer {handle} requires alloc_table"
            ))
        })?;

        for range in &buffer.dirty_ranges {
            let aligned_start = align_down_u64(range.start, wgpu::COPY_BUFFER_ALIGNMENT);
            let aligned_end =
                align_up_u64(range.end, wgpu::COPY_BUFFER_ALIGNMENT)?.min(buffer.size_bytes);
            let len = aligned_end
                .checked_sub(aligned_start)
                .ok_or_else(|| ExecutorError::Validation("invalid dirty range".into()))?;
            let len_usize = usize::try_from(len)
                .map_err(|_| ExecutorError::Validation("buffer dirty range too large".into()))?;
            let mut data = vec![0u8; len_usize];

            let alloc_offset = backing
                .alloc_offset_bytes
                .checked_add(aligned_start)
                .ok_or_else(|| ExecutorError::Validation("buffer alloc offset overflow".into()))?;
            let src_gpa = table.resolve_gpa(backing.alloc_id, alloc_offset, len)?;
            guest_memory.read(src_gpa, &mut data)?;
            self.queue
                .write_buffer(&buffer.buffer, aligned_start, &data);
        }

        buffer.dirty_ranges.clear();
        Ok(())
    }

    fn flush_texture_if_dirty(
        &mut self,
        handle: u32,
        guest_memory: &mut dyn GuestMemory,
        alloc_table: Option<&AllocTable>,
    ) -> Result<(), ExecutorError> {
        let Some(tex) = self.textures.get_mut(&handle) else {
            return Err(ExecutorError::Validation(format!(
                "unknown texture {handle}"
            )));
        };
        let Some(backing) = tex.backing else {
            // Host-owned textures are updated through UPLOAD_RESOURCE.
            return Ok(());
        };
        if tex.dirty_ranges.is_empty() {
            return Ok(());
        }

        let table = alloc_table.ok_or_else(|| {
            ExecutorError::Validation(format!(
                "dirty guest-backed texture {handle} requires alloc_table"
            ))
        })?;

        let is_x8 = is_x8_format(tex.format_raw);
        let is_bc = is_bc_format(tex.format_raw);

        // Determine which rows of each subresource intersect the dirty byte ranges and upload only
        // those rows.
        //
        // This avoids clobbering GPU-side writes (e.g. CLEAR/DRAW/COPY) in untouched rows when the
        // guest later performs a partial CPU update and emits `RESOURCE_DIRTY_RANGE`.
        let mut dirty_idx = 0usize;
        let dirty_ranges = &tex.dirty_ranges;
        for sub in &tex.subresource_layouts {
            let sub_start = sub.offset_bytes;
            let sub_end = sub
                .offset_bytes
                .checked_add(sub.size_bytes)
                .ok_or_else(|| ExecutorError::Validation("subresource range overflow".into()))?;

            while dirty_idx < dirty_ranges.len() && dirty_ranges[dirty_idx].end <= sub_start {
                dirty_idx += 1;
            }
            if dirty_idx >= dirty_ranges.len() {
                break;
            }
            if dirty_ranges[dirty_idx].start >= sub_end {
                continue;
            }

            let row_pitch = u64::from(sub.row_pitch_bytes);
            if row_pitch == 0 {
                return Err(ExecutorError::Validation(format!(
                    "dirty guest-backed texture {handle} has subresource with row_pitch_bytes=0 (mip_level={} array_layer={})",
                    sub.mip_level, sub.array_layer
                )));
            }

            let mut dirty_row_ranges = Vec::<Range<u64>>::new();
            for r in dirty_ranges.iter().skip(dirty_idx) {
                if r.start >= sub_end {
                    break;
                }
                if r.end <= sub_start {
                    continue;
                }
                let inter_start = r.start.max(sub_start);
                let inter_end = r.end.min(sub_end);
                if inter_start >= inter_end {
                    continue;
                }

                let rel_start = inter_start
                    .checked_sub(sub_start)
                    .ok_or_else(|| ExecutorError::Validation("dirty range underflow".into()))?;
                let rel_end = inter_end
                    .checked_sub(sub_start)
                    .ok_or_else(|| ExecutorError::Validation("dirty range underflow".into()))?;

                let start_row = rel_start / row_pitch;
                let end_row = rel_end
                    .checked_add(row_pitch - 1)
                    .ok_or_else(|| ExecutorError::Validation("dirty row range overflow".into()))?
                    / row_pitch;
                dirty_row_ranges.push(start_row..end_row);
            }
            coalesce_ranges(&mut dirty_row_ranges);

            for row_range in dirty_row_ranges {
                let start_row_u32: u32 = row_range.start.try_into().map_err(|_| {
                    ExecutorError::Validation("RESOURCE_DIRTY_RANGE: start_row out of range".into())
                })?;
                let end_row_u32: u32 = row_range.end.try_into().map_err(|_| {
                    ExecutorError::Validation("RESOURCE_DIRTY_RANGE: end_row out of range".into())
                })?;
                if start_row_u32 >= end_row_u32 {
                    continue;
                }

                if end_row_u32 > sub.rows_in_layout {
                    return Err(ExecutorError::Validation(format!(
                        "RESOURCE_DIRTY_RANGE rows out of bounds for texture {handle} (mip_level={} array_layer={} rows {}..{} rows_in_layout={})",
                        sub.mip_level, sub.array_layer, start_row_u32, end_row_u32, sub.rows_in_layout
                    )));
                }

                let row_count = end_row_u32 - start_row_u32;
                let (origin_y, copy_height) = if is_bc {
                    let origin_y = start_row_u32.checked_mul(4).ok_or_else(|| {
                        ExecutorError::Validation("RESOURCE_DIRTY_RANGE: origin_y overflow".into())
                    })?;
                    let remaining_height = sub.height.checked_sub(origin_y).ok_or_else(|| {
                        ExecutorError::Validation(
                            "RESOURCE_DIRTY_RANGE: origin_y out of bounds".into(),
                        )
                    })?;
                    let max_height = row_count.checked_mul(4).ok_or_else(|| {
                        ExecutorError::Validation("RESOURCE_DIRTY_RANGE: height overflow".into())
                    })?;
                    (origin_y, remaining_height.min(max_height))
                } else {
                    let origin_y = start_row_u32;
                    let remaining_height = sub.height.checked_sub(origin_y).ok_or_else(|| {
                        ExecutorError::Validation(
                            "RESOURCE_DIRTY_RANGE: origin_y out of bounds".into(),
                        )
                    })?;
                    (origin_y, remaining_height.min(row_count))
                };

                match tex.upload_transform {
                    TextureUploadTransform::Direct => {
                        let layout = texture_copy_layout(sub.width, copy_height, tex.format_raw)?;
                        if layout.rows_in_layout != row_count {
                            return Err(ExecutorError::Validation(format!(
                                "RESOURCE_DIRTY_RANGE: row_count mismatch (expected {}, found {})",
                                row_count, layout.rows_in_layout
                            )));
                        }

                        let upload_bpr = layout.padded_bytes_per_row;
                        let rows = layout.rows_in_layout;
                        let row_bytes = layout.unpadded_bytes_per_row as u64;

                        let staging_size = (upload_bpr as u64)
                            .checked_mul(u64::from(rows))
                            .ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: staging size overflow".into(),
                                )
                            })?;
                        let staging_size_usize: usize = staging_size.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "RESOURCE_DIRTY_RANGE: staging size out of range".into(),
                            )
                        })?;
                        let mut staging = vec![0u8; staging_size_usize];

                        let upload_bpr_usize: usize = upload_bpr.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "RESOURCE_DIRTY_RANGE: bytes_per_row out of range".into(),
                            )
                        })?;
                        let row_bytes_usize: usize =
                            layout.unpadded_bytes_per_row.try_into().map_err(|_| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: row size out of range".into(),
                                )
                            })?;

                        for row in 0..rows {
                            let row_idx = start_row_u32.checked_add(row).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: row index overflow".into(),
                                )
                            })?;
                            let row_off =
                                u64::from(row_idx).checked_mul(row_pitch).ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: row offset overflow".into(),
                                    )
                                })?;
                            let alloc_offset = backing
                                .alloc_offset_bytes
                                .checked_add(sub.offset_bytes)
                                .and_then(|v| v.checked_add(row_off))
                                .ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: alloc offset overflow".into(),
                                    )
                                })?;
                            let src_gpa =
                                table.resolve_gpa(backing.alloc_id, alloc_offset, row_bytes)?;

                            let dst_off = row as usize * upload_bpr_usize;
                            let dst_end = dst_off + row_bytes_usize;
                            guest_memory.read(
                                src_gpa,
                                staging.get_mut(dst_off..dst_end).ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: staging OOB".into(),
                                    )
                                })?,
                            )?;
                            if is_x8 {
                                force_opaque_alpha_rgba8(
                                    staging.get_mut(dst_off..dst_end).ok_or_else(|| {
                                        ExecutorError::Validation(
                                            "RESOURCE_DIRTY_RANGE: staging OOB".into(),
                                        )
                                    })?,
                                );
                            }
                        }

                        let (copy_extent_width, copy_extent_height) = if is_bc {
                            (align_up_u32(sub.width, 4)?, align_up_u32(copy_height, 4)?)
                        } else {
                            (sub.width, copy_height)
                        };

                        self.queue.write_texture(
                            wgpu::ImageCopyTexture {
                                texture: &tex.texture,
                                mip_level: sub.mip_level,
                                origin: wgpu::Origin3d {
                                    x: 0,
                                    y: origin_y,
                                    z: sub.array_layer,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            &staging,
                            wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(upload_bpr),
                                rows_per_image: Some(rows),
                            },
                            wgpu::Extent3d {
                                width: copy_extent_width,
                                height: copy_extent_height,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                    TextureUploadTransform::B5G6R5ToRgba8
                    | TextureUploadTransform::B5G5R5A1ToRgba8 => {
                        // Packed 16-bit formats are stored as RGBA8 on the host.
                        let b5_layout =
                            texture_copy_layout(sub.width, copy_height, tex.format_raw)?;
                        if u64::from(sub.row_pitch_bytes)
                            < u64::from(b5_layout.unpadded_bytes_per_row)
                        {
                            return Err(ExecutorError::Validation(format!(
                                "subresource row_pitch_bytes={} smaller than minimum row size {}",
                                sub.row_pitch_bytes, b5_layout.unpadded_bytes_per_row
                            )));
                        }
                        if b5_layout.rows_in_layout != row_count {
                            return Err(ExecutorError::Validation(format!(
                                "RESOURCE_DIRTY_RANGE: row_count mismatch (expected {}, found {})",
                                row_count, b5_layout.rows_in_layout
                            )));
                        }

                        let rgba_layout = texture_copy_layout(
                            sub.width,
                            copy_height,
                            pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                        )?;
                        let upload_bpr = rgba_layout.padded_bytes_per_row;
                        let rows = rgba_layout.rows_in_layout;

                        let staging_size = (upload_bpr as u64)
                            .checked_mul(u64::from(rows))
                            .ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: staging size overflow".into(),
                                )
                            })?;
                        let staging_size_usize: usize = staging_size.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "RESOURCE_DIRTY_RANGE: staging size out of range".into(),
                            )
                        })?;
                        let mut staging = vec![0u8; staging_size_usize];

                        let upload_bpr_usize: usize = upload_bpr.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "RESOURCE_DIRTY_RANGE: bytes_per_row out of range".into(),
                            )
                        })?;
                        let dst_row_bytes_usize: usize =
                            rgba_layout.unpadded_bytes_per_row.try_into().map_err(|_| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: row size out of range".into(),
                                )
                            })?;
                        let src_row_bytes = u64::from(b5_layout.unpadded_bytes_per_row);
                        let src_row_bytes_usize: usize =
                            b5_layout.unpadded_bytes_per_row.try_into().map_err(|_| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: row size out of range".into(),
                                )
                            })?;
                        let mut row_buf = vec![0u8; src_row_bytes_usize];

                        for row in 0..rows {
                            let row_idx = start_row_u32.checked_add(row).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: row index overflow".into(),
                                )
                            })?;
                            let row_off =
                                u64::from(row_idx).checked_mul(row_pitch).ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: row offset overflow".into(),
                                    )
                                })?;
                            let alloc_offset = backing
                                .alloc_offset_bytes
                                .checked_add(sub.offset_bytes)
                                .and_then(|v| v.checked_add(row_off))
                                .ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: alloc offset overflow".into(),
                                    )
                                })?;
                            let src_gpa =
                                table.resolve_gpa(backing.alloc_id, alloc_offset, src_row_bytes)?;
                            guest_memory.read(src_gpa, &mut row_buf)?;

                            let dst_off = row as usize * upload_bpr_usize;
                            let dst_end = dst_off + dst_row_bytes_usize;
                            let dst_slice = staging.get_mut(dst_off..dst_end).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: staging OOB".into(),
                                )
                            })?;

                            match tex.upload_transform {
                                TextureUploadTransform::B5G6R5ToRgba8 => {
                                    expand_b5g6r5_unorm_to_rgba8(
                                        row_buf.get(..src_row_bytes_usize).ok_or_else(|| {
                                            ExecutorError::Validation(
                                                "RESOURCE_DIRTY_RANGE: row_buf OOB".into(),
                                            )
                                        })?,
                                        dst_slice,
                                    );
                                }
                                TextureUploadTransform::B5G5R5A1ToRgba8 => {
                                    expand_b5g5r5a1_unorm_to_rgba8(
                                        row_buf.get(..src_row_bytes_usize).ok_or_else(|| {
                                            ExecutorError::Validation(
                                                "RESOURCE_DIRTY_RANGE: row_buf OOB".into(),
                                            )
                                        })?,
                                        dst_slice,
                                    );
                                }
                                _ => unreachable!(),
                            }
                        }

                        self.queue.write_texture(
                            wgpu::ImageCopyTexture {
                                texture: &tex.texture,
                                mip_level: sub.mip_level,
                                origin: wgpu::Origin3d {
                                    x: 0,
                                    y: origin_y,
                                    z: sub.array_layer,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            &staging,
                            wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(upload_bpr),
                                rows_per_image: Some(rows),
                            },
                            wgpu::Extent3d {
                                width: sub.width,
                                height: copy_height,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                    TextureUploadTransform::Bc1ToRgba8
                    | TextureUploadTransform::Bc2ToRgba8
                    | TextureUploadTransform::Bc3ToRgba8
                    | TextureUploadTransform::Bc7ToRgba8 => {
                        let bc_layout =
                            texture_copy_layout(sub.width, copy_height, tex.format_raw)?;
                        if bc_layout.rows_in_layout != row_count {
                            return Err(ExecutorError::Validation(format!(
                                "RESOURCE_DIRTY_RANGE: row_count mismatch (expected {}, found {})",
                                row_count, bc_layout.rows_in_layout
                            )));
                        }

                        let rows = bc_layout.rows_in_layout;
                        let row_bytes = bc_layout.unpadded_bytes_per_row as u64;

                        let packed_len =
                            row_bytes.checked_mul(u64::from(rows)).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: BC size overflow".into(),
                                )
                            })?;
                        let packed_len_usize: usize = packed_len.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "RESOURCE_DIRTY_RANGE: BC size out of range".into(),
                            )
                        })?;
                        let mut packed_bc = vec![0u8; packed_len_usize];

                        let row_bytes_usize: usize =
                            bc_layout.unpadded_bytes_per_row.try_into().map_err(|_| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: BC row size out of range".into(),
                                )
                            })?;
                        for row in 0..rows {
                            let row_idx = start_row_u32.checked_add(row).ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: row index overflow".into(),
                                )
                            })?;
                            let row_off =
                                u64::from(row_idx).checked_mul(row_pitch).ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: row offset overflow".into(),
                                    )
                                })?;
                            let alloc_offset = backing
                                .alloc_offset_bytes
                                .checked_add(sub.offset_bytes)
                                .and_then(|v| v.checked_add(row_off))
                                .ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: alloc offset overflow".into(),
                                    )
                                })?;
                            let src_gpa =
                                table.resolve_gpa(backing.alloc_id, alloc_offset, row_bytes)?;

                            let dst_off = row as usize * row_bytes_usize;
                            let dst_end = dst_off + row_bytes_usize;
                            guest_memory.read(
                                src_gpa,
                                packed_bc.get_mut(dst_off..dst_end).ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: BC staging OOB".into(),
                                    )
                                })?,
                            )?;
                        }

                        let decompressed = match tex.upload_transform {
                            TextureUploadTransform::Bc1ToRgba8 => {
                                decompress_bc1_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            TextureUploadTransform::Bc2ToRgba8 => {
                                decompress_bc2_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            TextureUploadTransform::Bc3ToRgba8 => {
                                decompress_bc3_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            TextureUploadTransform::Bc7ToRgba8 => {
                                decompress_bc7_rgba8(sub.width, copy_height, &packed_bc)
                            }
                            _ => unreachable!(),
                        };

                        let rgba_layout = texture_copy_layout(
                            sub.width,
                            copy_height,
                            pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                        )?;
                        let upload_bpr = rgba_layout.padded_bytes_per_row;

                        let upload_bpr_usize: usize = upload_bpr.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "RESOURCE_DIRTY_RANGE: RGBA bytes_per_row out of range".into(),
                            )
                        })?;
                        let row_bytes_usize: usize =
                            rgba_layout.unpadded_bytes_per_row.try_into().map_err(|_| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: RGBA row size out of range".into(),
                                )
                            })?;

                        let staging_size = (upload_bpr as u64)
                            .checked_mul(u64::from(copy_height))
                            .ok_or_else(|| {
                                ExecutorError::Validation(
                                    "RESOURCE_DIRTY_RANGE: RGBA staging size overflow".into(),
                                )
                            })?;
                        let staging_size_usize: usize = staging_size.try_into().map_err(|_| {
                            ExecutorError::Validation(
                                "RESOURCE_DIRTY_RANGE: RGBA staging size out of range".into(),
                            )
                        })?;
                        let mut staging = vec![0u8; staging_size_usize];

                        for y in 0..copy_height as usize {
                            let src_start = y * row_bytes_usize;
                            let src_end = src_start + row_bytes_usize;
                            let dst_start = y * upload_bpr_usize;
                            staging
                                .get_mut(dst_start..dst_start + row_bytes_usize)
                                .ok_or_else(|| {
                                    ExecutorError::Validation(
                                        "RESOURCE_DIRTY_RANGE: RGBA staging OOB".into(),
                                    )
                                })?
                                .copy_from_slice(decompressed.get(src_start..src_end).ok_or_else(
                                    || {
                                        ExecutorError::Validation(
                                            "RESOURCE_DIRTY_RANGE: RGBA source OOB".into(),
                                        )
                                    },
                                )?);
                        }

                        self.queue.write_texture(
                            wgpu::ImageCopyTexture {
                                texture: &tex.texture,
                                mip_level: sub.mip_level,
                                origin: wgpu::Origin3d {
                                    x: 0,
                                    y: origin_y,
                                    z: sub.array_layer,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            &staging,
                            wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(upload_bpr),
                                rows_per_image: Some(copy_height),
                            },
                            wgpu::Extent3d {
                                width: sub.width,
                                height: copy_height,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }
            }
        }

        tex.dirty_ranges.clear();
        Ok(())
    }
}

fn coalesce_ranges(ranges: &mut Vec<Range<u64>>) {
    // Use an unstable sort to avoid allocating a scratch buffer for large guest-controlled range
    // lists. We don't require stable ordering when start positions are equal.
    ranges.sort_unstable_by_key(|r| r.start);

    // Coalesce in-place to avoid a second allocation proportional to `ranges.len()`.
    let mut out_len = 0usize;
    for i in 0..ranges.len() {
        let r = ranges[i].clone();
        if r.start >= r.end {
            continue;
        }

        if out_len == 0 {
            ranges[0] = r;
            out_len = 1;
            continue;
        }

        let last = &mut ranges[out_len - 1];
        if r.start <= last.end {
            last.end = last.end.max(r.end);
        } else {
            ranges[out_len] = r;
            out_len += 1;
        }
    }
    ranges.truncate(out_len);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
fn coalesce_ranges_u32(ranges: &mut Vec<Range<u32>>) {
    ranges.sort_unstable_by_key(|r| r.start);

    let mut out_len = 0usize;
    for i in 0..ranges.len() {
        let r = ranges[i].clone();
        if r.start >= r.end {
            continue;
        }

        if out_len == 0 {
            ranges[0] = r;
            out_len = 1;
            continue;
        }

        let last = &mut ranges[out_len - 1];
        if r.start <= last.end {
            last.end = last.end.max(r.end);
        } else {
            ranges[out_len] = r;
            out_len += 1;
        }
    }
    ranges.truncate(out_len);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[cfg(not(target_os = "linux"))]
    use std::sync::{Mutex, OnceLock};

    #[cfg(not(target_os = "linux"))]
    fn reset_executor(exec: &mut AeroGpuExecutor) {
        // Dropping `wgpu` resources between unit tests has been observed to segfault on some CI
        // backends. These tests frequently reuse the same handles (1, 2, …), so we still need to
        // clear the executor maps — but do so by leaking the old maps rather than dropping their
        // contents.
        //
        // The leak is test-only and bounded by the (small) number of unit tests that allocate
        // resources.
        let buffers = std::mem::take(&mut exec.buffers);
        std::mem::forget(buffers);

        let textures = std::mem::take(&mut exec.textures);
        std::mem::forget(textures);

        exec.state = ExecutorState::default();
    }

    #[cfg(not(target_os = "linux"))]
    fn shared_executor_bc_off() -> Option<&'static Mutex<AeroGpuExecutor>> {
        static EXEC: OnceLock<Option<&'static Mutex<AeroGpuExecutor>>> = OnceLock::new();
        EXEC.get_or_init(|| {
            let ctx = pollster::block_on(crate::test_wgpu::create_device_exact(
                wgpu::Features::empty(),
            ))?;
            let exec = AeroGpuExecutor::new(ctx.device, ctx.queue).expect("executor init");
            Some(Box::leak(Box::new(Mutex::new(exec))))
        })
        .as_deref()
    }

    #[cfg(not(target_os = "linux"))]
    fn shared_executor_bc_on() -> Option<&'static Mutex<AeroGpuExecutor>> {
        static EXEC: OnceLock<Option<&'static Mutex<AeroGpuExecutor>>> = OnceLock::new();
        EXEC.get_or_init(|| {
            // On Linux CI we frequently only have software adapters; creating multiple Vulkan
            // devices (especially when enabling optional features like BC compression) has been
            // observed to segfault in some sandbox environments. Prefer skipping these unit tests
            // on Linux and rely on the integration test suite (which already has platform-specific
            // skip logic) to cover BC-enabled behavior.
            if cfg!(target_os = "linux") {
                return None;
            }

            // Native BC sampling paths can be flaky on some Linux CI adapters (especially software
            // implementations). Prefer skipping these unit tests rather than producing hard
            // failures (or segfaults) in environments that don't reliably support BC.
            //
            // Integration tests cover the BC-enabled path on machines with known-good adapters.
            let allow_software_adapter = !cfg!(target_os = "linux");

            crate::test_wgpu::ensure_runtime_dir();

            let backends = wgpu::Backends::all() - wgpu::Backends::GL;
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..Default::default()
            });

            for adapter in instance.enumerate_adapters(backends) {
                let info = adapter.get_info();
                if info.backend == wgpu::Backend::Gl {
                    continue;
                }
                if !adapter
                    .features()
                    .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
                {
                    continue;
                }
                if !allow_software_adapter && info.device_type == wgpu::DeviceType::Cpu {
                    continue;
                }

                let Ok((device, queue)) = pollster::block_on(adapter.request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("aerogpu.executor unit-test bc device"),
                        required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                        required_limits: wgpu::Limits::downlevel_defaults(),
                    },
                    None,
                )) else {
                    continue;
                };

                let exec = AeroGpuExecutor::new(device, queue).expect("executor init");
                return Some(Box::leak(Box::new(Mutex::new(exec))));
            }

            None
        })
        .as_deref()
    }

    #[cfg(not(target_os = "linux"))]
    fn with_executor<R>(bc_enabled: bool, f: impl FnOnce(&mut AeroGpuExecutor) -> R) -> Option<R> {
        let exec = if bc_enabled {
            shared_executor_bc_on()?
        } else {
            shared_executor_bc_off()?
        };
        let mut exec = exec.lock().unwrap();
        reset_executor(&mut exec);
        Some(f(&mut exec))
    }

    // The majority of these executor unit tests validate behavior by allocating a headless wgpu
    // device and executing command streams. For `wasm32-unknown-unknown` CI builds we only compile
    // these tests (they are not executed), and wgpu WebGPU types are not `Send`/`Sync` on wasm.
    //
    // Provide a stub helper so tests can early-return via `let Some(...) = with_executor(...) else { return; };`
    // without needing to build a global executor cache on wasm.
    #[cfg(target_arch = "wasm32")]
    fn with_executor<R>(
        _bc_enabled: bool,
        _f: impl FnOnce(&mut AeroGpuExecutor) -> R,
    ) -> Option<R> {
        None
    }

    #[test]
    fn is_x8_format_includes_srgb_variants() {
        assert!(is_x8_format(pci::AerogpuFormat::B8G8R8X8Unorm as u32));
        assert!(is_x8_format(pci::AerogpuFormat::R8G8B8X8Unorm as u32));
        assert!(is_x8_format(pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32));
        assert!(is_x8_format(pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32));

        assert!(!is_x8_format(pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32));
        assert!(!is_x8_format(pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32));
    }

    #[test]
    fn map_buffer_usage_flags_includes_storage_for_uav_srv_buffers() {
        // Raw/structured buffers are translated into `var<storage>` bindings on the host.
        // wgpu requires buffers used in storage bindings to have STORAGE usage set at creation.
        let usage = map_buffer_usage_flags(cmd::AEROGPU_RESOURCE_USAGE_STORAGE);
        assert!(usage.contains(wgpu::BufferUsages::STORAGE));
        assert!(usage.contains(wgpu::BufferUsages::COPY_DST));
        assert!(usage.contains(wgpu::BufferUsages::COPY_SRC));
    }

    #[cfg(not(target_os = "linux"))]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MapExpectation {
        Ok {
            format: wgpu::TextureFormat,
            transform: TextureUploadTransform,
        },
        Unsupported,
    }

    macro_rules! executor_format_expectations {
        ($($variant:ident => { bc_off: $bc_off:expr, bc_on: $bc_on:expr $(,)? },)+) => {
            // Keep the protocol-format list and the match tables in sync by generating everything
            // from the same source of truth.
            //
            // Both matches are intentionally exhaustive (no `_ => ...`), so adding a new protocol
            // enum variant forces these tests to be updated.
            const ALL_PROTOCOL_FORMATS: &[pci::AerogpuFormat] = &[
                $(pci::AerogpuFormat::$variant,)+
            ];

            #[cfg(not(target_os = "linux"))]
            fn expected_map_bc_off(format: pci::AerogpuFormat) -> MapExpectation {
                match format {
                    $(pci::AerogpuFormat::$variant => $bc_off,)+
                }
            }

            #[cfg(not(target_os = "linux"))]
            fn expected_map_bc_on(format: pci::AerogpuFormat) -> MapExpectation {
                match format {
                    $(pci::AerogpuFormat::$variant => $bc_on,)+
                }
            }
        };
    }

    executor_format_expectations! {
        Invalid => { bc_off: MapExpectation::Unsupported, bc_on: MapExpectation::Unsupported },

        B8G8R8A8Unorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8Unorm, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8Unorm, transform: TextureUploadTransform::Direct },
        },
        B8G8R8X8Unorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8Unorm, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8Unorm, transform: TextureUploadTransform::Direct },
        },
        R8G8B8A8Unorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Direct },
        },
        R8G8B8X8Unorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Direct },
        },

        B5G6R5Unorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::B5G6R5ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::B5G6R5ToRgba8 },
        },
        B5G5R5A1Unorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::B5G5R5A1ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::B5G5R5A1ToRgba8 },
        },

        B8G8R8A8UnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8UnormSrgb, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8UnormSrgb, transform: TextureUploadTransform::Direct },
        },
        B8G8R8X8UnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8UnormSrgb, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bgra8UnormSrgb, transform: TextureUploadTransform::Direct },
        },
        R8G8B8A8UnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Direct },
        },
        R8G8B8X8UnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Direct },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Direct },
        },

        // The minimal executor does not currently support depth formats.
        D24UnormS8Uint => { bc_off: MapExpectation::Unsupported, bc_on: MapExpectation::Unsupported },
        D32Float => { bc_off: MapExpectation::Unsupported, bc_on: MapExpectation::Unsupported },

        BC1RgbaUnorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Bc1ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc1RgbaUnorm, transform: TextureUploadTransform::Direct },
        },
        BC1RgbaUnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Bc1ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc1RgbaUnormSrgb, transform: TextureUploadTransform::Direct },
        },
        BC2RgbaUnorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Bc2ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc2RgbaUnorm, transform: TextureUploadTransform::Direct },
        },
        BC2RgbaUnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Bc2ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc2RgbaUnormSrgb, transform: TextureUploadTransform::Direct },
        },
        BC3RgbaUnorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Bc3ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc3RgbaUnorm, transform: TextureUploadTransform::Direct },
        },
        BC3RgbaUnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Bc3ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc3RgbaUnormSrgb, transform: TextureUploadTransform::Direct },
        },
        BC7RgbaUnorm => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8Unorm, transform: TextureUploadTransform::Bc7ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc7RgbaUnorm, transform: TextureUploadTransform::Direct },
        },
        BC7RgbaUnormSrgb => {
            bc_off: MapExpectation::Ok { format: wgpu::TextureFormat::Rgba8UnormSrgb, transform: TextureUploadTransform::Bc7ToRgba8 },
            bc_on: MapExpectation::Ok { format: wgpu::TextureFormat::Bc7RgbaUnormSrgb, transform: TextureUploadTransform::Direct },
        },
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn map_format_conformance_bc_disabled() {
        let Some(()) = with_executor(/*bc_enabled=*/ false, |exec| {
            for &format in ALL_PROTOCOL_FORMATS {
                let got = exec.map_format(format as u32, /*width=*/ 4, /*height=*/ 4, 1);
                match expected_map_bc_off(format) {
                    MapExpectation::Ok {
                        format: expected_format,
                        transform: expected_transform,
                    } => {
                        let (wgpu_format, transform) = got.unwrap_or_else(|err| {
                            panic!(
                                "map_format should accept protocol format {format:?} ({}), got error: {err:?}",
                                format as u32
                            )
                        });
                        assert_eq!(wgpu_format, expected_format, "format={format:?}");
                        assert_eq!(transform, expected_transform, "format={format:?}");
                    }
                    MapExpectation::Unsupported => match got {
                        Ok(v) => panic!(
                            "map_format should reject protocol format {format:?} ({}), got Ok({v:?})",
                            format as u32
                        ),
                        Err(ExecutorError::Validation(message)) => {
                            assert!(
                                message.contains("unsupported aerogpu_format"),
                                "unexpected error message for {format:?}: {message}"
                            );
                        }
                        Err(other) => panic!(
                            "expected validation error for {format:?}, got {other:?}"
                        ),
                    },
                }
            }
        }) else {
            return;
        };
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn map_format_conformance_bc_enabled() {
        let Some(()) = with_executor(/*bc_enabled=*/ true, |exec| {
            for &format in ALL_PROTOCOL_FORMATS {
                let got = exec.map_format(format as u32, /*width=*/ 4, /*height=*/ 4, 1);
                match expected_map_bc_on(format) {
                    MapExpectation::Ok {
                        format: expected_format,
                        transform: expected_transform,
                    } => {
                        let (wgpu_format, transform) = got.unwrap_or_else(|err| {
                            panic!(
                                "map_format should accept protocol format {format:?} ({}), got error: {err:?}",
                                format as u32
                            )
                        });
                        assert_eq!(wgpu_format, expected_format, "format={format:?}");
                        assert_eq!(transform, expected_transform, "format={format:?}");
                    }
                    MapExpectation::Unsupported => match got {
                        Ok(v) => panic!(
                            "map_format should reject protocol format {format:?} ({}), got Ok({v:?})",
                            format as u32
                        ),
                        Err(ExecutorError::Validation(message)) => {
                            assert!(
                                message.contains("unsupported aerogpu_format"),
                                "unexpected error message for {format:?}: {message}"
                            );
                        }
                        Err(other) => panic!(
                            "expected validation error for {format:?}, got {other:?}"
                        ),
                    },
                }
            }
        }) else {
            return;
        };
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct CopyLayoutExpectation {
        block_w: u32,
        block_h: u32,
        block_bytes: u32,
    }

    fn expected_copy_layout(format: pci::AerogpuFormat) -> Option<CopyLayoutExpectation> {
        // Intentionally exhaustive (no `_ => ...`) so adding a new protocol format forces a test
        // update.
        match format {
            pci::AerogpuFormat::Invalid => None,

            pci::AerogpuFormat::B8G8R8A8Unorm
            | pci::AerogpuFormat::B8G8R8X8Unorm
            | pci::AerogpuFormat::R8G8B8A8Unorm
            | pci::AerogpuFormat::R8G8B8X8Unorm
            | pci::AerogpuFormat::B8G8R8A8UnormSrgb
            | pci::AerogpuFormat::B8G8R8X8UnormSrgb
            | pci::AerogpuFormat::R8G8B8A8UnormSrgb
            | pci::AerogpuFormat::R8G8B8X8UnormSrgb => Some(CopyLayoutExpectation {
                block_w: 1,
                block_h: 1,
                block_bytes: 4,
            }),

            pci::AerogpuFormat::B5G6R5Unorm | pci::AerogpuFormat::B5G5R5A1Unorm => {
                Some(CopyLayoutExpectation {
                    block_w: 1,
                    block_h: 1,
                    block_bytes: 2,
                })
            }

            pci::AerogpuFormat::BC1RgbaUnorm | pci::AerogpuFormat::BC1RgbaUnormSrgb => {
                Some(CopyLayoutExpectation {
                    block_w: 4,
                    block_h: 4,
                    block_bytes: 8,
                })
            }

            pci::AerogpuFormat::BC2RgbaUnorm
            | pci::AerogpuFormat::BC2RgbaUnormSrgb
            | pci::AerogpuFormat::BC3RgbaUnorm
            | pci::AerogpuFormat::BC3RgbaUnormSrgb
            | pci::AerogpuFormat::BC7RgbaUnorm
            | pci::AerogpuFormat::BC7RgbaUnormSrgb => Some(CopyLayoutExpectation {
                block_w: 4,
                block_h: 4,
                block_bytes: 16,
            }),

            // The stable executor does not support depth formats yet.
            pci::AerogpuFormat::D24UnormS8Uint | pci::AerogpuFormat::D32Float => None,
        }
    }

    #[test]
    fn texture_copy_layout_covers_all_protocol_formats() {
        // Use non-multiple-of-4 dimensions to exercise BC div_ceil block rounding.
        let width = 5;
        let height = 7;

        for &format in ALL_PROTOCOL_FORMATS {
            let got = texture_copy_layout(width, height, format as u32);
            match expected_copy_layout(format) {
                Some(exp) => {
                    let layout = got.unwrap_or_else(|err| {
                        panic!(
                            "texture_copy_layout should accept format {format:?} ({}), got error: {err:?}",
                            format as u32
                        )
                    });

                    assert_eq!(layout.block_w, exp.block_w, "format={format:?}");
                    assert_eq!(layout.block_h, exp.block_h, "format={format:?}");
                    assert_eq!(layout.block_bytes, exp.block_bytes, "format={format:?}");

                    let blocks_w = width.div_ceil(exp.block_w);
                    let blocks_h = height.div_ceil(exp.block_h);
                    let expected_unpadded = blocks_w * exp.block_bytes;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let expected_padded = expected_unpadded.div_ceil(align) * align;

                    assert_eq!(layout.rows_in_layout, blocks_h, "format={format:?}");
                    assert_eq!(
                        layout.unpadded_bytes_per_row, expected_unpadded,
                        "format={format:?}"
                    );
                    assert_eq!(
                        layout.padded_bytes_per_row, expected_padded,
                        "format={format:?}"
                    );
                }
                None => match got {
                    Ok(v) => panic!(
                        "texture_copy_layout should reject format {format:?} ({}), got Ok({v:?})",
                        format as u32
                    ),
                    Err(ExecutorError::Validation(message)) => {
                        assert!(
                            message.contains("unsupported aerogpu_format"),
                            "unexpected error for {format:?}: {message}"
                        );
                    }
                    Err(other) => {
                        panic!("expected validation error for {format:?}, got {other:?}")
                    }
                },
            }
        }
    }

    fn build_alloc_table_with_stride(entries: &[(u32, u64, u64)], entry_stride: u32) -> Vec<u8> {
        let size_bytes =
            ring::AerogpuAllocTableHeader::SIZE_BYTES as u32 + entries.len() as u32 * entry_stride;
        let mut bytes = vec![0u8; size_bytes as usize];

        bytes[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
        bytes[8..12].copy_from_slice(&size_bytes.to_le_bytes());
        bytes[12..16].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        bytes[16..20].copy_from_slice(&entry_stride.to_le_bytes());
        // reserved0 stays zeroed.

        for (i, (alloc_id, gpa, size_bytes)) in entries.iter().copied().enumerate() {
            let base = ring::AerogpuAllocTableHeader::SIZE_BYTES + i * entry_stride as usize;
            bytes[base..base + 4].copy_from_slice(&alloc_id.to_le_bytes());
            // flags = 0
            bytes[base + 8..base + 16].copy_from_slice(&gpa.to_le_bytes());
            bytes[base + 16..base + 24].copy_from_slice(&size_bytes.to_le_bytes());
            // reserved0 stays zeroed.
        }

        bytes
    }

    fn build_alloc_table(entries: &[(u32, u64, u64)]) -> Vec<u8> {
        build_alloc_table_with_stride(entries, ring::AerogpuAllocEntry::SIZE_BYTES as u32)
    }

    #[test]
    fn coalesce_ranges_merges_overlapping_and_adjacent() {
        let mut ranges = vec![10u64..12, 0..4, 4..8, 11..15, 20..20];
        coalesce_ranges(&mut ranges);
        assert_eq!(ranges, vec![0..8, 10..15]);
    }

    #[test]
    fn coalesce_ranges_u32_merges_overlapping_and_adjacent() {
        let mut ranges = vec![10u32..12, 0..4, 4..8, 11..15, 20..20];
        coalesce_ranges_u32(&mut ranges);
        assert_eq!(ranges, vec![0..8, 10..15]);
    }

    #[test]
    fn upload_resource_texture_row_range_rejects_offset_that_overflows_u32_rows() {
        let row_pitch_bytes = 16u64;
        let offset_bytes = row_pitch_bytes * (u64::from(u32::MAX) + 1);
        let size_bytes = row_pitch_bytes;
        let err =
            upload_resource_texture_row_range(1, offset_bytes, size_bytes, row_pitch_bytes, 4)
                .unwrap_err();
        match err {
            ExecutorError::Validation(message) => {
                assert!(message.contains("out of bounds"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn alloc_table_decode_accepts_valid_entries() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(1, 0x1000, 0x2000), (2, 0x3000, 0x4000)]);
        let table_gpa = 0x100u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let table =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap();
        assert_eq!(table.get(1).unwrap().gpa, 0x1000);
        assert_eq!(table.get(1).unwrap().size_bytes, 0x2000);
        assert_eq!(table.get(2).unwrap().gpa, 0x3000);
        assert_eq!(table.get(2).unwrap().size_bytes, 0x4000);
    }

    #[test]
    fn alloc_table_decode_accepts_extended_entry_stride_bytes() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let entry_stride = ring::AerogpuAllocEntry::SIZE_BYTES as u32 + 16;
        let table_bytes = build_alloc_table_with_stride(
            &[(1, 0x1000, 0x2000), (2, 0x3000, 0x4000)],
            entry_stride,
        );
        let table_gpa = 0x120u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let table =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap();
        assert_eq!(table.get(1).unwrap().gpa, 0x1000);
        assert_eq!(table.get(1).unwrap().size_bytes, 0x2000);
        assert_eq!(table.get(2).unwrap().gpa, 0x3000);
        assert_eq!(table.get(2).unwrap().size_bytes, 0x4000);
    }

    #[test]
    fn alloc_table_decode_accepts_zero_gpa() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(1, 0, 0x2000)]);
        let table_gpa = 0x180u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let table =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap();
        assert_eq!(table.get(1).unwrap().gpa, 0);
        assert_eq!(table.get(1).unwrap().size_bytes, 0x2000);
    }

    #[test]
    fn alloc_table_decode_rejects_alloc_id_zero() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(0, 0x1000, 0x2000)]);
        let table_gpa = 0x200u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let err =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap_err();
        match err {
            ExecutorError::Validation(message) => {
                assert!(message.contains("alloc_id must be non-zero"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn alloc_table_decode_rejects_duplicate_alloc_id() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let table_bytes = build_alloc_table(&[(1, 0x1000, 0x2000), (1, 0x3000, 0x4000)]);
        let table_gpa = 0x300u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let err =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap_err();
        match err {
            ExecutorError::Validation(message) => {
                assert!(message.contains("duplicate"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn alloc_table_decode_rejects_size_bytes_too_small_for_layout() {
        let mut guest = crate::guest_memory::VecGuestMemory::new(4096);
        let mut table_bytes = build_alloc_table(&[(1, 0x1000, 0x2000)]);
        // Corrupt the header size_bytes field so the prefix validation fails.
        table_bytes[8..12]
            .copy_from_slice(&(ring::AerogpuAllocTableHeader::SIZE_BYTES as u32).to_le_bytes());
        let table_gpa = 0x400u64;
        guest.write(table_gpa, &table_bytes).unwrap();

        let err =
            AllocTable::decode_from_guest_memory(&mut guest, table_gpa, table_bytes.len() as u32)
                .unwrap_err();
        match err {
            ExecutorError::Validation(message) => {
                assert!(message.contains("BadSizeField"), "{message}");
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn ia_buffers_are_bindable_as_storage_for_vertex_pulling() {
        let Some(()) = with_executor(/*bc_enabled=*/ false, |exec| {
            // Ensure the backend supports compute+storage before asserting. Some downlevel
            // backends (e.g. WebGL2) cannot run vertex pulling compute prepasses.
            let wgsl = r#"
struct Data {
    values: array<u32>,
};

@group(0) @binding(0) var<storage, read> data: Data;

@compute @workgroup_size(1)
fn main() {
    let _x: u32 = data.values[0];
}
"#;
            let shader = exec
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("aerogpu.executor ia storage bind test shader"),
                    source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(wgsl)),
                });
            let bgl = exec
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("aerogpu.executor ia storage bind test bgl"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(4),
                        },
                        count: None,
                    }],
                });
            let pipeline_layout =
                exec.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("aerogpu.executor ia storage bind test pipeline layout"),
                        bind_group_layouts: &[&bgl],
                        push_constant_ranges: &[],
                    });

            exec.device.push_error_scope(wgpu::ErrorFilter::Validation);
            exec.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("aerogpu.executor ia storage bind test pipeline"),
                    layout: Some(&pipeline_layout),
                    module: &shader,
                    entry_point: "main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                });
            exec.device.poll(wgpu::Maintain::Wait);
            let err = pollster::block_on(exec.device.pop_error_scope());
            if err.is_some() {
                // Compute/storage pipelines aren't available on this adapter.
                return;
            }

            const VB: u32 = 1;
            const IB: u32 = 2;
            for (handle, usage_flags) in [
                (VB, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER),
                (IB, cmd::AEROGPU_RESOURCE_USAGE_INDEX_BUFFER),
            ] {
                exec.exec_create_buffer(handle, usage_flags, 16, 0, 0, None)
                    .expect("CREATE_BUFFER should succeed");
            }

            for (label, handle) in [("vertex", VB), ("index", IB)] {
                let buffer = &exec
                    .buffers
                    .get(&handle)
                    .unwrap_or_else(|| panic!("{label} buffer should exist"))
                    .buffer;

                exec.device.push_error_scope(wgpu::ErrorFilter::Validation);
                exec.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aerogpu.executor ia storage bind test bg"),
                    layout: &bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                exec.device.poll(wgpu::Maintain::Wait);
                let err = pollster::block_on(exec.device.pop_error_scope());
                assert!(
                    err.is_none(),
                    "{label} buffer must be bindable as STORAGE for vertex pulling, got: {err:?}"
                );
            }
        }) else {
            return;
        };
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn create_texture2d_bc_falls_back_when_dimensions_not_block_aligned_even_if_bc_enabled() {
        let Some(()) = with_executor(/*bc_enabled=*/ true, |exec| {
            exec.exec_create_texture2d(
                CreateTexture2dArgs {
                    texture_handle: 1,
                    usage_flags: cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: pci::AerogpuFormat::BC1RgbaUnorm as u32,
                    width: 9,
                    height: 9,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes: 0,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
                None,
            )
            .expect("CREATE_TEXTURE2D must succeed");

            let tex = exec.textures.get(&1).expect("texture must exist");
            assert_eq!(tex.format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(tex.upload_transform, TextureUploadTransform::Bc1ToRgba8);
        }) else {
            // Adapter/device does not support BC compression; nothing to validate here.
            return;
        };
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn create_texture2d_bc_falls_back_for_tiny_dimensions_even_if_bc_enabled() {
        let Some(()) = with_executor(/*bc_enabled=*/ true, |exec| {
            // wgpu validation rejects creating BC textures unless the base mip is block-aligned
            // (4x4), even when the base mip is smaller than a full block (e.g. 1x1 BC1).
            exec.exec_create_texture2d(
                CreateTexture2dArgs {
                    texture_handle: 1,
                    usage_flags: cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: pci::AerogpuFormat::BC1RgbaUnorm as u32,
                    width: 1,
                    height: 1,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes: 0,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
                None,
            )
            .expect("CREATE_TEXTURE2D must succeed");

            let tex = exec.textures.get(&1).expect("texture must exist");
            assert_eq!(tex.format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(tex.upload_transform, TextureUploadTransform::Bc1ToRgba8);
        }) else {
            // Adapter/device does not support BC compression; nothing to validate here.
            return;
        };
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn create_texture2d_bc_falls_back_when_intermediate_mip_is_not_block_aligned_even_if_bc_enabled(
    ) {
        let Some(()) = with_executor(/*bc_enabled=*/ true, |exec| {
            // 12x12 with mip_levels=2 produces mip1=6x6. Some backends conservatively validate that
            // mip levels >= 4 remain block-aligned, so we fall back to an RGBA8 texture + CPU
            // decompression.
            exec.exec_create_texture2d(
                CreateTexture2dArgs {
                    texture_handle: 1,
                    usage_flags: cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: pci::AerogpuFormat::BC1RgbaUnorm as u32,
                    width: 12,
                    height: 12,
                    mip_levels: 2,
                    array_layers: 1,
                    row_pitch_bytes: 0,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
                None,
            )
            .expect("CREATE_TEXTURE2D must succeed");

            let tex = exec.textures.get(&1).expect("texture must exist");
            assert_eq!(tex.format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(tex.upload_transform, TextureUploadTransform::Bc1ToRgba8);
        }) else {
            // Adapter/device does not support BC compression; nothing to validate here.
            return;
        };
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn copy_texture2d_rejects_host_format_mismatch_after_bc_fallback() {
        let Some(()) = with_executor(/*bc_enabled=*/ true, |exec| {
            exec.exec_create_texture2d(
                CreateTexture2dArgs {
                    texture_handle: 1,
                    usage_flags: cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: pci::AerogpuFormat::BC1RgbaUnorm as u32,
                    width: 8,
                    height: 8,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes: 0,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
                None,
            )
            .expect("CREATE_TEXTURE2D must succeed");
            exec.exec_create_texture2d(
                CreateTexture2dArgs {
                    texture_handle: 2,
                    usage_flags: cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: pci::AerogpuFormat::BC1RgbaUnorm as u32,
                    width: 9,
                    height: 9,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes: 0,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
                None,
            )
            .expect("CREATE_TEXTURE2D must succeed");

            let mut guest = crate::guest_memory::VecGuestMemory::new(0x1000);
            let mut pending_writebacks = Vec::new();
            let err = exec
                .exec_copy_texture2d(
                    CopyTexture2dArgs {
                        dst_texture: 2,
                        src_texture: 1,
                        dst_mip_level: 0,
                        dst_array_layer: 0,
                        src_mip_level: 0,
                        src_array_layer: 0,
                        dst_x: 0,
                        dst_y: 0,
                        src_x: 0,
                        src_y: 0,
                        width: 4,
                        height: 4,
                        flags: 0,
                    },
                    &mut guest,
                    None,
                    &mut pending_writebacks,
                )
                .unwrap_err();
            match err {
                ExecutorError::Validation(message) => {
                    assert!(message.contains("internal format mismatch"), "{message}");
                }
                other => panic!("expected validation error, got {other:?}"),
            }
        }) else {
            // Adapter/device does not support BC compression; nothing to validate here.
            return;
        };
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn upload_resource_texture2d_supports_mip_array_subresource_offsets() {
        let Some(()) = with_executor(/*bc_enabled=*/ false, |exec| {
            exec.exec_create_texture2d(
                CreateTexture2dArgs {
                    texture_handle: 1,
                    usage_flags: cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                    width: 4,
                    height: 4,
                    mip_levels: 3,
                    array_layers: 2,
                    // Deliberately pad mip0 so mip+layer subresource offsets are not multiples of
                    // `row_pitch_bytes`.
                    row_pitch_bytes: 64,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
                None,
            )
            .expect("CREATE_TEXTURE2D must succeed");

            let (mip0_pitch, sub) = {
                let tex = exec.textures.get(&1).expect("texture must exist");
                let mip0_pitch = u64::from(tex.subresource_layouts[0].row_pitch_bytes);
                let sub = tex
                    .subresource_layouts
                    .iter()
                    .find(|s| s.mip_level == 1 && s.array_layer == 1)
                    .copied()
                    .expect("mip1 layer1 layout");
                (mip0_pitch, sub)
            };
            assert!(
                sub.offset_bytes % mip0_pitch != 0,
                "expected non-mip0-aligned offset (offset={} mip0_pitch={})",
                sub.offset_bytes,
                mip0_pitch
            );

            let payload_len: usize = sub.size_bytes.try_into().expect("payload size fits usize");
            let mut payload = vec![0u8; payload_len];
            for (i, b) in payload.iter_mut().enumerate() {
                *b = 0xA0u8.wrapping_add(i as u8);
            }

            exec.exec_upload_resource(1, sub.offset_bytes, sub.size_bytes, &payload)
                .expect("UPLOAD_RESOURCE must succeed");

            let bytes_per_row = 256u32;
            let out_size = u64::from(bytes_per_row) * u64::from(sub.height);
            let out = exec.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu.executor.test.upload_resource.readback"),
                size: out_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu.executor.test.upload_resource.encoder"),
                });
            {
                let tex = exec.textures.get(&1).expect("texture must exist");
                encoder.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture: &tex.texture,
                        mip_level: sub.mip_level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: sub.array_layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &out,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: Some(sub.height),
                        },
                    },
                    wgpu::Extent3d {
                        width: sub.width,
                        height: sub.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
            exec.queue.submit([encoder.finish()]);

            let readback = exec
                .read_buffer_to_vec_blocking(&out, out_size, "upload_resource texture readback")
                .expect("readback must succeed");
            let row_bytes = (sub.width * 4) as usize;
            for y in 0..sub.height as usize {
                let src_start = y * sub.row_pitch_bytes as usize;
                let src_end = src_start + row_bytes;
                let dst_start = y * bytes_per_row as usize;
                let dst_end = dst_start + row_bytes;
                assert_eq!(
                    &readback[dst_start..dst_end],
                    &payload[src_start..src_end],
                    "row {y} bytes"
                );
            }
        }) else {
            return;
        };
    }

    #[test]
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "linux")))]
    fn upload_resource_texture2d_accepts_packed_mip_array_payload() {
        let Some(()) = with_executor(/*bc_enabled=*/ false, |exec| {
            exec.exec_create_texture2d(
                CreateTexture2dArgs {
                    texture_handle: 1,
                    usage_flags: cmd::AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: pci::AerogpuFormat::R8G8B8A8Unorm as u32,
                    width: 4,
                    height: 4,
                    mip_levels: 3,
                    array_layers: 2,
                    row_pitch_bytes: 64,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
                None,
            )
            .expect("CREATE_TEXTURE2D must succeed");

            let (total_bytes, tail) = {
                let tex = exec.textures.get(&1).expect("texture must exist");
                let total_bytes = tex
                    .subresource_layouts
                    .last()
                    .and_then(|last| last.offset_bytes.checked_add(last.size_bytes))
                    .expect("total packed bytes");

                // Put a distinct marker in the tail subresource (mip2, layer1) to prove that the
                // executor can split a single UPLOAD_RESOURCE payload across packed subresources.
                let tail = tex
                    .subresource_layouts
                    .iter()
                    .find(|s| s.mip_level == 2 && s.array_layer == 1)
                    .copied()
                    .expect("mip2 layer1 layout");
                (total_bytes, tail)
            };

            let mut payload = vec![0u8; usize::try_from(total_bytes).expect("payload fits usize")];
            assert_eq!(tail.width, 1);
            assert_eq!(tail.height, 1);
            assert_eq!(tail.size_bytes, 4);
            payload[tail.offset_bytes as usize..tail.offset_bytes as usize + 4]
                .copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

            exec.exec_upload_resource(1, 0, total_bytes, &payload)
                .expect("UPLOAD_RESOURCE full packed payload must succeed");

            let bytes_per_row = 256u32;
            let out_size = u64::from(bytes_per_row) * u64::from(tail.height);
            let out = exec.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aerogpu.executor.test.upload_resource.full_payload.readback"),
                size: out_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = exec
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aerogpu.executor.test.upload_resource.full_payload.encoder"),
                });
            {
                let tex = exec.textures.get(&1).expect("texture must exist");
                encoder.copy_texture_to_buffer(
                    wgpu::ImageCopyTexture {
                        texture: &tex.texture,
                        mip_level: tail.mip_level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: tail.array_layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::ImageCopyBuffer {
                        buffer: &out,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: Some(tail.height),
                        },
                    },
                    wgpu::Extent3d {
                        width: tail.width,
                        height: tail.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
            exec.queue.submit([encoder.finish()]);

            let readback = exec
                .read_buffer_to_vec_blocking(
                    &out,
                    out_size,
                    "upload_resource full payload texture readback",
                )
                .expect("readback must succeed");
            assert_eq!(&readback[0..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
        }) else {
            return;
        };
    }
}
