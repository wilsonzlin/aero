use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;

use crate::bc_decompress::{
    decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8,
};
use crate::texture_format::{
    select_texture_format, TextureFormat, TextureFormatSelection, TextureUploadTransform,
};
use crate::GpuCapabilities;

/// Key used to identify cached textures (typically the guest VRAM address).
pub type TextureKey = u64;

#[derive(Debug, Clone)]
pub struct TextureDesc {
    pub size: wgpu::Extent3d,
    pub mip_level_count: u32,
    pub sample_count: u32,
    pub dimension: wgpu::TextureDimension,
    pub format: TextureFormat,
    pub usage: wgpu::TextureUsages,
    pub label: Option<String>,
}

impl TextureDesc {
    pub fn new_2d(
        width: u32,
        height: u32,
        format: TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Self {
        Self {
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            label: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TextureRegion {
    pub origin: wgpu::Origin3d,
    pub size: wgpu::Extent3d,
    pub mip_level: u32,
}

impl TextureRegion {
    pub fn full(size: wgpu::Extent3d) -> Self {
        Self {
            origin: wgpu::Origin3d::ZERO,
            size,
            mip_level: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextureViewDesc {
    pub format: Option<wgpu::TextureFormat>,
    pub dimension: Option<wgpu::TextureViewDimension>,
    pub aspect: wgpu::TextureAspect,
    pub base_mip_level: u32,
    pub mip_level_count: Option<u32>,
    pub base_array_layer: u32,
    pub array_layer_count: Option<u32>,
}

impl Default for TextureViewDesc {
    fn default() -> Self {
        Self {
            format: None,
            dimension: None,
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: None,
            base_array_layer: 0,
            array_layer_count: None,
        }
    }
}

impl TextureViewDesc {
    fn to_wgpu(&self) -> wgpu::TextureViewDescriptor<'_> {
        wgpu::TextureViewDescriptor {
            label: None,
            format: self.format,
            dimension: self.dimension,
            aspect: self.aspect,
            base_mip_level: self.base_mip_level,
            mip_level_count: self.mip_level_count,
            base_array_layer: self.base_array_layer,
            array_layer_count: self.array_layer_count,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SamplerDesc {
    pub address_mode_u: wgpu::AddressMode,
    pub address_mode_v: wgpu::AddressMode,
    pub address_mode_w: wgpu::AddressMode,
    pub mag_filter: wgpu::FilterMode,
    pub min_filter: wgpu::FilterMode,
    pub mipmap_filter: wgpu::FilterMode,
}

impl Default for SamplerDesc {
    fn default() -> Self {
        Self {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SamplerKey {
    address_mode_u: wgpu::AddressMode,
    address_mode_v: wgpu::AddressMode,
    address_mode_w: wgpu::AddressMode,
    mag_filter: wgpu::FilterMode,
    min_filter: wgpu::FilterMode,
    mipmap_filter: wgpu::FilterMode,
}

impl From<SamplerDesc> for SamplerKey {
    fn from(desc: SamplerDesc) -> Self {
        Self {
            address_mode_u: desc.address_mode_u,
            address_mode_v: desc.address_mode_v,
            address_mode_w: desc.address_mode_w,
            mag_filter: desc.mag_filter,
            min_filter: desc.min_filter,
            mipmap_filter: desc.mipmap_filter,
        }
    }
}

impl SamplerDesc {
    fn to_wgpu(self) -> wgpu::SamplerDescriptor<'static> {
        wgpu::SamplerDescriptor {
            label: None,
            address_mode_u: self.address_mode_u,
            address_mode_v: self.address_mode_v,
            address_mode_w: self.address_mode_w,
            mag_filter: self.mag_filter,
            min_filter: self.min_filter,
            mipmap_filter: self.mipmap_filter,
            ..Default::default()
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct TextureManagerStats {
    pub textures_created: u64,
    pub textures_evicted: u64,
    pub total_gpu_bytes: u64,

    pub view_cache_hits: u64,
    pub view_cache_misses: u64,
    pub views_created: u64,

    pub sampler_cache_hits: u64,
    pub sampler_cache_misses: u64,
    pub samplers_created: u64,

    pub uploads_write_texture: u64,
    pub uploads_staging_copy: u64,

    pub bc_cpu_fallback_uploads: u64,
    pub bc_cpu_fallback_input_bytes: u64,
    pub bc_cpu_fallback_output_bytes: u64,
}

struct UploadContext<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    stats: &'a mut TextureManagerStats,
}

#[derive(Debug, Error)]
pub enum TextureManagerError {
    #[error("texture {0:#x} not found")]
    TextureNotFound(TextureKey),

    #[error("mip level {mip_level} out of range (mip_level_count={mip_level_count})")]
    MipLevelOutOfRange {
        mip_level: u32,
        mip_level_count: u32,
    },

    #[error("write region out of bounds: origin={origin:?} size={size:?} mip_size={mip_size:?} mip={mip_level}")]
    RegionOutOfBounds {
        origin: wgpu::Origin3d,
        size: wgpu::Extent3d,
        mip_size: wgpu::Extent3d,
        mip_level: u32,
    },

    #[error("BC-compressed writes must be 4x4 block aligned: origin=({origin_x},{origin_y}) size=({width},{height}) mip_size=({mip_width},{mip_height})")]
    BcRegionNotBlockAligned {
        origin_x: u32,
        origin_y: u32,
        width: u32,
        height: u32,
        mip_width: u32,
        mip_height: u32,
    },

    #[error("texture upload data length mismatch: expected {expected} bytes, got {actual}")]
    DataLengthMismatch { expected: usize, actual: usize },
}

struct TextureEntry {
    texture: Arc<wgpu::Texture>,
    desc: TextureDesc,
    selection: TextureFormatSelection,
    gpu_bytes: u64,
    last_used: u64,
    view_cache: HashMap<TextureViewDesc, Arc<wgpu::TextureView>>,
}

pub struct TextureManager<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    caps: GpuCapabilities,
    budget_bytes: Option<u64>,

    textures: HashMap<TextureKey, TextureEntry>,
    sampler_cache: HashMap<SamplerKey, Arc<wgpu::Sampler>>,

    stats: TextureManagerStats,
    use_counter: u64,
}

impl<'a> TextureManager<'a> {
    fn next_use_counter(&mut self) -> u64 {
        let value = self.use_counter;
        self.use_counter = self.use_counter.wrapping_add(1);
        value
    }

    pub fn new(device: &'a wgpu::Device, queue: &'a wgpu::Queue, caps: GpuCapabilities) -> Self {
        Self {
            device,
            queue,
            caps,
            budget_bytes: None,
            textures: HashMap::new(),
            sampler_cache: HashMap::new(),
            stats: TextureManagerStats::default(),
            use_counter: 0,
        }
    }

    pub fn with_budget(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        caps: GpuCapabilities,
        budget_bytes: u64,
    ) -> Self {
        let mut mgr = Self::new(device, queue, caps);
        mgr.budget_bytes = Some(budget_bytes);
        mgr
    }

    pub fn stats(&self) -> &TextureManagerStats {
        &self.stats
    }

    pub fn create_texture(&mut self, key: TextureKey, mut desc: TextureDesc) -> Arc<wgpu::Texture> {
        let selection = select_texture_format(
            desc.format,
            self.caps,
            desc.size.width,
            desc.size.height,
            desc.mip_level_count,
        );

        // Upload APIs require COPY_DST.
        desc.usage |= wgpu::TextureUsages::COPY_DST;

        let texture = Arc::new(self.device.create_texture(&wgpu::TextureDescriptor {
            label: desc.label.as_deref(),
            size: desc.size,
            mip_level_count: desc.mip_level_count,
            sample_count: desc.sample_count,
            dimension: desc.dimension,
            format: selection.actual,
            usage: desc.usage,
            view_formats: &[],
        }));

        let gpu_bytes =
            estimate_texture_size_bytes(selection.actual, desc.size, desc.mip_level_count);

        if let Some(old) = self.textures.remove(&key) {
            self.stats.total_gpu_bytes = self.stats.total_gpu_bytes.saturating_sub(old.gpu_bytes);
        }

        let last_used = self.use_counter;
        self.use_counter = self.use_counter.wrapping_add(1);

        self.textures.insert(
            key,
            TextureEntry {
                texture: texture.clone(),
                desc,
                selection,
                gpu_bytes,
                last_used,
                view_cache: HashMap::new(),
            },
        );

        self.stats.textures_created += 1;
        self.stats.total_gpu_bytes += gpu_bytes;
        self.evict_to_budget();
        texture
    }

    pub fn texture(&mut self, key: TextureKey) -> Result<Arc<wgpu::Texture>, TextureManagerError> {
        let last_used = self.next_use_counter();
        let entry = self
            .textures
            .get_mut(&key)
            .ok_or(TextureManagerError::TextureNotFound(key))?;
        entry.last_used = last_used;
        Ok(entry.texture.clone())
    }

    pub fn texture_format(
        &mut self,
        key: TextureKey,
    ) -> Result<wgpu::TextureFormat, TextureManagerError> {
        let last_used = self.next_use_counter();
        let entry = self
            .textures
            .get_mut(&key)
            .ok_or(TextureManagerError::TextureNotFound(key))?;
        entry.last_used = last_used;
        Ok(entry.selection.actual)
    }

    pub fn view(
        &mut self,
        key: TextureKey,
        desc: TextureViewDesc,
    ) -> Result<Arc<wgpu::TextureView>, TextureManagerError> {
        let last_used = self.next_use_counter();
        let entry = self
            .textures
            .get_mut(&key)
            .ok_or(TextureManagerError::TextureNotFound(key))?;
        entry.last_used = last_used;

        if let Some(view) = entry.view_cache.get(&desc) {
            self.stats.view_cache_hits += 1;
            return Ok(view.clone());
        }

        self.stats.view_cache_misses += 1;
        let view = Arc::new(entry.texture.create_view(&desc.to_wgpu()));
        entry.view_cache.insert(desc, view.clone());
        self.stats.views_created += 1;
        Ok(view)
    }

    pub fn sampler(&mut self, desc: SamplerDesc) -> Arc<wgpu::Sampler> {
        let key = SamplerKey::from(desc);
        if let Some(existing) = self.sampler_cache.get(&key) {
            self.stats.sampler_cache_hits += 1;
            return existing.clone();
        }

        self.stats.sampler_cache_misses += 1;
        let sampler = Arc::new(self.device.create_sampler(&desc.to_wgpu()));
        self.sampler_cache.insert(key, sampler.clone());
        self.stats.samplers_created += 1;
        sampler
    }

    pub fn write_texture(
        &mut self,
        key: TextureKey,
        data: &[u8],
    ) -> Result<(), TextureManagerError> {
        let size = {
            let entry = self
                .textures
                .get(&key)
                .ok_or(TextureManagerError::TextureNotFound(key))?;
            entry.desc.size
        };
        self.write_texture_region(key, TextureRegion::full(size), data)
    }

    pub fn write_texture_region(
        &mut self,
        key: TextureKey,
        region: TextureRegion,
        data: &[u8],
    ) -> Result<(), TextureManagerError> {
        let last_used = self.next_use_counter();
        let (selection, requested_format, mip_level_count, mip_size, texture) = {
            let entry = self
                .textures
                .get_mut(&key)
                .ok_or(TextureManagerError::TextureNotFound(key))?;
            entry.last_used = last_used;
            let mip_level_count = entry.desc.mip_level_count;
            let mip_size = mip_extent(entry.desc.size, region.mip_level);
            (
                entry.selection,
                entry.desc.format,
                mip_level_count,
                mip_size,
                entry.texture.clone(),
            )
        };

        validate_region(region, mip_level_count, mip_size)?;

        let mut upload = UploadContext {
            device: self.device,
            queue: self.queue,
            stats: &mut self.stats,
        };

        match selection.upload_transform {
            TextureUploadTransform::Direct => upload_direct(
                &mut upload,
                &texture,
                region,
                requested_format,
                mip_size,
                data,
            ),
            TextureUploadTransform::Bc1ToRgba8 => {
                let blocks_w = region.size.width.div_ceil(4) as usize;
                let blocks_h = region.size.height.div_ceil(4) as usize;
                let expected = blocks_w * blocks_h * 8;
                if data.len() != expected {
                    return Err(TextureManagerError::DataLengthMismatch {
                        expected,
                        actual: data.len(),
                    });
                }

                let decompressed =
                    decompress_bc1_rgba8(region.size.width, region.size.height, data);
                upload.stats.bc_cpu_fallback_uploads += 1;
                upload.stats.bc_cpu_fallback_input_bytes += data.len() as u64;
                upload.stats.bc_cpu_fallback_output_bytes += decompressed.len() as u64;

                upload_rgba8(&mut upload, &texture, region, &decompressed)
            }
            TextureUploadTransform::Bc2ToRgba8 => {
                let blocks_w = region.size.width.div_ceil(4) as usize;
                let blocks_h = region.size.height.div_ceil(4) as usize;
                let expected = blocks_w * blocks_h * 16;
                if data.len() != expected {
                    return Err(TextureManagerError::DataLengthMismatch {
                        expected,
                        actual: data.len(),
                    });
                }

                let decompressed =
                    decompress_bc2_rgba8(region.size.width, region.size.height, data);
                upload.stats.bc_cpu_fallback_uploads += 1;
                upload.stats.bc_cpu_fallback_input_bytes += data.len() as u64;
                upload.stats.bc_cpu_fallback_output_bytes += decompressed.len() as u64;

                upload_rgba8(&mut upload, &texture, region, &decompressed)
            }
            TextureUploadTransform::Bc3ToRgba8 => {
                let blocks_w = region.size.width.div_ceil(4) as usize;
                let blocks_h = region.size.height.div_ceil(4) as usize;
                let expected = blocks_w * blocks_h * 16;
                if data.len() != expected {
                    return Err(TextureManagerError::DataLengthMismatch {
                        expected,
                        actual: data.len(),
                    });
                }

                let decompressed =
                    decompress_bc3_rgba8(region.size.width, region.size.height, data);
                upload.stats.bc_cpu_fallback_uploads += 1;
                upload.stats.bc_cpu_fallback_input_bytes += data.len() as u64;
                upload.stats.bc_cpu_fallback_output_bytes += decompressed.len() as u64;

                upload_rgba8(&mut upload, &texture, region, &decompressed)
            }
            TextureUploadTransform::Bc7ToRgba8 => {
                let blocks_w = region.size.width.div_ceil(4) as usize;
                let blocks_h = region.size.height.div_ceil(4) as usize;
                let expected = blocks_w * blocks_h * 16;
                if data.len() != expected {
                    return Err(TextureManagerError::DataLengthMismatch {
                        expected,
                        actual: data.len(),
                    });
                }

                let decompressed =
                    decompress_bc7_rgba8(region.size.width, region.size.height, data);
                upload.stats.bc_cpu_fallback_uploads += 1;
                upload.stats.bc_cpu_fallback_input_bytes += data.len() as u64;
                upload.stats.bc_cpu_fallback_output_bytes += decompressed.len() as u64;

                upload_rgba8(&mut upload, &texture, region, &decompressed)
            }
            TextureUploadTransform::B5G6R5ToRgba8 | TextureUploadTransform::B5G5R5A1ToRgba8 => {
                unreachable!("TextureManager does not produce B5 upload transforms")
            }
        }
    }

    fn evict_to_budget(&mut self) {
        let Some(budget) = self.budget_bytes else {
            return;
        };

        // Keep at least one texture around even if it exceeds budget.
        while self.stats.total_gpu_bytes > budget && self.textures.len() > 1 {
            let Some((lru_key, lru_bytes)) = self
                .textures
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, e)| (*k, e.gpu_bytes))
            else {
                break;
            };

            self.textures.remove(&lru_key);
            self.stats.textures_evicted += 1;
            self.stats.total_gpu_bytes = self.stats.total_gpu_bytes.saturating_sub(lru_bytes);
        }
    }
}

fn mip_extent(size: wgpu::Extent3d, mip_level: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: (size.width >> mip_level).max(1),
        height: (size.height >> mip_level).max(1),
        depth_or_array_layers: size.depth_or_array_layers,
    }
}

fn validate_region(
    region: TextureRegion,
    mip_level_count: u32,
    mip_size: wgpu::Extent3d,
) -> Result<(), TextureManagerError> {
    if region.mip_level >= mip_level_count {
        return Err(TextureManagerError::MipLevelOutOfRange {
            mip_level: region.mip_level,
            mip_level_count,
        });
    }

    if region.origin.x + region.size.width > mip_size.width
        || region.origin.y + region.size.height > mip_size.height
        || region.origin.z + region.size.depth_or_array_layers > mip_size.depth_or_array_layers
    {
        return Err(TextureManagerError::RegionOutOfBounds {
            origin: region.origin,
            size: region.size,
            mip_size,
            mip_level: region.mip_level,
        });
    }

    Ok(())
}

fn align_to(value: u32, alignment: u32) -> u32 {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

fn estimate_texture_size_bytes(
    format: wgpu::TextureFormat,
    size: wgpu::Extent3d,
    mip_level_count: u32,
) -> u64 {
    let mut total = 0u64;
    for level in 0..mip_level_count {
        let mip = mip_extent(size, level);
        total += estimate_mip_level_size_bytes(format, mip.width, mip.height)
            * size.depth_or_array_layers as u64;
    }
    total
}

fn estimate_mip_level_size_bytes(format: wgpu::TextureFormat, width: u32, height: u32) -> u64 {
    match format {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
            width as u64 * height as u64 * 4
        }
        wgpu::TextureFormat::Bc1RgbaUnorm | wgpu::TextureFormat::Bc1RgbaUnormSrgb => {
            let blocks_w = width.div_ceil(4);
            let blocks_h = height.div_ceil(4);
            blocks_w as u64 * blocks_h as u64 * 8
        }
        wgpu::TextureFormat::Bc2RgbaUnorm | wgpu::TextureFormat::Bc2RgbaUnormSrgb => {
            let blocks_w = width.div_ceil(4);
            let blocks_h = height.div_ceil(4);
            blocks_w as u64 * blocks_h as u64 * 16
        }
        wgpu::TextureFormat::Bc3RgbaUnorm
        | wgpu::TextureFormat::Bc3RgbaUnormSrgb
        | wgpu::TextureFormat::Bc7RgbaUnorm
        | wgpu::TextureFormat::Bc7RgbaUnormSrgb => {
            let blocks_w = width.div_ceil(4);
            let blocks_h = height.div_ceil(4);
            blocks_w as u64 * blocks_h as u64 * 16
        }
        _ => width as u64 * height as u64 * 4,
    }
}

#[allow(clippy::too_many_arguments)]
fn upload_direct(
    ctx: &mut UploadContext<'_>,
    texture: &wgpu::Texture,
    region: TextureRegion,
    format: TextureFormat,
    mip_size: wgpu::Extent3d,
    data: &[u8],
) -> Result<(), TextureManagerError> {
    match format {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => {
            upload_rgba8(ctx, texture, region, data)
        }

        TextureFormat::Bc1RgbaUnorm | TextureFormat::Bc1RgbaUnormSrgb => {
            upload_bc(ctx, texture, region, mip_size, data, 8)
        }
        TextureFormat::Bc2RgbaUnorm | TextureFormat::Bc2RgbaUnormSrgb => {
            upload_bc(ctx, texture, region, mip_size, data, 16)
        }
        TextureFormat::Bc3RgbaUnorm | TextureFormat::Bc3RgbaUnormSrgb => {
            upload_bc(ctx, texture, region, mip_size, data, 16)
        }
        TextureFormat::Bc7RgbaUnorm | TextureFormat::Bc7RgbaUnormSrgb => {
            upload_bc(ctx, texture, region, mip_size, data, 16)
        }
    }
}

fn upload_rgba8(
    ctx: &mut UploadContext<'_>,
    texture: &wgpu::Texture,
    region: TextureRegion,
    data: &[u8],
) -> Result<(), TextureManagerError> {
    let width = region.size.width;
    let height = region.size.height;
    let expected = width as usize * height as usize * 4;
    if data.len() != expected {
        return Err(TextureManagerError::DataLengthMismatch {
            expected,
            actual: data.len(),
        });
    }

    let unpadded_bpr = width * 4;
    let padded_bpr = align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let layout_rows = height;

    upload_with_alignment(
        ctx,
        texture,
        region,
        region.size,
        data,
        unpadded_bpr,
        padded_bpr,
        layout_rows,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upload_bc(
    ctx: &mut UploadContext<'_>,
    texture: &wgpu::Texture,
    region: TextureRegion,
    mip_size: wgpu::Extent3d,
    data: &[u8],
    block_bytes: u32,
) -> Result<(), TextureManagerError> {
    // Origin must be block-aligned.
    if !region.origin.x.is_multiple_of(4) || !region.origin.y.is_multiple_of(4) {
        return Err(TextureManagerError::BcRegionNotBlockAligned {
            origin_x: region.origin.x,
            origin_y: region.origin.y,
            width: region.size.width,
            height: region.size.height,
            mip_width: mip_size.width,
            mip_height: mip_size.height,
        });
    }

    // Size must be block-aligned unless the copy reaches the mip edge.
    if (!region.size.width.is_multiple_of(4)
        && region.origin.x + region.size.width != mip_size.width)
        || (!region.size.height.is_multiple_of(4)
            && region.origin.y + region.size.height != mip_size.height)
    {
        return Err(TextureManagerError::BcRegionNotBlockAligned {
            origin_x: region.origin.x,
            origin_y: region.origin.y,
            width: region.size.width,
            height: region.size.height,
            mip_width: mip_size.width,
            mip_height: mip_size.height,
        });
    }

    let blocks_w = region.size.width.div_ceil(4);
    let blocks_h = region.size.height.div_ceil(4);
    let unpadded_bpr = blocks_w * block_bytes;
    let expected = unpadded_bpr as usize * blocks_h as usize;
    if data.len() != expected {
        return Err(TextureManagerError::DataLengthMismatch {
            expected,
            actual: data.len(),
        });
    }

    // WebGPU validates BC uploads/copies against the physical (block-rounded) extent. This means
    // small mips such as 2x2 are uploaded as a 4x4 block.
    let copy_size = wgpu::Extent3d {
        width: blocks_w * 4,
        height: blocks_h * 4,
        depth_or_array_layers: region.size.depth_or_array_layers,
    };

    let padded_bpr = align_to(unpadded_bpr, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let layout_rows = blocks_h;

    upload_with_alignment(
        ctx,
        texture,
        region,
        copy_size,
        data,
        unpadded_bpr,
        padded_bpr,
        layout_rows,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upload_with_alignment(
    ctx: &mut UploadContext<'_>,
    texture: &wgpu::Texture,
    region: TextureRegion,
    copy_size: wgpu::Extent3d,
    data: &[u8],
    unpadded_bpr: u32,
    padded_bpr: u32,
    layout_rows: u32,
) {
    let needs_padding = padded_bpr != unpadded_bpr;
    let threshold = 256 * 1024usize;

    let bytes_per_row = Some(padded_bpr);
    let rows_per_image = Some(layout_rows);

    let data_to_upload: Cow<'_, [u8]> = if needs_padding {
        let mut padded = vec![0u8; padded_bpr as usize * layout_rows as usize];
        for row in 0..layout_rows as usize {
            let src_start = row * unpadded_bpr as usize;
            let src_end = src_start + unpadded_bpr as usize;
            let dst_start = row * padded_bpr as usize;
            padded[dst_start..dst_start + unpadded_bpr as usize]
                .copy_from_slice(&data[src_start..src_end]);
        }
        padded.into()
    } else {
        data.into()
    };

    if data_to_upload.len() <= threshold {
        ctx.stats.uploads_write_texture += 1;
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: region.mip_level,
                origin: region.origin,
                aspect: wgpu::TextureAspect::All,
            },
            data_to_upload.as_ref(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row,
                rows_per_image,
            },
            copy_size,
        );
        return;
    }

    ctx.stats.uploads_staging_copy += 1;
    let staging = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aero-gpu.texture_upload_staging"),
        size: data_to_upload.len() as u64,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&staging, 0, data_to_upload.as_ref());

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aero-gpu.texture_upload_staging.encoder"),
        });
    encoder.copy_buffer_to_texture(
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row,
                rows_per_image,
            },
        },
        wgpu::ImageCopyTexture {
            texture,
            mip_level: region.mip_level,
            origin: region.origin,
            aspect: wgpu::TextureAspect::All,
        },
        copy_size,
    );
    ctx.queue.submit([encoder.finish()]);
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_arch = "wasm32"))]
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    async fn create_device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir = std::env::temp_dir().join(format!(
                    "aero-gpu-texture-manager-xdg-runtime-{}",
                    std::process::id()
                ));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
                wgpu::Backends::PRIMARY
            },
            ..Default::default()
        });

        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
        {
            Some(adapter) => Some(adapter),
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        };

        let adapter = adapter?;

        adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-gpu texture_manager test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .ok()
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn create_texture_bc_falls_back_when_dimensions_not_block_aligned() {
        pollster::block_on(async {
            let Some((device, queue)) = create_device_queue().await else {
                return;
            };

            let mut caps = GpuCapabilities::from_device(&device);
            // Simulate a BC-enabled device even if the underlying adapter doesn't support BC: the
            // texture must still fall back based on its dimensions.
            caps.supports_bc_texture_compression = true;

            let mut mgr = TextureManager::new(&device, &queue, caps);
            mgr.create_texture(
                1,
                TextureDesc::new_2d(
                    9,
                    9,
                    TextureFormat::Bc1RgbaUnorm,
                    wgpu::TextureUsages::TEXTURE_BINDING,
                ),
            );

            let entry = mgr.textures.get(&1).expect("texture must exist");
            assert_eq!(entry.selection.actual, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                entry.selection.upload_transform,
                TextureUploadTransform::Bc1ToRgba8
            );
        });
    }
}
