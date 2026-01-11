use std::collections::HashMap;

use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuHandle, AerogpuShaderStage,
    AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_SCANOUT, AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use anyhow::{anyhow, bail, Context, Result};

use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    MappedInputLayout, VsInputSignatureElement,
};
use crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap;
use crate::{parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, ShaderStage, Sm4Program};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackingInfo {
    pub alloc_id: u32,
    pub alloc_offset_bytes: u32,
}

#[derive(Debug)]
pub struct BufferResource {
    pub buffer: wgpu::Buffer,
    pub size: u64,
    pub usage_flags: u32,
    pub backing: Option<BackingInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub format: wgpu::TextureFormat,
    pub row_pitch_bytes: u32,
}

#[derive(Debug)]
pub struct Texture2dResource {
    pub texture: wgpu::Texture,
    pub desc: Texture2dDesc,
    pub usage_flags: u32,
    pub backing: Option<BackingInfo>,
    host_shadow: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default)]
pub struct ShaderReflection {
    /// Vertex shader input signature mapping, derived from the DXBC `ISGN` chunk when available.
    pub vs_input_signature: Vec<VsInputSignatureElement>,
}

#[derive(Debug)]
pub struct ShaderResource {
    pub stage: AerogpuShaderStage,
    pub dxbc_hash_fnv1a64: u64,
    pub wgsl: String,
    pub module: wgpu::ShaderModule,
    pub reflection: ShaderReflection,
}

#[derive(Clone, Debug)]
pub struct InputLayoutResource {
    pub layout: InputLayoutDesc,
    pub mapping_cache: HashMap<u64, MappedInputLayout>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRange {
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

impl DirtyRange {
    pub fn end_bytes(self) -> Result<u64> {
        self.offset_bytes
            .checked_add(self.size_bytes)
            .ok_or_else(|| anyhow!("byte range overflows u64"))
    }
}

pub trait GuestMemory {
    fn read(&self, guest_phys_addr: u64, dst: &mut [u8]) -> Result<()>;
}

pub struct AerogpuResourceManager {
    device: wgpu::Device,
    queue: wgpu::Queue,

    buffers: HashMap<AerogpuHandle, BufferResource>,
    textures2d: HashMap<AerogpuHandle, Texture2dResource>,
    shaders: HashMap<AerogpuHandle, ShaderResource>,
    input_layouts: HashMap<AerogpuHandle, InputLayoutResource>,
}

impl AerogpuResourceManager {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            device,
            queue,
            buffers: HashMap::new(),
            textures2d: HashMap::new(),
            shaders: HashMap::new(),
            input_layouts: HashMap::new(),
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn create_buffer(
        &mut self,
        handle: AerogpuHandle,
        usage_flags: u32,
        size_bytes: u64,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
    ) -> Result<()> {
        self.ensure_resource_handle_unused(handle)?;

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu buffer"),
            size: size_bytes,
            usage: map_buffer_usage_flags(usage_flags),
            mapped_at_creation: false,
        });

        let backing = (backing_alloc_id != 0).then_some(BackingInfo {
            alloc_id: backing_alloc_id,
            alloc_offset_bytes: backing_offset_bytes,
        });

        self.buffers.insert(
            handle,
            BufferResource {
                buffer,
                size: size_bytes,
                usage_flags,
                backing,
            },
        );
        Ok(())
    }

    pub fn create_texture2d(
        &mut self,
        handle: AerogpuHandle,
        usage_flags: u32,
        format: u32,
        width: u32,
        height: u32,
        mip_levels: u32,
        array_layers: u32,
        row_pitch_bytes: u32,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
    ) -> Result<()> {
        self.ensure_resource_handle_unused(handle)?;

        if width == 0 || height == 0 {
            bail!("CreateTexture2d: width/height must be non-zero");
        }
        if mip_levels == 0 {
            bail!("CreateTexture2d: mip_levels must be >= 1");
        }
        if array_layers == 0 {
            bail!("CreateTexture2d: array_layers must be >= 1");
        }
        if backing_alloc_id != 0 && row_pitch_bytes == 0 {
            bail!("CreateTexture2d: row_pitch_bytes is required for allocation-backed textures");
        }

        let wgpu_format = map_aerogpu_format(format)?;
        if row_pitch_bytes != 0 {
            let bpp = bytes_per_texel(wgpu_format)?;
            let min_row_pitch = width
                .checked_mul(bpp)
                .ok_or_else(|| anyhow!("CreateTexture2d: row_pitch overflow"))?;
            if row_pitch_bytes < min_row_pitch {
                bail!(
                    "CreateTexture2d: row_pitch_bytes {} is smaller than required {}",
                    row_pitch_bytes,
                    min_row_pitch
                );
            }
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: array_layers,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage: map_texture_usage_flags(usage_flags),
            view_formats: &[],
        });

        let backing = (backing_alloc_id != 0).then_some(BackingInfo {
            alloc_id: backing_alloc_id,
            alloc_offset_bytes: backing_offset_bytes,
        });

        let desc = Texture2dDesc {
            width,
            height,
            mip_levels,
            array_layers,
            format: wgpu_format,
            row_pitch_bytes,
        };

        let host_shadow = if backing.is_some() {
            None
        } else {
            Some(vec![0u8; texture_total_size_bytes(&desc)?])
        };

        self.textures2d.insert(
            handle,
            Texture2dResource {
                texture,
                desc,
                usage_flags,
                backing,
                host_shadow,
            },
        );
        Ok(())
    }

    pub fn create_shader_dxbc(
        &mut self,
        handle: AerogpuHandle,
        stage: u32,
        dxbc_bytes: &[u8],
    ) -> Result<()> {
        self.ensure_shader_handle_unused(handle)?;

        let stage = match stage {
            x if x == AerogpuShaderStage::Vertex as u32 => AerogpuShaderStage::Vertex,
            x if x == AerogpuShaderStage::Pixel as u32 => AerogpuShaderStage::Pixel,
            x if x == AerogpuShaderStage::Compute as u32 => AerogpuShaderStage::Compute,
            _ => bail!("CreateShaderDxbc: unknown aerogpu_shader_stage {stage}"),
        };

        let dxbc_hash_fnv1a64 = fnv1a64(dxbc_bytes);

        let dxbc = DxbcFile::parse(dxbc_bytes).context("parse DXBC container")?;
        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("parse SM4/SM5 program")?;
        let parsed_stage = match program.stage {
            ShaderStage::Vertex => AerogpuShaderStage::Vertex,
            ShaderStage::Pixel => AerogpuShaderStage::Pixel,
            ShaderStage::Compute => AerogpuShaderStage::Compute,
            other => bail!("CreateShaderDxbc: unsupported DXBC shader stage {other:?}"),
        };
        if parsed_stage != stage {
            bail!("CreateShaderDxbc: stage mismatch (cmd={stage:?}, dxbc={parsed_stage:?})");
        }
        let wgsl = match try_translate_sm4_signature_driven(&dxbc, &program, &signatures) {
            Ok(wgsl) => wgsl,
            Err(_) => translate_sm4_to_wgsl_bootstrap(&program)
                .map_err(|e| anyhow!("DXBC->WGSL translation failed: {e}"))?
                .wgsl,
        };

        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aerogpu shader module"),
                source: wgpu::ShaderSource::Wgsl(wgsl.clone().into()),
            });

        let reflection = build_shader_reflection(stage, &signatures);

        self.shaders.insert(
            handle,
            ShaderResource {
                stage,
                dxbc_hash_fnv1a64,
                wgsl,
                module,
                reflection,
            },
        );
        Ok(())
    }

    pub fn create_input_layout(&mut self, handle: AerogpuHandle, blob: Vec<u8>) -> Result<()> {
        self.ensure_input_layout_handle_unused(handle)?;

        self.input_layouts.insert(
            handle,
            InputLayoutResource {
                layout: InputLayoutDesc::parse(&blob)
                    .map_err(|e| anyhow!("failed to parse ILAY input layout blob: {e}"))?,
                mapping_cache: HashMap::new(),
            },
        );
        Ok(())
    }

    pub fn destroy_resource(&mut self, handle: AerogpuHandle) -> Result<()> {
        let mut removed = false;
        removed |= self.buffers.remove(&handle).is_some();
        removed |= self.textures2d.remove(&handle).is_some();
        if removed {
            Ok(())
        } else {
            bail!("DestroyResource: unknown handle {handle}")
        }
    }

    pub fn destroy_shader(&mut self, handle: AerogpuHandle) -> Result<()> {
        if self.shaders.remove(&handle).is_none() {
            bail!("DestroyShader: unknown handle {handle}");
        }
        Ok(())
    }

    pub fn destroy_input_layout(&mut self, handle: AerogpuHandle) -> Result<()> {
        if self.input_layouts.remove(&handle).is_none() {
            bail!("DestroyInputLayout: unknown handle {handle}");
        }
        Ok(())
    }

    pub fn buffer(&self, handle: AerogpuHandle) -> Result<&BufferResource> {
        self.buffers
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown buffer handle {handle}"))
    }

    pub fn texture2d(&self, handle: AerogpuHandle) -> Result<&Texture2dResource> {
        self.textures2d
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown texture2d handle {handle}"))
    }

    pub fn shader(&self, handle: AerogpuHandle) -> Result<&ShaderResource> {
        self.shaders
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown shader handle {handle}"))
    }

    pub fn input_layout(&self, handle: AerogpuHandle) -> Result<&InputLayoutResource> {
        self.input_layouts
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown input layout handle {handle}"))
    }

    /// Map an ILAY input layout + currently-bound vertex buffer strides into WebGPU vertex layouts.
    ///
    /// The mapping is cached per input layout, keyed by `(vertex_shader_dxbc_hash, slot_strides)`.
    ///
    /// If the vertex shader's `ISGN` signature is unavailable, this falls back to mapping ILAY
    /// semantics in declaration order to shader locations `0..N` (sufficient for bring-up shaders).
    pub fn input_layout_vertex_buffer_layouts(
        &mut self,
        input_layout_handle: AerogpuHandle,
        vertex_shader_handle: AerogpuHandle,
        slot_strides: &[u32],
    ) -> Result<&MappedInputLayout> {
        let vs = self
            .shaders
            .get(&vertex_shader_handle)
            .ok_or_else(|| anyhow!("unknown shader handle {vertex_shader_handle}"))?;
        if vs.stage != AerogpuShaderStage::Vertex {
            bail!("shader {vertex_shader_handle} is not a vertex shader");
        }

        let layout = self
            .input_layouts
            .get_mut(&input_layout_handle)
            .ok_or_else(|| anyhow!("unknown input layout handle {input_layout_handle}"))?;

        let cache_key = hash_input_layout_mapping_key(vs.dxbc_hash_fnv1a64, slot_strides);
        if !layout.mapping_cache.contains_key(&cache_key) {
            let desc = &layout.layout;

            let vs_signature = if vs.reflection.vs_input_signature.is_empty() {
                build_fallback_vs_signature(desc)
            } else {
                vs.reflection.vs_input_signature.clone()
            };

            let binding = InputLayoutBinding::new(desc, slot_strides);
            let mapped = map_layout_to_shader_locations_compact(&binding, &vs_signature)
                .map_err(|e| anyhow!("{e}"))?;
            layout.mapping_cache.insert(cache_key, mapped);
        }

        Ok(layout
            .mapping_cache
            .get(&cache_key)
            .expect("mapping cache entry must exist"))
    }

    pub fn upload_resource(
        &mut self,
        handle: AerogpuHandle,
        range: DirtyRange,
        bytes: &[u8],
    ) -> Result<()> {
        let _end = range.end_bytes()?;
        let size_usize: usize = range
            .size_bytes
            .try_into()
            .context("UploadResource: size too large")?;
        if bytes.len() != size_usize {
            bail!(
                "UploadResource: payload size mismatch: cmd says {} bytes, payload has {}",
                range.size_bytes,
                bytes.len()
            );
        }

        if let Some(buf) = self.buffers.get(&handle) {
            validate_range_in_resource(range, buf.size)?;
            if buf.backing.is_some() {
                bail!("UploadResource: buffer {handle} is backed by a guest allocation");
            }
            self.queue
                .write_buffer(&buf.buffer, range.offset_bytes, bytes);
            return Ok(());
        }

        if let Some(tex) = self.textures2d.get_mut(&handle) {
            let total_bytes = texture_total_size_bytes(&tex.desc)? as u64;
            validate_range_in_resource(range, total_bytes)?;
            if tex.backing.is_some() {
                bail!("UploadResource: texture {handle} is backed by a guest allocation");
            }

            let shadow = tex
                .host_shadow
                .as_mut()
                .ok_or_else(|| anyhow!("UploadResource: texture {handle} missing host shadow"))?;
            let start = range.offset_bytes as usize;
            shadow[start..start + bytes.len()].copy_from_slice(bytes);

            // For P0, conservatively re-upload the entire texture from the host shadow.
            upload_texture_from_linear_bytes(&self.queue, &tex.texture, &tex.desc, shadow)?;
            return Ok(());
        }

        bail!("UploadResource: unknown handle {handle}");
    }

    /// Ensure a backed buffer is present in the corresponding `wgpu::Buffer`.
    ///
    /// P0 behaviour is conservative: if any dirty range is reported, the entire
    /// buffer is re-uploaded from the guest backing allocation.
    pub fn ensure_buffer_uploaded(
        &mut self,
        handle: AerogpuHandle,
        dirty: DirtyRange,
        guest_mem: &dyn GuestMemory,
        alloc_table: &HashMap<u32, AerogpuAllocEntry>,
    ) -> Result<()> {
        let buf = self
            .buffers
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown buffer handle {handle}"))?;

        validate_range_in_resource(dirty, buf.size)?;

        let Some(backing) = buf.backing else {
            return Ok(());
        };
        let alloc = alloc_table.get(&backing.alloc_id).ok_or_else(|| {
            anyhow!(
                "missing alloc entry {} for buffer {handle}",
                backing.alloc_id
            )
        })?;

        let start = alloc
            .gpa
            .checked_add(backing.alloc_offset_bytes as u64)
            .ok_or_else(|| anyhow!("alloc GPA+offset overflows u64"))?;
        let end = (backing.alloc_offset_bytes as u64)
            .checked_add(buf.size)
            .ok_or_else(|| anyhow!("alloc offset+buffer size overflows u64"))?;
        if end > alloc.size_bytes {
            bail!(
                "buffer {handle} backing range out of bounds: alloc_size={} offset={} size={}",
                alloc.size_bytes,
                backing.alloc_offset_bytes,
                buf.size
            );
        }

        let size_usize: usize = buf.size.try_into().context("buffer too large")?;
        let mut tmp = vec![0u8; size_usize];
        guest_mem
            .read(start, &mut tmp)
            .with_context(|| format!("read guest backing for buffer {handle}"))?;

        self.queue.write_buffer(&buf.buffer, 0, &tmp);
        Ok(())
    }

    /// Ensure a backed texture is present in the corresponding `wgpu::Texture`.
    ///
    /// P0 behaviour is conservative: if any dirty range is reported, the entire
    /// texture is re-uploaded from the guest backing allocation.
    pub fn ensure_texture_uploaded(
        &mut self,
        handle: AerogpuHandle,
        dirty: DirtyRange,
        guest_mem: &dyn GuestMemory,
        alloc_table: &HashMap<u32, AerogpuAllocEntry>,
    ) -> Result<()> {
        let tex = self
            .textures2d
            .get_mut(&handle)
            .ok_or_else(|| anyhow!("unknown texture2d handle {handle}"))?;

        let total_bytes = texture_total_size_bytes(&tex.desc)? as u64;
        validate_range_in_resource(dirty, total_bytes)?;

        let Some(backing) = tex.backing else {
            return Ok(());
        };
        let alloc = alloc_table.get(&backing.alloc_id).ok_or_else(|| {
            anyhow!(
                "missing alloc entry {} for texture {handle}",
                backing.alloc_id
            )
        })?;

        let start = alloc
            .gpa
            .checked_add(backing.alloc_offset_bytes as u64)
            .ok_or_else(|| anyhow!("alloc GPA+offset overflows u64"))?;
        let end = (backing.alloc_offset_bytes as u64)
            .checked_add(total_bytes)
            .ok_or_else(|| anyhow!("alloc offset+texture size overflows u64"))?;
        if end > alloc.size_bytes {
            bail!(
                "texture {handle} backing range out of bounds: alloc_size={} offset={} size={}",
                alloc.size_bytes,
                backing.alloc_offset_bytes,
                total_bytes
            );
        }

        let size_usize: usize = total_bytes.try_into().context("texture too large")?;
        let mut tmp = vec![0u8; size_usize];
        guest_mem
            .read(start, &mut tmp)
            .with_context(|| format!("read guest backing for texture {handle}"))?;

        upload_texture_from_linear_bytes(&self.queue, &tex.texture, &tex.desc, &tmp)?;
        Ok(())
    }

    fn ensure_resource_handle_unused(&self, handle: AerogpuHandle) -> Result<()> {
        if self.buffers.contains_key(&handle) || self.textures2d.contains_key(&handle) {
            bail!("resource handle {handle} is already in use");
        }
        Ok(())
    }

    fn ensure_shader_handle_unused(&self, handle: AerogpuHandle) -> Result<()> {
        if self.shaders.contains_key(&handle) {
            bail!("shader handle {handle} is already in use");
        }
        Ok(())
    }

    fn ensure_input_layout_handle_unused(&self, handle: AerogpuHandle) -> Result<()> {
        if self.input_layouts.contains_key(&handle) {
            bail!("input layout handle {handle} is already in use");
        }
        Ok(())
    }
}

fn try_translate_sm4_signature_driven(
    dxbc: &DxbcFile<'_>,
    program: &Sm4Program,
    signatures: &crate::ShaderSignatures,
) -> Result<String> {
    let module = program.decode().context("decode SM4/5 token stream")?;
    let translated = translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")?;
    Ok(translated.wgsl)
}

fn build_shader_reflection(
    stage: AerogpuShaderStage,
    signatures: &crate::ShaderSignatures,
) -> ShaderReflection {
    let mut reflection = ShaderReflection::default();

    if stage == AerogpuShaderStage::Vertex {
        if let Some(isgn) = signatures.isgn.as_ref() {
            reflection.vs_input_signature = isgn
                .parameters
                .iter()
                .map(|p| VsInputSignatureElement {
                    semantic_name_hash: fnv1a_32(p.semantic_name.as_bytes()),
                    semantic_index: p.semantic_index,
                    input_register: p.register,
                })
                .collect();
        }
    }

    reflection
}

fn build_fallback_vs_signature(desc: &InputLayoutDesc) -> Vec<VsInputSignatureElement> {
    let mut seen: HashMap<(u32, u32), u32> = HashMap::new();
    let mut out: Vec<VsInputSignatureElement> = Vec::new();

    for elem in &desc.elements {
        let key = (elem.semantic_name_hash, elem.semantic_index);
        if seen.contains_key(&key) {
            continue;
        }
        let reg = out.len() as u32;
        seen.insert(key, reg);
        out.push(VsInputSignatureElement {
            semantic_name_hash: key.0,
            semantic_index: key.1,
            input_register: reg,
        });
    }

    out
}

fn hash_input_layout_mapping_key(vs_dxbc_hash_fnv1a64: u64, slot_strides: &[u32]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, &vs_dxbc_hash_fnv1a64.to_le_bytes());
    fnv1a64_update(&mut hash, &(slot_strides.len() as u32).to_le_bytes());
    for &stride in slot_strides {
        fnv1a64_update(&mut hash, &stride.to_le_bytes());
    }
    hash
}

pub fn map_aerogpu_format(format: u32) -> Result<wgpu::TextureFormat> {
    Ok(match format {
        x if x == AerogpuFormat::B8G8R8A8Unorm as u32 => wgpu::TextureFormat::Bgra8Unorm,
        x if x == AerogpuFormat::B8G8R8X8Unorm as u32 => wgpu::TextureFormat::Bgra8Unorm,
        x if x == AerogpuFormat::R8G8B8A8Unorm as u32 => wgpu::TextureFormat::Rgba8Unorm,
        x if x == AerogpuFormat::R8G8B8X8Unorm as u32 => wgpu::TextureFormat::Rgba8Unorm,
        x if x == AerogpuFormat::D24UnormS8Uint as u32 => wgpu::TextureFormat::Depth24PlusStencil8,
        x if x == AerogpuFormat::D32Float as u32 => wgpu::TextureFormat::Depth32Float,
        _ => bail!("unsupported aerogpu_format {format}"),
    })
}

pub fn map_buffer_usage_flags(usage_flags: u32) -> wgpu::BufferUsages {
    let mut out = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    if (usage_flags & AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER) != 0 {
        out |= wgpu::BufferUsages::VERTEX;
    }
    if (usage_flags & AEROGPU_RESOURCE_USAGE_INDEX_BUFFER) != 0 {
        out |= wgpu::BufferUsages::INDEX;
    }
    if (usage_flags & AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER) != 0 {
        out |= wgpu::BufferUsages::UNIFORM;
    }
    out
}

pub fn map_texture_usage_flags(usage_flags: u32) -> wgpu::TextureUsages {
    let mut out = wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
    if (usage_flags & AEROGPU_RESOURCE_USAGE_TEXTURE) != 0 {
        out |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    if (usage_flags
        & (AEROGPU_RESOURCE_USAGE_RENDER_TARGET
            | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL
            | AEROGPU_RESOURCE_USAGE_SCANOUT))
        != 0
    {
        out |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    out
}

pub fn validate_range_in_resource(range: DirtyRange, total_size_bytes: u64) -> Result<()> {
    let end = range.end_bytes()?;
    if range.offset_bytes > total_size_bytes {
        bail!(
            "byte range out of bounds: offset={} total={}",
            range.offset_bytes,
            total_size_bytes
        );
    }
    if end > total_size_bytes {
        bail!(
            "byte range out of bounds: offset={} size={} end={} total={}",
            range.offset_bytes,
            range.size_bytes,
            end,
            total_size_bytes
        );
    }
    Ok(())
}

fn bytes_per_texel(format: wgpu::TextureFormat) -> Result<u32> {
    Ok(match format {
        wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Depth24PlusStencil8
        | wgpu::TextureFormat::Depth32Float => 4,
        other => bail!("bytes_per_texel: unsupported format {other:?}"),
    })
}

fn mip_extent(v: u32, level: u32) -> u32 {
    (v >> level).max(1)
}

fn texture_mip_row_pitch_bytes(desc: &Texture2dDesc, mip_level: u32) -> Result<u32> {
    let bpp = bytes_per_texel(desc.format)?;
    if mip_level == 0 {
        let row_pitch = if desc.row_pitch_bytes != 0 {
            desc.row_pitch_bytes
        } else {
            desc.width
                .checked_mul(bpp)
                .ok_or_else(|| anyhow!("row_pitch overflow"))?
        };
        let min_row_pitch = desc
            .width
            .checked_mul(bpp)
            .ok_or_else(|| anyhow!("row_pitch overflow"))?;
        if row_pitch < min_row_pitch {
            bail!(
                "row_pitch_bytes {} is smaller than required {}",
                row_pitch,
                min_row_pitch
            );
        }
        return Ok(row_pitch);
    }

    let width = mip_extent(desc.width, mip_level);
    Ok(width
        .checked_mul(bpp)
        .ok_or_else(|| anyhow!("row_pitch overflow"))?)
}

fn texture_subresource_size_bytes(desc: &Texture2dDesc, mip_level: u32) -> Result<u64> {
    let row_pitch = texture_mip_row_pitch_bytes(desc, mip_level)? as u64;
    let height = mip_extent(desc.height, mip_level) as u64;
    row_pitch
        .checked_mul(height)
        .ok_or_else(|| anyhow!("subresource size overflows u64"))
}

pub fn texture_total_size_bytes(desc: &Texture2dDesc) -> Result<usize> {
    let mut total: u64 = 0;
    for _layer in 0..desc.array_layers {
        for mip in 0..desc.mip_levels {
            total = total
                .checked_add(texture_subresource_size_bytes(desc, mip)?)
                .ok_or_else(|| anyhow!("texture size overflows u64"))?;
        }
    }
    total.try_into().context("texture too large")
}

fn upload_texture_from_linear_bytes(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    desc: &Texture2dDesc,
    bytes: &[u8],
) -> Result<()> {
    let expected = texture_total_size_bytes(desc)?;
    if bytes.len() != expected {
        bail!(
            "texture upload size mismatch: expected {} bytes, got {}",
            expected,
            bytes.len()
        );
    }

    let bpp = bytes_per_texel(desc.format)?;
    let mut offset = 0usize;
    for layer in 0..desc.array_layers {
        for mip in 0..desc.mip_levels {
            let width = mip_extent(desc.width, mip);
            let height = mip_extent(desc.height, mip);
            let row_pitch = texture_mip_row_pitch_bytes(desc, mip)?;
            let subresource_len = texture_subresource_size_bytes(desc, mip)? as usize;
            let data = bytes
                .get(offset..offset + subresource_len)
                .ok_or_else(|| anyhow!("texture upload out of bounds"))?;

            let unpadded_bytes_per_row = width
                .checked_mul(bpp)
                .ok_or_else(|| anyhow!("bytes_per_row overflow"))?;

            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let needs_repack = height > 1 && (row_pitch % align != 0);

            let mut repacked = None;
            let upload_bytes_per_row = if needs_repack {
                let padded = ((unpadded_bytes_per_row + align - 1) / align) * align;
                let mut tmp = vec![0u8; padded as usize * height as usize];
                for y in 0..height as usize {
                    let src_start = y * row_pitch as usize;
                    let dst_start = y * padded as usize;
                    tmp[dst_start..dst_start + unpadded_bytes_per_row as usize].copy_from_slice(
                        &data[src_start..src_start + unpadded_bytes_per_row as usize],
                    );
                }
                repacked = Some(tmp);
                padded
            } else {
                row_pitch
            };
            let upload_bytes = repacked.as_deref().unwrap_or(data);

            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                upload_bytes,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(upload_bytes_per_row),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );

            offset += subresource_len;
        }
    }

    Ok(())
}

const FNV1A64_OFFSET_BASIS: u64 = 14695981039346656037;
const FNV1A64_PRIME: u64 = 1099511628211;

fn fnv1a64_update(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(FNV1A64_PRIME);
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, bytes);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_layout::{AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION};

    #[test]
    fn maps_aerogpu_formats() {
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::B8G8R8A8Unorm as u32).unwrap(),
            wgpu::TextureFormat::Bgra8Unorm
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::R8G8B8A8Unorm as u32).unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
        assert!(map_aerogpu_format(AerogpuFormat::Invalid as u32).is_err());
    }

    #[test]
    fn maps_usage_flags_conservatively() {
        let bu = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
        assert!(bu.contains(wgpu::BufferUsages::COPY_SRC));
        assert!(bu.contains(wgpu::BufferUsages::COPY_DST));
        assert!(bu.contains(wgpu::BufferUsages::VERTEX));

        let tu = map_texture_usage_flags(AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
        assert!(tu.contains(wgpu::TextureUsages::COPY_SRC));
        assert!(tu.contains(wgpu::TextureUsages::COPY_DST));
        assert!(tu.contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
    }

    #[test]
    fn validates_byte_ranges() {
        validate_range_in_resource(
            DirtyRange {
                offset_bytes: 0,
                size_bytes: 4,
            },
            4,
        )
        .unwrap();
        assert!(validate_range_in_resource(
            DirtyRange {
                offset_bytes: 4,
                size_bytes: 1,
            },
            4
        )
        .is_err());
    }

    #[test]
    fn computes_texture_total_size() {
        let desc = Texture2dDesc {
            width: 4,
            height: 4,
            mip_levels: 2,
            array_layers: 1,
            format: wgpu::TextureFormat::Rgba8Unorm,
            row_pitch_bytes: 0,
        };
        // mip0: 4*4*4 = 64, mip1: 2*2*4 = 16
        assert_eq!(texture_total_size_bytes(&desc).unwrap(), 80);
    }

    #[test]
    fn parses_input_layout_blob_v1() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
        blob.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
        blob.extend_from_slice(&1u32.to_le_bytes()); // element_count
        blob.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                     // element
        blob.extend_from_slice(&123u32.to_le_bytes()); // semantic_name_hash
        blob.extend_from_slice(&2u32.to_le_bytes()); // semantic_index
        blob.extend_from_slice(&28u32.to_le_bytes()); // dxgi_format
        blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot
        blob.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
        blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class
        blob.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

        let desc = InputLayoutDesc::parse(&blob).unwrap();
        assert_eq!(desc.header.magic, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        assert_eq!(desc.header.version, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        assert_eq!(desc.header.element_count, 1);
        assert_eq!(desc.elements.len(), 1);
        assert_eq!(desc.elements[0].semantic_name_hash, 123);
        assert_eq!(desc.elements[0].dxgi_format, 28);
    }
}
