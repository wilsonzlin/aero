use anyhow::{anyhow, Result};

use std::sync::Arc;

use super::{
    align_copy_bytes_per_row, bc_mip_chain_compatible, format_info, D3DFormat, D3DPool, FormatInfo,
    GuestResourceId, LockFlags, ResourceManager, TextureUploadDesc, TextureUsageKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureKind {
    Texture2D {
        width: u32,
        height: u32,
        levels: u32,
    },
    Cube {
        size: u32,
        levels: u32,
    },
}

impl TextureKind {
    pub fn mip_levels(self) -> u32 {
        match self {
            Self::Texture2D { levels, .. } | Self::Cube { levels, .. } => levels,
        }
    }

    pub fn extent(self) -> wgpu::Extent3d {
        match self {
            Self::Texture2D { width, height, .. } => wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            Self::Cube { size, .. } => wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 6,
            },
        }
    }

    pub fn array_layers(self) -> u32 {
        match self {
            Self::Texture2D { .. } => 1,
            Self::Cube { .. } => 6,
        }
    }

    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Texture2D { width, height, .. } => (width, height),
            Self::Cube { size, .. } => (size, size),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TextureDesc {
    pub kind: TextureKind,
    pub format: D3DFormat,
    pub pool: D3DPool,
    pub usage: TextureUsageKind,
}

#[derive(Clone, Debug)]
struct LockState {
    level: u32,
    layer: u32,
    flags: LockFlags,
}

pub struct LockedRect<'a> {
    pub pitch: u32,
    pub data: &'a mut [u8],
}

#[derive(Debug)]
pub struct Texture {
    pub(crate) desc: TextureDesc,
    pub(crate) info: FormatInfo,
    pub(crate) gpu: Option<Arc<wgpu::Texture>>,
    pub(crate) view: Option<Arc<wgpu::TextureView>>,
    pub(crate) shadow: Option<Vec<Vec<u8>>>,
    pub(crate) last_used_frame: u64,

    lock: Option<LockState>,
    lock_data: Vec<u8>,
}

fn format_info_for_texture_desc(
    desc: &TextureDesc,
    device_features: wgpu::Features,
) -> Result<FormatInfo> {
    let mut info = format_info(desc.format, device_features, desc.usage)?;

    if device_features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        && matches!(
            desc.format,
            D3DFormat::Dxt1 | D3DFormat::Dxt3 | D3DFormat::Dxt5
        )
    {
        let (width, height) = desc.kind.dimensions();
        let mip_levels = desc.kind.mip_levels();

        if !bc_mip_chain_compatible(width, height, mip_levels) {
            info.force_decompress_dxt_to_bgra8();
        }
    }

    Ok(info)
}

impl Texture {
    pub fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    pub fn wgpu_format(&self) -> wgpu::TextureFormat {
        self.info.wgpu
    }

    pub fn view_arc(
        &mut self,
        device: &wgpu::Device,
        uploads: &mut super::UploadQueue,
        frame: u64,
    ) -> Arc<wgpu::TextureView> {
        self.last_used_frame = frame;
        self.ensure_gpu(device, uploads);
        Arc::clone(self.view.as_ref().unwrap())
    }

    pub fn gpu_bytes(&self) -> Option<usize> {
        self.gpu.as_ref().map(|_| {
            let (w, h) = self.desc.kind.dimensions();
            let layers = self.desc.kind.array_layers() as usize;
            let mut total = 0usize;
            for level in 0..self.desc.kind.mip_levels() {
                total += self.info.upload_mip_level_byte_len(w, h, level) * layers;
            }
            total
        })
    }

    pub fn evict_gpu(&mut self) -> bool {
        if self.desc.pool != D3DPool::Managed {
            return false;
        }
        if self.gpu.is_none() {
            return false;
        }
        self.gpu = None;
        self.view = None;
        true
    }

    fn ensure_gpu(&mut self, device: &wgpu::Device, uploads: &mut super::UploadQueue) {
        if self.gpu.is_some() {
            return;
        }

        if device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
            && matches!(
                self.desc.format,
                D3DFormat::Dxt1 | D3DFormat::Dxt3 | D3DFormat::Dxt5
            )
        {
            let (width, height) = self.desc.kind.dimensions();
            let mip_levels = self.desc.kind.mip_levels();
            if !bc_mip_chain_compatible(width, height, mip_levels) {
                self.info.force_decompress_dxt_to_bgra8();
            }
        }

        let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d9.texture"),
            size: self.desc.kind.extent(),
            mip_level_count: self.desc.kind.mip_levels(),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.info.wgpu,
            usage: texture_usage_flags(self.desc.usage),
            view_formats: &[],
        }));

        self.gpu = Some(texture);
        self.view = Some(self.create_default_view());

        // Re-upload managed contents.
        if let Some(shadow) = &self.shadow {
            let layers = self.desc.kind.array_layers();
            let levels = self.desc.kind.mip_levels();
            for layer in 0..layers {
                for level in 0..levels {
                    let idx = (layer * levels + level) as usize;
                    if let Some(data) = shadow.get(idx) {
                        let _ = self.schedule_upload(layer, level, data, uploads);
                    }
                }
            }
        }
    }

    fn create_default_view(&self) -> Arc<wgpu::TextureView> {
        let tex = self.gpu.as_ref().unwrap();
        let dimension = match self.desc.kind {
            TextureKind::Texture2D { .. } => wgpu::TextureViewDimension::D2,
            TextureKind::Cube { .. } => wgpu::TextureViewDimension::Cube,
        };
        Arc::new(tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("aero-d3d9.texture_view"),
            format: None,
            dimension: Some(dimension),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: None,
            base_array_layer: 0,
            array_layer_count: None,
        }))
    }

    fn schedule_upload(
        &self,
        layer: u32,
        level: u32,
        d3d_data: &[u8],
        uploads: &mut super::UploadQueue,
    ) -> Result<()> {
        let tex = self
            .gpu
            .as_ref()
            .ok_or_else(|| anyhow!("texture has no GPU backing"))?;

        let (width, height) = self.desc.kind.dimensions();
        let (mip_w, mip_h) = self.info.mip_dimensions(width, height, level);

        let (upload_data, upload_bytes_per_row_unpadded, upload_rows_per_image) =
            if self.info.decompress_to_bgra8 {
                let decompressed =
                    decompress_dxt_to_bgra8(self.desc.format, mip_w, mip_h, d3d_data)?;
                (
                    decompressed,
                    mip_w * 4,
                    mip_h, // texel rows
                )
            } else {
                // Potentially patch X8R8G8B8 alpha (treat as opaque).
                let mut upload_data = d3d_data.to_vec();
                if self.info.force_opaque_alpha {
                    for px in upload_data.chunks_exact_mut(4) {
                        px[3] = 0xFF;
                    }
                }

                (
                    upload_data,
                    self.info.upload_bytes_per_row(width, level),
                    self.info.upload_rows_per_image(height, level),
                )
            };

        // Pad rows to WebGPU alignment.
        let padded_bpr = align_copy_bytes_per_row(upload_bytes_per_row_unpadded);
        let padded = if padded_bpr == upload_bytes_per_row_unpadded {
            upload_data
        } else {
            pad_rows(
                &upload_data,
                upload_bytes_per_row_unpadded as usize,
                padded_bpr as usize,
                upload_rows_per_image as usize,
            )
        };

        // WebGPU requires block-compressed texture uploads to use the physical (block-rounded)
        // extent, even for mips that are smaller than a full block (e.g. 2x2 still uploads as 4x4).
        let (copy_width, copy_height) = if self.info.upload_is_compressed {
            let blocks_w = mip_w.div_ceil(self.info.upload_block_width);
            let blocks_h = mip_h.div_ceil(self.info.upload_block_height);
            let copy_width = blocks_w
                .checked_mul(self.info.upload_block_width)
                .ok_or_else(|| anyhow!("BC upload width overflows u32"))?;
            let copy_height = blocks_h
                .checked_mul(self.info.upload_block_height)
                .ok_or_else(|| anyhow!("BC upload height overflows u32"))?;
            (copy_width, copy_height)
        } else {
            (mip_w, mip_h)
        };

        let desc = TextureUploadDesc {
            mip_level: level,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: wgpu::TextureAspect::All,
            size: wgpu::Extent3d {
                width: copy_width,
                height: copy_height,
                depth_or_array_layers: 1,
            },
            bytes_per_row: padded_bpr,
            rows_per_image: upload_rows_per_image,
        };
        uploads.write_texture(
            tex,
            desc.mip_level,
            desc.origin,
            desc.aspect,
            desc.size,
            desc.bytes_per_row,
            desc.rows_per_image,
            &padded,
        );
        Ok(())
    }

    pub fn lock_rect(
        &mut self,
        level: u32,
        layer: u32,
        flags: LockFlags,
    ) -> Result<LockedRect<'_>> {
        if self.lock.is_some() {
            return Err(anyhow!("texture already locked"));
        }

        let levels = self.desc.kind.mip_levels();
        if level >= levels {
            return Err(anyhow!("mip level out of range"));
        }

        let layers = self.desc.kind.array_layers();
        if layer >= layers {
            return Err(anyhow!("array layer out of range"));
        }

        if flags.contains(LockFlags::READONLY) && self.shadow.is_none() {
            return Err(anyhow!("READONLY lock requires managed shadow data"));
        }

        let (width, height) = self.desc.kind.dimensions();
        let pitch = self.info.d3d_mip_level_pitch(width, level);
        let len = self.info.d3d_mip_level_byte_len(width, height, level);

        self.lock_data.resize(len, 0);

        if flags.contains(LockFlags::READONLY) || !flags.contains(LockFlags::DISCARD) {
            if let Some(shadow) = &self.shadow {
                let idx = (layer * levels + level) as usize;
                if let Some(data) = shadow.get(idx) {
                    self.lock_data.as_mut_slice().copy_from_slice(data);
                }
            }
        }

        self.lock = Some(LockState {
            level,
            layer,
            flags,
        });

        Ok(LockedRect {
            pitch,
            data: &mut self.lock_data,
        })
    }

    pub fn unlock_rect(
        &mut self,
        device: &wgpu::Device,
        uploads: &mut super::UploadQueue,
    ) -> Result<()> {
        let Some(lock) = self.lock.take() else {
            return Err(anyhow!("texture not locked"));
        };

        let levels = self.desc.kind.mip_levels();
        if !lock.flags.contains(LockFlags::READONLY) {
            if let Some(shadow) = &mut self.shadow {
                let idx = (lock.layer * levels + lock.level) as usize;
                if let Some(dst) = shadow.get_mut(idx) {
                    dst.as_mut_slice().copy_from_slice(&self.lock_data);
                }
            }
        }

        self.ensure_gpu(device, uploads);

        if !lock.flags.contains(LockFlags::READONLY) {
            self.schedule_upload(lock.layer, lock.level, &self.lock_data, uploads)?;
        }

        Ok(())
    }
}

fn texture_usage_flags(kind: TextureUsageKind) -> wgpu::TextureUsages {
    match kind {
        TextureUsageKind::Sampled => {
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST
        }
        TextureUsageKind::RenderTarget => {
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT
        }
        TextureUsageKind::DepthStencil => {
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST
        }
    }
}

fn pad_rows(src: &[u8], row_bytes: usize, padded_row_bytes: usize, rows: usize) -> Vec<u8> {
    let mut out = vec![0u8; padded_row_bytes * rows];
    for row in 0..rows {
        let src_off = row * row_bytes;
        let dst_off = row * padded_row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    out
}

impl ResourceManager {
    pub fn create_texture(&mut self, id: GuestResourceId, desc: TextureDesc) -> Result<()> {
        if self.textures.contains_key(&id) {
            return Err(anyhow!("texture id already exists: {}", id));
        }

        let info = format_info_for_texture_desc(&desc, self.device.features())?;

        let texture = Arc::new(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-d3d9.texture"),
            size: desc.kind.extent(),
            mip_level_count: desc.kind.mip_levels(),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: info.wgpu,
            usage: texture_usage_flags(desc.usage),
            view_formats: &[],
        }));

        let mut tex = Texture {
            desc,
            info,
            gpu: Some(texture),
            view: None,
            shadow: None,
            last_used_frame: self.frame_index,
            lock: None,
            lock_data: Vec::new(),
        };
        tex.view = Some(tex.create_default_view());

        if desc.pool == D3DPool::Managed {
            let (w, h) = desc.kind.dimensions();
            let levels = desc.kind.mip_levels();
            let layers = desc.kind.array_layers();
            let mut shadow = Vec::with_capacity((levels * layers) as usize);
            for _layer in 0..layers {
                for level in 0..levels {
                    shadow.push(vec![0u8; tex.info.d3d_mip_level_byte_len(w, h, level)]);
                }
            }
            tex.shadow = Some(shadow);
        }

        self.textures.insert(id, tex);
        Ok(())
    }

    pub fn texture_mut(&mut self, id: GuestResourceId) -> Result<&mut Texture> {
        self.textures
            .get_mut(&id)
            .ok_or_else(|| anyhow!("texture not found: {}", id))
    }

    pub fn texture(&self, id: GuestResourceId) -> Result<&Texture> {
        self.textures
            .get(&id)
            .ok_or_else(|| anyhow!("texture not found: {}", id))
    }

    pub fn destroy_texture(&mut self, id: GuestResourceId) -> bool {
        self.textures.remove(&id).is_some()
    }

    pub fn lock_texture_rect(
        &mut self,
        id: GuestResourceId,
        level: u32,
        layer: u32,
        flags: LockFlags,
    ) -> Result<LockedRect<'_>> {
        self.texture_mut(id)?.lock_rect(level, layer, flags)
    }

    pub fn unlock_texture_rect(&mut self, id: GuestResourceId) -> Result<()> {
        let frame = self.frame_index;
        let device = Arc::clone(&self.device);
        let uploads = &mut self.uploads;
        let tex = self
            .textures
            .get_mut(&id)
            .ok_or_else(|| anyhow!("texture not found: {}", id))?;
        tex.last_used_frame = frame;
        tex.unlock_rect(&device, uploads)
    }

    pub fn texture_view(&mut self, id: GuestResourceId) -> Result<Arc<wgpu::TextureView>> {
        let frame = self.frame_index;
        let device = Arc::clone(&self.device);
        let uploads = &mut self.uploads;
        let tex = self
            .textures
            .get_mut(&id)
            .ok_or_else(|| anyhow!("texture not found: {}", id))?;
        Ok(tex.view_arc(&device, uploads, frame))
    }
}

fn decompress_dxt_to_bgra8(
    format: D3DFormat,
    width: u32,
    height: u32,
    src: &[u8],
) -> Result<Vec<u8>> {
    match format {
        D3DFormat::Dxt1 => decompress_bc1_to_bgra8(width, height, src),
        D3DFormat::Dxt3 => decompress_bc2_to_bgra8(width, height, src),
        D3DFormat::Dxt5 => decompress_bc3_to_bgra8(width, height, src),
        _ => Err(anyhow!(
            "decompress_dxt_to_bgra8 called for non-DXT format {:?}",
            format
        )),
    }
}

fn decompress_bc1_to_bgra8(width: u32, height: u32, src: &[u8]) -> Result<Vec<u8>> {
    let blocks_w = width.div_ceil(4);
    let blocks_h = height.div_ceil(4);
    let expected = (blocks_w * blocks_h * 8) as usize;
    if src.len() < expected {
        return Err(anyhow!(
            "BC1 data too small (got {}, need {})",
            src.len(),
            expected
        ));
    }
    let mut out = vec![0u8; (width * height * 4) as usize];

    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_off = ((by * blocks_w + bx) * 8) as usize;
            let c0 = u16::from_le_bytes([src[block_off], src[block_off + 1]]);
            let c1 = u16::from_le_bytes([src[block_off + 2], src[block_off + 3]]);
            let indices = u32::from_le_bytes([
                src[block_off + 4],
                src[block_off + 5],
                src[block_off + 6],
                src[block_off + 7],
            ]);

            let mut colors = [[0u8; 4]; 4];
            colors[0] = rgb565_to_bgra8(c0, 255);
            colors[1] = rgb565_to_bgra8(c1, 255);

            if c0 > c1 {
                colors[2] = interp_bgra(colors[0], colors[1], 2, 1, 3);
                colors[3] = interp_bgra(colors[0], colors[1], 1, 2, 3);
            } else {
                colors[2] = interp_bgra(colors[0], colors[1], 1, 1, 2);
                colors[3] = [0, 0, 0, 0];
            }

            for py in 0..4u32 {
                for px in 0..4u32 {
                    let pixel_index = py * 4 + px;
                    let sel = ((indices >> (2 * pixel_index)) & 0x3) as usize;
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= width || y >= height {
                        continue;
                    }
                    let dst_off = ((y * width + x) * 4) as usize;
                    out[dst_off..dst_off + 4].copy_from_slice(&colors[sel]);
                }
            }
        }
    }

    Ok(out)
}

fn decompress_bc2_to_bgra8(width: u32, height: u32, src: &[u8]) -> Result<Vec<u8>> {
    let blocks_w = width.div_ceil(4);
    let blocks_h = height.div_ceil(4);
    let expected = (blocks_w * blocks_h * 16) as usize;
    if src.len() < expected {
        return Err(anyhow!(
            "BC2 data too small (got {}, need {})",
            src.len(),
            expected
        ));
    }
    let mut out = vec![0u8; (width * height * 4) as usize];

    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_off = ((by * blocks_w + bx) * 16) as usize;
            let alpha_bits = u64::from_le_bytes(src[block_off..block_off + 8].try_into().unwrap());
            let color_off = block_off + 8;
            let c0 = u16::from_le_bytes([src[color_off], src[color_off + 1]]);
            let c1 = u16::from_le_bytes([src[color_off + 2], src[color_off + 3]]);
            let indices = u32::from_le_bytes([
                src[color_off + 4],
                src[color_off + 5],
                src[color_off + 6],
                src[color_off + 7],
            ]);

            let mut colors = [[0u8; 4]; 4];
            colors[0] = rgb565_to_bgra8(c0, 255);
            colors[1] = rgb565_to_bgra8(c1, 255);
            // BC2 color always uses 4-color mode.
            colors[2] = interp_bgra(colors[0], colors[1], 2, 1, 3);
            colors[3] = interp_bgra(colors[0], colors[1], 1, 2, 3);

            for py in 0..4u32 {
                for px in 0..4u32 {
                    let pixel_index = py * 4 + px;
                    let sel = ((indices >> (2 * pixel_index)) & 0x3) as usize;
                    let a4 = ((alpha_bits >> (4 * pixel_index)) & 0xF) as u8;
                    let alpha = a4 * 17;
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= width || y >= height {
                        continue;
                    }
                    let dst_off = ((y * width + x) * 4) as usize;
                    let mut bgra = colors[sel];
                    bgra[3] = alpha;
                    out[dst_off..dst_off + 4].copy_from_slice(&bgra);
                }
            }
        }
    }

    Ok(out)
}

fn decompress_bc3_to_bgra8(width: u32, height: u32, src: &[u8]) -> Result<Vec<u8>> {
    let blocks_w = width.div_ceil(4);
    let blocks_h = height.div_ceil(4);
    let expected = (blocks_w * blocks_h * 16) as usize;
    if src.len() < expected {
        return Err(anyhow!(
            "BC3 data too small (got {}, need {})",
            src.len(),
            expected
        ));
    }
    let mut out = vec![0u8; (width * height * 4) as usize];

    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_off = ((by * blocks_w + bx) * 16) as usize;
            let a0 = src[block_off];
            let a1 = src[block_off + 1];
            let alpha_idx_bits: u64 = {
                let mut bytes = [0u8; 8];
                bytes[..6].copy_from_slice(&src[block_off + 2..block_off + 8]);
                u64::from_le_bytes(bytes)
            };
            let alpha_table = bc3_alpha_table(a0, a1);

            let color_off = block_off + 8;
            let c0 = u16::from_le_bytes([src[color_off], src[color_off + 1]]);
            let c1 = u16::from_le_bytes([src[color_off + 2], src[color_off + 3]]);
            let indices = u32::from_le_bytes([
                src[color_off + 4],
                src[color_off + 5],
                src[color_off + 6],
                src[color_off + 7],
            ]);

            let mut colors = [[0u8; 4]; 4];
            colors[0] = rgb565_to_bgra8(c0, 255);
            colors[1] = rgb565_to_bgra8(c1, 255);
            colors[2] = interp_bgra(colors[0], colors[1], 2, 1, 3);
            colors[3] = interp_bgra(colors[0], colors[1], 1, 2, 3);

            for py in 0..4u32 {
                for px in 0..4u32 {
                    let pixel_index = py * 4 + px;
                    let sel = ((indices >> (2 * pixel_index)) & 0x3) as usize;
                    let a_sel = ((alpha_idx_bits >> (3 * pixel_index)) & 0x7) as usize;
                    let alpha = alpha_table[a_sel];
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= width || y >= height {
                        continue;
                    }
                    let dst_off = ((y * width + x) * 4) as usize;
                    let mut bgra = colors[sel];
                    bgra[3] = alpha;
                    out[dst_off..dst_off + 4].copy_from_slice(&bgra);
                }
            }
        }
    }

    Ok(out)
}

fn bc3_alpha_table(a0: u8, a1: u8) -> [u8; 8] {
    let a0_u16 = u16::from(a0);
    let a1_u16 = u16::from(a1);

    let mut table = [0u8; 8];
    table[0] = a0;
    table[1] = a1;
    if a0_u16 > a1_u16 {
        table[2] = ((6u16 * a0_u16 + a1_u16) / 7) as u8;
        table[3] = ((5u16 * a0_u16 + 2u16 * a1_u16) / 7) as u8;
        table[4] = ((4u16 * a0_u16 + 3u16 * a1_u16) / 7) as u8;
        table[5] = ((3u16 * a0_u16 + 4u16 * a1_u16) / 7) as u8;
        table[6] = ((2u16 * a0_u16 + 5u16 * a1_u16) / 7) as u8;
        table[7] = ((a0_u16 + 6u16 * a1_u16) / 7) as u8;
    } else {
        table[2] = ((4u16 * a0_u16 + a1_u16) / 5) as u8;
        table[3] = ((3u16 * a0_u16 + 2u16 * a1_u16) / 5) as u8;
        table[4] = ((2u16 * a0_u16 + 3u16 * a1_u16) / 5) as u8;
        table[5] = ((a0_u16 + 4u16 * a1_u16) / 5) as u8;
        table[6] = 0;
        table[7] = 255;
    }
    table
}

fn rgb565_to_bgra8(c: u16, alpha: u8) -> [u8; 4] {
    let r5 = ((c >> 11) & 0x1F) as u8;
    let g6 = ((c >> 5) & 0x3F) as u8;
    let b5 = (c & 0x1F) as u8;

    let r = (r5 as u16 * 255 / 31) as u8;
    let g = (g6 as u16 * 255 / 63) as u8;
    let b = (b5 as u16 * 255 / 31) as u8;
    [b, g, r, alpha]
}

fn interp_bgra(c0: [u8; 4], c1: [u8; 4], w0: u16, w1: u16, denom: u16) -> [u8; 4] {
    [
        ((w0 * c0[0] as u16 + w1 * c1[0] as u16) / denom) as u8,
        ((w0 * c0[1] as u16 + w1 * c1[1] as u16) / denom) as u8,
        ((w0 * c0[2] as u16 + w1 * c1[2] as u16) / denom) as u8,
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bc1_solid_block(rgb565: u16) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..2].copy_from_slice(&rgb565.to_le_bytes());
        out[2..4].copy_from_slice(&rgb565.to_le_bytes());
        // All indices 0.
        out
    }

    #[test]
    fn bc1_decompress_solid_color() {
        // Solid red (approx) in RGB565: 0b11111_000000_00000
        let data = bc1_solid_block(0xf800);
        let out = decompress_bc1_to_bgra8(4, 4, &data).unwrap();
        // First pixel should be red-ish with full alpha.
        assert_eq!(out[0], 0); // B
        assert_eq!(out[1], 0); // G
        assert!(out[2] >= 250); // R
        assert_eq!(out[3], 255);
    }

    #[test]
    fn dxt1_native_bc_when_block_aligned() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Texture2D {
                width: 8,
                height: 8,
                levels: 4,
            },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bc1RgbaUnorm);
        assert!(!info.decompress_to_bgra8);
    }

    #[test]
    fn dxt1_fallback_to_bgra8_when_not_block_aligned() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Texture2D {
                width: 9,
                height: 9,
                levels: 4,
            },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
        assert!(info.decompress_to_bgra8);
    }

    #[test]
    fn dxt1_small_mips_are_allowed_for_native_bc() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Texture2D {
                width: 4,
                height: 4,
                levels: 3, // 4x4, 2x2, 1x1
            },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bc1RgbaUnorm);
        assert!(!info.decompress_to_bgra8);
    }

    #[test]
    fn dxt1_fallback_to_bgra8_when_intermediate_mip_not_block_aligned() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Texture2D {
                width: 12,
                height: 12,
                levels: 4, // 12x12, 6x6 (misaligned), 3x3, 1x1
            },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
        assert!(info.decompress_to_bgra8);
    }

    #[test]
    fn dxt3_native_bc_when_block_aligned() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Texture2D {
                width: 8,
                height: 8,
                levels: 4,
            },
            format: D3DFormat::Dxt3,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bc2RgbaUnorm);
        assert!(!info.decompress_to_bgra8);
    }

    #[test]
    fn dxt5_fallback_to_bgra8_when_not_block_aligned() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Texture2D {
                width: 9,
                height: 9,
                levels: 4,
            },
            format: D3DFormat::Dxt5,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
        assert!(info.decompress_to_bgra8);
    }

    #[test]
    fn dxt1_cube_native_bc_when_block_aligned() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Cube { size: 8, levels: 4 },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bc1RgbaUnorm);
        assert!(!info.decompress_to_bgra8);
    }

    #[test]
    fn dxt1_cube_fallback_to_bgra8_when_not_block_aligned() {
        let features = wgpu::Features::TEXTURE_COMPRESSION_BC;
        let desc = TextureDesc {
            kind: TextureKind::Cube { size: 9, levels: 4 },
            format: D3DFormat::Dxt1,
            pool: D3DPool::Default,
            usage: TextureUsageKind::Sampled,
        };

        let info = format_info_for_texture_desc(&desc, features).unwrap();
        assert_eq!(info.wgpu, wgpu::TextureFormat::Bgra8Unorm);
        assert!(info.decompress_to_bgra8);
    }
}
